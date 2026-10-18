//! Screens by push. The client names the cells it shows (`watch`, the whole set each time, like
//! `view`); one thread reads the due ones with a single tmux call and writes a `screen` event for
//! each that changed. A cell is read often while it is hot (something was just typed or its
//! settings changed) or still changing, and rarely when quiet. A pane that no longer holds its run
//! gets one `changed` event and is not read again.
use super::{
    Screen, emit, pane_of, read_screen, read_screens, resize, screen_json, small, verified,
};
use crate::{Core, control::Control};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

/// After typing or a change of settings a cell is read this often, for `HOT`; for the first
/// `RUSH` after typing, back to back, because the program answers within a few ms and each read
/// itself takes ~6 ms.
const HOT_EVERY: Duration = Duration::from_millis(15);
const RUSH_EVERY: Duration = Duration::from_millis(3);
const HOT: Duration = Duration::from_millis(800);
const RUSH: Duration = Duration::from_millis(120);
/// The program needs a moment to answer a key; reading sooner would find nothing new.
const ANSWER_AFTER: Duration = Duration::from_millis(6);
/// A cell whose pane is covered by a control connection is read when its pane wrote; it is also
/// read this often in case something changed without output (a resize, a new run).
const SAFETY: Duration = Duration::from_secs(1);
/// The most cells one watch holds.
const MAX_CELLS: usize = 64;

/// How long to wait before the next read of a cell that was unchanged `quiet` reads in a row.
fn every(quiet: u32) -> Duration {
    Duration::from_millis(match quiet {
        0..=2 => 40,
        3..=8 => 100,
        _ => 250,
    })
}

struct Cell {
    key: String,
    /// The client's number for these settings: unchanged key and rev keep what was read.
    rev: u64,
    agent: String,
    run: String,
    width: u16,
    height: u16,
    scroll: usize,
    pane: Option<String>,
    last: Option<Screen>,
    quiet: u32,
    next: Instant,
    hot_until: Instant,
    rush_until: Instant,
    reported: bool,
    /// Its output reaches us over a control connection, so it is read when `dirty`, not by timer.
    controlled: bool,
    dirty: bool,
    safety: Instant,
}

impl Cell {
    /// When this cell is next to be read.
    fn due_at(&self) -> Instant {
        match (self.controlled, self.dirty) {
            (false, _) | (true, true) => self.next,
            (true, false) => self.next.max(self.safety),
        }
    }
}

#[derive(Default)]
struct State {
    cells: Vec<Cell>,
    stop: bool,
    started: bool,
    control: Option<Arc<Control>>,
}

pub struct Watch {
    state: Mutex<State>,
    wake: Condvar,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Watch {
    pub fn new() -> Arc<Watch> {
        Arc::new(Watch {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        })
    }

    /// The `watch` op: replace the set of cells. A cell whose pane's window is not at the asked
    /// size is resized first (once per change, as the `screen` op does).
    pub fn set(
        self: &Arc<Self>,
        core: &Core,
        req: &Value,
        asked: &mut HashMap<String, (u16, u16)>,
    ) -> Result<Value> {
        let list = req["cells"].as_array().cloned().unwrap_or_default();
        ensure!(list.len() <= MAX_CELLS, "too many watched cells");
        let now = Instant::now();
        let mut cells = vec![];
        for c in &list {
            let text = |k: &str| c[k].as_str().unwrap_or_default().to_string();
            let mut cell = Cell {
                key: text("key"),
                rev: c["rev"].as_u64().unwrap_or(0),
                agent: text("agent"),
                run: text("run"),
                width: small(c, "width")?,
                height: small(c, "height")?,
                scroll: usize::try_from(c["scroll"].as_u64().unwrap_or(0)).unwrap_or(0),
                pane: None,
                last: None,
                quiet: 0,
                next: now,
                hot_until: now + HOT,
                rush_until: now,
                reported: false,
                controlled: false,
                dirty: false,
                safety: now,
            };
            ensure!(!cell.key.is_empty(), "a watched cell needs a key");
            cell.pane = pane_of(core, &cell.agent, &cell.run).ok();
            cells.push(cell);
        }
        let mut st = lock(&self.state);
        // Unchanged cells keep what they have read.
        for cell in &mut cells {
            if let Some(old) = st
                .cells
                .iter_mut()
                .find(|o| o.key == cell.key && o.rev == cell.rev)
            {
                cell.last = old.last.take();
                cell.quiet = old.quiet;
                cell.next = old.next;
                cell.hot_until = old.hot_until;
                cell.rush_until = old.rush_until;
                cell.reported = old.reported;
                cell.controlled = old.controlled;
                cell.dirty = old.dirty;
                cell.safety = old.safety;
                cell.pane = old.pane.take().or(cell.pane.take());
            } else if let Some(pane) = cell.pane.clone() {
                let size = (cell.width, cell.height);
                if size.0 >= 2 && size.1 >= 2 && asked.get(&pane) != Some(&size) {
                    if verified(core, &pane, &cell.run).is_ok() {
                        asked.insert(pane.clone(), size);
                        let _ = resize(core, &pane, size.0, size.1);
                    } else {
                        cell.pane = None;
                    }
                }
            }
        }
        let n = cells.len();
        let control = st
            .control
            .get_or_insert_with(|| {
                let control = Control::new(core);
                let me = self.clone();
                control.on_dirty(Arc::new(move |pane| me.dirty(pane)));
                control
            })
            .clone();
        control.watch(
            &cells
                .iter()
                .filter_map(|c| c.pane.clone())
                .collect::<Vec<_>>(),
        );
        st.cells = cells;
        if !st.started && n > 0 {
            st.started = true;
            let (core, me) = (core.clone(), self.clone());
            thread::spawn(move || me.run(&core, &control));
        }
        self.wake.notify_all();
        Ok(json!({"watching": n}))
    }

    /// Something went to this run (keys, a paste, an image): read its cell at once and often.
    pub fn heat(&self, run: &str) {
        let now = Instant::now();
        let mut st = lock(&self.state);
        let mut any = false;
        for c in st.cells.iter_mut().filter(|c| c.run == run) {
            c.hot_until = now + HOT;
            c.rush_until = now + RUSH;
            // A covered pane answers with output, which makes the cell dirty: nothing to wait for.
            c.next = if c.controlled {
                now
            } else {
                now + ANSWER_AFTER
            };
            c.quiet = 0;
            any = true;
        }
        if any {
            self.wake.notify_all();
        }
    }

    /// The control connections of this bridge, once cells are watched.
    pub fn control(&self) -> Option<Arc<Control>> {
        lock(&self.state).control.clone()
    }

    /// A pane wrote (reported by the control connection).
    fn dirty(&self, pane: &str) {
        let mut st = lock(&self.state);
        let mut any = false;
        for c in st
            .cells
            .iter_mut()
            .filter(|c| c.pane.as_deref() == Some(pane))
        {
            c.dirty = true;
            any = true;
        }
        if any {
            self.wake.notify_all();
        }
    }

    /// End the thread (the client closed the pipe).
    pub fn stop(&self) {
        lock(&self.state).stop = true;
        self.wake.notify_all();
    }

    fn run(&self, core: &Core, control: &Arc<Control>) {
        loop {
            // Which cells have a control connection now (this starts the ones that have none yet).
            let panes: Vec<(String, String)> = lock(&self.state)
                .cells
                .iter()
                .filter(|c| c.scroll == 0 && !c.reported)
                .filter_map(|c| Some((c.key.clone(), c.pane.clone()?)))
                .collect();
            let covered: Vec<(String, bool)> = panes
                .into_iter()
                .map(|(key, pane)| (key, control.covers(&pane)))
                .collect();
            control.sweep();
            // The cells due now, copied out so no lock is held while tmux runs.
            let due: Vec<(String, u64, String, String, u16, usize, bool)> = {
                let mut st = lock(&self.state);
                for (key, yes) in covered {
                    for c in st.cells.iter_mut().filter(|c| c.key == key) {
                        c.controlled = yes;
                    }
                }
                loop {
                    if st.stop {
                        return;
                    }
                    let now = Instant::now();
                    let due: Vec<_> = st
                        .cells
                        .iter_mut()
                        .filter(|c| c.due_at() <= now && !c.reported)
                        .map(|c| {
                            c.dirty = false;
                            (
                                c.key.clone(),
                                c.rev,
                                c.pane.clone().unwrap_or_default(),
                                c.run.clone(),
                                c.height,
                                c.scroll,
                                c.controlled,
                            )
                        })
                        .collect();
                    if !due.is_empty() {
                        break due;
                    }
                    let wait = st
                        .cells
                        .iter()
                        .filter(|c| !c.reported)
                        .map(|c| c.due_at().saturating_duration_since(now))
                        .min()
                        .unwrap_or(Duration::from_millis(250))
                        .min(Duration::from_millis(250));
                    st = self
                        .wake
                        .wait_timeout(st, wait.max(Duration::from_millis(1)))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            };
            // Live cells in one call (over the control connection when they have one), scrolled-back
            // ones (their window needs the pane's height) each on their own, a cell without a pane
            // is a change.
            let mut read: Vec<Option<Result<Option<Screen>>>> =
                (0..due.len()).map(|_| None).collect();
            let live: Vec<usize> = (0..due.len())
                .filter(|&i| due[i].5 == 0 && !due[i].2.is_empty())
                .collect();
            let batch = |ix: &[usize]| -> Vec<(&str, &str, u16)> {
                ix.iter()
                    .map(|&i| (due[i].2.as_str(), due[i].3.as_str(), due[i].4))
                    .collect()
            };
            let (via, plain): (Vec<usize>, Vec<usize>) = live.iter().partition(|&&i| due[i].6);
            let mut plain = plain;
            match (!via.is_empty())
                .then(|| control.read_screens(&batch(&via)))
                .flatten()
            {
                Some(answers) => {
                    for (&i, r) in via.iter().zip(answers) {
                        read[i] = Some(r);
                    }
                }
                None => plain.extend(&via),
            }
            if !plain.is_empty() {
                for (&i, r) in plain.iter().zip(read_screens(core, &batch(&plain))) {
                    read[i] = Some(r);
                }
            }
            for (i, d) in due.iter().enumerate() {
                if read[i].is_none() {
                    read[i] = Some(if d.2.is_empty() {
                        Ok(None)
                    } else {
                        read_screen(core, &d.2, &d.3, d.4, d.5)
                    });
                }
            }
            for (d, r) in due.iter().zip(read) {
                self.finish(&d.0, d.1, r.unwrap_or(Ok(None)));
            }
        }
    }

    /// Take one read: push it if the cell still has these settings and the screen changed.
    fn finish(&self, key: &str, rev: u64, read: Result<Option<Screen>>) {
        let mut push = None;
        {
            let mut st = lock(&self.state);
            let Some(c) = st.cells.iter_mut().find(|c| c.key == key && c.rev == rev) else {
                return;
            };
            let now = Instant::now();
            match read {
                Ok(Some(s)) => {
                    if c.last.as_ref() == Some(&s) {
                        c.quiet += 1;
                    } else {
                        c.quiet = 0;
                        let mut ev = screen_json(&s);
                        ev["ev"] = json!("screen");
                        ev["key"] = json!(c.key);
                        ev["rev"] = json!(c.rev);
                        push = Some(ev);
                        c.last = Some(s);
                    }
                    c.safety = now + SAFETY;
                    c.next = now
                        + if now < c.rush_until {
                            RUSH_EVERY
                        } else if now < c.hot_until {
                            HOT_EVERY
                        } else if c.controlled {
                            // Output makes it dirty, so this only limits how often a pane that
                            // writes all the time is read and pushed.
                            if c.quiet <= 2 { every(0) } else { HOT_EVERY }
                        } else {
                            every(c.quiet)
                        };
                }
                Ok(None) => {
                    c.reported = true;
                    push = Some(json!({"ev": "changed", "key": c.key, "rev": c.rev}));
                }
                // tmux itself failed this time: try again soon.
                Err(_) => c.next = now + every(3),
            }
        }
        if let Some(ev) = push {
            let _ = emit(ev);
        }
    }
}
