//! A little dry, lab-coat humour in places where nothing is at stake: waiting, empty screens,
//! goodbyes, tips. Never in errors, confirmations' facts, or anything agents read. All lines are
//! original. `[ui] wit = false` in the config file turns them into plain text.

/// Rotates with the process id so a restart may show another line.
fn pick(lines: &[&'static str]) -> &'static str {
    lines[std::process::id() as usize % lines.len()]
}

pub fn loading(wit: bool) -> &'static str {
    if !wit {
        return "Loading…";
    }
    pick(&[
        "Asking tmux nicely…",
        "Counting agents across dimensions…",
        "Reticulating timelines…",
        "Warming up the portal. It's mostly a progress bar.",
    ])
}

pub fn welcome(wit: bool) -> [&'static str; 2] {
    if wit {
        [
            "Welcome to pctl.",
            "Every universe starts somewhere. This one starts with a project.",
        ]
    } else {
        ["Welcome to pctl.", "Add a project, then its first agent."]
    }
}

pub fn empty_project(wit: bool) -> &'static str {
    if wit {
        "Statistically the quietest place in the multiverse."
    } else {
        ""
    }
}

/// After "N waiting for you" in the status line.
pub fn waiting(wit: bool, n: usize) -> String {
    match (wit, n) {
        (false, n) => format!("! {n} waiting for you"),
        (true, 1) => "! 1 waiting for you (a human, specifically)".into(),
        (true, n) => format!("! {n} waiting for you (humans: still useful)"),
    }
}

/// The stop question keeps its facts first; the joke, if any, comes last.
pub fn stop_question(wit: bool, name: &str) -> String {
    let base =
        format!("Stop {name}? Its process ends; Start brings it back in the same conversation.");
    if wit {
        format!("{base} Rude, but legal.")
    } else {
        base
    }
}

pub fn tip(wit: bool) -> &'static str {
    if !wit {
        return "Any key closes this.";
    }
    pick(&[
        "Tip: prefix twice sends the prefix to the agent. Keys inside keys.",
        "Tip: the wheel scrolls back. The past is read-only, as it should be.",
        "Tip: right-click an agent. It won't mind.",
        "Tip: agents keep running after you leave. They never needed supervision.",
    ])
}

pub fn goodbye(wit: bool) -> &'static str {
    if wit {
        "Left pctl. Your agents keep working; they never needed supervision anyway."
    } else {
        "Left pctl. The agents keep running."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_mode_has_no_jokes_and_facts_stay_first() {
        assert_eq!(loading(false), "Loading…");
        assert_eq!(waiting(false, 2), "! 2 waiting for you");
        assert!(waiting(true, 2).starts_with("! 2 waiting for you"));
        let q = stop_question(true, "a");
        assert!(q.starts_with("Stop a? Its process ends;"), "{q}");
        assert!(!stop_question(false, "a").contains("Rude"));
        assert!(empty_project(false).is_empty());
    }
}
