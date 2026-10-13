//! Regression probe driven by tests/input_guard_pty.py through a real PTY.
use crossterm::{event, terminal};
use std::{
    io::Write,
    time::{Duration, Instant},
};
fn main() {
    terminal::enable_raw_mode().unwrap();
    let result = run();
    terminal::disable_raw_mode().unwrap();
    if let Err(error) = result {
        eprintln!("GUARD_ERROR:{error}");
        std::process::exit(2);
    }
}
fn run() -> anyhow::Result<()> {
    let mut guard = pctl::terminal_input::TerminalInputGuard::default();
    let end = Instant::now() + Duration::from_secs(3);
    println!("READY");
    std::io::stdout().flush()?;
    if std::env::args().any(|arg| arg == "--cursor-check") {
        let (x, y) = crossterm::cursor::position()?;
        println!("CURSOR:{x},{y}");
    }
    while Instant::now() < end {
        for event in guard.ready(Instant::now())? {
            println!("EVENT:{event:?}");
        }
        if event::poll(guard.wait_limit(Instant::now(), Duration::from_millis(50)))? {
            for event in guard.filter(event::read()?, Instant::now())? {
                println!("EVENT:{event:?}");
            }
        }
        std::io::stdout().flush()?;
    }
    Ok(())
}
