//! Password checks through PAM (service "foonlock") on a worker thread, so
//! fail delays and slow modules never stall rendering.
//!
//! The UI starts a check with the password already typed; it answers the
//! first secret prompt. Further prompts (2FA etc.) go to the UI as events
//! and the worker waits for the answer.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    ptr,
    sync::mpsc::{Receiver, Sender, channel},
    thread,
    time::Duration,
};

const SERVICE: &CStr = c"foonlock";
/// Where Linux-PAM looks for the service file. Without it PAM falls back
/// to "other", which normally denies everything: the lock could never be
/// lifted.
pub const SERVICE_FILES: [&str; 2] = ["/etc/pam.d/foonlock", "/usr/lib/pam.d/foonlock"];

pub enum Request {
    Start { user: String, secret: String },
    /// Answer to `Event::Prompt`; None cancels the check.
    Answer(Option<String>),
}

pub enum Event {
    Prompt { text: String, secret: bool },
    Info(String),
    Error(String),
    /// The check finished: Ok unlocks, Err holds the reason.
    Done(Result<(), String>),
}

pub struct Auth {
    tx: Sender<Request>,
    pub rx: Receiver<Event>,
}

impl Auth {
    pub fn send(&self, req: Request) {
        let _ = self.tx.send(req);
    }

    pub fn pam() -> Auth {
        spawn(|user, secret, conv| authenticate(None, Some(user), secret, conv))
    }

    /// No PAM: any user, password "password".
    pub fn mock() -> Auth {
        spawn(|_, mut secret, _| {
            thread::sleep(Duration::from_millis(600));
            let ok = secret == "password";
            wipe(&mut secret);
            if ok { Ok(()) } else { Err("ACCESS DENIED".into()) }
        })
    }
}

/// What the conversation callback talks to.
struct Conv {
    tx: Sender<Event>,
    rx: Receiver<Request>,
    /// The typed password, for the first secret prompt.
    secret: Option<String>,
}

type Check = fn(&str, String, &mut Conv) -> Result<(), String>;

fn spawn(check: Check) -> Auth {
    let (tx, req_rx) = channel();
    let (ev_tx, rx) = channel();
    thread::spawn(move || {
        let mut conv = Conv { tx: ev_tx, rx: req_rx, secret: None };
        while let Ok(req) = conv.rx.recv() {
            // Stray answers (e.g. a cancel racing the end of a check) are
            // dropped here.
            let Request::Start { user, secret } = req else { continue };
            let result = check(&user, secret, &mut conv);
            if let Some(s) = &mut conv.secret {
                wipe(s);
            }
            conv.secret = None;
            if conv.tx.send(Event::Done(result)).is_err() {
                break;
            }
        }
    });
    Auth { tx, rx }
}

/// Overwrites a password before it is dropped.
pub fn wipe(s: &mut String) {
    // SAFETY: zero bytes are valid UTF-8.
    unsafe { s.as_bytes_mut().fill(0) };
    s.clear();
}

// Linux-PAM, from <security/_pam_types.h>.
const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_AUTH_ERR: c_int = 7;
const PAM_MAXTRIES: c_int = 11;
const PAM_CONV_ERR: c_int = 19;
const PAM_REFRESH_CRED: c_int = 0x0010;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start_confdir(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        confdir: *const c_char,
        pamh: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_setcred(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut c_void, status: c_int) -> c_int;
    fn pam_strerror(pamh: *mut c_void, errnum: c_int) -> *const c_char;
}

/// `confdir` replaces /etc/pam.d and `user` None makes PAM ask for it,
/// for tests.
fn authenticate(confdir: Option<&CStr>, user: Option<&str>, secret: String, conv: &mut Conv) -> Result<(), String> {
    conv.secret = Some(secret);
    let user = user.map(CString::new).transpose().map_err(|_| "BAD USER NAME")?;
    let pam_conv = PamConv { conv: converse, appdata_ptr: (conv as *mut Conv).cast() };
    let mut pamh = ptr::null_mut();
    // SAFETY: all pointers outlive the PAM handle, which is ended below.
    unsafe {
        let confdir = confdir.map_or(ptr::null(), CStr::as_ptr);
        let rc = pam_start_confdir(SERVICE.as_ptr(), user.as_ref().map_or(ptr::null(), |u| u.as_ptr()), &pam_conv, confdir, &mut pamh);
        if rc != PAM_SUCCESS {
            return Err(format!("PAM_START: {}", strerror(pamh, rc)));
        }
        let rc = pam_authenticate(pamh, 0);
        let result = match rc {
            PAM_SUCCESS => {
                // Renews Kerberos tickets and the like; not fatal.
                pam_setcred(pamh, PAM_REFRESH_CRED);
                Ok(())
            }
            PAM_AUTH_ERR => Err("ACCESS DENIED".into()),
            PAM_MAXTRIES => Err("TOO MANY ATTEMPTS".into()),
            PAM_CONV_ERR => Err("CANCELLED".into()),
            rc => Err(strerror(pamh, rc)),
        };
        pam_end(pamh, rc);
        result
    }
}

unsafe fn strerror(pamh: *mut c_void, rc: c_int) -> String {
    let s = unsafe { pam_strerror(pamh, rc) };
    if s.is_null() {
        return format!("PAM ERROR {rc}");
    }
    unsafe { CStr::from_ptr(s) }.to_string_lossy().to_uppercase()
}

extern "C" fn converse(n: c_int, msgs: *mut *const PamMessage, resp: *mut *mut PamResponse, data: *mut c_void) -> c_int {
    if n <= 0 || msgs.is_null() || resp.is_null() || data.is_null() {
        return PAM_CONV_ERR;
    }
    let n = n as usize;
    // SAFETY: PAM hands us the Conv from `authenticate` and `n` messages
    // (Linux-PAM layout: an array of pointers). The responses are
    // malloc'ed because PAM frees them.
    unsafe {
        let conv = &mut *data.cast::<Conv>();
        let out = libc::calloc(n, size_of::<PamResponse>()).cast::<PamResponse>();
        if out.is_null() {
            return PAM_BUF_ERR;
        }
        for i in 0..n {
            let m = &**msgs.add(i);
            let text = if m.msg.is_null() { String::new() } else { CStr::from_ptr(m.msg).to_string_lossy().into_owned() };
            let answer = match m.msg_style {
                PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => {
                    let secret = m.msg_style == PAM_PROMPT_ECHO_OFF;
                    match conv.secret.take_if(|_| secret) {
                        Some(s) => Some(s),
                        None => ask(conv, text, secret),
                    }
                }
                PAM_ERROR_MSG => {
                    let _ = conv.tx.send(Event::Error(text));
                    continue;
                }
                PAM_TEXT_INFO => {
                    let _ = conv.tx.send(Event::Info(text));
                    continue;
                }
                _ => None,
            };
            let Some(mut answer) = answer else {
                free_responses(out, n);
                return PAM_CONV_ERR;
            };
            let p = c_string(answer.as_bytes());
            wipe(&mut answer);
            if p.is_null() {
                free_responses(out, n);
                return PAM_BUF_ERR;
            }
            (*out.add(i)).resp = p;
        }
        *resp = out;
    }
    PAM_SUCCESS
}

/// A malloc'ed, NUL-terminated copy for PAM (which frees it), made
/// without intermediate copies that would leave the password behind.
/// Null on allocation failure or an embedded NUL.
unsafe fn c_string(b: &[u8]) -> *mut c_char {
    if b.contains(&0) {
        return ptr::null_mut();
    }
    unsafe {
        let p = libc::malloc(b.len() + 1).cast::<u8>();
        if !p.is_null() {
            ptr::copy_nonoverlapping(b.as_ptr(), p, b.len());
            *p.add(b.len()) = 0;
        }
        p.cast()
    }
}

/// Shows a prompt in the UI and waits for the answer.
fn ask(conv: &mut Conv, text: String, secret: bool) -> Option<String> {
    conv.tx.send(Event::Prompt { text, secret }).ok()?;
    loop {
        match conv.rx.recv().ok()? {
            Request::Answer(a) => return a,
            Request::Start { mut secret, .. } => wipe(&mut secret),
        }
    }
}

unsafe fn free_responses(out: *mut PamResponse, n: usize) {
    for i in 0..n {
        let r = unsafe { (*out.add(i)).resp };
        if !r.is_null() {
            unsafe {
                libc::explicit_bzero(r.cast(), libc::strlen(r));
                libc::free(r.cast());
            }
        }
    }
    unsafe { libc::free(out.cast()) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Run {
        result: Result<(), String>,
        events: Vec<Event>,
        /// Whether a prompt took the typed password.
        secret_used: bool,
    }

    /// Runs one check against a throwaway PAM config, with `answers`
    /// queued for prompts that go to the UI. Never touches /etc/pam.d (or
    /// pam_faillock's counters): the user does not exist.
    fn check(name: &str, stack: &str, user: Option<&str>, answers: &[Option<&str>]) -> Run {
        let dir = std::env::temp_dir().join(format!("foonlock-pam-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("foonlock"), stack).unwrap();
        let (tx, rx) = channel();
        let (ev_tx, ev_rx) = channel();
        for a in answers {
            tx.send(Request::Answer(a.map(String::from))).unwrap();
        }
        let mut conv = Conv { tx: ev_tx, rx, secret: None };
        let confdir = CString::new(dir.to_str().unwrap()).unwrap();
        let result = authenticate(Some(&confdir), user, "hunter2".into(), &mut conv);
        fs::remove_dir_all(&dir).unwrap();
        Run { result, events: ev_rx.try_iter().collect(), secret_used: conv.secret.is_none() }
    }

    const NOBODY: Option<&str> = Some("foonlock-no-such-user");

    #[test]
    fn permit() {
        let run = check("permit", "auth required pam_permit.so\n", NOBODY, &[]);
        assert!(run.result.is_ok() && run.events.is_empty());
    }

    #[test]
    fn deny() {
        let run = check("deny", "auth required pam_deny.so\n", NOBODY, &[]);
        assert_eq!(run.result, Err("ACCESS DENIED".into()));
    }

    /// pam_unix asks for a password even for unknown users; the typed one
    /// answers it, so the UI sees no prompt.
    #[test]
    fn typed_password_answers_first_prompt() {
        let run = check("unix", "auth required pam_unix.so\n", NOBODY, &[]);
        assert!(run.result.is_err() && run.secret_used);
        assert!(!run.events.iter().any(|e| matches!(e, Event::Prompt { .. })));
    }

    /// Without a user pam_unix first asks "login:", which goes to the UI;
    /// the typed password must survive it for the password prompt.
    #[test]
    fn visible_prompt_goes_to_ui() {
        let run = check("login", "auth required pam_unix.so\n", None, &[Some("foonlock-no-such-user")]);
        assert!(run.result.is_err() && run.secret_used);
        assert!(matches!(&run.events[..], [Event::Prompt { secret: false, .. }]));
    }

    #[test]
    fn cancelling_a_prompt_fails() {
        let run = check("cancel", "auth required pam_unix.so\n", None, &[None]);
        assert!(run.result.is_err() && !run.secret_used);
    }

    #[test]
    fn wipe_clears() {
        let mut s = String::from("secret");
        wipe(&mut s);
        assert!(s.is_empty());
    }
}
