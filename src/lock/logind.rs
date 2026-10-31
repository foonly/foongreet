//! logind's PrepareForSleep signal, through libsystemd's sd-bus, so the
//! screen can be blanked before a suspend: the display keeps its last
//! frame while suspended and shows it again on resume.
//!
//! A sleep delay inhibitor makes logind wait (up to InhibitDelayMaxSec,
//! 5 s by default) until we release it, i.e. until the black frame is
//! on screen.

use std::{
    cell::RefCell,
    ffi::{CStr, c_char, c_int, c_void},
    os::fd::{FromRawFd, OwnedFd, RawFd},
    ptr,
};

#[repr(C)]
struct BusError {
    name: *const c_char,
    message: *const c_char,
    need_free: c_int,
}

type Handler = extern "C" fn(*mut c_void, *mut c_void, *mut BusError) -> c_int;

#[link(name = "systemd")]
unsafe extern "C" {
    fn sd_bus_open_system(bus: *mut *mut c_void) -> c_int;
    fn sd_bus_flush_close_unref(bus: *mut c_void) -> *mut c_void;
    fn sd_bus_get_fd(bus: *mut c_void) -> c_int;
    fn sd_bus_process(bus: *mut c_void, msg: *mut *mut c_void) -> c_int;
    fn sd_bus_match_signal(
        bus: *mut c_void,
        slot: *mut *mut c_void,
        sender: *const c_char,
        path: *const c_char,
        interface: *const c_char,
        member: *const c_char,
        callback: Handler,
        userdata: *mut c_void,
    ) -> c_int;
    fn sd_bus_call_method(
        bus: *mut c_void,
        destination: *const c_char,
        path: *const c_char,
        interface: *const c_char,
        member: *const c_char,
        error: *mut BusError,
        reply: *mut *mut c_void,
        types: *const c_char,
        ...
    ) -> c_int;
    fn sd_bus_message_read(msg: *mut c_void, types: *const c_char, ...) -> c_int;
    fn sd_bus_message_unref(msg: *mut c_void) -> *mut c_void;
    fn sd_bus_error_free(error: *mut BusError);
}

const LOGIN1: &CStr = c"org.freedesktop.login1";
const PATH: &CStr = c"/org/freedesktop/login1";
const MANAGER: &CStr = c"org.freedesktop.login1.Manager";

pub struct Logind {
    bus: *mut c_void,
    /// PrepareForSleep values received since the last `events`; boxed so
    /// the signal handler's pointer to it stays valid.
    pending: Box<RefCell<Vec<bool>>>,
    inhibitor: Option<OwnedFd>,
}

impl Logind {
    pub fn connect() -> Result<Logind, String> {
        let mut bus = ptr::null_mut();
        // SAFETY: plain sd-bus calls; `pending` is boxed and outlives the
        // bus, which is closed in Drop.
        unsafe {
            let rc = sd_bus_open_system(&mut bus);
            if rc < 0 {
                return Err(format!("system bus: {}", std::io::Error::from_raw_os_error(-rc)));
            }
            let mut logind = Logind { bus, pending: Box::default(), inhibitor: None };
            let rc = sd_bus_match_signal(
                bus,
                ptr::null_mut(),
                LOGIN1.as_ptr(),
                PATH.as_ptr(),
                MANAGER.as_ptr(),
                c"PrepareForSleep".as_ptr(),
                on_prepare_for_sleep,
                (&*logind.pending as *const RefCell<Vec<bool>>).cast_mut().cast(),
            );
            if rc < 0 {
                return Err(format!("PrepareForSleep: {}", std::io::Error::from_raw_os_error(-rc)));
            }
            logind.inhibit();
            Ok(logind)
        }
    }

    /// For the event loop: readable when there is something to `events`.
    pub fn fd(&self) -> RawFd {
        unsafe { sd_bus_get_fd(self.bus) }
    }

    /// Reads the bus; returns PrepareForSleep values (true: about to
    /// sleep, false: resumed) in order.
    pub fn events(&self) -> Vec<bool> {
        // SAFETY: the bus is open; the handler only touches `pending`.
        while unsafe { sd_bus_process(self.bus, ptr::null_mut()) } > 0 {}
        self.pending.take()
    }

    /// Takes a sleep delay inhibitor, unless held already.
    pub fn inhibit(&mut self) {
        if self.inhibitor.is_some() {
            return;
        }
        let mut error = BusError { name: ptr::null(), message: ptr::null(), need_free: 0 };
        let mut reply = ptr::null_mut();
        // SAFETY: the "ssss" arguments are NUL-terminated strings; the fd
        // read from the reply belongs to it, so it is duplicated first.
        unsafe {
            let rc = sd_bus_call_method(
                self.bus,
                LOGIN1.as_ptr(),
                PATH.as_ptr(),
                MANAGER.as_ptr(),
                c"Inhibit".as_ptr(),
                &mut error,
                &mut reply,
                c"ssss".as_ptr(),
                c"sleep".as_ptr(),
                c"foonlock".as_ptr(),
                c"Blanking the screen before suspend".as_ptr(),
                c"delay".as_ptr(),
            );
            if rc < 0 {
                let msg = if error.message.is_null() { String::new() } else { CStr::from_ptr(error.message).to_string_lossy().into_owned() };
                eprintln!("foonlock: logind Inhibit: {msg}");
                sd_bus_error_free(&mut error);
                return;
            }
            let mut fd: c_int = -1;
            if sd_bus_message_read(reply, c"h".as_ptr(), &mut fd as *mut c_int) >= 0 && fd >= 0 {
                let dup = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3);
                if dup >= 0 {
                    self.inhibitor = Some(OwnedFd::from_raw_fd(dup));
                }
            }
            sd_bus_message_unref(reply);
        }
    }

    /// Lets the suspend go ahead.
    pub fn release(&mut self) {
        self.inhibitor = None;
    }
}

impl Drop for Logind {
    fn drop(&mut self) {
        unsafe { sd_bus_flush_close_unref(self.bus) };
    }
}

extern "C" fn on_prepare_for_sleep(msg: *mut c_void, userdata: *mut c_void, _: *mut BusError) -> c_int {
    let mut sleeping: c_int = 0;
    // SAFETY: userdata is Logind::pending; "b" reads into an int.
    unsafe {
        if sd_bus_message_read(msg, c"b".as_ptr(), &mut sleeping as *mut c_int) >= 0 {
            (*userdata.cast::<RefCell<Vec<bool>>>()).borrow_mut().push(sleeping != 0);
        }
    }
    0
}
