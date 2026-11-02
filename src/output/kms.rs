//! DRM/KMS output: dumb buffers with vsynced page flips on the login
//! screen, a static image on any other connected screen.

use super::{Output, Screen};
use drm::{
    Device as _,
    buffer::{Buffer as _, DrmFourcc},
    control::{
        Device as ControlDevice, Mode, ModeTypeFlags, PageFlipFlags, connector, crtc, dumbbuffer::DumbBuffer,
        framebuffer,
    },
};
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::fd::{AsFd, AsRawFd, BorrowedFd},
};

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl ControlDevice for Card {}

/// One connector driven by one CRTC.
struct Head {
    name: String,
    /// See `Screen::id`.
    id: String,
    /// A laptop panel under a closed lid: kept dark unless chosen.
    lid_closed: bool,
    conn: connector::Handle,
    crtc: crtc::Handle,
    mode: Mode,
    /// The static image shown while the login is elsewhere.
    blank: Option<(DumbBuffer, framebuffer::Handle)>,
    /// Front and back buffer, once the login has been here.
    flip: Vec<(DumbBuffer, framebuffer::Handle)>,
}

/// The login is drawn on the current head with page flips; other
/// connected screens get a single static image, drawn once.
pub struct Kms {
    card: Card,
    heads: Vec<Head>,
    cur: usize,
    front: usize,
    flip_pending: bool,
    /// The current head needs a modeset.
    needs_modeset: bool,
    /// So do all the others (at start and after a VT switch).
    others_stale: bool,
    warning: Option<String>,
}

impl Kms {
    /// `output` names the connector for the login (e.g. "DP-1"); the first
    /// connected one is used if it is unset or not connected. `fill` draws
    /// the static image for the other screens.
    pub fn open(output: Option<&str>, fill: &mut dyn FnMut(usize, usize, &mut [u32], usize)) -> io::Result<Kms> {
        let mut last_err = io::Error::new(io::ErrorKind::NotFound, "no DRM device with a connected display");
        for i in 0..8 {
            let path = format!("/dev/dri/card{i}");
            let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else { continue };
            let card = Card(file);
            match Self::heads(&card, i) {
                Ok(heads) => return Self::finish(card, heads, output, fill),
                Err(e) => last_err = io::Error::new(e.kind(), format!("{path}: {e}")),
            }
        }
        Err(last_err)
    }

    /// Describes the connected screens, in the order F3 cycles through,
    /// without taking over the display. Works while a compositor is running.
    pub fn list(output: Option<&str>) -> io::Result<Vec<String>> {
        for i in 0..8 {
            let Ok(file) = OpenOptions::new().read(true).open(format!("/dev/dri/card{i}")) else { continue };
            let Ok(heads) = Self::heads(&Card(file), i) else { continue };
            let cur = Self::pick(&heads, output).0;
            return Ok(heads
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let (w, ht) = h.mode.size();
                    let role = match () {
                        _ if i == cur => "login",
                        _ if h.lid_closed => "off (lid closed)",
                        _ => "empty tube",
                    };
                    format!("{}  {w}x{ht}@{}  id {}  -> {role}", h.name, h.mode.vrefresh(), h.id)
                })
                .collect());
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no DRM device with a connected display"))
    }

    /// Every connected connector of card `card_no`, each with its own
    /// CRTC. Panels under a closed lid go last.
    fn heads(card: &Card, card_no: usize) -> io::Result<Vec<Head>> {
        let res = card.resource_handles()?;
        let closed = lid_closed();
        let mut heads: Vec<Head> = Vec::new();
        for &handle in res.connectors() {
            let info = card.get_connector(handle, false)?;
            if info.state() != connector::State::Connected || info.modes().is_empty() {
                continue;
            }
            let mode = *info
                .modes()
                .iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .unwrap_or(&info.modes()[0]);

            // Prefer the CRTC already driving this connector, then any free one.
            let encoders = info.current_encoder().into_iter().chain(info.encoders().iter().copied());
            let free = |c: &crtc::Handle| !heads.iter().any(|h| h.crtc == *c);
            let crtc = encoders.filter_map(|e| card.get_encoder(e).ok()).find_map(|enc| {
                enc.crtc()
                    .filter(free)
                    .or_else(|| res.filter_crtcs(enc.possible_crtcs()).into_iter().find(free))
            });
            if let Some(crtc) = crtc {
                let name = info.to_string();
                let internal = matches!(
                    info.interface(),
                    connector::Interface::EmbeddedDisplayPort | connector::Interface::LVDS | connector::Interface::DSI
                );
                heads.push(Head {
                    id: monitor_id(card_no, &name),
                    lid_closed: internal && closed,
                    name,
                    conn: handle,
                    crtc,
                    mode,
                    blank: None,
                    flip: Vec::new(),
                });
            }
        }
        if heads.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no connected display"));
        }
        heads.sort_by_key(|h| h.lid_closed);
        Ok(heads)
    }

    /// The head for the login: `output` if it is connected and not under a
    /// closed lid, else the first. Also returns a warning if `output` is
    /// not connected.
    fn pick(heads: &[Head], output: Option<&str>) -> (usize, Option<String>) {
        let Some(name) = output else { return (0, None) };
        match heads.iter().position(|h| h.name == name) {
            Some(i) if !heads[i].lid_closed => (i, None),
            Some(_) => (0, None),
            None => (0, Some(format!("OUTPUT {name} NOT CONNECTED"))),
        }
    }

    fn finish(
        card: Card,
        mut heads: Vec<Head>,
        output: Option<&str>,
        fill: &mut dyn FnMut(usize, usize, &mut [u32], usize),
    ) -> io::Result<Kms> {
        let (cur, warning) = Self::pick(&heads, output);
        for head in &mut heads {
            if head.lid_closed {
                continue;
            }
            let (w, h) = head.mode.size();
            let mut db = card.create_dumb_buffer((w as u32, h as u32), DrmFourcc::Xrgb8888, 32)?;
            let fb = card.add_framebuffer(&db, 24, 32)?;
            let stride = db.pitch() as usize / 4;
            {
                let mut map = card.map_dumb_buffer(&mut db)?;
                let (_, pixels, _) = unsafe { map.align_to_mut::<u32>() };
                fill(w as usize, h as usize, pixels, stride);
            }
            head.blank = Some((db, fb));
        }
        let mut kms = Kms {
            card,
            heads,
            cur,
            front: 0,
            flip_pending: false,
            needs_modeset: true,
            others_stale: true,
            warning,
        };
        kms.add_flip_buffers(cur)?;
        Ok(kms)
    }

    fn add_flip_buffers(&mut self, i: usize) -> io::Result<()> {
        let head = &mut self.heads[i];
        let (w, h) = head.mode.size();
        while head.flip.len() < 2 {
            let db = self.card.create_dumb_buffer((w as u32, h as u32), DrmFourcc::Xrgb8888, 32)?;
            let fb = self.card.add_framebuffer(&db, 24, 32)?;
            head.flip.push((db, fb));
        }
        Ok(())
    }

    fn wait_flip(&mut self) {
        if !self.flip_pending {
            return;
        }
        let mut pfd = libc::pollfd { fd: self.card.0.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // Bounded wait: if we lost the VT the event may never arrive.
        if unsafe { libc::poll(&mut pfd, 1, 100) } > 0 {
            if let Ok(events) = self.card.receive_events() {
                events.for_each(drop);
            }
        } else {
            self.needs_modeset = true;
        }
        self.flip_pending = false;
    }

    fn modeset(&self, head: &Head, fb: framebuffer::Handle) -> io::Result<()> {
        self.card.set_crtc(head.crtc, Some(fb), (0, 0), &[head.conn], Some(head.mode))
    }

    /// Shows a head's static image, or turns it off under a closed lid.
    fn show_blank(&self, head: &Head) -> io::Result<()> {
        match &head.blank {
            Some((_, fb)) => self.modeset(head, *fb),
            None => self.card.set_crtc(head.crtc, None, (0, 0), &[], None),
        }
    }
}

impl Output for Kms {
    fn size(&self) -> (usize, usize) {
        let (w, h) = self.heads[self.cur].mode.size();
        (w as usize, h as usize)
    }

    fn present(&mut self, draw: &mut dyn FnMut(&mut [u32], usize)) -> io::Result<()> {
        self.wait_flip();
        let back = 1 - self.front;
        let (db, fb) = &mut self.heads[self.cur].flip[back];
        let fb = *fb;
        let stride = db.pitch() as usize / 4;
        {
            let mut map = self.card.map_dumb_buffer(db)?;
            let (_, pixels, _) = unsafe { map.align_to_mut::<u32>() };
            draw(pixels, stride);
        }

        let head = &self.heads[self.cur];
        if self.needs_modeset {
            // Fails while another VT holds DRM master; retried next frame.
            if self.modeset(head, fb).is_ok() {
                self.needs_modeset = false;
                self.front = back;
                if self.others_stale {
                    self.others_stale = false;
                    for (i, head) in self.heads.iter().enumerate() {
                        if i != self.cur {
                            let _ = self.show_blank(head);
                        }
                    }
                }
            }
        } else if self.card.page_flip(head.crtc, fb, PageFlipFlags::EVENT, None).is_ok() {
            self.flip_pending = true;
            self.front = back;
        } else {
            self.needs_modeset = true;
        }
        Ok(())
    }

    fn shown(&self) -> bool {
        !self.needs_modeset
    }

    fn warning(&self) -> Option<String> {
        self.warning.clone()
    }

    fn screens(&self) -> Vec<Screen> {
        self.heads.iter().map(|h| Screen { id: h.id.clone(), usable: !h.lid_closed }).collect()
    }

    fn current(&self) -> usize {
        self.cur
    }

    fn select(&mut self, i: usize) -> bool {
        if i == self.cur || self.heads.get(i).is_none_or(|h| h.lid_closed) {
            return false;
        }
        if let Err(e) = self.add_flip_buffers(i) {
            eprintln!("foongreet: moving the login to {}: {e}", self.heads[i].name);
            return false;
        }
        self.wait_flip();
        // Only the two screens involved change; the others keep their
        // image without a modeset.
        if !self.others_stale && self.show_blank(&self.heads[self.cur]).is_err() {
            self.others_stale = true;
        }
        self.cur = i;
        self.needs_modeset = true;
        true
    }

    fn release(&mut self) {
        self.wait_flip();
        let _ = self.card.release_master_lock();
    }

    fn acquire(&mut self) {
        // Fails only if someone else is master; the modeset retries.
        let _ = self.card.acquire_master_lock();
        self.needs_modeset = true;
        self.others_stale = true;
    }
}

impl Drop for Kms {
    fn drop(&mut self) {
        self.wait_flip();
        let bufs: Vec<_> = self.heads.iter_mut().flat_map(|h| h.blank.take().into_iter().chain(h.flip.drain(..))).collect();
        for (db, fb) in bufs {
            let _ = self.card.destroy_framebuffer(fb);
            let _ = self.card.destroy_dumb_buffer(db);
        }
        let _ = self.card.release_master_lock();
    }
}

/// Whether a laptop lid is closed. Unknown counts as open.
fn lid_closed() -> bool {
    let Ok(dir) = fs::read_dir("/proc/acpi/button/lid") else { return false };
    dir.flatten().any(|e| fs::read_to_string(e.path().join("state")).is_ok_and(|s| s.contains("closed")))
}

/// A monitor's id: a hash of its EDID, so it stays the same on another
/// connector (docks renumber them). The connector name if there is none.
fn monitor_id(card_no: usize, name: &str) -> String {
    match fs::read(format!("/sys/class/drm/card{card_no}-{name}/edid")) {
        Ok(edid) if !edid.is_empty() => {
            let hash = edid.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
            format!("{hash:016x}")
        }
        _ => name.to_string(),
    }
}
