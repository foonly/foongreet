//! foonlock: a Wayland screen locker with the foongreet CRT look, using
//! ext-session-lock-v1. Run it from hypridle's `lock_cmd`.
//!
//! If foonlock dies while locked, the compositor keeps the session locked
//! (it never falls back to unlocked), so a crash is safe but annoying.

mod auth;
mod keys;
mod ui;

use foongreet::{
    background, config, crt,
    crt::Crt,
    idle::Idle,
    font::Font,
    key::Key,
    tube::{Frame, Tube},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    session_lock::{SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface, SessionLockSurfaceConfigure},
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use std::{
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

const FRAME: Duration = Duration::from_millis(33);
/// Buffers per screen; with frame callbacks two are normally enough.
const MAX_BUFFERS: usize = 3;

const USAGE: &str = "\
usage: foonlock [options]

Locks the Wayland session (ext-session-lock-v1). Display settings come
from the foongreet config file; see config.example.toml.

  --config FILE       config file (default /etc/foongreet/config.toml)
  --mock              no PAM: the password is \"password\"
  --unlock-after SECS unlock by itself after SECS seconds (for testing)
  --demo              made-up hostname, kernel, memory and clock
";

struct Args {
    config: PathBuf,
    mock: bool,
    unlock_after: Option<f32>,
    demo: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { config: config::DEFAULT_PATH.into(), mock: false, unlock_after: None, demo: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--config" => a.config = val()?.into(),
            "--mock" => a.mock = true,
            "--unlock-after" => a.unlock_after = Some(val()?.parse().map_err(|_| "--unlock-after: bad number")?),
            "--demo" => a.demo = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option {arg}\n\n{USAGE}")),
        }
    }
    Ok(a)
}

/// One lock surface per output.
struct Screen {
    output: wl_output::WlOutput,
    surface: SessionLockSurface,
    /// Logical size from the last configure; None until the first one.
    size: Option<(u32, u32)>,
    scale: i32,
    crt: Option<Crt>,
    lum: Vec<u8>,
    bg: Option<Vec<u8>>,
    buffers: Vec<Buffer>,
    /// A frame callback is outstanding: the compositor has not shown the
    /// last frame yet (or the screen is off), so don't draw another.
    waiting: bool,
    /// An empty tube that is fully on and already drawn; nothing changes.
    settled: bool,
    /// Shows the lock box; the others show an empty tube.
    prompt: bool,
}

struct App {
    conn: Connection,
    qh: QueueHandle<App>,
    loop_handle: LoopHandle<'static, App>,
    registry: RegistryState,
    outputs: OutputState,
    compositor: CompositorState,
    shm: Shm,
    seat: SeatState,
    lock_state: SessionLockState,
    pool: SlotPool,
    keyboard: Option<wl_keyboard::WlKeyboard>,

    lock: Option<SessionLock>,
    locked: bool,
    screens: Vec<Screen>,
    display: config::Display,
    ui: ui::Ui,
    tube: Tube,
    /// Collapse of the last frame, for redraws between ticks.
    collapse: f32,
    /// Keys and PAM messages keep the prompt awake.
    idle: Idle,
    auth: auth::Auth,
    unlock_after: Option<f32>,
    unlock_at: Option<Instant>,
    exit: Option<Result<(), String>>,
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if !args.mock && !auth::SERVICE_FILES.iter().any(|f| std::path::Path::new(f).exists()) {
        return Err(format!("{} is missing, so no password could unlock; not locking (see README)", auth::SERVICE_FILES[0]));
    }
    let (config, warning) = config::load(&args.config);
    if let Some(w) = warning {
        eprintln!("foonlock: {w}");
    }
    let font = Font::load(&config.display.font).or_else(|e| {
        eprintln!("foonlock: {e}");
        Font::load(config::DEFAULT_FONT.as_ref())
    })?;

    crt::init_threads();
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn).map_err(|e| format!("wayland: {e}"))?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e}"))?;

    let mut app = App {
        conn: conn.clone(),
        qh: qh.clone(),
        loop_handle: event_loop.handle(),
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {e}"))?,
        pool: SlotPool::new(1920 * 1080 * 4, &shm).map_err(|e| format!("shm pool: {e}"))?,
        shm,
        seat: SeatState::new(&globals, &qh),
        lock_state: SessionLockState::new(&globals, &qh),
        keyboard: None,
        lock: None,
        locked: false,
        screens: Vec::new(),
        display: config.display,
        ui: ui::Ui::new(font, user_name(), "", args.demo),
        // No modeset happens, so no need to wait for monitors to sync.
        tube: Tube::new(0.0),
        collapse: 1.0,
        auth: if args.mock { auth::Auth::mock() } else { auth::Auth::pam() },
        unlock_after: args.unlock_after,
        unlock_at: None,
        idle: Idle::default(),
        exit: None,
    };
    // Output names arrive after the first roundtrip; they pick the prompt.
    queue.roundtrip(&mut app).map_err(|e| format!("wayland: {e}"))?;

    app.lock = Some(app.lock_state.lock(&qh).map_err(|_| "compositor does not support ext-session-lock-v1")?);
    for output in app.outputs.outputs().collect::<Vec<_>>() {
        app.add_screen(output);
    }
    app.pick_prompt();

    WaylandSource::new(conn, queue).insert(event_loop.handle()).map_err(|e| e.to_string())?;
    event_loop
        .handle()
        .insert_source(Timer::from_duration(FRAME), |_, _, app| {
            app.tick();
            TimeoutAction::ToDuration(FRAME)
        })
        .map_err(|e| e.to_string())?;

    loop {
        event_loop.dispatch(None, &mut app).map_err(|e| e.to_string())?;
        if let Some(result) = app.exit.take() {
            return result;
        }
    }
}

impl App {
    fn add_screen(&mut self, output: wl_output::WlOutput) {
        let Some(lock) = &self.lock else { return };
        let surface = lock.create_lock_surface(self.compositor.create_surface(&self.qh), &output, &self.qh);
        self.screens.push(Screen {
            output,
            surface,
            size: None,
            scale: 1,
            crt: None,
            lum: Vec::new(),
            bg: None,
            buffers: Vec::new(),
            waiting: false,
            settled: false,
            prompt: false,
        });
    }

    fn output_name(&self, output: &wl_output::WlOutput) -> String {
        self.outputs.info(output).and_then(|i| i.name).unwrap_or_default()
    }

    /// The configured output gets the lock box, else the first one.
    fn pick_prompt(&mut self) {
        let names: Vec<String> = self.screens.iter().map(|s| self.output_name(&s.output)).collect();
        let want = self.display.output.as_deref();
        let i = want.and_then(|w| names.iter().position(|n| n == w)).unwrap_or(0);
        for (j, s) in self.screens.iter_mut().enumerate() {
            if s.prompt != (i == j) {
                s.prompt = i == j;
                s.settled = false;
                s.bg = None;
            }
        }
        if let Some(name) = names.get(i) {
            self.ui.set_line(&name.to_uppercase());
        }
        if let Some(s) = self.screens.get_mut(i) {
            load_background(s, &self.display);
        }
    }

    fn screen_index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.screens.iter().position(|s| s.surface.wl_surface() == surface)
    }

    /// Sets up the renderer for a screen's current size and scale.
    fn resize(&mut self, i: usize) {
        let s = &mut self.screens[i];
        let Some((w, h)) = s.size else { return };
        let (pw, ph) = (w as usize * s.scale as usize, h as usize * s.scale as usize);
        let crt = Crt::new(pw, ph, self.display.phosphor);
        s.lum = vec![0; crt.lw * crt.lh];
        s.crt = Some(crt);
        s.buffers.clear();
        s.bg = None;
        s.settled = false;
        if s.prompt {
            load_background(s, &self.display);
        }
    }

    fn tick(&mut self) {
        while let Ok(ev) = self.auth.rx.try_recv() {
            self.ui.event(ev);
            self.idle.input();
        }
        if self.locked && (self.ui.done || self.unlock_at.is_some_and(|t| Instant::now() >= t)) {
            self.tube.power_off();
        }
        self.collapse = match self.tube.frame() {
            Frame::Collapse(c) => c,
            Frame::PoweredOn => {
                self.ui.restart_clock();
                self.idle.restart();
                0.0
            }
            Frame::Off => return self.unlock(),
        };
        let busy = !self.tube.is_on() || self.tube.powering_off() || self.ui.typing() || self.ui.busy();
        if let Some(dormant) = self.idle.update(self.ui.time(), busy) {
            self.ui.blink = !dormant;
        }
        for i in 0..self.screens.len() {
            self.draw(i, false);
        }
    }

    /// `force` draws even when nothing changed, for configures.
    fn draw(&mut self, i: usize, force: bool) {
        let collapse = self.collapse;
        let t = self.idle.crt_time(self.ui.time());
        let due = self.idle.due();
        let s = &mut self.screens[i];
        let (Some(crt), Some((w, h))) = (&mut s.crt, s.size) else { return };
        let unchanged = if s.prompt { due.is_none() } else { s.settled && collapse == 0.0 };
        if s.waiting || (unchanged && !force) {
            return;
        }
        crt.still = !s.prompt || self.idle.dormant();
        let (pw, ph) = (w as i32 * s.scale, h as i32 * s.scale);

        let idx = match s.buffers.iter().position(|b| b.canvas(&mut self.pool).is_some()) {
            Some(idx) => idx,
            None if s.buffers.len() < MAX_BUFFERS => {
                match self.pool.create_buffer(pw, ph, pw * 4, wl_shm::Format::Xrgb8888) {
                    Ok((buffer, _)) => s.buffers.push(buffer),
                    Err(e) => return eprintln!("foonlock: buffer: {e}"),
                }
                s.buffers.len() - 1
            }
            None => return,
        };

        if s.prompt && self.tube.is_on() {
            match &s.bg {
                Some(bg) => s.lum.copy_from_slice(bg),
                None => s.lum.fill(0),
            }
            self.ui.draw(&mut s.lum, crt.lw, crt.lh);
        } else {
            s.lum.fill(0);
        }

        let buffer = &s.buffers[idx];
        let Some(canvas) = buffer.canvas(&mut self.pool) else { return };
        // SAFETY: the pool is mmapped (page aligned) and slots are
        // allocated in whole pixels, so the bytes are aligned for u32.
        let (pre, px, _) = unsafe { canvas.align_to_mut::<u32>() };
        debug_assert!(pre.is_empty());
        crt.render(&s.lum, t, collapse, px, pw as usize);

        let surface = s.surface.wl_surface();
        surface.damage_buffer(0, 0, pw, ph);
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        if let Err(e) = buffer.attach_to(surface) {
            return eprintln!("foonlock: attach: {e}");
        }
        surface.commit();
        s.waiting = true;
        if s.prompt
            && let Some(second) = due
        {
            self.idle.drawn(second);
        }
        s.settled = !s.prompt && collapse == 0.0;
    }

    fn key(&mut self, k: Key) {
        self.idle.input();
        // Typing ahead during the power-on is fine; the box shows it once
        // the header is done.
        if self.locked && !self.tube.powering_off() {
            self.ui.key(k, &self.auth);
        }
    }

    fn unlock(&mut self) {
        if let Some(lock) = self.lock.take() {
            lock.unlock();
        }
        // Make sure the compositor has the unlock before exiting.
        let _ = self.conn.roundtrip();
        self.exit = Some(Ok(()));
    }
}

fn load_background(s: &mut Screen, display: &config::Display) {
    let (Some(path), Some(crt)) = (&display.background, &s.crt) else { return };
    match background::load(path, crt.lw, crt.lh, display.background_dim) {
        Ok(bg) => s.bg = Some(bg),
        Err(e) => eprintln!("foonlock: {e}"),
    }
}

fn user_name() -> String {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return std::env::var("USER").unwrap_or_default();
        }
        std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
    }
}

impl SessionLockHandler for App {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        self.locked = true;
        self.unlock_at = self.unlock_after.map(|s| Instant::now() + Duration::from_secs_f32(s));
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        let msg = if self.locked { "the compositor ended the lock" } else { "could not lock (another locker running?)" };
        self.exit = Some(Err(msg.into()));
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _: u32,
    ) {
        let Some(i) = self.screen_index(surface.wl_surface()) else { return };
        if self.screens[i].size != Some(configure.new_size) {
            self.screens[i].size = Some(configure.new_size);
            self.resize(i);
        }
        // Every configure needs a buffer committed in response.
        self.screens[i].waiting = false;
        self.screens[i].settled = false;
        self.draw(i, true);
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, factor: i32) {
        let Some(i) = self.screen_index(surface) else { return };
        if self.screens[i].scale != factor {
            self.screens[i].scale = factor;
            surface.set_buffer_scale(factor);
            self.resize(i);
            self.screens[i].waiting = false;
            self.draw(i, true);
        }
    }

    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(i) = self.screen_index(surface) {
            self.screens[i].waiting = false;
            self.tube.shown();
        }
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        // Screens plugged in while locked get locked too.
        if self.lock.is_some() && !self.screens.iter().any(|s| s.output == output) {
            self.add_screen(output);
        }
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let had_prompt = self.screens.iter().any(|s| s.output == output && s.prompt);
        self.screens.retain(|s| s.output != output);
        if had_prompt {
            self.pick_prompt();
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let repeat = Box::new(|app: &mut App, _: &wl_keyboard::WlKeyboard, ev: KeyEvent| {
                if let Some(k) = keys::map(&ev) {
                    app.key(k);
                }
            });
            match self.seat.get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), repeat) {
                Ok(kb) => self.keyboard = Some(kb),
                Err(e) => eprintln!("foonlock: keyboard: {e}"),
            }
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard
            && let Some(kb) = self.keyboard.take()
        {
            kb.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, ev: KeyEvent) {
        if let Some(k) = keys::map(&ev) {
            self.key(k);
        }
    }

    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("foonlock: {e}");
            ExitCode::FAILURE
        }
    }
}
