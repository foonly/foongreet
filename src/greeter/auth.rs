//! greetd IPC on a worker thread, so slow PAM round-trips (fail delays,
//! fingerprint readers) never stall rendering.

use greetd_ipc::{AuthMessageType, ErrorType, Request, Response, codec::SyncCodec};
use std::{
    os::unix::net::UnixStream,
    sync::mpsc::{Receiver, Sender, channel},
    thread,
    time::Duration,
};

pub struct Auth {
    tx: Sender<Request>,
    pub rx: Receiver<Response>,
}

impl Auth {
    pub fn send(&self, req: Request) {
        let _ = self.tx.send(req);
    }

    pub fn connect() -> Result<Auth, String> {
        let path = std::env::var("GREETD_SOCK").map_err(|_| "GREETD_SOCK is not set (use --mock outside greetd)")?;
        let mut stream = UnixStream::connect(&path).map_err(|e| format!("{path}: {e}"))?;
        Ok(spawn(move |req| {
            req.write_to(&mut stream).map_err(|e| e.to_string())?;
            Response::read_from(&mut stream).map_err(|e| e.to_string())
        }))
    }

    /// Fake greetd: any user, password "password".
    pub fn mock() -> Auth {
        spawn(|req| {
            thread::sleep(Duration::from_millis(600));
            Ok(match req {
                Request::CreateSession { .. } => Response::AuthMessage {
                    auth_message_type: AuthMessageType::Secret,
                    auth_message: "Password: ".into(),
                },
                Request::PostAuthMessageResponse { response } if response.as_deref() == Some("password") => {
                    Response::Success
                }
                Request::PostAuthMessageResponse { .. } => Response::Error {
                    error_type: ErrorType::AuthError,
                    description: "pam_authenticate: AUTH_ERR".into(),
                },
                Request::StartSession { cmd, env } => {
                    eprintln!("mock: would start {cmd:?} with {env:?}");
                    Response::Success
                }
                Request::CancelSession => Response::Success,
            })
        })
    }
}

fn spawn(mut call: impl FnMut(Request) -> Result<Response, String> + Send + 'static) -> Auth {
    let (tx, req_rx) = channel::<Request>();
    let (resp_tx, rx) = channel();
    thread::spawn(move || {
        for req in req_rx {
            let resp = call(req).unwrap_or_else(|e| Response::Error {
                error_type: ErrorType::Error,
                description: format!("greetd: {e}"),
            });
            if resp_tx.send(resp).is_err() {
                break;
            }
        }
    });
    Auth { tx, rx }
}
