use mdv::{LinkMap, Style, parse, to_ansi, to_html};
use std::{
    env, fs,
    io::{self, Write},
    process::ExitCode,
};
fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let first = args
        .next()
        .ok_or("usage: mdv view <file|folder> | mdv render <file> [--width N] [--html]")?;
    if first == "answer" {
        let id = args.next().ok_or("missing request id")?;
        let mut text = args.next().ok_or("missing answer")?;
        if args.next().is_some() {
            return Err("unexpected answer argument".into());
        }
        if text == "-" {
            use std::io::Read;
            text.clear();
            io::stdin()
                .take(16001)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
        }
        return mdv::threads::answer(&id, &text);
    }
    if first != "render" {
        let path = if first == "view" {
            args.next().ok_or("missing file or folder")?
        } else {
            first
        };
        let mut project = None;
        let mut look = None;
        let mut dark = false;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--look" => {
                    look = Some(match args.next().as_deref() {
                        Some("portal") => mdv::Look::Portal,
                        Some("garage") => mdv::Look::Garage,
                        _ => return Err("look must be portal or garage".into()),
                    })
                }
                "--dark" => dark = true,
                "--project" => project = Some(args.next().ok_or("missing project")?),
                _ => return Err("unexpected view argument".into()),
            }
        }
        return mdv::view::run(
            std::path::Path::new(&path),
            mdv::view::ViewOptions {
                project,
                look,
                dark,
            },
            mdv::view::Hooks::default(),
        );
    }
    let path = args.next().ok_or("missing file")?;
    let mut width = 80;
    let mut html = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--html" => html = true,
            "--width" => {
                width = args
                    .next()
                    .ok_or("missing width")?
                    .parse::<usize>()
                    .map_err(|_| "width must be a positive integer")?;
                if width == 0 {
                    return Err("width must be positive".into());
                }
            }
            _ => return Err(format!("unknown argument: {}", mdv::visible(&arg))),
        }
    }
    let src = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", mdv::visible(&path)))?;
    let doc = parse(&src);
    let output = if html {
        to_html(&doc, &src, &LinkMap::default())
    } else {
        to_ansi(&doc, &src, width, &Style::default())
            .into_iter()
            .map(|l| l.styled + "\n")
            .collect()
    };
    io::stdout()
        .lock()
        .write_all(output.as_bytes())
        .map_err(|e| e.to_string())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mdv: {}", mdv::visible(&e));
            ExitCode::FAILURE
        }
    }
}
