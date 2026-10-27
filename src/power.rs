//! Power off / reboot commands.
//!
//! The defaults go through logind, which allows them for the active local
//! session (the greeter's) without extra privileges.

use std::process::{Child, Command, Stdio};

#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    PowerOff,
    Reboot,
}

pub struct Power {
    pub poweroff: Vec<String>,
    pub reboot: Vec<String>,
    /// Report the command instead of running it (testing on a live system).
    pub dry_run: bool,
    child: Option<Child>,
}

impl Power {
    pub fn new(poweroff: Vec<String>, reboot: Vec<String>, dry_run: bool) -> Power {
        Power { poweroff, reboot, dry_run, child: None }
    }

    pub fn command(&self, action: Action) -> &[String] {
        match action {
            Action::PowerOff => &self.poweroff,
            Action::Reboot => &self.reboot,
        }
    }

    pub fn run(&mut self, action: Action) -> Result<(), String> {
        let cmd = self.command(action);
        let Some((prog, args)) = cmd.split_first() else { return Err("NO COMMAND CONFIGURED".into()) };
        let child = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{prog}: {e}"))?;
        self.child = Some(child);
        Ok(())
    }

    /// `Some` once the command has exited: `Ok` on success (the system is
    /// going down), `Err` with a message otherwise.
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        let status = self.child.as_mut()?.try_wait();
        let result = match status {
            Ok(None) => return None,
            Ok(Some(s)) if s.success() => Ok(()),
            Ok(Some(s)) => Err(format!("COMMAND FAILED ({s})")),
            Err(e) => Err(e.to_string()),
        };
        self.child = None;
        Some(result)
    }
}
