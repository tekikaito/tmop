//! New and exited processes on the Processes page: a process seen for the first time is marked
//! *new* for a while, one that disappears stays listed as an *exited* ghost row (its last values)
//! for as long, then goes.
//!
//! Pure: [`Highlights::observe`] takes one snapshot (its sequence number and rows) and the time
//! now, from a clock the caller passes in, and diffs it with the snapshot before by process
//! identity, `(pid, create_time)`: a PID reused by a new process is an exit plus a start. The UI
//! asks [`Highlights::mark`] for a row's mark and fade, and lists [`Highlights::ghosts`] after
//! the live rows ([`Highlights::shown_rows`]).
//!
//! No marks for a snapshot that is not a fresh step from the one seen before (a *baseline*): the
//! first one, the first after [`Highlights::rebase`] (updates paused) or [`Highlights::clear`]
//! (switched off), one after an empty snapshot (the sampler's placeholder before its first
//! sample), and one whose sequence number does not follow the last seen (samples missed while
//! the page was not drawn). A process that starts and ends between two snapshots is never seen.

use super::ProcessRow;
use std::borrow::Cow;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A process as the page names it: (pid, creation time).
pub type Key = (u32, i64);

/// What a row shows, with how much of its highlight time is left (1.0 just now, towards 0.0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    New(f32),
    Exited(f32),
}

/// A process gone since `since`, listed with the values of the last snapshot that had it.
#[derive(Clone, Debug)]
pub struct Ghost {
    pub row: ProcessRow,
    pub since: Instant,
}

/// The highlight time chosen in the settings, `None` when highlighting is off. Seconds outside
/// 1..=10 are clamped (the settings page offers 1, 2 and 5).
pub fn hold(on: bool, secs: u32) -> Option<Duration> {
    on.then(|| Duration::from_secs(u64::from(secs.clamp(1, 10))))
}

fn key(r: &ProcessRow) -> Key {
    (r.pid, r.create_time)
}

/// What is left of a highlight that started at `since` and lasts `hold`, or `None` when it is over.
fn left(since: Instant, now: Instant, hold: Duration) -> Option<f32> {
    let age = now.saturating_duration_since(since);
    (age < hold).then(|| 1.0 - age.as_secs_f32() / hold.as_secs_f32().max(f32::EPSILON))
}

#[derive(Default)]
pub struct Highlights {
    /// The sequence number of the last snapshot seen and its rows by key; `None` until one is.
    last: Option<(u64, HashMap<Key, ProcessRow>)>,
    /// The next snapshot is a baseline.
    rebase: bool,
    /// When each new process was first seen.
    born: HashMap<Key, Instant>,
    /// Exited processes, oldest exit first.
    ghosts: Vec<Ghost>,
}

impl Highlights {
    /// Feeds snapshot `seq` with `rows`, seen at `now`, and drops highlights older than `hold`. The
    /// snapshot seen last does nothing again.
    pub fn observe(&mut self, seq: u64, rows: &[ProcessRow], now: Instant, hold: Duration) {
        self.expire(now, hold);
        if self.last.as_ref().is_some_and(|(s, _)| *s == seq) {
            return;
        }
        let cur: HashMap<Key, ProcessRow> = rows.iter().map(|r| (key(r), r.clone())).collect();
        let baseline = self.rebase || self.last.as_ref().is_none_or(|(s, prev)| prev.is_empty() || seq != s.wrapping_add(1));
        self.rebase = false;
        // A ghost whose key is live again is no ghost: the key is the identity.
        self.ghosts.retain(|g| !cur.contains_key(&key(&g.row)));
        if let (false, Some((_, prev))) = (baseline, self.last.take()) {
            for k in cur.keys().filter(|k| !prev.contains_key(k)) {
                self.born.insert(*k, now);
            }
            let mut gone: Vec<ProcessRow> = prev.into_iter().filter(|(k, _)| !cur.contains_key(k)).map(|(_, r)| r).collect();
            gone.sort_by_key(key);
            for row in gone {
                self.born.remove(&key(&row));
                self.ghosts.push(Ghost { row, since: now });
            }
        }
        self.born.retain(|k, _| cur.contains_key(k));
        self.last = Some((seq, cur));
    }

    /// The next snapshot is a baseline (updates are paused: what changes until they resume is no
    /// start or exit seen as it happened). Highlights shown now run out as usual.
    pub fn rebase(&mut self) {
        self.rebase = true;
    }

    /// Forgets everything (highlighting switched off); the next snapshot is a baseline.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Drops the highlights older than `hold`.
    pub fn expire(&mut self, now: Instant, hold: Duration) {
        self.born.retain(|_, t| left(*t, now, hold).is_some());
        self.ghosts.retain(|g| left(g.since, now, hold).is_some());
    }

    /// The mark of process `k` at `now`.
    pub fn mark(&self, k: Key, now: Instant, hold: Duration) -> Option<Mark> {
        if let Some(g) = self.ghosts.iter().find(|g| key(&g.row) == k) {
            return left(g.since, now, hold).map(Mark::Exited);
        }
        self.born.get(&k).and_then(|t| left(*t, now, hold)).map(Mark::New)
    }

    /// The exited processes still listed, oldest exit first.
    pub fn ghosts(&self) -> &[Ghost] {
        &self.ghosts
    }

    /// Whether `k` is an exited process still listed (never true for a live key).
    pub fn is_ghost(&self, k: Key) -> bool {
        self.ghosts.iter().any(|g| key(&g.row) == k)
    }

    /// Something is highlighted (the page redraws to fade it).
    pub fn active(&self) -> bool {
        !self.born.is_empty() || !self.ghosts.is_empty()
    }

    /// The rows the page lists: `live`, then the ghosts. Live rows come first, so a lookup by
    /// PID that takes the first hit finds the live holder of a reused PID, never the ghost.
    pub fn shown_rows<'a>(&self, live: &'a [ProcessRow]) -> Cow<'a, [ProcessRow]> {
        if self.ghosts.is_empty() {
            return Cow::Borrowed(live);
        }
        let mut v = Vec::with_capacity(live.len() + self.ghosts.len());
        v.extend_from_slice(live);
        v.extend(self.ghosts.iter().filter(|g| !live.iter().any(|r| key(r) == key(&g.row))).map(|g| g.row.clone()));
        Cow::Owned(v)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tree;
    use super::super::Column;
    use super::*;
    use std::collections::HashSet;

    const HOLD: Duration = Duration::from_secs(2);

    fn row(pid: u32, ppid: u32, ct: i64, name: &str) -> ProcessRow {
        ProcessRow { pid, parent_pid: ppid, create_time: ct, name: name.into(), ..Default::default() }
    }

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn base() -> Vec<ProcessRow> {
        vec![row(1, 0, 1, "init"), row(10, 1, 5, "shell"), row(20, 1, 6, "daemon")]
    }

    #[test]
    fn the_first_snapshot_marks_nothing() {
        let t0 = Instant::now();
        let mut h = Highlights::default();
        // The sampler's placeholder (no rows), then the first real snapshot.
        h.observe(0, &[], t0, HOLD);
        h.observe(1, &base(), ms(t0, 100), HOLD);
        assert!(!h.active());
        assert!(base().iter().all(|r| h.mark(key(r), ms(t0, 100), HOLD).is_none()));
        // Also when the first snapshot seen is not the first one taken.
        let mut h = Highlights::default();
        h.observe(7, &base(), t0, HOLD);
        assert!(!h.active());
    }

    #[test]
    fn a_new_process_is_marked_and_fades() {
        let t0 = Instant::now();
        let mut h = Highlights::default();
        h.observe(1, &base(), t0, HOLD);
        let mut next = base();
        next.push(row(30, 10, 9, "cargo"));
        h.observe(2, &next, ms(t0, 1000), HOLD);
        assert_eq!(h.mark((30, 9), ms(t0, 1000), HOLD), Some(Mark::New(1.0)));
        assert_eq!(h.mark((30, 9), ms(t0, 2000), HOLD), Some(Mark::New(0.5)));
        assert!(h.mark((10, 5), ms(t0, 1000), HOLD).is_none(), "old processes are not new");
        assert!(h.mark((30, 9), ms(t0, 3000), HOLD).is_none(), "over after the hold");
        // The same snapshot seen again changes nothing; time alone ends the mark.
        h.observe(2, &next, ms(t0, 3000), HOLD);
        assert!(!h.active());
        // A shorter hold chosen meanwhile applies at once.
        let mut h = Highlights::default();
        h.observe(1, &base(), t0, HOLD);
        h.observe(2, &next, t0, HOLD);
        assert!(h.mark((30, 9), ms(t0, 1500), Duration::from_secs(1)).is_none());
    }

    #[test]
    fn an_exited_process_lingers_with_its_last_values_then_goes() {
        let t0 = Instant::now();
        let mut h = Highlights::default();
        let mut first = base();
        first[2].cpu_pct = 42.0;
        h.observe(1, &first, t0, HOLD);
        let after: Vec<ProcessRow> = base().into_iter().filter(|r| r.pid != 20).collect();
        h.observe(2, &after, ms(t0, 1000), HOLD);
        assert_eq!(h.mark((20, 6), ms(t0, 1000), HOLD), Some(Mark::Exited(1.0)));
        assert_eq!(h.ghosts().len(), 1);
        assert_eq!(h.ghosts()[0].row.cpu_pct, 42.0, "the last values");
        let shown = h.shown_rows(&after);
        assert_eq!(shown.iter().map(|r| r.pid).collect::<Vec<_>>(), [1, 10, 20], "live rows first, then the ghost");
        // Still there at the next snapshot, gone after the hold.
        h.observe(3, &after, ms(t0, 2000), HOLD);
        assert!(h.is_ghost((20, 6)));
        h.observe(4, &after, ms(t0, 3000), HOLD);
        assert!(!h.is_ghost((20, 6)) && h.ghosts().is_empty());
        assert!(matches!(h.shown_rows(&after), Cow::Borrowed(_)));
    }

    #[test]
    fn a_reused_pid_is_an_exit_and_a_start() {
        let t0 = Instant::now();
        let mut h = Highlights::default();
        h.observe(1, &base(), t0, HOLD);
        // PID 20 now belongs to another process, started later.
        let mut next: Vec<ProcessRow> = base().into_iter().filter(|r| r.pid != 20).collect();
        next.push(row(20, 1, 99, "new holder"));
        h.observe(2, &next, ms(t0, 500), HOLD);
        let now = ms(t0, 500);
        assert_eq!(h.mark((20, 6), now, HOLD), Some(Mark::Exited(1.0)));
        assert_eq!(h.mark((20, 99), now, HOLD), Some(Mark::New(1.0)));
        assert!(h.is_ghost((20, 6)) && !h.is_ghost((20, 99)));
        // Both are listed: the live one first, so a PID lookup finds it.
        let shown = h.shown_rows(&next);
        assert_eq!(shown.iter().filter(|r| r.pid == 20).map(|r| r.create_time).collect::<Vec<_>>(), [99, 6]);
    }

    #[test]
    fn a_key_seen_again_is_no_ghost() {
        // A creation time the OS could not read (0) makes a reused PID look like the same
        // process: the key is the identity, so it is live, not exited.
        let t0 = Instant::now();
        let mut h = Highlights::default();
        h.observe(1, &[row(1, 0, 1, "init"), row(5, 1, 0, "a")], t0, HOLD);
        h.observe(2, &[row(1, 0, 1, "init")], ms(t0, 100), HOLD);
        assert!(h.is_ghost((5, 0)));
        let back = [row(1, 0, 1, "init"), row(5, 1, 0, "b")];
        h.observe(3, &back, ms(t0, 200), HOLD);
        assert!(!h.is_ghost((5, 0)) && h.mark((5, 0), ms(t0, 200), HOLD).is_some_and(|m| matches!(m, Mark::New(_))));
        assert_eq!(h.shown_rows(&back).len(), 2);
    }

    #[test]
    fn spawn_and_exit_between_snapshots_is_not_seen_and_one_snapshot_lives_go_red() {
        let t0 = Instant::now();
        let mut h = Highlights::default();
        h.observe(1, &base(), t0, HOLD);
        // Something ran and ended between snapshots 1 and 2: no snapshot has it.
        h.observe(2, &base(), ms(t0, 1000), HOLD);
        assert!(!h.active());
        // Seen in exactly one snapshot: new, then exited (no longer new).
        let mut with = base();
        with.push(row(40, 10, 50, "flash"));
        h.observe(3, &with, ms(t0, 2000), HOLD);
        assert!(matches!(h.mark((40, 50), ms(t0, 2000), HOLD), Some(Mark::New(_))));
        h.observe(4, &base(), ms(t0, 2250), HOLD);
        assert_eq!(h.mark((40, 50), ms(t0, 2250), HOLD), Some(Mark::Exited(1.0)));
        assert_eq!(h.ghosts().len(), 1);
    }

    #[test]
    fn pause_and_resume_mark_nothing() {
        let t0 = Instant::now();
        let mut h = Highlights::default();
        h.observe(1, &base(), t0, HOLD);
        // Paused: the page rebases every frame. A manual refresh while paused is a baseline,
        // and so is the first snapshot after resuming, even if its number follows on.
        h.rebase();
        let mut changed: Vec<ProcessRow> = base().into_iter().filter(|r| r.pid != 20).collect();
        changed.push(row(50, 1, 70, "started while paused"));
        h.observe(2, &changed, ms(t0, 30_000), HOLD);
        assert!(!h.active(), "nothing flashes after a pause");
        // From then on changes are marked again.
        let mut next = changed.clone();
        next.push(row(51, 1, 80, "after"));
        h.observe(3, &next, ms(t0, 31_000), HOLD);
        assert!(matches!(h.mark((51, 80), ms(t0, 31_000), HOLD), Some(Mark::New(_))));
        assert!(h.mark((50, 70), ms(t0, 31_000), HOLD).is_none());
    }

    #[test]
    fn missed_snapshots_are_a_baseline() {
        // The page was not drawn for a while (another page, minimized): snapshots 2..=9 were never
        // seen, so what differs in 10 is not shown as starts and exits.
        let t0 = Instant::now();
        let mut h = Highlights::default();
        h.observe(1, &base(), t0, HOLD);
        let mut later = vec![row(1, 0, 1, "init")];
        later.extend((0..50u32).map(|k| row(100 + k, 1, 100 + k as i64, "many")));
        h.observe(10, &later, ms(t0, 9000), HOLD);
        assert!(!h.active());
    }

    #[test]
    fn rebase_keeps_marks_already_shown_and_clear_drops_them() {
        let t0 = Instant::now();
        let mut h = Highlights::default();
        h.observe(1, &base(), t0, HOLD);
        let mut next = base();
        next.retain(|r| r.pid != 20);
        next.push(row(30, 1, 9, "new"));
        h.observe(2, &next, t0, HOLD);
        h.rebase();
        h.observe(3, &next, ms(t0, 500), HOLD);
        assert!(h.is_ghost((20, 6)) && h.mark((30, 9), ms(t0, 500), HOLD).is_some());
        h.clear();
        assert!(!h.active() && h.ghosts().is_empty());
        h.observe(4, &base(), ms(t0, 600), HOLD);
        assert!(!h.active(), "after clear the next snapshot is a baseline");
    }

    #[test]
    fn hold_follows_the_settings() {
        assert_eq!(hold(false, 2), None);
        assert_eq!(hold(true, 2), Some(Duration::from_secs(2)));
        assert_eq!(hold(true, 0), Some(Duration::from_secs(1)));
        assert_eq!(hold(true, 500), Some(Duration::from_secs(10)));
    }

    /// (pid, create_time, depth) of each tree line.
    fn shape(rows: &[ProcessRow], t: &[tree::TreeRow]) -> Vec<(u32, i64, usize)> {
        t.iter().map(|t| (rows[t.index].pid, rows[t.index].create_time, t.depth)).collect()
    }

    /// Tree view over the shown rows: an exited child stays under its parent, live children stay
    /// under an exited parent, a collapsed node keeps an exited child hidden, and the new holder of
    /// a reused PID never adopts the ghost's children.
    #[test]
    fn ghosts_keep_their_place_in_the_tree() {
        let t0 = Instant::now();
        let all = |rows: &[ProcessRow]| (0..rows.len()).collect::<Vec<_>>();
        let first = vec![row(1, 0, 1, "init"), row(10, 1, 5, "shell"), row(11, 10, 6, "vim"), row(12, 10, 7, "make"), row(13, 12, 8, "cc")];
        let mut h = Highlights::default();
        h.observe(1, &first, t0, HOLD);
        // vim (a leaf) and make (the parent of cc) exit; cc lives on.
        let live: Vec<ProcessRow> = first.iter().filter(|r| r.pid != 11 && r.pid != 12).cloned().collect();
        h.observe(2, &live, ms(t0, 1000), HOLD);
        let shown = h.shown_rows(&live);
        let t = tree::build(&shown, &all(&shown), None, (Column::Pid, false), &HashSet::new());
        assert_eq!(shape(&shown, &t), [(1, 1, 0), (10, 5, 1), (11, 6, 2), (12, 7, 2), (13, 8, 3)]);
        // Collapsed, shell hides its exited child too.
        let collapsed: HashSet<Key> = [(10, 5)].into_iter().collect();
        let t = tree::build(&shown, &all(&shown), None, (Column::Pid, false), &collapsed);
        assert_eq!(shape(&shown, &t), [(1, 1, 0), (10, 5, 1)]);
        // A collapsed exited parent keeps its live child hidden while it lingers.
        let collapsed: HashSet<Key> = [(12, 7)].into_iter().collect();
        let t = tree::build(&shown, &all(&shown), None, (Column::Pid, false), &collapsed);
        assert!(!shape(&shown, &t).iter().any(|l| l.0 == 13));
        // PID 12 is reused by a younger process: cc is not its child, it becomes a root, and the
        // ghost of make keeps its own place under shell.
        let mut reused = live.clone();
        reused.push(row(12, 1, 50, "new 12"));
        h.observe(3, &reused, ms(t0, 1500), HOLD);
        let shown = h.shown_rows(&reused);
        let t = tree::build(&shown, &all(&shown), None, (Column::Pid, false), &HashSet::new());
        let lines = shape(&shown, &t);
        assert!(lines.contains(&(12, 7, 2)) && lines.contains(&(12, 50, 1)), "{lines:?}");
        assert!(lines.contains(&(13, 8, 0)), "cc is not adopted by the new holder of 12: {lines:?}");
        // After the hold the tree is the live one.
        h.observe(4, &reused, ms(t0, 4000), HOLD);
        let shown = h.shown_rows(&reused);
        assert_eq!(shown.len(), reused.len());
    }
}
