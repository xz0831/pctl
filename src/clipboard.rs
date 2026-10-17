//! The image on the clipboard of the computer this runs on. The app reads it (to send it to another
//! computer) and the bridge writes it (for an agent there that reads its own clipboard).
//!
//! Test seams: `PCTL_CLIP_READ` (a shell command that prints a PNG on stdout) and `PCTL_CLIP_WRITE`
//! (one that reads a PNG on stdin) stand in for the platform commands when set. Tests use them so
//! that no real clipboard is touched.
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// The largest image, in PNG bytes, either side takes.
pub const MAX_IMAGE: usize = 20 << 20;
/// A read of the clipboard that takes longer is given up; a write gets longer for a big image.
const READ_WITHIN: Duration = Duration::from_secs(2);
const WRITE_WITHIN: Duration = Duration::from_secs(10);
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&PNG_SIGNATURE)
}

fn sh(script: std::ffi::OsString) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(script);
    c
}

/// The PNG on the clipboard here, or None when there is none (or no tool to ask). Err: it took too
/// long, or the image is over `MAX_IMAGE`.
pub fn read_image() -> Result<Option<Vec<u8>>> {
    let mut hex = false;
    let cmd = if let Some(script) = std::env::var_os("PCTL_CLIP_READ") {
        sh(script)
    } else if cfg!(target_os = "macos") {
        hex = true;
        let mut c = Command::new("osascript");
        c.args(["-e", "the clipboard as «class PNGf»"]);
        c
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let mut c = Command::new("wl-paste");
        c.args(["--type", "image/png"]);
        c
    } else if std::env::var_os("DISPLAY").is_some() {
        let mut c = Command::new("xclip");
        c.args(["-selection", "clipboard", "-t", "image/png", "-o"]);
        c
    } else {
        return Ok(None);
    };
    // osascript prints each byte as two hex digits.
    let cap = if hex { 2 * MAX_IMAGE + 64 } else { MAX_IMAGE };
    let Some(out) = output_within(cmd, cap, READ_WITHIN)? else {
        return Ok(None);
    };
    let png = if hex {
        parse_pngf(&String::from_utf8_lossy(&out))
    } else {
        Some(out)
    };
    let png = png.filter(|p| is_png(p));
    ensure!(
        png.as_ref().is_none_or(|p| p.len() <= MAX_IMAGE),
        "image over {} MB",
        MAX_IMAGE >> 20
    );
    Ok(png)
}

/// The bytes inside `«data PNGf<hex>»`, which is how osascript prints a PNG.
pub fn parse_pngf(text: &str) -> Option<Vec<u8>> {
    let hex = text
        .split_once("«data PNGf")?
        .1
        .split_once('»')?
        .0
        .as_bytes();
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    hex.chunks(2)
        .map(|p| Some(digit(p[0])? << 4 | digit(p[1])?))
        .collect()
}

/// Put the PNG in `file` on the clipboard here.
pub fn write_image(file: &Path) -> Result<()> {
    let mut by_arg = false;
    let mut cmd = if let Some(script) = std::env::var_os("PCTL_CLIP_WRITE") {
        sh(script)
    } else if cfg!(target_os = "macos") {
        by_arg = true;
        let mut c = Command::new("osascript");
        c.args([
            "-e",
            "on run argv",
            "-e",
            "set the clipboard to (read (POSIX file (item 1 of argv)) as «class PNGf»)",
            "-e",
            "end run",
        ]);
        c.arg(file);
        c
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let mut c = Command::new("wl-copy");
        c.args(["--type", "image/png"]);
        c
    } else if std::env::var_os("DISPLAY").is_some() {
        let mut c = Command::new("xclip");
        c.args(["-selection", "clipboard", "-t", "image/png", "-i"]);
        c
    } else {
        bail!("no clipboard tool here (needs macOS, wl-copy or xclip)");
    };
    let stdin = if by_arg {
        Stdio::null()
    } else {
        File::open(file)?.into()
    };
    let mut child = cmd
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start the clipboard command")?;
    match wait_within(&mut child, WRITE_WITHIN)? {
        status if status.success() => Ok(()),
        status => bail!("the clipboard command failed ({status})"),
    }
}

/// Everything `cmd` prints, when it succeeds. None: it is not installed, it failed (the clipboard
/// holds no image) or it printed nothing. Err: it ran past `within`, or printed over `cap` bytes.
/// The command is killed in both cases.
fn output_within(mut cmd: Command, cap: usize, within: Duration) -> Result<Option<Vec<u8>>> {
    let mut child = match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("could not start the clipboard command"),
    };
    let mut stdout = child.stdout.take().context("stdout is piped")?;
    let (tx, rx) = mpsc::channel();
    // Not joined: a grandchild that keeps the pipe open must not hold the caller.
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = tx.send(
            stdout
                .by_ref()
                .take(cap as u64 + 1)
                .read_to_end(&mut buf)
                .map(|_| buf),
        );
    });
    let result = (|| -> Result<Option<Vec<u8>>> {
        let buf = rx
            .recv_timeout(within)
            .map_err(|_| anyhow!("the clipboard did not answer in time"))??;
        ensure!(buf.len() <= cap, "image over {} MB", MAX_IMAGE >> 20);
        let status = wait_within(&mut child, within)?;
        Ok((status.success() && !buf.is_empty()).then_some(buf))
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

/// Wait for `child` at most `within`; a child still running then is killed and reaped.
fn wait_within(child: &mut Child, within: Duration) -> Result<std::process::ExitStatus> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the clipboard command did not finish in time");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SHORT: Duration = Duration::from_millis(300);

    #[test]
    fn osascript_hex_becomes_bytes() {
        let png = parse_pngf("«data PNGf89504E470D0A1A0A00ff»\n").expect("hex");
        assert_eq!(png, [0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 255]);
        assert!(is_png(&png));
        assert_eq!(parse_pngf("«data PNGf»"), Some(vec![]));
        assert_eq!(parse_pngf("«data PNGf8»"), None, "odd number of digits");
        assert_eq!(parse_pngf("«data PNGf8G»"), None, "not hex");
        assert_eq!(parse_pngf("«data PNGf89"), None, "no closing mark");
        assert_eq!(parse_pngf("«data TIFF89»"), None);
        assert_eq!(parse_pngf(""), None);
    }

    #[test]
    fn a_png_needs_its_signature() {
        assert!(!is_png(b""));
        assert!(!is_png(&PNG_SIGNATURE[..7]));
        assert!(!is_png(b"GIF89a-not-a-png"));
    }

    #[test]
    fn output_is_returned_only_for_a_success_with_content() {
        let got = output_within(sh("printf abc".into()), 10, SHORT).expect("runs");
        assert_eq!(got.as_deref(), Some(&b"abc"[..]));
        assert_eq!(
            output_within(sh("exit 1".into()), 10, SHORT).expect("runs"),
            None
        );
        assert_eq!(
            output_within(sh("true".into()), 10, SHORT).expect("runs"),
            None
        );
        let missing = Command::new("pctl-no-such-clipboard-tool");
        assert_eq!(output_within(missing, 10, SHORT).expect("runs"), None);
    }

    #[test]
    fn output_over_the_cap_is_an_error() {
        let e = output_within(sh("printf 0123456789ABC; sleep 30".into()), 10, SHORT)
            .expect_err("over the cap");
        assert!(e.to_string().contains("over"), "{e:#}");
    }

    #[test]
    fn a_grandchild_holding_the_pipe_cannot_hold_the_caller() {
        let started = Instant::now();
        let e =
            output_within(sh("sleep 30 & echo x; wait".into()), 10, SHORT).expect_err("never ends");
        assert!(e.to_string().contains("in time"), "{e:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_write_command_gets_the_file_on_stdin() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        std::fs::write(&from, b"png-bytes").expect("write");
        let mut c = sh(format!("cat > {}", to.display()).into());
        c.stdin(File::open(&from).expect("open"));
        let mut child = c.spawn().expect("spawn");
        assert!(
            wait_within(&mut child, Duration::from_secs(5))
                .expect("ends")
                .success()
        );
        assert_eq!(std::fs::read(&to).expect("read"), b"png-bytes");
        let mut slow = sh("sleep 30".into()).spawn().expect("spawn");
        assert!(
            wait_within(&mut slow, SHORT).is_err(),
            "killed at the deadline"
        );
        assert!(slow.try_wait().expect("reaped").is_some());
    }
}
