//! The lock screen: state, key handling, the PAM conversation, and drawing
//! into the low-res luminance buffer of the screen that has the prompt.

use crate::auth::{self, Auth, Event, Request};
use foongreet::{
    canvas,
    canvas::{BRIGHT, Canvas, DIM, NORMAL},
    font::Font,
    key::Key,
};
use std::time::Instant;

const BOX_COLS: usize = 52;
const BOX_ROWS: usize = 7;
const LABEL_COLS: usize = 12;

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Typing the password.
    Idle,
    /// Waiting on PAM.
    Auth,
    /// PAM asked another question (2FA etc).
    Prompt,
    /// Unlocked; the tube is powering off.
    Unlocked,
}

enum Status {
    Ready,
    Info(String),
    Error(String),
}

pub struct Ui {
    pub font: Font,
    start: Instant,
    header: Vec<String>,
    demo: bool,
    user: String,

    secret: String,
    phase: Phase,
    /// Label and whether to mask, for a question from PAM.
    prompt: Option<(String, bool)>,
    /// Esc pressed during a PAM question; the failure that follows is ours.
    cancelled: bool,
    status: Status,
    status_at: f32,

    /// Whether the cursor blinks; off, it is hidden and the box static.
    pub blink: bool,
    /// Password accepted: unlock.
    pub done: bool,
}

impl Ui {
    /// `line` is the output the prompt is on, shown in the header.
    pub fn new(font: Font, user: String, line: &str, demo: bool) -> Ui {
        Ui {
            font,
            start: Instant::now(),
            header: canvas::header_lines(demo, line),
            demo,
            user,
            secret: String::new(),
            phase: Phase::Idle,
            prompt: None,
            cancelled: false,
            status: Status::Ready,
            status_at: 0.0,
            blink: true,
            done: false,
        }
    }

    /// The output the prompt moved to.
    pub fn set_line(&mut self, line: &str) {
        self.header = canvas::header_lines(self.demo, line);
    }

    /// Restarts animations (the boot header typing out) from the beginning.
    pub fn restart_clock(&mut self) {
        self.start = Instant::now();
        self.status_at = 0.0;
    }

    pub fn time(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    /// Whether the boot header is still typing out.
    pub fn typing(&self) -> bool {
        canvas::typing(&self.header, self.time())
    }

    /// Whether something on screen is animating: the dots while PAM works
    /// or a fresh error flashing.
    pub fn busy(&self) -> bool {
        let flashing = matches!(self.status, Status::Error(_)) && self.time() - self.status_at < 1.5;
        !matches!(self.phase, Phase::Idle | Phase::Prompt) || flashing
    }

    fn set_status(&mut self, s: Status) {
        self.status = s;
        self.status_at = self.time();
    }

    pub fn key(&mut self, k: Key, auth: &Auth) {
        match (self.phase, k) {
            (Phase::Auth | Phase::Unlocked, _) => {}
            (Phase::Prompt, Key::Enter) => {
                auth.send(Request::Answer(Some(std::mem::take(&mut self.secret))));
                self.phase = Phase::Auth;
            }
            (Phase::Prompt, Key::Esc) => {
                auth::wipe(&mut self.secret);
                auth.send(Request::Answer(None));
                self.cancelled = true;
                self.phase = Phase::Auth;
            }
            // An empty password would only count as a failed attempt
            // (and towards pam_faillock's lockout).
            (Phase::Idle, Key::Enter) if self.secret.is_empty() => {}
            (Phase::Idle, Key::Enter) => {
                let secret = std::mem::take(&mut self.secret);
                auth.send(Request::Start { user: self.user.clone(), secret });
                self.phase = Phase::Auth;
                self.set_status(Status::Info("VERIFYING".into()));
            }
            (Phase::Idle, Key::Esc) => auth::wipe(&mut self.secret),
            _ => self.edit(k),
        }
    }

    fn edit(&mut self, k: Key) {
        match k {
            Key::Char(c) if self.secret.chars().count() < 256 => self.secret.push(c),
            Key::Backspace => {
                self.secret.pop();
            }
            Key::ClearLine => auth::wipe(&mut self.secret),
            _ => {}
        }
    }

    pub fn event(&mut self, ev: Event) {
        match ev {
            Event::Prompt { text, secret } => {
                let label = text.trim().trim_end_matches(':').to_uppercase();
                self.prompt = Some((label, secret));
                auth::wipe(&mut self.secret);
                self.phase = Phase::Prompt;
                self.set_status(Status::Info("ADDITIONAL INPUT REQUIRED".into()));
            }
            Event::Info(m) => self.set_status(Status::Info(m.trim().to_uppercase())),
            Event::Error(m) => self.set_status(Status::Error(m.trim().to_uppercase())),
            Event::Done(Ok(())) => {
                self.phase = Phase::Unlocked;
                self.prompt = None;
                self.set_status(Status::Info("ACCESS GRANTED".into()));
                self.done = true;
            }
            Event::Done(Err(e)) => {
                self.phase = Phase::Idle;
                self.prompt = None;
                auth::wipe(&mut self.secret);
                let cancelled = std::mem::take(&mut self.cancelled);
                self.set_status(if cancelled { Status::Info("CANCELLED".into()) } else { Status::Error(e) });
            }
        }
    }

    /// Draws over `buf`, which holds the background already.
    pub fn draw(&self, buf: &mut [u8], w: usize, h: usize) {
        let t = self.time();
        let mut c = Canvas { buf, w, h, font: &self.font };
        let (cw, ch) = (self.font.width, self.font.height);
        let cols = w / cw;
        let rows = h / ch;

        if !c.header(&self.header, t, self.demo) {
            return;
        }
        let bc = (cols.saturating_sub(BOX_COLS)) / 2;
        let br = (rows.saturating_sub(BOX_ROWS)) / 2 + 1;
        let (x0, y0) = (bc * cw, br * ch);
        c.fill(x0 - cw, y0 - ch / 2, (BOX_COLS + 2) * cw, BOX_ROWS * ch + ch, 0);
        c.frame(bc, br, BOX_COLS, BOX_ROWS, " TERMINAL LOCKED ", NORMAL);

        let value_col = bc + 4 + LABEL_COLS;
        let value_cols = BOX_COLS - 6 - LABEL_COLS;
        let dots = |label: &str| {
            let label: String = label.chars().take(LABEL_COLS - 1).collect();
            format!("{label:.<width$}", width = LABEL_COLS - 1)
        };

        let user = self.user.to_uppercase();
        c.text((bc + 4) * cw, (br + 2) * ch, &dots("USER"), DIM);
        c.text(value_col * cw, (br + 2) * ch, &user, NORMAL);

        let (label, mask) = match &self.prompt {
            Some((label, secret)) => (label.as_str(), *secret),
            None => ("PASSWORD", true),
        };
        let input = matches!(self.phase, Phase::Idle | Phase::Prompt);
        let value = if mask { "*".repeat(self.secret.chars().count()) } else { self.secret.clone() };
        // Keep the end of long values visible.
        let skip = value.chars().count().saturating_sub(value_cols - 1);
        let shown: String = value.chars().skip(skip).collect();
        let row = (br + 3) * ch;
        if input {
            c.text((bc + 2) * cw, row, ">", BRIGHT);
        }
        c.text((bc + 4) * cw, row, &dots(label), if input { BRIGHT } else { DIM });
        c.text(value_col * cw, row, &shown, if input { BRIGHT } else { NORMAL });
        if input && self.blink && (t * 2.0) as u32 % 2 == 0 {
            c.fill((value_col + shown.chars().count()) * cw, row, cw, ch, BRIGHT);
        }

        let since = t - self.status_at;
        let (status, level) = match &self.status {
            _ if self.phase == Phase::Auth => {
                let dots = ".".repeat((t * 3.0) as usize % 4);
                let msg = match &self.status {
                    Status::Info(m) => m.clone(),
                    _ => "VERIFYING".into(),
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
        c.text((bc + 4) * cw, (br + 5) * ch, &status, level);

        let footer = "[ENTER] UNLOCK   [ESC] CLEAR";
        c.text(cols.saturating_sub(footer.len()) / 2 * cw, (rows - 3) * ch, footer, DIM);
    }
}
