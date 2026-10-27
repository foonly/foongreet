//! Login screen: state, key handling, the greetd conversation, and drawing
//! into the low-res luminance buffer.

use crate::{
    auth::Auth,
    font::Font,
    power::{Action, Power},
    sessions,
    sessions::Session,
    state::State,
};
use greetd_ipc::{AuthMessageType, ErrorType, Request, Response};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    ClearLine,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Esc,
    F1,
    F2,
    Interrupt,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    User,
    Secret,
    Session,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Editing the form.
    Idle,
    /// Waiting on greetd.
    Auth,
    /// greetd asked a question we could not answer from the form (2FA etc).
    Prompt,
    Starting,
    Cancelling,
}

enum Status {
    Ready,
    Info(String),
    Error(String),
}

// Intensities on the luminance buffer.
const DIM: u8 = 110;
const NORMAL: u8 = 190;
const BRIGHT: u8 = 255;

const BOX_COLS: usize = 52;
const BOX_ROWS: usize = 9;
const LABEL_COLS: usize = 12;

pub struct Ui {
    font: Font,
    bg: Option<Vec<u8>>,
    w: usize,
    h: usize,
    start: Instant,
    header: Vec<String>,

    user: String,
    secret: String,
    focus: Field,
    sessions: Vec<Session>,
    session: usize,
    state: State,

    phase: Phase,
    pending_secret: Option<String>,
    prompt: Option<(String, bool)>,
    status: Status,
    status_at: f32,

    power: Power,
    /// Waiting for Y/N before running a power action.
    confirm: Option<Action>,
    /// Power command running; the system should be going down.
    halting: Option<Action>,

    /// Session started; the greeter should exit.
    pub done: bool,
    /// Ctrl-C pressed (only honored in mock mode by the caller).
    pub interrupted: bool,
}

impl Ui {
    /// `user` overrides the remembered user when non-empty.
    pub fn new(font: Font, bg: Option<Vec<u8>>, w: usize, h: usize, sessions: Vec<Session>, user: String, state: State, power: Power) -> Ui {
        let user = if user.is_empty() { state.user.clone() } else { user };
        let focus = if user.is_empty() { Field::User } else { Field::Secret };
        let session = sessions.iter().position(|s| s.name == state.session).unwrap_or(0);
        Ui {
            font,
            bg,
            w,
            h,
            start: Instant::now(),
            header: header_lines(),
            user,
            secret: String::new(),
            focus,
            sessions,
            session,
            state,
            phase: Phase::Idle,
            pending_secret: None,
            prompt: None,
            status: Status::Ready,
            status_at: 0.0,
            power,
            confirm: None,
            halting: None,
            done: false,
            interrupted: false,
        }
    }

    /// Shows a startup problem (bad config, missing image) in the status line.
    pub fn warn(&mut self, msg: &str) {
        self.set_status(Status::Error(msg.into()));
    }

    pub fn time(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    fn set_status(&mut self, s: Status) {
        self.status = s;
        self.status_at = self.time();
    }

    /// Per-frame housekeeping: checks on a running power command.
    pub fn tick(&mut self) {
        if let Some(Err(e)) = self.power.poll() {
            self.halting = None;
            self.set_status(Status::Error(e.to_uppercase()));
        }
    }

    pub fn key(&mut self, k: Key, auth: &Auth) {
        if k == Key::Interrupt {
            self.interrupted = true;
            return;
        }
        if self.halting.is_some() {
            return;
        }
        if let Some(action) = self.confirm.take() {
            if matches!(k, Key::Char('y' | 'Y')) {
                self.power_action(action);
            } else {
                self.set_status(Status::Info("CANCELLED".into()));
            }
            return;
        }
        if matches!(k, Key::F1 | Key::F2) && matches!(self.phase, Phase::Idle | Phase::Prompt) {
            self.confirm = Some(if k == Key::F1 { Action::PowerOff } else { Action::Reboot });
            return;
        }
        match self.phase {
            Phase::Auth if k == Key::Esc => return self.cancel(auth, Status::Info("CANCELLED".into())),
            Phase::Auth | Phase::Starting | Phase::Cancelling => return,
            Phase::Prompt => {
                return match k {
                    Key::Enter => {
                        let answer = std::mem::take(&mut self.secret);
                        auth.send(Request::PostAuthMessageResponse { response: Some(answer) });
                        self.phase = Phase::Auth;
                    }
                    Key::Esc => self.cancel(auth, Status::Info("CANCELLED".into())),
                    _ => self.edit(k),
                };
            }
            Phase::Idle => {}
        }

        match k {
            Key::Tab | Key::Down => {
                self.focus = match self.focus {
                    Field::User => Field::Secret,
                    Field::Secret => Field::Session,
                    Field::Session => Field::User,
                }
            }
            Key::BackTab | Key::Up => {
                self.focus = match self.focus {
                    Field::User => Field::Session,
                    Field::Secret => Field::User,
                    Field::Session => Field::Secret,
                }
            }
            Key::Left if self.focus == Field::Session => {
                self.session = (self.session + self.sessions.len() - 1) % self.sessions.len()
            }
            Key::Right if self.focus == Field::Session => self.session = (self.session + 1) % self.sessions.len(),
            Key::Enter if self.focus == Field::User && !self.user.is_empty() => self.focus = Field::Secret,
            Key::Enter => self.submit(auth),
            Key::Esc => self.secret.clear(),
            _ => self.edit(k),
        }
    }

    fn power_action(&mut self, action: Action) {
        if self.power.dry_run {
            let cmd = self.power.command(action).join(" ").to_uppercase();
            return self.set_status(Status::Info(format!("DRY RUN: {cmd}")));
        }
        match self.power.run(action) {
            Ok(()) => self.halting = Some(action),
            Err(e) => self.set_status(Status::Error(e.to_uppercase())),
        }
    }

    fn edit(&mut self, k: Key) {
        let field = match self.focus {
            Field::User if self.phase == Phase::Idle => &mut self.user,
            Field::Secret => &mut self.secret,
            _ => return,
        };
        match k {
            Key::Char(c) if field.chars().count() < 256 => field.push(c),
            Key::Backspace => {
                field.pop();
            }
            Key::ClearLine => field.clear(),
            _ => {}
        }
    }

    fn submit(&mut self, auth: &Auth) {
        if self.user.is_empty() {
            self.focus = Field::User;
            return self.set_status(Status::Error("USER REQUIRED".into()));
        }
        self.pending_secret = Some(std::mem::take(&mut self.secret));
        self.phase = Phase::Auth;
        self.set_status(Status::Info("AUTHENTICATING".into()));
        auth.send(Request::CreateSession { username: self.user.clone() });
    }

    fn cancel(&mut self, auth: &Auth, status: Status) {
        self.pending_secret = None;
        self.prompt = None;
        self.secret.clear();
        self.focus = Field::Secret;
        self.phase = Phase::Cancelling;
        self.set_status(status);
        auth.send(Request::CancelSession);
    }

    pub fn response(&mut self, resp: Response, auth: &Auth) {
        if self.phase == Phase::Cancelling {
            self.phase = Phase::Idle;
            return;
        }
        match resp {
            Response::AuthMessage { auth_message_type: kind, auth_message: msg } => match kind {
                AuthMessageType::Secret if self.pending_secret.is_some() => {
                    auth.send(Request::PostAuthMessageResponse { response: self.pending_secret.take() });
                }
                AuthMessageType::Secret | AuthMessageType::Visible => {
                    let label = msg.trim().trim_end_matches(':').to_uppercase();
                    self.prompt = Some((label, matches!(kind, AuthMessageType::Secret)));
                    self.secret.clear();
                    self.focus = Field::Secret;
                    self.phase = Phase::Prompt;
                    self.set_status(Status::Info("ADDITIONAL INPUT REQUIRED".into()));
                }
                AuthMessageType::Info => {
                    self.set_status(Status::Info(msg.trim().to_uppercase()));
                    auth.send(Request::PostAuthMessageResponse { response: None });
                }
                AuthMessageType::Error => {
                    self.set_status(Status::Error(msg.trim().to_uppercase()));
                    auth.send(Request::PostAuthMessageResponse { response: None });
                }
            },
            Response::Success if self.phase == Phase::Starting => self.done = true,
            Response::Success => {
                let s = &self.sessions[self.session];
                let cmd = if s.cmd.is_empty() { vec![sessions::login_shell(&self.user)] } else { s.cmd.clone() };
                let name = s.name.to_uppercase();
                self.state.user = self.user.clone();
                self.state.session = s.name.clone();
                if let Err(e) = self.state.save() {
                    eprintln!("foongreet: saving state: {e}");
                }
                auth.send(Request::StartSession { cmd, env: s.env.clone() });
                self.prompt = None;
                self.phase = Phase::Starting;
                self.set_status(Status::Info(format!("LOADING {name}")));
            }
            Response::Error { error_type, description } => {
                let msg = match error_type {
                    ErrorType::AuthError => "ACCESS DENIED".to_string(),
                    ErrorType::Error => description.to_uppercase(),
                };
                self.cancel(auth, Status::Error(msg));
            }
        }
    }

    pub fn draw(&self, buf: &mut [u8]) {
        let t = self.time();
        match &self.bg {
            Some(bg) => buf.copy_from_slice(bg),
            None => buf.fill(0),
        }
        let mut c = Canvas { buf, w: self.w, h: self.h, font: &self.font };
        let (cw, ch) = (self.font.width, self.font.height);
        let cols = self.w / cw;
        let rows = self.h / ch;

        // Boot header, typed out at startup.
        let mut budget = (t * 90.0) as usize;
        for (i, line) in self.header.iter().enumerate() {
            let n = line.chars().count().min(budget);
            budget -= n;
            let shown: String = line.chars().take(n).collect();
            c.text(2 * cw, (1 + i) * ch, &shown, if i == 0 { BRIGHT } else { NORMAL });
        }

        let clock = clock();
        c.text((cols.saturating_sub(clock.len() + 2)) * cw, ch, &clock, NORMAL);

        // Login box, appears once the header has finished typing.
        if budget == 0 {
            return;
        }
        let bc = (cols.saturating_sub(BOX_COLS)) / 2;
        let br = (rows.saturating_sub(BOX_ROWS)) / 2 + 1;
        let (x0, y0) = (bc * cw, br * ch);
        c.fill(x0 - cw, y0 - ch / 2, (BOX_COLS + 2) * cw, BOX_ROWS * ch + ch, 0);
        c.frame(bc, br, BOX_COLS, BOX_ROWS, " SYSTEM LOGIN ", NORMAL);

        let blink = (t * 2.0) as u32 % 2 == 0;
        let input = matches!(self.phase, Phase::Idle | Phase::Prompt);
        let value_col = bc + 4 + LABEL_COLS;
        let value_cols = BOX_COLS - 6 - LABEL_COLS;

        let (secret_label, mask) = match &self.prompt {
            Some((label, secret)) => (label.as_str(), *secret),
            None => ("PASSWORD", true),
        };
        let secret_shown = if mask { "*".repeat(self.secret.chars().count()) } else { self.secret.clone() };
        let session_name = self.sessions[self.session].name.to_uppercase();
        let session_shown = if self.focus == Field::Session && input {
            format!("< {session_name} >")
        } else {
            format!("  {session_name}")
        };

        let fields = [
            (Field::User, "USER", self.user.clone()),
            (Field::Secret, secret_label, secret_shown),
            (Field::Session, "SESSION", session_shown),
        ];
        for (i, (field, label, value)) in fields.iter().enumerate() {
            let row = br + 2 + i;
            let focused = self.focus == *field && input;
            let level = if focused { BRIGHT } else { DIM };
            if focused {
                c.text((bc + 2) * cw, row * ch, ">", BRIGHT);
            }
            let label: String = label.chars().take(LABEL_COLS - 1).collect();
            c.text((bc + 4) * cw, row * ch, &format!("{label:.<width$}", width = LABEL_COLS - 1), level);

            // Keep the end of long values visible.
            let skip = value.chars().count().saturating_sub(value_cols - 1);
            let shown: String = value.chars().skip(skip).collect();
            c.text(value_col * cw, row * ch, &shown, if focused { BRIGHT } else { NORMAL });
            if focused && *field != Field::Session && blink {
                c.fill((value_col + shown.chars().count()) * cw, row * ch, cw, ch, BRIGHT);
            }
        }

        let since = t - self.status_at;
        let (status, level) = match &self.status {
            _ if self.halting.is_some() => {
                let dots = ".".repeat((t * 3.0) as usize % 4);
                let msg = if self.halting == Some(Action::PowerOff) { "SYSTEM HALTING" } else { "SYSTEM RESTARTING" };
                (format!("{msg}{dots}"), BRIGHT)
            }
            _ if self.confirm.is_some() => {
                let what = if self.confirm == Some(Action::PowerOff) { "POWER OFF" } else { "REBOOT" };
                (format!("{what} SYSTEM? [Y/N]"), BRIGHT)
            }
            _ if matches!(self.phase, Phase::Auth | Phase::Starting) => {
                let dots = ".".repeat((t * 3.0) as usize % 4);
                let msg = match &self.status {
                    Status::Info(m) => m.clone(),
                    _ => "WORKING".into(),
                };
                (format!("{msg}{dots}"), NORMAL)
            }
            Status::Ready => ("READY.".into(), DIM),
            Status::Info(m) => (m.clone(), NORMAL),
            // Flash a few times, then stay lit.
            Status::Error(_) if since < 1.5 && (since * 4.0) as u32 % 2 == 1 => (String::new(), 0),
            Status::Error(m) => (format!("*** {m} ***"), BRIGHT),
        };
        let status: String = status.chars().take(BOX_COLS - 6).collect();
        c.text((bc + 4) * cw, (br + 6) * ch, &status, level);

        let footer = ["[TAB] FIELD   [</>] SESSION   [ENTER] LOGIN   [ESC] CLEAR", "[F1] POWER OFF   [F2] REBOOT"];
        for (i, line) in footer.iter().enumerate() {
            c.text(cols.saturating_sub(line.len()) / 2 * cw, (rows - 3 + i) * ch, line, DIM);
        }
    }
}

struct Canvas<'a> {
    buf: &'a mut [u8],
    w: usize,
    h: usize,
    font: &'a Font,
}

impl Canvas<'_> {
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, v: u8) {
        for yy in y.min(self.h)..(y + h).min(self.h) {
            self.buf[yy * self.w + x.min(self.w)..yy * self.w + (x + w).min(self.w)].fill(v);
        }
    }

    fn text(&mut self, x: usize, y: usize, s: &str, v: u8) {
        let (fw, fh) = (self.font.width, self.font.height);
        for (i, c) in s.chars().enumerate() {
            let g = self.font.glyph(c);
            let gx = x + i * fw;
            for row in 0..fh {
                let py = y + row;
                if py >= self.h {
                    break;
                }
                for col in 0..fw {
                    let px = gx + col;
                    if px < self.w && self.font.pixel(g, col, row) {
                        self.buf[py * self.w + px] = v;
                    }
                }
            }
        }
    }

    /// Double-line box in character cells with a centered title.
    fn frame(&mut self, col: usize, row: usize, w: usize, h: usize, title: &str, v: u8) {
        let (cw, ch) = (self.font.width, self.font.height);
        let mut top: Vec<char> = std::iter::once('╔').chain(std::iter::repeat_n('═', w - 2)).chain(['╗']).collect();
        let start = (w - title.chars().count()) / 2;
        for (i, c) in title.chars().enumerate() {
            top[start + i] = c;
        }
        let top: String = top.into_iter().collect();
        let bottom: String = std::iter::once('╚').chain(std::iter::repeat_n('═', w - 2)).chain(['╝']).collect();
        self.text(col * cw, row * ch, &top, v);
        self.text(col * cw, (row + h - 1) * ch, &bottom, v);
        for r in row + 1..row + h - 1 {
            self.text(col * cw, r * ch, "║", v);
            self.text((col + w - 1) * cw, r * ch, "║", v);
        }
    }
}

fn header_lines() -> Vec<String> {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    let mem_kb = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| m.lines().next().and_then(|l| l.split_whitespace().nth(1)).map(str::to_string))
        .unwrap_or_else(|| "640".into());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    vec![
        "FOONGREET TERMINAL SYSTEM  REV 0.1".into(),
        "(C) 1986-2026 FOON MICROSYSTEMS".into(),
        String::new(),
        format!("MEMORY TEST ... {mem_kb}K OK"),
        format!("KERNEL ........ {}", kernel.trim().to_uppercase()),
        format!("NODE .......... {}", host.trim().to_uppercase()),
        format!("LINE .......... {}", crate::tty::name()),
    ]
}

fn clock() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}
