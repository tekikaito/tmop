//! CPU affinity: which logical processors a process may run on ("Set affinity..." in the
//! context menu of the Processes page).
//!
//! * Windows: `GetProcessAffinityMask` / `SetProcessAffinityMask`. A mask holds the 64 processors
//!   of one processor group. On a machine with more than one group a process can span groups and
//!   a plain mask would be wrong, so the action is hidden there ([`supported_with_groups`]).
//! * Linux: `sched_getaffinity` / `sched_setaffinity`, applied to every thread of the process.
//! * macOS has no supported API for it: the action is hidden (`ProcCaps::affinity`).
//!
//! The pieces that decide something (the checkbox [`Selection`], mask conversion, the CPU list
//! parser, the wording) are pure and tested everywhere; the OS calls are tested on this OS
//! against a child process the test starts itself.

use super::ProcessRow;

/// Whether affinity can be offered on a machine with `groups` processor groups: only with one
/// (Windows reports 1 when it has no groups to speak of; Linux has no groups: pass 1).
pub const fn supported_with_groups(groups: usize) -> bool {
    groups <= 1
}

/// The processor groups of this machine (1 where the idea does not exist).
pub fn processor_groups() -> usize {
    #[cfg(windows)]
    {
        super::imp::sys::processor_groups()
    }
    #[cfg(not(windows))]
    {
        1
    }
}

/// The logical processors of the system that a process may be bound to, with a checkbox each:
/// what the dialog edits. At least one stays checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Logical processor numbers, ascending.
    cpus: Vec<usize>,
    checked: Vec<bool>,
}

impl Selection {
    /// `cpus` are the processors there are, `current` the ones the process may use now. A
    /// current set that matches none of `cpus` (which the OS does not give) selects them all.
    pub fn new(cpus: Vec<usize>, current: &[usize]) -> Self {
        let mut checked: Vec<bool> = cpus.iter().map(|c| current.contains(c)).collect();
        if !checked.iter().any(|c| *c) {
            checked.iter_mut().for_each(|c| *c = true);
        }
        Self { cpus, checked }
    }

    pub fn cpus(&self) -> &[usize] {
        &self.cpus
    }

    /// Whether the processor at position `i` of [`Selection::cpus`] is checked.
    pub fn is_checked(&self, i: usize) -> bool {
        self.checked.get(i).copied().unwrap_or(false)
    }

    /// Flips the box at position `i`; the last checked box cannot be unchecked (returns false
    /// then, and for a position that does not exist).
    pub fn toggle(&mut self, i: usize) -> bool {
        let Some(on) = self.checked.get(i).copied() else { return false };
        if on && self.count() == 1 {
            return false;
        }
        self.checked[i] = !on;
        true
    }

    pub fn count(&self) -> usize {
        self.checked.iter().filter(|c| **c).count()
    }

    /// Every processor is checked.
    pub fn all(&self) -> bool {
        self.count() == self.cpus.len()
    }

    /// The "All processors" box: checks everything, or, when everything is checked already,
    /// leaves only the first (at least one must stay).
    pub fn toggle_all(&mut self) {
        let all = self.all();
        for (i, c) in self.checked.iter_mut().enumerate() {
            *c = !all || i == 0;
        }
    }

    /// The checked processors, ascending.
    pub fn chosen(&self) -> Vec<usize> {
        self.cpus.iter().zip(&self.checked).filter(|(_, on)| **on).map(|(c, _)| *c).collect()
    }

    /// "all processors", "processor 3" or "processors 0-3, 6".
    pub fn label(&self) -> String {
        if self.all() {
            "all processors".into()
        } else {
            describe(&self.chosen())
        }
    }
}

/// "processor 3" or "processors 0-3, 6" for ascending `cpus`.
pub fn describe(cpus: &[usize]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < cpus.len() {
        let start = cpus[i];
        let mut end = start;
        while i + 1 < cpus.len() && cpus[i + 1] == end + 1 {
            i += 1;
            end = cpus[i];
        }
        parts.push(if end > start + 1 { format!("{start}-{end}") } else if end == start + 1 { format!("{start}, {end}") } else { start.to_string() });
        i += 1;
    }
    format!("processor{} {}", if cpus.len() == 1 { "" } else { "s" }, parts.join(", "))
}

/// The processors set in a Windows affinity mask, ascending.
#[cfg(any(windows, test))]
pub fn mask_to_cpus(mask: u64) -> Vec<usize> {
    (0..64).filter(|b| mask >> b & 1 == 1).collect()
}

/// The affinity mask of `cpus`; `None` when one of them does not fit in 64 bits.
#[cfg(any(windows, test))]
pub fn cpus_to_mask(cpus: &[usize]) -> Option<u64> {
    cpus.iter().try_fold(0u64, |m, c| (*c < 64).then(|| m | 1u64 << c))
}

/// The processors of a kernel CPU list such as `0-3,6,8-9` (the format of
/// `/sys/devices/system/cpu/online`), ascending; bad pieces are skipped.
#[cfg(any(target_os = "linux", test))]
pub fn parse_cpu_list(text: &str) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for part in text.trim().split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                    // A range bigger than any machine is a typo of the kernel's file, not a list.
                    if a <= b && b < 65536 {
                        out.extend(a..=b);
                    }
                }
            }
            None => out.extend(part.parse::<usize>()),
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The selection for `row`: the processors there are, with the ones it may use now checked.
pub fn read(row: &ProcessRow) -> Result<Selection, String> {
    let (cpus, current) = sys::get(row.pid, row.create_time)?;
    if cpus.is_empty() {
        return Err("the processors could not be listed".into());
    }
    Ok(Selection::new(cpus, &current))
}

/// Binds `row` to the checked processors; the text for the status line.
pub fn apply(row: &ProcessRow, sel: &Selection) -> Result<String, String> {
    if sel.count() == 0 {
        return Err("at least one processor must stay selected".into());
    }
    sys::set(row.pid, row.create_time, &sel.chosen()).map(|_| format!("{} affinity: {}", row.name, sel.label()))
}

#[cfg(any(windows, target_os = "linux"))]
use super::imp::sys;

/// No backend: never reached, the caps hide the action.
#[cfg(not(any(windows, target_os = "linux")))]
mod sys {
    pub fn get(_pid: u32, _start: i64) -> Result<(Vec<usize>, Vec<usize>), String> {
        Err(format!("not available on {}", crate::platform::os_name()))
    }
    pub fn set(_pid: u32, _start: i64, _cpus: &[usize]) -> Result<(), String> {
        Err(format!("not available on {}", crate::platform::os_name()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(n: usize, on: &[usize]) -> Selection {
        Selection::new((0..n).collect(), on)
    }

    #[test]
    fn masks_and_processor_lists_convert_both_ways() {
        assert_eq!(mask_to_cpus(0b1011), [0, 1, 3]);
        assert_eq!(mask_to_cpus(u64::MAX).len(), 64);
        assert_eq!(cpus_to_mask(&[0, 1, 3]), Some(0b1011));
        assert_eq!(cpus_to_mask(&[63]), Some(1 << 63));
        assert_eq!(cpus_to_mask(&[64]), None);
        assert_eq!(cpus_to_mask(&[]), Some(0));
        for m in [1u64, 0xF0F0, u64::MAX, 1 << 63] {
            assert_eq!(cpus_to_mask(&mask_to_cpus(m)), Some(m));
        }
    }

    #[test]
    fn the_selection_starts_from_the_current_set() {
        let s = sel(4, &[1, 2]);
        assert_eq!((s.is_checked(0), s.is_checked(1), s.is_checked(2), s.is_checked(3)), (false, true, true, false));
        assert_eq!(s.chosen(), [1, 2]);
        assert!(!s.all());
        // An empty or foreign current set cannot be shown as "nothing": all are checked.
        assert!(sel(4, &[]).all());
        assert!(sel(4, &[9]).all());
    }

    #[test]
    fn the_last_checked_processor_stays() {
        let mut s = sel(3, &[1]);
        assert!(!s.toggle(1));
        assert_eq!(s.chosen(), [1]);
        assert!(s.toggle(2));
        assert!(s.toggle(1));
        assert_eq!(s.chosen(), [2]);
        assert!(!s.toggle(2) && !s.toggle(7));
    }

    #[test]
    fn all_processors_toggles_between_everything_and_the_first() {
        let mut s = sel(4, &[2]);
        s.toggle_all();
        assert!(s.all());
        s.toggle_all();
        assert_eq!(s.chosen(), [0]);
        s.toggle_all();
        assert!(s.all());
    }

    #[test]
    fn processors_are_described_in_ranges() {
        assert_eq!(describe(&[3]), "processor 3");
        assert_eq!(describe(&[0, 1]), "processors 0, 1");
        assert_eq!(describe(&[0, 1, 2, 3, 6]), "processors 0-3, 6");
        assert_eq!(describe(&[0, 2, 4]), "processors 0, 2, 4");
        assert_eq!(sel(4, &[0, 1, 2, 3]).label(), "all processors");
        assert_eq!(sel(8, &[4, 5, 6, 7]).label(), "processors 4-7");
    }

    #[test]
    fn kernel_cpu_lists_parse() {
        assert_eq!(parse_cpu_list("0-3,6,8-9\n"), [0, 1, 2, 3, 6, 8, 9]);
        assert_eq!(parse_cpu_list("0"), [0]);
        assert_eq!(parse_cpu_list(""), Vec::<usize>::new());
        assert_eq!(parse_cpu_list("3-1,x,2,2, 4 - 5"), [2, 4, 5]);
        assert_eq!(parse_cpu_list("0-99999999"), Vec::<usize>::new());
    }

    #[test]
    fn only_a_single_group_machine_gets_the_action() {
        assert!(supported_with_groups(0) && supported_with_groups(1));
        assert!(!supported_with_groups(2));
    }

    /// A child this test starts (and always kills): read its processors, bind it to the first
    /// one, read that back, release it again.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn a_child_process_can_be_bound_and_released() {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        };
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        crate::platform::hide_console(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let start = {
            #[cfg(target_os = "linux")]
            {
                super::super::imp::start_of(child.id()).unwrap_or(0)
            }
            #[cfg(not(target_os = "linux"))]
            {
                0
            }
        };
        let row = ProcessRow { pid: child.id(), create_time: start, name: "child".into(), ..Default::default() };
        let result = std::panic::catch_unwind(|| {
            let s = read(&row).unwrap();
            let (all, initial) = (s.cpus().to_vec(), s.chosen());
            assert!(!initial.is_empty());
            if all.len() > 1 && initial.len() > 1 {
                let first = initial[0];
                let mut one = Selection::new(all.clone(), &[first]);
                assert_eq!(one.chosen(), [first]);
                assert_eq!(apply(&row, &one).unwrap(), format!("child affinity: processor {first}"));
                assert_eq!(read(&row).unwrap().chosen(), [first]);
                // Back to what it was.
                one = Selection::new(all, &initial);
                apply(&row, &one).unwrap();
            }
            assert_eq!(read(&row).unwrap().chosen(), initial);
        });
        let _ = child.kill();
        let _ = child.wait();
        result.unwrap();
    }
}
