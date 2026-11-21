//! The parent/child tree of the Processes page (its *Tree view*), as a flat list in display order.
//!
//! Pure: [`build`] takes the rows of one snapshot and returns what the table lists, top to bottom,
//! each with its depth. Only `parent_pid` and `create_time` are used, so it works the same on every
//! OS. A parent link holds when the parent is in the list and is not younger than the child (a
//! younger "parent" is a reused PID); PID 0 (Windows' System Idle Process, macOS `kernel_task`) and
//! a process naming itself are never parents, and a loop of links (which the checks above leave
//! possible only with equal creation times) is cut at its oldest member. A process without a valid
//! parent is a root.
//!
//! Cost: one hash map of PIDs, one pass over the parent links, one sort per child list; O(n log n)
//! for n processes, no search for children.

use super::{compare, Column, ProcessRow};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// A process as the selection and the collapsed set name it: (pid, creation time).
pub type Key = (u32, i64);

/// One line of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeRow {
    /// Index into the rows given to [`build`].
    pub index: usize,
    /// 0 for a root.
    pub depth: usize,
    /// It has children in the tree shown (while filtering: children kept by the filter).
    pub has_children: bool,
    /// Its children follow it. Always true for a node with children while a filter is active.
    pub expanded: bool,
    /// It matches the filter itself; false for an ancestor shown only for a match below it.
    /// Always true without a filter.
    pub matched: bool,
}

fn key(r: &ProcessRow) -> Key {
    (r.pid, r.create_time)
}

/// The parent of every row in `members` (indexed like `rows`; `None` for a root and for rows
/// that are not members), loops cut.
pub fn parents(rows: &[ProcessRow], members: &[usize]) -> Vec<Option<usize>> {
    let mut by_pid: HashMap<u32, usize> = HashMap::with_capacity(members.len());
    for &i in members {
        by_pid.entry(rows[i].pid).or_insert(i);
    }
    let mut parent = vec![None; rows.len()];
    for &i in members {
        let r = &rows[i];
        if r.parent_pid == 0 || r.parent_pid == r.pid {
            continue;
        }
        parent[i] = by_pid.get(&r.parent_pid).copied().filter(|&p| p != i && rows[p].create_time <= r.create_time);
    }
    // Loops: walk up from every member; a walk that meets its own path has found one.
    // 0 not seen, 1 on the current path, 2 done. Every row is on a path once: O(n).
    let mut state = vec![0u8; rows.len()];
    let mut path: Vec<usize> = Vec::new();
    for &start in members {
        path.clear();
        let mut cur = start;
        loop {
            match state[cur] {
                2 => break,
                1 => {
                    let from = path.iter().position(|&x| x == cur).unwrap_or(0);
                    if let Some(&oldest) = path[from..].iter().min_by_key(|&&x| (rows[x].create_time, rows[x].pid)) {
                        parent[oldest] = None;
                    }
                    break;
                }
                _ => {
                    state[cur] = 1;
                    path.push(cur);
                    match parent[cur] {
                        Some(p) => cur = p,
                        None => break,
                    }
                }
            }
        }
        for &x in &path {
            state[x] = 2;
        }
    }
    parent
}

/// The tree of `members` (indices into `rows`) in display order.
///
/// Siblings, and the roots, sort by `sort` (column, descending), ties by name, so the list does
/// not shuffle between samples. With `filter`, a row is listed when it matches or has a matching
/// descendant, and every node is expanded; without, the nodes in `collapsed` hide their children.
pub fn build(rows: &[ProcessRow], members: &[usize], filter: Option<&dyn Fn(&ProcessRow) -> bool>, sort: (Column, bool), collapsed: &HashSet<Key>) -> Vec<TreeRow> {
    let parent = parents(rows, members);
    let n = rows.len();
    // Which rows are listed, and which match.
    let mut matched = vec![filter.is_none(); n];
    let mut keep = vec![false; n];
    match filter {
        None => {
            for &i in members {
                keep[i] = true;
            }
        }
        Some(f) => {
            for &i in members {
                if !f(&rows[i]) {
                    continue;
                }
                matched[i] = true;
                // Up to the first ancestor already kept: each row is marked once.
                let mut cur = i;
                while !keep[cur] {
                    keep[cur] = true;
                    match parent[cur] {
                        Some(p) => cur = p,
                        None => break,
                    }
                }
            }
        }
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots = Vec::new();
    for &i in members {
        if !keep[i] {
            continue;
        }
        match parent[i] {
            Some(p) => children[p].push(i),
            None => roots.push(i),
        }
    }
    let (col, desc) = sort;
    let order = |a: &usize, b: &usize| -> Ordering {
        let o = compare(&rows[*a], &rows[*b], col).then_with(|| compare(&rows[*a], &rows[*b], Column::Name));
        if desc {
            o.reverse()
        } else {
            o
        }
    };
    roots.sort_by(order);
    for c in children.iter_mut().filter(|c| c.len() > 1) {
        c.sort_by(order);
    }
    let mut out = Vec::with_capacity(roots.len());
    let mut stack: Vec<(usize, usize)> = roots.iter().rev().map(|&i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        let has_children = !children[i].is_empty();
        let expanded = has_children && (filter.is_some() || !collapsed.contains(&key(&rows[i])));
        out.push(TreeRow { index: i, depth, has_children, expanded, matched: matched[i] });
        if expanded {
            stack.extend(children[i].iter().rev().map(|&c| (c, depth + 1)));
        }
    }
    out
}

/// Forgets the collapsed nodes whose process is gone.
pub fn prune(collapsed: &mut HashSet<Key>, rows: &[ProcessRow]) {
    if collapsed.is_empty() {
        return;
    }
    let live: HashSet<Key> = rows.iter().map(key).collect();
    collapsed.retain(|k| live.contains(k));
}

/// The line of the parent of line `at`: the nearest line above it one level up.
pub fn parent_line(tree: &[TreeRow], at: usize) -> Option<usize> {
    let depth = tree.get(at)?.depth.checked_sub(1)?;
    tree[..at].iter().rposition(|t| t.depth == depth)
}

/// Whether line `inner` lies below line `at` in its subtree.
pub fn in_subtree(tree: &[TreeRow], at: usize, inner: usize) -> bool {
    let Some(top) = tree.get(at) else { return false };
    inner > at && inner < tree.len() && tree[at + 1..=inner].iter().all(|t| t.depth > top.depth)
}

/// What Left or Right does on the selected line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Collapse or expand this line.
    Toggle(usize),
    /// Select this line.
    Select(usize),
}

/// Left on line `at`: an expanded node collapses; a leaf, a collapsed node, or any node while
/// filtering (the tree is then fully expanded) selects its parent, as in file managers.
pub fn left(tree: &[TreeRow], at: usize, filtering: bool) -> Option<Step> {
    let t = tree.get(at)?;
    if t.has_children && t.expanded && !filtering {
        return Some(Step::Toggle(at));
    }
    parent_line(tree, at).map(Step::Select)
}

/// Right on line `at`: a collapsed node expands, an expanded one selects its first child.
pub fn right(tree: &[TreeRow], at: usize, filtering: bool) -> Option<Step> {
    let t = tree.get(at)?;
    if !t.has_children {
        return None;
    }
    if !t.expanded && !filtering {
        return Some(Step::Toggle(at));
    }
    (at + 1 < tree.len()).then_some(Step::Select(at + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, ppid: u32, ct: i64, name: &str) -> ProcessRow {
        ProcessRow { pid, parent_pid: ppid, create_time: ct, name: name.into(), ..Default::default() }
    }

    fn all(rows: &[ProcessRow]) -> Vec<usize> {
        (0..rows.len()).collect()
    }

    /// (pid, depth) of each line.
    fn shape(rows: &[ProcessRow], t: &[TreeRow]) -> Vec<(u32, usize)> {
        t.iter().map(|t| (rows[t.index].pid, t.depth)).collect()
    }

    fn by_pid() -> (Column, bool) {
        (Column::Pid, false)
    }

    #[test]
    fn nests_children_under_their_parent() {
        let rows = vec![row(1, 0, 1, "init"), row(30, 1, 3, "c"), row(20, 1, 2, "b"), row(21, 20, 4, "b1"), row(40, 0, 1, "other")];
        let t = build(&rows, &all(&rows), None, by_pid(), &HashSet::new());
        assert_eq!(shape(&rows, &t), [(1, 0), (20, 1), (21, 2), (30, 1), (40, 0)]);
        assert!(t[0].has_children && t[0].expanded && t[1].has_children && !t[2].has_children && !t[2].expanded);
        assert!(t.iter().all(|t| t.matched));
    }

    #[test]
    fn orphans_are_roots() {
        // 5's parent 99 is gone; it is a root of its own, with its child.
        let rows = vec![row(1, 0, 1, "init"), row(5, 99, 3, "orphan"), row(6, 5, 4, "kid")];
        let t = build(&rows, &all(&rows), None, by_pid(), &HashSet::new());
        assert_eq!(shape(&rows, &t), [(1, 0), (5, 0), (6, 1)]);
    }

    #[test]
    fn a_younger_parent_is_a_reused_pid() {
        // 10 names 7 as its parent, but this 7 started after 10: the original 7 is gone.
        let rows = vec![row(7, 1, 50, "new holder of 7"), row(10, 7, 20, "old child"), row(11, 7, 50, "same tick")];
        let t = build(&rows, &all(&rows), None, by_pid(), &HashSet::new());
        // A child started in the same tick as its parent still nests (`<=`).
        assert_eq!(shape(&rows, &t), [(7, 0), (11, 1), (10, 0)]);
    }

    #[test]
    fn pid_zero_and_self_parents_make_no_loop() {
        // Windows: System Idle Process is pid 0 with parent 0, System is 4 with parent 0.
        let rows = vec![row(0, 0, 0, "System Idle Process"), row(4, 0, 0, "System"), row(9, 9, 5, "self")];
        let t = build(&rows, &all(&rows), None, by_pid(), &HashSet::new());
        assert_eq!(shape(&rows, &t), [(0, 0), (4, 0), (9, 0)]);
    }

    #[test]
    fn loops_are_cut_at_the_oldest_member() {
        // 2 -> 3 -> 4 -> 2 with equal creation times passes the age check; the walk cuts it.
        let rows = vec![row(2, 4, 7, "a"), row(3, 2, 7, "b"), row(4, 3, 7, "c"), row(5, 3, 8, "tail")];
        for members in [vec![0, 1, 2, 3], vec![3, 2, 1, 0], vec![1, 3, 0, 2]] {
            let p = parents(&rows, &members);
            // 2 (same time, lowest pid) becomes the root, whatever the walk starts from.
            assert_eq!(p, [None, Some(0), Some(1), Some(1)], "{members:?}");
            let t = build(&rows, &members, None, by_pid(), &HashSet::new());
            assert_eq!(shape(&rows, &t), [(2, 0), (3, 1), (4, 2), (5, 2)]);
        }
        // A two-node loop.
        let rows = vec![row(8, 9, 1, "x"), row(9, 8, 1, "y")];
        let t = build(&rows, &all(&rows), None, by_pid(), &HashSet::new());
        assert_eq!(shape(&rows, &t), [(8, 0), (9, 1)]);
    }

    #[test]
    fn siblings_and_roots_sort_both_ways() {
        let mut rows = vec![row(1, 0, 1, "root"), row(2, 1, 2, "low"), row(3, 1, 3, "high"), row(4, 1, 4, "mid"), row(9, 0, 1, "busy root")];
        rows[1].cpu_pct = 1.0;
        rows[2].cpu_pct = 9.0;
        rows[3].cpu_pct = 5.0;
        rows[4].cpu_pct = 50.0;
        let t = build(&rows, &all(&rows), None, (Column::Cpu, true), &HashSet::new());
        assert_eq!(shape(&rows, &t), [(9, 0), (1, 0), (3, 1), (4, 1), (2, 1)]);
        let t = build(&rows, &all(&rows), None, (Column::Cpu, false), &HashSet::new());
        assert_eq!(shape(&rows, &t), [(1, 0), (2, 1), (4, 1), (3, 1), (9, 0)]);
        // Equal values fall back to the name.
        let t = build(&rows, &all(&rows), None, (Column::Threads, false), &HashSet::new());
        assert_eq!(shape(&rows, &t), [(9, 0), (1, 0), (3, 1), (2, 1), (4, 1)]);
    }

    #[test]
    fn filter_keeps_matches_with_their_ancestors() {
        let rows = vec![row(1, 0, 1, "init"), row(2, 1, 2, "shell"), row(3, 2, 3, "cargo"), row(4, 2, 4, "vim"), row(5, 1, 5, "cargo-watch"), row(6, 0, 1, "lone")];
        let mut collapsed = HashSet::new();
        // Collapsed nodes open while filtering.
        collapsed.insert((2, 2));
        let f = |r: &ProcessRow| r.name.contains("cargo");
        let t = build(&rows, &all(&rows), Some(&f), by_pid(), &collapsed);
        assert_eq!(shape(&rows, &t), [(1, 0), (2, 1), (3, 2), (5, 1)]);
        assert_eq!(t.iter().map(|t| t.matched).collect::<Vec<_>>(), [false, false, true, true]);
        assert!(t[1].expanded && t[1].has_children);
        // Only kept children count: shell has one listed child, cargo none.
        assert!(!t[2].has_children);
        let none = |_: &ProcessRow| false;
        assert!(build(&rows, &all(&rows), Some(&none), by_pid(), &collapsed).is_empty());
    }

    #[test]
    fn collapsed_nodes_hide_their_subtree() {
        let rows = vec![row(1, 0, 1, "init"), row(2, 1, 2, "a"), row(3, 2, 3, "a1"), row(4, 1, 4, "b")];
        let mut collapsed = HashSet::new();
        collapsed.insert((2, 2));
        let t = build(&rows, &all(&rows), None, by_pid(), &collapsed);
        assert_eq!(shape(&rows, &t), [(1, 0), (2, 1), (4, 1)]);
        assert!(t[1].has_children && !t[1].expanded);
        // A key of a reused pid does not collapse the new process.
        let mut other = HashSet::new();
        other.insert((2, 99));
        assert_eq!(build(&rows, &all(&rows), None, by_pid(), &other).len(), 4);
        // Collapsing the root hides everything below it.
        collapsed.insert((1, 1));
        assert_eq!(shape(&rows, &build(&rows, &all(&rows), None, by_pid(), &collapsed)), [(1, 0)]);
    }

    #[test]
    fn rows_left_out_are_not_parents() {
        // TMOP itself (2) is not listed: its child becomes a root.
        let rows = vec![row(1, 0, 1, "init"), row(2, 1, 2, "tmop"), row(3, 2, 3, "helper")];
        let t = build(&rows, &[0, 2], None, by_pid(), &HashSet::new());
        assert_eq!(shape(&rows, &t), [(1, 0), (3, 0)]);
    }

    #[test]
    fn prune_drops_gone_processes() {
        let rows = vec![row(1, 0, 1, "a"), row(2, 1, 2, "b")];
        let mut c: HashSet<Key> = [(1, 1), (2, 3), (7, 7)].into_iter().collect();
        prune(&mut c, &rows);
        assert_eq!(c, [(1, 1)].into_iter().collect());
    }

    #[test]
    fn left_and_right_move_like_a_file_manager() {
        let rows = vec![row(1, 0, 1, "init"), row(2, 1, 2, "a"), row(3, 2, 3, "a1"), row(4, 1, 4, "b")];
        let mut collapsed = HashSet::new();
        let t = build(&rows, &all(&rows), None, by_pid(), &collapsed);
        // Lines: 0 init, 1 a, 2 a1, 3 b.
        assert_eq!(left(&t, 1, false), Some(Step::Toggle(1)), "an open node closes");
        assert_eq!(left(&t, 2, false), Some(Step::Select(1)), "a leaf goes to its parent");
        assert_eq!(left(&t, 3, false), Some(Step::Select(0)));
        assert_eq!(left(&t, 0, true), None, "a root while filtering: nothing above");
        assert_eq!(left(&t, 1, true), Some(Step::Select(0)), "while filtering nodes stay open");
        assert_eq!(right(&t, 1, false), Some(Step::Select(2)), "an open node goes to its first child");
        assert_eq!(right(&t, 2, false), None);
        collapsed.insert((2, 2));
        let t = build(&rows, &all(&rows), None, by_pid(), &collapsed);
        // Lines: 0 init, 1 a (closed), 2 b.
        assert_eq!(right(&t, 1, false), Some(Step::Toggle(1)));
        assert_eq!(left(&t, 1, false), Some(Step::Select(0)), "a closed node goes to its parent");
        assert!(in_subtree(&t, 0, 2) && !in_subtree(&t, 1, 2) && !in_subtree(&t, 2, 2));
        assert_eq!(parent_line(&t, 0), None);
    }

    #[test]
    fn large_flat_and_deep_lists_build() {
        // 20 000 children of one root and a chain 5 000 deep: no recursion, no quadratic lookups.
        let mut rows = vec![row(1, 0, 1, "root")];
        rows.extend((0..20_000u32).map(|k| row(100 + k, 1, 2, "child")));
        rows.extend((0..5_000u32).map(|k| row(50_000 + k, if k == 0 { 1 } else { 50_000 + k - 1 }, 3 + k as i64, "chain")));
        let t = build(&rows, &all(&rows), None, by_pid(), &HashSet::new());
        assert_eq!(t.len(), rows.len());
        assert_eq!(t.iter().map(|t| t.depth).max(), Some(5_000));
    }
}
