//! Guard the manager's terminal-input boundary. Crossterm 0.29 decodes OSC
//! replies as Alt+']' plus ordinary keys; those keys must never become actions
//! or draft/form text. This module does not read or redraw agent output.
use anyhow::{Result, bail};
use crossterm::event::{Event, KeyCode, KeyModifiers};
use std::time::{Duration, Instant};

pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
pub const ESCAPE_DELAY: Duration = Duration::from_millis(250);
const MAX_RESPONSE_BYTES: usize = 4096;

#[derive(Debug)]
struct Response {
    began: Instant,
    bytes: usize,
    escaped: bool,
}

#[derive(Debug, Default)]
pub struct TerminalInputGuard {
    response: Option<Response>,
    // Legacy terminals cannot distinguish a standalone Esc from the beginning
    // of a fragmented sequence without waiting briefly. Quarantine it so an
    // OSC reply cannot cancel a modal or discard unsaved local text.
    pending_escape: Option<(Instant, Event)>,
    // After a genuine standalone Esc is released, a later bare ']' is ambiguous.
    // Fail closed on that single next-key case rather than risking OSC payload
    // keys. This boolean does not block input or retain an unbounded buffer.
    released_escape: bool,
}

impl TerminalInputGuard {
    pub fn check_timeout(&self, now: Instant) -> Result<()> {
        if self
            .response
            .as_ref()
            .is_some_and(|r| now.saturating_duration_since(r.began) >= RESPONSE_TIMEOUT)
        {
            bail!(
                "unterminated terminal control response; manager closed safely, native agents retained; reopen the manager"
            );
        }
        Ok(())
    }

    pub fn wait_limit(&self, now: Instant, proposed: Duration) -> Duration {
        let proposed = self.pending_escape.as_ref().map_or(proposed, |(began, _)| {
            proposed.min(ESCAPE_DELAY.saturating_sub(now.saturating_duration_since(*began)))
        });
        self.response.as_ref().map_or(proposed, |r| {
            proposed.min(RESPONSE_TIMEOUT.saturating_sub(now.saturating_duration_since(r.began)))
        })
    }

    /// Release a standalone Esc only after the disambiguation deadline. A later
    /// bare ']' fails closed; there is no timeout path that exposes OSC payload.
    pub fn ready(&mut self, now: Instant) -> Result<Vec<Event>> {
        self.check_timeout(now)?;
        if self
            .pending_escape
            .as_ref()
            .is_some_and(|(began, _)| now.saturating_duration_since(*began) >= ESCAPE_DELAY)
        {
            let (_, event) = self.pending_escape.take().unwrap();
            self.released_escape = true;
            return Ok(vec![event]);
        }
        Ok(Vec::new())
    }

    /// Return only actual application events. This guards observed 7-bit OSC
    /// framing (ESC ] ... BEL/ST) after Crossterm decoding. Raw 8-bit C1 bytes
    /// are outside this UTF-8 terminal contract: Crossterm may discard those
    /// introducers before this boundary can observe them.
    pub fn filter(&mut self, event: Event, now: Instant) -> Result<Vec<Event>> {
        let mut output = self.ready(now)?;
        if let Some(response) = &mut self.response {
            if let Event::Key(key) = &event {
                let alt = key.modifiers.contains(KeyModifiers::ALT);
                let control = key.modifiers.contains(KeyModifiers::CONTROL);
                let bel = key.code == KeyCode::Char('g') && control && !alt;
                let st = key.code == KeyCode::Char('\\') && (alt || response.escaped);
                let utf8_st = key.code == KeyCode::Char('\u{009c}');
                if bel || st || utf8_st {
                    self.response = None;
                    self.released_escape = false;
                    return Ok(output);
                }
                response.escaped = key.code == KeyCode::Esc;
                response.bytes = response.bytes.saturating_add(match key.code {
                    KeyCode::Char(ch) => ch.len_utf8(),
                    _ => 1,
                });
            } else if let Event::Paste(text) = &event {
                response.bytes = response.bytes.saturating_add(text.len());
                response.escaped = false;
            }
            if response.bytes > MAX_RESPONSE_BYTES {
                bail!(
                    "terminal control response exceeds 4096 bytes; manager closed safely, native agents retained"
                );
            }
            if matches!(event, Event::Resize(_, _)) {
                output.push(event);
            }
            return Ok(output);
        }
        if matches!(event, Event::Resize(_, _)) {
            output.push(event);
            return Ok(output);
        }
        // Only a following key disambiguates a released Esc. Mouse/focus/paste
        // events may interleave with terminal replies and must not erase the
        // next-key tombstone. If they flush a pending Esc, retain that marker.
        if !matches!(event, Event::Key(_)) {
            if let Some((_, pending)) = self.pending_escape.take() {
                output.push(pending);
                self.released_escape = true;
            }
            output.push(event);
            return Ok(output);
        }
        let osc = matches!(&event, Event::Key(key) if
            (key.code == KeyCode::Char(']') && key.modifiers.contains(KeyModifiers::ALT))
            || key.code == KeyCode::Char('\u{009d}'));
        let bare_bracket = matches!(&event, Event::Key(key) if key.code == KeyCode::Char(']') && key.modifiers.is_empty());
        if bare_bracket && self.released_escape {
            bail!(
                "ambiguous late terminal response after Esc; manager closed safely, native agents retained"
            );
        }
        if osc || (bare_bracket && self.pending_escape.is_some()) {
            let began = self.pending_escape.take().map_or(now, |(began, _)| began);
            self.released_escape = false;
            self.response = Some(Response {
                began,
                bytes: 0,
                escaped: false,
            });
            return Ok(output);
        }
        if let Some((_, pending)) = self.pending_escape.take() {
            output.push(pending);
        }
        self.released_escape = false;
        if matches!(&event, Event::Key(key) if key.code == KeyCode::Esc) {
            self.pending_escape = Some((now, event));
        } else {
            output.push(event);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
    fn key(ch: char, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(ch), modifiers))
    }
    fn esc() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }
    #[test]
    fn response_cannot_invoke_any_shortcut_or_editor_input() {
        for terminator in [
            key('g', KeyModifiers::CONTROL),
            key('\\', KeyModifiers::ALT),
        ] {
            let now = Instant::now();
            let mut guard = TerminalInputGuard::default();
            assert_eq!(
                guard.filter(key('a', KeyModifiers::NONE), now).unwrap(),
                vec![key('a', KeyModifiers::NONE)]
            );
            assert!(
                guard
                    .filter(key(']', KeyModifiers::ALT), now)
                    .unwrap()
                    .is_empty()
            );
            for ch in "4;102;rgb:8787/8787/8787ryxSuisqN한글".chars() {
                assert!(
                    guard
                        .filter(key(ch, KeyModifiers::NONE), now)
                        .unwrap()
                        .is_empty()
                );
            }
            assert!(guard.filter(terminator, now).unwrap().is_empty());
            assert_eq!(
                guard.filter(key('z', KeyModifiers::NONE), now).unwrap(),
                vec![key('z', KeyModifiers::NONE)]
            );
        }
    }
    #[test]
    fn fragmented_introducer_never_cancels_and_st_never_releases_payload() {
        let now = Instant::now();
        let mut guard = TerminalInputGuard::default();
        assert!(guard.filter(esc(), now).unwrap().is_empty());
        assert!(
            guard
                .filter(
                    key(']', KeyModifiers::NONE),
                    now + Duration::from_millis(50)
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            guard
                .filter(
                    key('r', KeyModifiers::NONE),
                    now + Duration::from_millis(60)
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            guard
                .filter(esc(), now + Duration::from_millis(70))
                .unwrap()
                .is_empty()
        );
        assert!(
            guard
                .filter(
                    key('\\', KeyModifiers::NONE),
                    now + Duration::from_millis(80)
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            guard
                .filter(
                    key('한', KeyModifiers::NONE),
                    now + Duration::from_millis(90)
                )
                .unwrap(),
            vec![key('한', KeyModifiers::NONE)]
        );
    }
    #[test]
    fn ordinary_escape_unicode_paste_and_mouse_are_preserved() {
        let now = Instant::now();
        let mut guard = TerminalInputGuard::default();
        assert!(guard.filter(esc(), now).unwrap().is_empty());
        assert_eq!(guard.ready(now + ESCAPE_DELAY).unwrap(), vec![esc()]);
        for event in [
            key('한', KeyModifiers::NONE),
            Event::Paste("literal rgb text".into()),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 12,
                row: 4,
                modifiers: KeyModifiers::NONE,
            }),
            Event::Resize(72, 14),
        ] {
            assert_eq!(
                guard.filter(event.clone(), now + ESCAPE_DELAY).unwrap(),
                vec![event]
            );
        }
        guard.filter(esc(), now + ESCAPE_DELAY).unwrap();
        assert_eq!(
            guard
                .filter(key('a', KeyModifiers::NONE), now + ESCAPE_DELAY)
                .unwrap(),
            vec![esc(), key('a', KeyModifiers::NONE)]
        );
    }
    #[test]
    fn late_escape_fragment_fails_closed_without_payload_dispatch() {
        let now = Instant::now();
        let mut guard = TerminalInputGuard::default();
        guard.filter(esc(), now).unwrap();
        assert_eq!(guard.ready(now + ESCAPE_DELAY).unwrap(), vec![esc()]);
        assert!(
            guard
                .filter(key(']', KeyModifiers::NONE), now + Duration::from_secs(10))
                .is_err()
        );
    }
    #[test]
    fn unterminated_and_oversized_responses_fail_closed() {
        let now = Instant::now();
        let mut guard = TerminalInputGuard::default();
        guard.filter(key(']', KeyModifiers::ALT), now).unwrap();
        assert_eq!(
            guard.wait_limit(now, Duration::from_secs(10)),
            RESPONSE_TIMEOUT
        );
        assert!(
            guard
                .filter(key('y', KeyModifiers::NONE), now + RESPONSE_TIMEOUT)
                .is_err()
        );
        let mut guard = TerminalInputGuard::default();
        guard.filter(key(']', KeyModifiers::ALT), now).unwrap();
        assert!(
            guard
                .filter(Event::Paste("r".repeat(MAX_RESPONSE_BYTES + 1)), now)
                .is_err()
        );
    }
    #[test]
    fn resize_preserves_pending_escape_deadline_and_unrelated_bracket() {
        let now = Instant::now();
        let mut guard = TerminalInputGuard::default();
        assert_eq!(
            guard.filter(key(']', KeyModifiers::NONE), now).unwrap(),
            vec![key(']', KeyModifiers::NONE)]
        );
        guard.filter(esc(), now).unwrap();
        assert_eq!(guard.wait_limit(now, Duration::from_secs(10)), ESCAPE_DELAY);
        assert_eq!(
            guard
                .filter(Event::Resize(80, 24), now + Duration::from_millis(50))
                .unwrap(),
            vec![Event::Resize(80, 24)]
        );
        assert!(
            guard
                .filter(
                    key(']', KeyModifiers::NONE),
                    now + Duration::from_millis(120)
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            guard
                .filter(
                    key('r', KeyModifiers::NONE),
                    now + Duration::from_millis(130)
                )
                .unwrap()
                .is_empty()
        );
        assert!(
            guard
                .filter(
                    key('g', KeyModifiers::CONTROL),
                    now + Duration::from_millis(140)
                )
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn interleaved_nonkey_events_cannot_erase_escape_tombstone() {
        let now = Instant::now();
        for flush in [false, true] {
            for event in [
                Event::FocusGained,
                Event::Paste("local text".into()),
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 2,
                    row: 3,
                    modifiers: KeyModifiers::NONE,
                }),
            ] {
                let mut guard = TerminalInputGuard::default();
                guard.filter(esc(), now).unwrap();
                if flush {
                    assert_eq!(guard.ready(now + ESCAPE_DELAY).unwrap(), vec![esc()]);
                }
                guard
                    .filter(
                        event,
                        now + if flush {
                            ESCAPE_DELAY
                        } else {
                            Duration::from_millis(50)
                        },
                    )
                    .unwrap();
                assert!(
                    guard
                        .filter(key(']', KeyModifiers::NONE), now + Duration::from_secs(1))
                        .is_err()
                );
            }
        }
    }
}
