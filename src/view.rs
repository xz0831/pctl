//! `pctl view`: the document reader (mdv) with pctl's clipboard and `tell` behind its hooks.
use crate::{
    Core, clipboard,
    tell::{self, Verdict},
};
use anyhow::{Result, anyhow};
use mdv::view::{AskOutcome, AskRequest, CopyOutcome, Hooks, ViewOptions};
use std::{io::Write, path::Path};

pub fn run(core: &Core, path: &Path, options: ViewOptions, remote: bool) -> Result<()> {
    // The reader asks `pctl` about sessions; it must be this binary on this state, not an installed one.
    // SAFETY: nothing else runs yet; this is called from the command's own thread before any other.
    unsafe {
        if let Ok(exe) = std::env::current_exe() {
            std::env::set_var("PCTL_BIN", exe);
        }
        std::env::set_var("PCTL_STATE", &core.state);
        std::env::set_var("PCTL_TMUX", &core.tmux);
    }
    mdv::view::run(path, options, hooks(core, remote)).map_err(|e| anyhow!(e))
}

/// `remote`: the person is not at this computer (the app shows it from another one).
pub fn hooks(core: &Core, remote: bool) -> Hooks {
    let core = core.clone();
    Hooks {
        copy: Box::new(move |text| copy(text, remote)),
        ask: Box::new(move |request| ask(&core, request)),
    }
}

/// An OSC 52 write on our own stdout and, on this computer's own desktop, a clipboard command
/// too, as the app does for a drag. Over ssh that clipboard is not the person's.
fn copy(text: &str, remote: bool) -> CopyOutcome {
    // The status line is a few characters wide in a narrow cell; the app copies a mouse drag itself.
    if remote {
        return CopyOutcome::Failed("drag to copy".into());
    }
    if text.len() >= clipboard::MAX_COPY {
        return CopyOutcome::Failed(format!(
            "too long to copy ({} characters)",
            text.chars().count()
        ));
    }
    let mut out = std::io::stdout();
    let sent = out
        .write_all(mdv::reader::osc52(text).as_bytes())
        .and_then(|()| out.flush());
    let local = std::env::var_os("SSH_CONNECTION").is_none();
    let command_ok = local && clipboard::write_text(text).is_ok();
    copy_outcome(&clipboard::CopyWay::from_env(command_ok), sent.is_ok())
}

/// Only a copy known to have landed is `Copied`; a terminal known to drop OSC 52 is a failure.
fn copy_outcome(way: &clipboard::CopyWay, sent: bool) -> CopyOutcome {
    if way.command_ok || (way.honours_osc52 && sent) {
        CopyOutcome::Copied
    } else if way.vte {
        CopyOutcome::Failed("this terminal ignores copying: Shift+drag, then ctrl+shift+c".into())
    } else if sent {
        CopyOutcome::SentToTerminal
    } else {
        CopyOutcome::Failed("could not write to the terminal".into())
    }
}

/// `tell` the recipient the question, if it is still the run the reader checked.
fn ask(core: &Core, request: &AskRequest) -> AskOutcome {
    let who = &request.recipient;
    match core.session_get(&who.name) {
        Ok(s) if s["run_id"] == who.run_id.as_str() => {}
        Ok(_) => return AskOutcome::Failed("recipient changed; try again".into()),
        Err(e) => return AskOutcome::Failed(format!("{e:#}")),
    }
    ask_outcome(&tell::run(
        core,
        &tell::Request {
            to: who.name.clone(),
            text: request.message.clone(),
            wait: false,
            context: None,
            from_line: None,
        },
    ))
}

/// The words are the ones the reader's delivery state looks for.
fn ask_outcome(out: &tell::Outcome) -> AskOutcome {
    match out.result {
        Verdict::Blocked => AskOutcome::Blocked,
        Verdict::Reached => AskOutcome::Delivered("도달".into()),
        Verdict::Queued => AskOutcome::Delivered("대기열".into()),
        Verdict::TypedUnconfirmed => AskOutcome::Delivered("입력함·확인 안 됨".into()),
        // tell keeps it and delivers it once the main can take it: done here, so no resend.
        Verdict::QueuedBlocked => AskOutcome::Delivered("보관(열리면 전달)".into()),
        v if v.exit_code() == 0 => AskOutcome::Delivered(out.plain()),
        v => AskOutcome::Failed(out.note.clone().unwrap_or_else(|| v.name().into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn way(command_ok: bool, honours_osc52: bool, vte: bool) -> clipboard::CopyWay {
        clipboard::CopyWay {
            command_ok,
            honours_osc52,
            vte,
        }
    }

    #[test]
    fn a_document_on_another_computer_is_not_copied_by_a_command_here() {
        assert!(matches!(
            copy("text", true),
            CopyOutcome::Failed(why) if why.contains("drag")
        ));
    }

    #[test]
    fn copy_result_is_honest() {
        assert_eq!(
            copy_outcome(&way(true, false, true), true),
            CopyOutcome::Copied
        );
        assert_eq!(
            copy_outcome(&way(false, true, false), true),
            CopyOutcome::Copied
        );
        assert_eq!(
            copy_outcome(&way(false, false, false), true),
            CopyOutcome::SentToTerminal
        );
        assert!(matches!(
            copy_outcome(&way(false, false, true), true),
            CopyOutcome::Failed(why) if why.contains("Shift+drag")
        ));
        // Nothing reached the terminal and no command ran: not "Copied", not "sent".
        assert!(matches!(
            copy_outcome(&way(false, true, false), false),
            CopyOutcome::Failed(_)
        ));
    }

    #[test]
    fn tell_verdicts_map_to_ask_outcomes() {
        let out = |v, note: Option<&str>| tell::Outcome {
            result: v,
            to: "x-main".into(),
            context: None,
            reply: None,
            note: note.map(String::from),
            queue: None,
        };
        assert_eq!(
            ask_outcome(&out(Verdict::Reached, None)),
            AskOutcome::Delivered("도달".into())
        );
        assert_eq!(
            ask_outcome(&out(Verdict::Queued, None)),
            AskOutcome::Delivered("대기열".into())
        );
        assert_eq!(
            ask_outcome(&out(Verdict::Blocked, Some("waits"))),
            AskOutcome::Blocked
        );
        assert_eq!(
            ask_outcome(&out(Verdict::NotSent, Some("the session is stopped"))),
            AskOutcome::Failed("the session is stopped".into())
        );
        assert_eq!(
            ask_outcome(&out(Verdict::NoConnection, None)),
            AskOutcome::Failed("no_connection".into())
        );
    }
}
