//! DRM/KMS output: dumb buffers with vsynced page flips on the login
//! screen, a static image on any other connected screen.

use super::Output;
use drm::{
    Device as _,
    buffer::{Buffer as _, DrmFourcc},
    control::{
        Device as ControlDevice, Mode, ModeTypeFlags, PageFlipFlags, connector, crtc, dumbbuffer::DumbBuffer,
        framebuffer,
    },
};
use std::{
    fs::{File, OpenOptions},
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
    conn: connector::Handle,
    crtc: crtc::Handle,
    mode: Mode,
    bufs: Vec<(DumbBuffer, framebuffer::Handle)>,
}

/// The login is drawn on the primary head with page flips; other connected
/// screens get a single static image, drawn once.
pub struct Kms {
    card: Card,
    primary: Head,
    others: Vec<Head>,
    front: usize,
    flip_pending: bool,
    needs_modeset: bool,
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
            match Self::heads(&card) {
                Ok(heads) => return Self::finish(card, heads, output, fill),
                Err(e) => last_err = io::Error::new(e.kind(), format!("{path}: {e}")),
            }
        }
        Err(last_err)
    }

    /// Describes the connected screens, primary first, without taking over
    /// the display. Works while a compositor is running.
    pub fn list(output: Option<&str>) -> io::Result<Vec<String>> {
        for i in 0..8 {
            let Ok(file) = OpenOptions::new().read(true).open(format!("/dev/dri/card{i}")) else { continue };
            let Ok(mut heads) = Self::heads(&Card(file)) else { continue };
            if let Some(i) = output.and_then(|name| heads.iter().position(|h| h.name == name)) {
                heads.swap(0, i);
            }
            return Ok(heads
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let (w, ht) = h.mode.size();
                    let role = if i == 0 { "login" } else { "empty tube" };
                    format!("{}  {w}x{ht}@{}  crtc {:?}  -> {role}", h.name, h.mode.vrefresh(), h.crtc)
                })
                .collect());
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no DRM device with a connected display"))
    }

    /// Every connected connector, each with its own CRTC.
    fn heads(card: &Card) -> io::Result<Vec<Head>> {
        let res = card.resource_handles()?;
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
                heads.push(Head { name: info.to_string(), conn: handle, crtc, mode, bufs: Vec::new() });
            }
        }
        if heads.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no connected display"));
        }
        Ok(heads)
    }

    fn finish(
        card: Card,
        mut heads: Vec<Head>,
        output: Option<&str>,
        fill: &mut dyn FnMut(usize, usize, &mut [u32], usize),
    ) -> io::Result<Kms> {
        let mut warning = None;
        if let Some(name) = output {
            match heads.iter().position(|h| h.name == name) {
                Some(i) => heads.swap(0, i),
                None => warning = Some(format!("OUTPUT {name} NOT CONNECTED")),
            }
        }

        for (i, head) in heads.iter_mut().enumerate() {
            let (w, h) = head.mode.size();
            for _ in 0..if i == 0 { 2 } else { 1 } {
                let mut db = card.create_dumb_buffer((w as u32, h as u32), DrmFourcc::Xrgb8888, 32)?;
                let fb = card.add_framebuffer(&db, 24, 32)?;
                if i > 0 {
                    let stride = db.pitch() as usize / 4;
                    let mut map = card.map_dumb_buffer(&mut db)?;
                    let (_, pixels, _) = unsafe { map.align_to_mut::<u32>() };
                    fill(w as usize, h as usize, pixels, stride);
                }
                head.bufs.push((db, fb));
            }
        }

        let primary = heads.remove(0);
        Ok(Kms { card, primary, others: heads, front: 0, flip_pending: false, needs_modeset: true, warning })
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
}

impl Output for Kms {
    fn size(&self) -> (usize, usize) {
        let (w, h) = self.primary.mode.size();
        (w as usize, h as usize)
    }

    fn present(&mut self, draw: &mut dyn FnMut(&mut [u32], usize)) -> io::Result<()> {
        self.wait_flip();
        let back = 1 - self.front;
        let (db, fb) = &mut self.primary.bufs[back];
        let fb = *fb;
        let stride = db.pitch() as usize / 4;
        {
            let mut map = self.card.map_dumb_buffer(db)?;
            let (_, pixels, _) = unsafe { map.align_to_mut::<u32>() };
            draw(pixels, stride);
        }

        if self.needs_modeset {
            // Fails while another VT holds DRM master; retried next frame.
            if self.modeset(&self.primary, fb).is_ok() {
                self.needs_modeset = false;
                self.front = back;
                for head in &self.others {
                    let _ = self.modeset(head, head.bufs[0].1);
                }
            }
        } else if self.card.page_flip(self.primary.crtc, fb, PageFlipFlags::EVENT, None).is_ok() {
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
}

impl Drop for Kms {
    fn drop(&mut self) {
        self.wait_flip();
        let heads = std::iter::once(&mut self.primary).chain(self.others.iter_mut());
        for (db, fb) in heads.flat_map(|h| h.bufs.drain(..)).collect::<Vec<_>>() {
            let _ = self.card.destroy_framebuffer(fb);
            let _ = self.card.destroy_dumb_buffer(db);
        }
        let _ = self.card.release_master_lock();
    }
}
