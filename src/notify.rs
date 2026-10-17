//! A desktop notification from this computer, for terminals that ignore OSC 777 (MATE Terminal and
//! other VTE ones). The app still writes the OSC 777 for those that show it.
//!
//! Test seam: `PCTL_NOTIFY_SEND` is a command (run as `<command> -- pctl <body>`) that stands in for
//! `notify-send` and for the platform and display checks.
use crate::clipboard::wait_within;
use std::{
    process::{Command, Stdio},
    thread,
    time::Duration,
};

/// `notify-send` talks to a notification daemon over D-Bus; one that is stuck is cut off.
const SEND_WITHIN: Duration = Duration::from_secs(10);

/// Linux with a display, not over ssh: only then is this computer the person's desktop.
fn wanted(linux: bool, ssh: bool, display: bool) -> bool {
    linux && !ssh && display
}

/// Show `body` as a notification titled `pctl`, on a thread of its own so drawing never waits.
/// Nothing happens where the conditions do not hold or `notify-send` is not installed.
pub fn desktop(body: &str) {
    let ssh = std::env::var_os("SSH_CONNECTION").is_some();
    let mut cmd = if let Some(program) = std::env::var_os("PCTL_NOTIFY_SEND") {
        if ssh {
            return;
        }
        Command::new(program)
    } else if wanted(
        cfg!(target_os = "linux"),
        ssh,
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some(),
    ) {
        Command::new("notify-send")
    } else {
        return;
    };
    // `--` keeps an agent name that starts with `-` from being read as an option.
    cmd.args(["--", "pctl", body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    thread::spawn(move || {
        if let Ok(mut child) = cmd.spawn() {
            let _ = wait_within(&mut child, SEND_WITHIN);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_local_linux_desktop_gets_a_notification() {
        assert!(wanted(true, false, true));
        assert!(
            !wanted(false, false, true),
            "macOS: the terminal shows OSC 777"
        );
        assert!(!wanted(true, true, true), "over ssh");
        assert!(!wanted(true, false, false), "no display");
    }
}
