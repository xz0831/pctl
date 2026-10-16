//! `pctl web`: a small phone page over HTTP that lists sessions, shows one session's screen and
//! types a line into it. It runs only when started. It listens only on loopback or a Tailscale
//! address, and every API call needs the bearer token from a 0600 file. Input goes through
//! `session_prompt`, so a session waiting for a human decision refuses it.
use crate::Core;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

const PAGE: &str = include_str!("web/index.html");
const SCRIPT: &str = include_str!("web/app.js");
const STYLE: &str = include_str!("web/app.css");
const MAX_HEAD: u64 = 16 * 1024;
const MAX_BODY: usize = 80 * 1024; // a 64 KiB prompt plus JSON escaping
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                   base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Loopback (tests) or Tailscale's ranges only: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn listen_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_loopback() || (v.octets()[0] == 100 && v.octets()[1] & 0xc0 == 64),
        IpAddr::V6(v) => v.is_loopback() || v.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// A regular file with no group or other access, holding at least 32 visible ASCII characters.
pub fn read_token(path: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    let m = std::fs::symlink_metadata(path).context("token file")?;
    ensure!(m.is_file(), "token file must be a regular file");
    ensure!(
        m.permissions().mode() & 0o077 == 0,
        "token file must be mode 0600 (no group or other access)"
    );
    let t = std::fs::read_to_string(path).context("token file")?;
    let t = t.trim();
    ensure!(
        t.len() >= 32 && t.bytes().all(|b| b.is_ascii_graphic()),
        "token must be at least 32 visible ASCII characters"
    );
    Ok(t.to_string())
}

/// Compare without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

struct Request {
    method: String,
    path: String,
    auth: Option<String>,
    body: Vec<u8>,
}

fn read_request(r: &mut impl BufRead) -> Result<Request> {
    let mut head = String::new();
    let mut used = 0;
    loop {
        let mut line = String::new();
        let n = r.by_ref().take(MAX_HEAD - used).read_line(&mut line)?;
        used += n as u64;
        ensure!(
            n > 0 && line.ends_with('\n'),
            "head too large or connection closed"
        );
        if line.trim_end().is_empty() {
            break;
        }
        head.push_str(&line);
    }
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let (mut len, mut auth) = (0, None);
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => len = v.trim().parse().context("content-length")?,
            "authorization" => auth = v.trim().strip_prefix("Bearer ").map(str::to_string),
            _ => {}
        }
    }
    ensure!(len <= MAX_BODY, "body too large");
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok(Request {
        method: method.into(),
        path: path.into(),
        auth,
        body,
    })
}

fn respond(s: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    write!(
        s,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         X-Frame-Options: DENY\r\nContent-Security-Policy: {CSP}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    s.write_all(body)
}

/// The JSON API. Every call is a POST with a JSON body; names travel in the body, not the URL.
fn api(core: &Core, path: &str, body: &[u8]) -> (u16, Value) {
    let v: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let name = v["name"].as_str().unwrap_or("");
    let r = match path {
        "/api/sessions" => core.session_list().map(|all| {
            all.iter()
                .map(|s| json!({"name": s["name"], "project": s["project"], "harness": s["harness"], "status": s["status"], "branch": s["branch"]}))
                .collect()
        }),
        "/api/screen" => {
            let lines = v["lines"].as_u64().unwrap_or(60).clamp(1, 500) as usize;
            core.session_get(name).and_then(|s| {
                Ok(json!({"name": name, "status": s["status"], "text": core.session_read(name, lines)?}))
            })
        }
        "/api/prompt" => core.session_prompt(name, v["text"].as_str().unwrap_or("")),
        _ => return (404, json!({"error": "not found"})),
    };
    match r {
        Ok(v) => (200, v),
        Err(e) => {
            let m = format!("{e:#}");
            let code = if m.starts_with("blocked") { 409 } else { 400 };
            (
                code,
                json!({"error": m, "error_code": crate::error_code(&m)}),
            )
        }
    }
}

/// Answer one connection. Returns the status and "METHOD PATH" for the log line.
fn handle(core: &Core, token: &str, s: &mut TcpStream) -> (u16, String) {
    let req = match read_request(&mut BufReader::new(&*s)) {
        Ok(r) => r,
        Err(_) => {
            let _ = respond(s, 400, "text/plain", b"bad request");
            // Closing with unread input (an oversized body) sends a reset that can reach the
            // client before the 400; drain a bounded amount first.
            let _ = s.shutdown(std::net::Shutdown::Write);
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = std::io::copy(&mut Read::take(&*s, 1 << 20), &mut std::io::sink());
            return (400, "-".into());
        }
    };
    let what = format!("{} {}", req.method, req.path);
    let (code, ctype, body) = match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => (200, "text/html; charset=utf-8", PAGE.as_bytes().to_vec()),
        ("GET", "/app.js") => (
            200,
            "text/javascript; charset=utf-8",
            SCRIPT.as_bytes().to_vec(),
        ),
        ("GET", "/app.css") => (200, "text/css; charset=utf-8", STYLE.as_bytes().to_vec()),
        ("POST", p) if p.starts_with("/api/") => {
            if !req
                .auth
                .is_some_and(|a| same(a.as_bytes(), token.as_bytes()))
            {
                std::thread::sleep(Duration::from_secs(1)); // slow down guessing
                (
                    401,
                    "application/json",
                    br#"{"error":"unauthorized"}"#.to_vec(),
                )
            } else {
                let (code, v) = api(core, p, &req.body);
                (code, "application/json", v.to_string().into_bytes())
            }
        }
        _ => (404, "text/plain", b"not found".to_vec()),
    };
    let _ = respond(s, code, ctype, &body);
    (code, what)
}

/// Serve until killed. Prints `{"listening": "<addr>"}` once bound; logs method, path and status
/// to stderr, never the token, the session names or the text.
pub fn serve(core: &Core, listen: &str, token_file: &Path) -> Result<()> {
    let addr: SocketAddr = listen.parse().context("--listen must be IP:PORT")?;
    if !listen_allowed(addr.ip()) {
        bail!(
            "refused: listen only on loopback or a Tailscale address (100.64.0.0/10, fd7a:115c:a1e0::/48), not {}",
            addr.ip()
        );
    }
    let token = read_token(token_file)?;
    let l = TcpListener::bind(addr).with_context(|| format!("bind {addr}"))?;
    println!("{}", json!({"listening": l.local_addr()?.to_string()}));
    std::io::stdout().flush()?;
    // shortcut: one connection at a time (one person, quick tmux reads); use threads if it lags.
    for c in l.incoming() {
        let Ok(mut s) = c else { continue };
        let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
        let (code, what) = handle(core, &token, &mut s);
        eprintln!("{} {what} {code}", crate::now());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listen_only_on_loopback_or_tailscale() {
        for ok in [
            "127.0.0.1",
            "100.64.0.1",
            "100.101.2.3",
            "100.127.255.254",
            "::1",
            "fd7a:115c:a1e0::1",
        ] {
            assert!(listen_allowed(ok.parse().unwrap()), "{ok}");
        }
        for no in [
            "0.0.0.0",
            "::",
            "192.168.1.5",
            "10.0.0.1",
            "100.63.0.1",
            "100.128.0.1",
            "8.8.8.8",
            "fd7a:115c:a1e1::1",
        ] {
            assert!(!listen_allowed(no.parse().unwrap()), "{no}");
        }
    }
    #[test]
    fn token_file_must_be_private_and_long() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("pctl-web-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("t");
        std::fs::write(&f, format!("{}\n", "a".repeat(40))).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token(&f).unwrap_err().to_string().contains("0600"));
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token(&f).unwrap(), "a".repeat(40));
        std::fs::write(&f, "short").unwrap();
        assert!(read_token(&f).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }
    #[test]
    fn request_parsing_and_limits() {
        let raw = b"POST /api/prompt HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer abc\r\nContent-Length: 4\r\n\r\nbodyEXTRA";
        let r = read_request(&mut &raw[..]).unwrap();
        assert_eq!(
            (
                r.method.as_str(),
                r.path.as_str(),
                r.auth.as_deref(),
                &r.body[..]
            ),
            ("POST", "/api/prompt", Some("abc"), &b"body"[..])
        );
        let big = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert!(read_request(&mut big.as_bytes()).is_err());
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_HEAD as usize));
        assert!(read_request(&mut long.as_bytes()).is_err());
        assert!(same(b"token", b"token") && !same(b"token", b"tokem") && !same(b"tok", b"token"));
    }
}
