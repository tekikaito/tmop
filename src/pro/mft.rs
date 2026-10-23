//! NTFS Master File Table reader: the fast disk-space scan method (elevated, local NTFS volume).
//!
//! The volume is opened raw (`\\.\C:`), `$MFT` is located through `FSCTL_GET_NTFS_VOLUME_DATA` and
//! its own record 0, and its data runs are streamed in 4 MiB chunks. Every FILE record gets its
//! update-sequence fixups applied and is parsed for name, parent, size, modified time and
//! attributes; the folder tree is then built in memory (no directory is ever opened). Any problem
//! returns `Err` and the caller falls back to the parallel walker.
//!
//! Notes: hard links count once (one record per file, the best-namespace name is used);
//! alternate data streams are ignored; junction folders are empty in the MFT, so their targets
//! are not counted twice; `$MFT` with an `$ATTRIBUTE_LIST` whose `$DATA` lives in extension
//! records is not supported (fallback).
//!
//! The parsing half is pure and unit-tested on synthetic records; only [`eligible`] and [`scan`]
//! touch Windows.

use super::diskspace::{ext_of, FileRec, Names, Node, Publisher, F_HIDDEN, F_REPARSE, NO_PARENT};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// The volume an MFT scan would read.
#[derive(Clone, Debug, Default)]
pub struct VolumeInfo {
    /// `C:\`
    pub root: String,
    /// `\\.\C:`
    pub device: String,
}

/// Some(...) when the root's volume is a local NTFS volume and TMOP is elevated.
#[cfg(windows)]
pub fn eligible(root: &Path) -> Option<VolumeInfo> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW};
    if !super::winutil::is_elevated() {
        return None;
    }
    let s = root.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let b = s.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None;
    }
    let letter = (b[0] as char).to_ascii_uppercase();
    let vroot = format!("{letter}:\\");
    let w = super::winutil::wide(&vroot);
    // SAFETY: valid NUL-terminated root; the buffer is sized as passed.
    let ty = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) };
    if ty != 2 && ty != 3 {
        return None; // removable or fixed only
    }
    let mut fs = [0u16; 64];
    // SAFETY: as above.
    unsafe { GetVolumeInformationW(PCWSTR(w.as_ptr()), None, None, None, None, Some(&mut fs)) }.ok()?;
    (super::winutil::from_wide(&fs) == "NTFS").then(|| VolumeInfo { root: vroot, device: format!(r"\\.\{letter}:") })
}

#[cfg(not(windows))]
pub fn eligible(_: &Path) -> Option<VolumeInfo> {
    None
}

/// Parsed FILE record (what the tree builder needs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub in_use: bool,
    pub is_dir: bool,
    /// Index of the base record for extension records (`None` for base records).
    pub base: Option<u64>,
    /// Low 48 bits of the parent reference of the chosen `$FILE_NAME`.
    pub parent: u64,
    /// Win32 namespace name preferred; a DOS-only name is kept when it is the only one.
    pub name: String,
    /// `$STANDARD_INFORMATION` modified time (FILETIME ticks).
    pub mtime: u64,
    /// `$STANDARD_INFORMATION` file attributes.
    pub attrs: u32,
    /// Unnamed `$DATA`: real size when non-resident, value length when resident.
    pub size: Option<u64>,
    pub has_attr_list: bool,
}

pub const ROOT_RECORD: u64 = 5;
pub const FIRST_USER_RECORD: u64 = 24;
const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const ATTR_END: u32 = 0xFFFF_FFFF;

fn short(o: usize) -> String {
    format!("record truncated at {o}")
}
fn u16le(b: &[u8], o: usize) -> Result<u16, String> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| short(o))
}
fn u32le(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| short(o))
}
fn u64le(b: &[u8], o: usize) -> Result<u64, String> {
    b.get(o..o + 8).and_then(|s| s.try_into().ok()).map(u64::from_le_bytes).ok_or_else(|| short(o))
}

/// Applies the update-sequence fixups in place. Err when the signature is not "FILE" or a sector
/// tag mismatches (torn write).
pub fn apply_fixups(rec: &mut [u8], bytes_per_sector: u32) -> Result<(), String> {
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return Err("not a FILE record".into());
    }
    let (off, count) = (u16le(rec, 4)? as usize, u16le(rec, 6)? as usize);
    let bps = bytes_per_sector as usize;
    if count == 0 || bps < 2 || off + count * 2 > rec.len() {
        return Err("bad update sequence array".into());
    }
    let usn = [rec[off], rec[off + 1]];
    for k in 1..count {
        let end = k * bps;
        if end > rec.len() {
            return Err("update sequence array longer than the record".into());
        }
        if rec[end - 2..end] != usn {
            return Err(format!("fixup mismatch in sector {k}"));
        }
        rec[end - 2] = rec[off + 2 * k];
        rec[end - 1] = rec[off + 2 * k + 1];
    }
    Ok(())
}

/// One attribute header inside a record.
struct Attr {
    ty: u32,
    off: usize,
    len: usize,
    non_resident: bool,
    name_len: u8,
}

/// Walks the attribute chain of a (fixed-up) record.
fn walk_attrs(rec: &[u8], mut f: impl FnMut(&Attr) -> Result<(), String>) -> Result<(), String> {
    let mut o = u16le(rec, 20)? as usize;
    let used = u32le(rec, 24)? as usize;
    let end = if used >= 24 && used <= rec.len() { used } else { rec.len() };
    loop {
        if o + 4 > end {
            return Err("attribute list not terminated".into());
        }
        let ty = u32le(rec, o)?;
        if ty == ATTR_END {
            return Ok(());
        }
        if o + 24 > end {
            return Err(short(o));
        }
        let len = u32le(rec, o + 4)? as usize;
        if len < 24 || o + len > end {
            return Err(format!("bad attribute length {len} at {o}"));
        }
        f(&Attr { ty, off: o, len, non_resident: rec[o + 8] != 0, name_len: rec[o + 9] })?;
        o += len;
    }
}

/// Resident value of `a` as (offset, length) inside the record.
fn resident(rec: &[u8], a: &Attr) -> Result<(usize, usize), String> {
    let (vl, vo) = (u32le(rec, a.off + 16)? as usize, u16le(rec, a.off + 20)? as usize);
    if vo + vl > a.len {
        return Err(format!("resident value past its attribute at {}", a.off));
    }
    Ok((a.off + vo, vl))
}

fn rank(ns: u8) -> u8 {
    match ns {
        3 => 4, // Win32 and DOS
        1 => 3, // Win32
        0 => 2, // POSIX
        _ => 1, // DOS only
    }
}

/// Parses one fixed-up record. `Ok(None)` for unused records.
pub fn parse_record(rec: &[u8]) -> Result<Option<Record>, String> {
    if rec.len() >= 4 && &rec[0..4] == b"BAAD" {
        return Ok(None);
    }
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return Err("not a FILE record".into());
    }
    let flags = u16le(rec, 22)?;
    if flags & 1 == 0 {
        return Ok(None);
    }
    let base = u64le(rec, 32)? & REF_MASK;
    let mut r = Record { in_use: true, is_dir: flags & 2 != 0, base: (base != 0).then_some(base), ..Default::default() };
    let mut best = 0u8;
    walk_attrs(rec, |a| {
        match a.ty {
            0x10 if !a.non_resident => {
                let (v, n) = resident(rec, a)?;
                if n >= 36 {
                    r.mtime = u64le(rec, v + 8)?;
                    r.attrs = u32le(rec, v + 32)?;
                }
            }
            0x20 => r.has_attr_list = true,
            0x30 if !a.non_resident => {
                let (v, n) = resident(rec, a)?;
                if n < 66 {
                    return Err(format!("short $FILE_NAME at {}", a.off));
                }
                let (name_len, ns) = (rec[v + 64] as usize, rec[v + 65]);
                if 66 + name_len * 2 > n {
                    return Err(format!("$FILE_NAME name past its attribute at {}", a.off));
                }
                if rank(ns) > best {
                    best = rank(ns);
                    r.parent = u64le(rec, v)? & REF_MASK;
                    let units = rec[v + 66..v + 66 + name_len * 2].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c));
                    r.name = char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect();
                }
            }
            0x80 if a.name_len == 0 && r.size.is_none() => {
                if a.non_resident {
                    if a.len < 56 {
                        return Err(format!("short non-resident $DATA at {}", a.off));
                    }
                    if u64le(rec, a.off + 16)? == 0 {
                        r.size = Some(u64le(rec, a.off + 48)?);
                    }
                } else {
                    r.size = Some(u32le(rec, a.off + 16)? as u64);
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(Some(r))
}

/// Decodes a run list into (lcn, clusters); `lcn` is `None` for sparse runs.
fn decode_runs(runs: &[u8]) -> Result<Vec<(Option<u64>, u64)>, String> {
    let mut out = Vec::new();
    let (mut o, mut lcn) = (0usize, 0i64);
    while o < runs.len() {
        let h = runs[o];
        if h == 0 {
            break;
        }
        let (lb, ob) = ((h & 0xF) as usize, (h >> 4) as usize);
        if lb == 0 || lb > 8 || ob > 8 {
            return Err(format!("bad run header {h:#04x}"));
        }
        let body = runs.get(o + 1..o + 1 + lb + ob).ok_or("run list truncated")?;
        let len = body[..lb].iter().rev().fold(0u64, |a, b| a << 8 | *b as u64);
        if ob == 0 {
            out.push((None, len));
        } else {
            let raw = body[lb..].iter().rev().fold(0u64, |a, b| a << 8 | *b as u64);
            let shift = 64 - 8 * ob as u32;
            let delta = ((raw << shift) as i64) >> shift; // sign-extend
            lcn = lcn.checked_add(delta).filter(|l| *l >= 0).ok_or("run list leaves the volume")?;
            out.push((Some(lcn as u64), len));
        }
        o += 1 + lb + ob;
    }
    Ok(out)
}

/// Decodes a non-resident run list into (lcn, clusters) extents; sparse runs are skipped.
pub fn parse_runlist(runs: &[u8]) -> Result<Vec<(u64, u64)>, String> {
    Ok(decode_runs(runs)?.into_iter().filter_map(|(l, c)| l.map(|l| (l, c))).collect())
}

/// Parses `$MFT`'s own record 0 (fixed up): the extents of its unnamed `$DATA`. Err when the
/// run list in record 0 does not cover the whole attribute (pieces in extension records).
pub fn mft_extents(record0: &[u8], bytes_per_cluster: u32) -> Result<Vec<(u64, u64)>, String> {
    let (mut found, mut list) = (None, false);
    walk_attrs(record0, |a| {
        match a.ty {
            0x20 => list = true,
            0x80 if a.non_resident && a.name_len == 0 && found.is_none() && a.len >= 56 && u64le(record0, a.off + 16)? == 0 => {
                let (real, ro) = (u64le(record0, a.off + 48)?, u16le(record0, a.off + 32)? as usize);
                if ro >= a.len {
                    return Err("bad $MFT run list offset".into());
                }
                found = Some((decode_runs(&record0[a.off + ro..a.off + a.len])?, real));
            }
            _ => {}
        }
        Ok(())
    })?;
    let (runs, real) = found.ok_or("$MFT has no non-resident $DATA")?;
    if runs.iter().any(|r| r.0.is_none()) {
        return Err("sparse $MFT".into());
    }
    let clusters: u64 = runs.iter().map(|r| r.1).sum();
    if clusters.saturating_mul(bytes_per_cluster as u64) < real {
        return Err(if list { "$MFT has an $ATTRIBUTE_LIST with $DATA in extension records".into() } else { "$MFT run list is shorter than its size".into() });
    }
    Ok(runs.into_iter().filter_map(|(l, c)| l.map(|l| (l, c))).collect())
}

/// Everything the scan publishes, before the roll-up.
pub struct TreeParts {
    pub nodes: Vec<Node>,
    pub files: Vec<FileRec>,
    pub names: Names,
    pub exts: HashMap<String, (u64, u64)>,
    pub files_count: u64,
    pub dirs: u64,
    pub bytes: u64,
}

/// Builds the tree below `root_record` (the MFT index of the scan root; [`ROOT_RECORD`] for a
/// whole volume). Node indices are assigned parent-first. System metafiles (records below 24 other
/// than the root) and everything below them are left out, as are orphans.
pub fn build_tree(records: &[(u64, Record)], root_record: u64, root: &Path, cancel: &AtomicBool) -> Result<TreeParts, String> {
    let max = records.iter().map(|r| r.0).max().ok_or("no records")?;
    if max > u32::MAX as u64 {
        return Err("MFT too large".into());
    }
    let max = max as usize;
    if !records.iter().any(|(n, r)| *n == root_record && r.base.is_none() && r.is_dir) {
        return Err("scan root is not a folder record of this MFT".into());
    }
    // Sizes that live in extension records (attribute lists).
    let mut ext_size: HashMap<u64, u64> = HashMap::new();
    for (_, r) in records {
        if let (Some(b), Some(s)) = (r.base, r.size) {
            ext_size.entry(b).or_insert(s);
        }
    }
    // Children by parent, as a compact counting-sort adjacency.
    let linked = |n: u64, r: &Record| r.in_use && r.base.is_none() && !r.name.is_empty() && n >= FIRST_USER_RECORD && (r.parent as usize) <= max;
    let mut start = vec![0u32; max + 2];
    for (n, r) in records {
        if linked(*n, r) {
            start[r.parent as usize + 1] += 1;
        }
    }
    for i in 1..start.len() {
        start[i] += start[i - 1];
    }
    let mut fill = start.clone();
    let mut kids = vec![0u32; start[max + 1] as usize];
    for (i, (n, r)) in records.iter().enumerate() {
        if linked(*n, r) {
            kids[fill[r.parent as usize] as usize] = i as u32;
            fill[r.parent as usize] += 1;
        }
    }
    let mut seen = vec![false; max + 1];
    seen[root_record as usize] = true;
    let mut p = TreeParts { nodes: vec![Node { name: root.display().to_string(), parent: NO_PARENT, ..Default::default() }], files: Vec::new(), names: Names::default(), exts: HashMap::new(), files_count: 0, dirs: 0, bytes: 0 };
    let mut stack = vec![(root_record as usize, 0u32)];
    let mut tick = 0u32;
    while let Some((rn, node)) = stack.pop() {
        tick += 1;
        if tick.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        for k in start[rn]..start[rn + 1] {
            let (cn, cr) = &records[kids[k as usize] as usize];
            if std::mem::replace(&mut seen[*cn as usize], true) {
                continue;
            }
            if cr.is_dir {
                let child = p.nodes.len() as u32;
                p.nodes.push(Node { name: cr.name.clone(), parent: node, ..Default::default() });
                p.nodes[node as usize].children.push(child);
                p.dirs += 1;
                stack.push((*cn as usize, child));
            } else {
                let size = cr.size.or_else(|| ext_size.get(cn).copied()).unwrap_or(0);
                let reparse = cr.attrs & 0x400 != 0;
                if reparse && size == 0 {
                    continue; // symlinks and app-exec links are not files
                }
                let flags = if cr.attrs & 0x6 != 0 { F_HIDDEN } else { 0 } | if reparse { F_REPARSE } else { 0 };
                let (name_off, name_len) = p.names.push(&cr.name);
                p.files.push(FileRec { size, mtime: cr.mtime, dir: node, name_off, name_len, flags });
                let n = &mut p.nodes[node as usize];
                n.own += size;
                n.files += 1;
                p.files_count += 1;
                p.bytes += size;
                let x = p.exts.entry(ext_of(&cr.name)).or_default();
                x.0 += size;
                x.1 += 1;
            }
        }
    }
    Ok(p)
}

#[cfg(not(windows))]
pub fn scan(_root: &Path, _vol: &VolumeInfo, _cancel: &AtomicBool, _publish: &Publisher) -> Result<(), String> {
    Err("MFT reading is only available on Windows".into())
}

/// Reads the MFT of `vol` and publishes the tree below `root`. `Err` means "use the walker".
#[cfg(windows)]
pub fn scan(root: &Path, vol: &VolumeInfo, cancel: &AtomicBool, publish: &Publisher) -> Result<(), String> {
    use super::diskspace::ScanMethod;
    use std::time::{Duration, Instant};
    use windows::Win32::System::Ioctl::NTFS_VOLUME_DATA_BUFFER;
    publish.progress(|p| {
        p.method = ScanMethod::Mft;
        p.current = "Reading MFT 0%".into();
    });
    let h = io::open(&vol.device, 0x8000_0000, 0)?; // GENERIC_READ
    let geo: NTFS_VOLUME_DATA_BUFFER = io::volume_data(&h)?;
    let (bps, bpc, rs) = (geo.BytesPerSector, geo.BytesPerCluster, geo.BytesPerFileRecordSegment);
    if !rs.is_power_of_two() || !(512..=65_536).contains(&rs) || bps == 0 || bpc == 0 || geo.MftStartLcn < 0 || geo.MftValidDataLength <= 0 {
        return Err("unexpected NTFS geometry".into());
    }
    let root_record = io::root_record(root, geo.VolumeSerialNumber as u64)?;
    let mut rec0 = vec![0u8; rs as usize];
    io::read_at(&h, geo.MftStartLcn as u64 * bpc as u64, &mut rec0)?;
    apply_fixups(&mut rec0, bps)?;
    let extents = mft_extents(&rec0, bpc)?;
    let valid = geo.MftValidDataLength as u64;
    let rs64 = rs as u64;
    const CHUNK: u64 = 4 << 20;
    let mut buf = vec![0u8; CHUNK as usize];
    let mut records: Vec<(u64, Record)> = Vec::new();
    let (mut bad, mut seen_records) = (0u64, 0u64);
    let (mut files, mut dirs, mut bytes) = (0u64, 0u64, 0u64);
    let mut last = Instant::now();
    let mut vofs = 0u64; // byte offset inside $MFT of the current extent
    'extents: for (lcn, clusters) in extents {
        let ext_bytes = clusters * bpc as u64;
        let mut pos = 0u64;
        while pos < ext_bytes {
            if cancel.load(Ordering::Relaxed) {
                publish.finish(None, true, ScanMethod::Mft);
                return Ok(());
            }
            let at = vofs + pos;
            if at >= valid {
                break 'extents;
            }
            let n = CHUNK.min(ext_bytes - pos).min(valid - at);
            let n = n - n % rs64;
            if n == 0 {
                break 'extents;
            }
            io::read_at(&h, lcn * bpc as u64 + pos, &mut buf[..n as usize])?;
            for (i, chunk) in buf[..n as usize].chunks_exact_mut(rs as usize).enumerate() {
                if &chunk[0..4] != b"FILE" {
                    continue;
                }
                let rn = at / rs64 + i as u64;
                seen_records += 1;
                let parsed = apply_fixups(chunk, bps).and_then(|_| parse_record(chunk));
                match parsed {
                    Ok(Some(r)) => {
                        let keep = match r.base {
                            None => (rn >= FIRST_USER_RECORD || rn == ROOT_RECORD) && !r.name.is_empty(),
                            Some(_) => r.size.is_some(),
                        };
                        if keep {
                            if r.base.is_none() {
                                if r.is_dir {
                                    dirs += 1;
                                } else {
                                    files += 1;
                                    bytes += r.size.unwrap_or(0);
                                }
                            }
                            records.push((rn, r));
                        }
                    }
                    Ok(None) => {}
                    Err(_) => bad += 1,
                }
            }
            if bad > 1000 && bad * 50 > seen_records {
                return Err(format!("too many unreadable MFT records ({bad})"));
            }
            pos += n;
            if last.elapsed() > Duration::from_millis(100) {
                last = Instant::now();
                let pct = (vofs + pos) * 100 / valid.max(1);
                publish.progress(|p| {
                    p.files = files;
                    p.dirs = dirs;
                    p.bytes = bytes;
                    p.current = format!("Reading MFT {pct}%");
                });
            }
        }
        vofs += ext_bytes;
    }
    publish.progress(|p| p.current = "Building the folder tree\u{2026}".into());
    let parts = match build_tree(&records, root_record, root, cancel) {
        Ok(p) => p,
        Err(e) if e == "cancelled" => {
            publish.finish(None, true, ScanMethod::Mft);
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    drop(records);
    let (fc, d, b) = (parts.files_count, parts.dirs, parts.bytes);
    publish.progress(|p| {
        p.files = fc;
        p.dirs = d + 1;
        p.bytes = b;
    });
    let tree = publish.build_owned(parts.nodes, parts.files, parts.names, &parts.exts, ScanMethod::Mft);
    publish.finish(Some(tree), false, ScanMethod::Mft);
    Ok(())
}

#[cfg(windows)]
mod io {
    use std::ffi::c_void;
    use std::path::Path;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FileIdInfo, GetFileInformationByHandleEx, ReadFile, SetFilePointerEx, FILE_BEGIN, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
    use windows::Win32::System::IO::DeviceIoControl;

    pub struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: handle from CreateFileW, closed once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn open(path: &str, access: u32, flags: u32) -> Result<Handle, String> {
        let w = crate::pro::winutil::wide(path);
        // SAFETY: valid NUL-terminated path.
        unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(flags),
                None,
            )
        }
        .map(Handle)
        .map_err(|e| format!("cannot open {path}: {}", e.message()))
    }

    pub fn volume_data(h: &Handle) -> Result<NTFS_VOLUME_DATA_BUFFER, String> {
        let mut d = NTFS_VOLUME_DATA_BUFFER::default();
        let mut ret = 0u32;
        // SAFETY: output buffer is the struct the control code fills.
        unsafe { DeviceIoControl(h.0, FSCTL_GET_NTFS_VOLUME_DATA, None, 0, Some(&mut d as *mut _ as *mut c_void), std::mem::size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32, Some(&mut ret), None) }
            .map_err(|e| format!("FSCTL_GET_NTFS_VOLUME_DATA: {}", e.message()))?;
        Ok(d)
    }

    pub fn read_at(h: &Handle, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let mut got = 0u32;
        // SAFETY: `buf` is writable for its length; offsets are cluster-aligned.
        unsafe {
            SetFilePointerEx(h.0, offset as i64, None, FILE_BEGIN).map_err(|e| format!("seek: {}", e.message()))?;
            ReadFile(h.0, Some(buf), Some(&mut got), None).map_err(|e| format!("read: {}", e.message()))?;
        }
        if got as usize != buf.len() {
            return Err(format!("short read ({got} of {} bytes)", buf.len()));
        }
        Ok(())
    }

    /// MFT index of the scan root (must live on the volume with serial `serial`).
    pub fn root_record(root: &Path, serial: u64) -> Result<u64, String> {
        let h = open(&root.to_string_lossy(), 0, FILE_FLAG_BACKUP_SEMANTICS.0)?;
        let mut info = FILE_ID_INFO::default();
        // SAFETY: output is the struct FileIdInfo fills.
        unsafe { GetFileInformationByHandleEx(h.0, FileIdInfo, &mut info as *mut _ as *mut c_void, std::mem::size_of::<FILE_ID_INFO>() as u32) }
            .map_err(|e| format!("file id: {}", e.message()))?;
        if info.VolumeSerialNumber != serial {
            return Err("scan root is on another volume (mount point)".into());
        }
        let id = info.FileId.Identifier;
        Ok(u64::from_le_bytes([id[0], id[1], id[2], id[3], id[4], id[5], id[6], id[7]]) & super::REF_MASK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put16(b: &mut [u8], o: usize, v: u16) {
        b[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put32(b: &mut [u8], o: usize, v: u32) {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put64(b: &mut [u8], o: usize, v: u64) {
        b[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }

    /// Record under construction: header + attributes appended at `at`.
    struct Rb {
        r: Vec<u8>,
        at: usize,
    }

    impl Rb {
        fn new(is_dir: bool, in_use: bool, base: u64) -> Rb {
            let mut r = vec![0u8; 1024];
            r[0..4].copy_from_slice(b"FILE");
            put16(&mut r, 4, 0x30);
            put16(&mut r, 6, 3);
            put16(&mut r, 20, 0x38);
            put16(&mut r, 22, in_use as u16 | if is_dir { 2 } else { 0 });
            put32(&mut r, 28, 1024);
            put64(&mut r, 32, base);
            Rb { r, at: 0x38 }
        }
        fn resident(&mut self, ty: u32, value: &[u8]) -> &mut Self {
            let len = (24 + value.len()).div_ceil(8) * 8;
            let o = self.at;
            put32(&mut self.r, o, ty);
            put32(&mut self.r, o + 4, len as u32);
            put32(&mut self.r, o + 16, value.len() as u32);
            put16(&mut self.r, o + 20, 24);
            self.r[o + 24..o + 24 + value.len()].copy_from_slice(value);
            self.at += len;
            self
        }
        fn nonres(&mut self, ty: u32, end_vcn: u64, real: u64, runs: &[u8]) -> &mut Self {
            let len = (64 + runs.len()).div_ceil(8) * 8;
            let o = self.at;
            put32(&mut self.r, o, ty);
            put32(&mut self.r, o + 4, len as u32);
            self.r[o + 8] = 1;
            put64(&mut self.r, o + 24, end_vcn);
            put16(&mut self.r, o + 32, 64);
            put64(&mut self.r, o + 48, real);
            self.r[o + 64..o + 64 + runs.len()].copy_from_slice(runs);
            self.at += len;
            self
        }
        fn std_info(&mut self, mtime: u64, attrs: u32) -> &mut Self {
            let mut v = [0u8; 72];
            put64(&mut v, 8, mtime);
            put32(&mut v, 32, attrs);
            self.resident(0x10, &v)
        }
        fn name(&mut self, parent: u64, ns: u8, name: &str) -> &mut Self {
            let u: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
            let mut v = vec![0u8; 66 + u.len()];
            put64(&mut v, 0, parent);
            v[64] = name.encode_utf16().count() as u8;
            v[65] = ns;
            v[66..].copy_from_slice(&u);
            self.resident(0x30, &v)
        }
        fn finish(&mut self) -> Vec<u8> {
            let o = self.at;
            put32(&mut self.r, o, ATTR_END);
            put32(&mut self.r, 24, (o + 8) as u32);
            self.r.clone()
        }
    }

    fn synth(is_dir: bool, parent: u64, names: &[(u8, &str)], size: Option<(bool, u64)>, mtime: u64, attrs: u32) -> Vec<u8> {
        let mut b = Rb::new(is_dir, true, 0);
        b.std_info(mtime, attrs);
        for (ns, n) in names {
            b.name(parent, *ns, n);
        }
        match size {
            Some((true, v)) => {
                b.resident(0x80, &vec![0u8; v as usize]);
            }
            Some((false, v)) => {
                b.nonres(0x80, 0, v, &[0x11, 0x01, 0x10, 0]);
            }
            None => {}
        }
        b.finish()
    }

    #[test]
    fn fixups_round_trip() {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        put16(&mut r, 4, 0x30);
        put16(&mut r, 6, 3);
        put16(&mut r, 0x30, 0x1234);
        put16(&mut r, 0x32, 0xAABB);
        put16(&mut r, 0x34, 0xCCDD);
        put16(&mut r, 510, 0x1234);
        put16(&mut r, 1022, 0x1234);
        let mut ok = r.clone();
        apply_fixups(&mut ok, 512).unwrap();
        assert_eq!(&ok[510..512], &0xAABBu16.to_le_bytes());
        assert_eq!(&ok[1022..1024], &0xCCDDu16.to_le_bytes());
        let mut torn = r.clone();
        put16(&mut torn, 1022, 0x9999);
        assert!(apply_fixups(&mut torn, 512).is_err());
        let mut sig = r;
        sig[0] = b'X';
        assert!(apply_fixups(&mut sig, 512).is_err());
    }

    #[test]
    fn parses_file_and_dir_records() {
        let r = parse_record(&synth(false, 24, &[(2, "LONGNA~1.TXT"), (1, "long name.txt")], Some((true, 10)), 123, 0x20)).unwrap().unwrap();
        assert_eq!((r.name.as_str(), r.parent, r.size, r.mtime, r.attrs, r.is_dir), ("long name.txt", 24, Some(10), 123, 0x20, false));
        let r = parse_record(&synth(false, 5, &[(1, "a.txt"), (3, "b.txt")], None, 0, 0)).unwrap().unwrap();
        assert_eq!(r.name, "b.txt", "Win32&DOS beats Win32");
        let r = parse_record(&synth(false, 5, &[(0, "posix"), (2, "DOS")], None, 0, 0)).unwrap().unwrap();
        assert_eq!(r.name, "posix");
        let r = parse_record(&synth(false, 5, &[(2, "ONLY~1")], None, 0, 0)).unwrap().unwrap();
        assert_eq!(r.name, "ONLY~1", "DOS-only name is kept when alone");
        let r = parse_record(&synth(false, 5, &[(1, "big.bin")], Some((false, 5_000_000)), 0, 0)).unwrap().unwrap();
        assert_eq!(r.size, Some(5_000_000));
        let d = parse_record(&synth(true, 5, &[(1, "dir")], None, 0, 0x10)).unwrap().unwrap();
        assert!(d.is_dir && d.size.is_none());
        let mut unused = Rb::new(false, false, 0);
        assert_eq!(parse_record(&unused.finish()).unwrap(), None);
        let mut baad = synth(false, 5, &[(1, "x")], None, 0, 0);
        baad[0..4].copy_from_slice(b"BAAD");
        assert_eq!(parse_record(&baad).unwrap(), None);
        let ext = {
            let mut b = Rb::new(false, true, 25 | (3 << 48));
            b.nonres(0x80, 0, 77, &[0x11, 0x01, 0x10, 0]);
            b.finish()
        };
        let e = parse_record(&ext).unwrap().unwrap();
        assert_eq!((e.base, e.size), (Some(25), Some(77)));
        // Truncated attribute: length runs past the record.
        let mut bad = synth(false, 5, &[(1, "x")], None, 0, 0);
        put32(&mut bad, 0x38 + 4, 5000);
        assert!(parse_record(&bad).is_err());
        let mut bad = synth(false, 5, &[(1, "x")], None, 0, 0);
        put32(&mut bad, 0x38 + 4, 8);
        assert!(parse_record(&bad).is_err());
    }

    #[test]
    fn runlist_decoding() {
        let runs = [0x21, 0x18, 0x34, 0x56, 0x11, 0x08, 0xF0, 0x00];
        assert_eq!(parse_runlist(&runs).unwrap(), vec![(0x5634, 0x18), (0x5634 - 16, 8)]);
        // Sparse run in the middle is skipped, the next run is relative to the previous real LCN.
        let sparse = [0x11, 0x04, 0x10, 0x01, 0x05, 0x11, 0x02, 0x03, 0x00];
        assert_eq!(parse_runlist(&sparse).unwrap(), vec![(0x10, 4), (0x13, 2)]);
        assert!(parse_runlist(&[0x21, 0x18]).is_err());
        assert!(parse_runlist(&[0x10, 0x01, 0x00]).is_err());
        assert!(parse_runlist(&[0x91, 0x01, 0x01, 0x00]).is_err());
        assert!(parse_runlist(&[0x11, 0x01, 0x80, 0x00]).is_err()); // LCN -128
    }

    #[test]
    fn mft_extents_from_record0() {
        let mut b = Rb::new(false, true, 0);
        b.name(5, 3, "$MFT");
        b.nonres(0x80, 0x1f, 0x20 * 4096, &[0x21, 0x20, 0x00, 0x10, 0x00]);
        let ok = b.finish();
        assert_eq!(mft_extents(&ok, 4096).unwrap(), vec![(0x1000, 0x20)]);
        // With an $ATTRIBUTE_LIST and only part of the data in record 0.
        let mut b = Rb::new(false, true, 0);
        b.name(5, 3, "$MFT");
        b.resident(0x20, &[0u8; 32]);
        b.nonres(0x80, 0x0f, 0x40 * 4096, &[0x21, 0x10, 0x00, 0x10, 0x00]);
        let err = mft_extents(&b.finish(), 4096).unwrap_err();
        assert!(err.contains("ATTRIBUTE_LIST"), "{err}");
        // No $DATA at all.
        let mut b = Rb::new(false, true, 0);
        b.name(5, 3, "$MFT");
        assert!(mft_extents(&b.finish(), 4096).is_err());
    }

    fn rec(is_dir: bool, parent: u64, name: &str, size: Option<u64>) -> Record {
        Record { in_use: true, is_dir, parent, name: name.into(), size, mtime: 1, ..Default::default() }
    }

    #[test]
    fn builds_tree_from_records() {
        let records = vec![
            (5, rec(true, 5, ".", None)),
            (24, rec(true, 5, "a", None)),
            (25, rec(false, 24, "x.bin", Some(1 << 20))),
            (26, rec(false, 11, "$Secure", Some(999))),
            (27, Record { in_use: true, base: Some(25), size: Some(7), ..Default::default() }),
            (28, rec(false, 99, "orphan.txt", Some(5))),
            (29, rec(false, 5, "nosize.bin", None)),
            (30, Record { attrs: 0x400, ..rec(false, 5, "link", Some(0)) }),
            (31, Record { attrs: 0x2, ..rec(false, 24, "hidden.txt", Some(3)) }),
        ];
        let cancel = AtomicBool::new(false);
        let p = build_tree(&records, ROOT_RECORD, Path::new("C:\\"), &cancel).unwrap();
        assert_eq!(p.nodes.len(), 2);
        assert_eq!(p.nodes[0].parent, NO_PARENT);
        assert_eq!(p.nodes[1].name, "a");
        assert_eq!(p.nodes[0].children, vec![1]);
        for i in 1..p.nodes.len() {
            assert!((p.nodes[i].parent as usize) < i);
        }
        assert_eq!(p.files.len(), 3, "x.bin, nosize.bin, hidden.txt");
        let big = p.files.iter().find(|f| p.names.get(f.name_off, f.name_len) == "x.bin").unwrap();
        assert_eq!((big.size, big.dir), (1 << 20, 1));
        let h = p.files.iter().find(|f| p.names.get(f.name_off, f.name_len) == "hidden.txt").unwrap();
        assert!(h.hidden());
        assert_eq!(p.nodes[1].own, (1 << 20) + 3);
        assert_eq!(p.bytes, (1 << 20) + 3);
        // Subfolder root and extension-record size merge.
        let records = vec![(5, rec(true, 5, ".", None)), (24, rec(true, 5, "a", None)), (25, rec(false, 24, "y.bin", None)), (27, Record { in_use: true, base: Some(25), size: Some(7), ..Default::default() })];
        let p = build_tree(&records, 24, Path::new("C:\\a"), &cancel).unwrap();
        assert_eq!(p.nodes.len(), 1);
        assert_eq!(p.files[0].size, 7);
        assert!(build_tree(&records, 25, Path::new("C:\\a\\y.bin"), &cancel).is_err(), "a file is not a scan root");
        cancel.store(true, Ordering::Relaxed);
        let many: Vec<(u64, Record)> = std::iter::once((5, rec(true, 5, ".", None))).chain((24..9000).map(|i| (i, rec(true, 5, "d", None)))).collect();
        assert!(build_tree(&many, ROOT_RECORD, Path::new("C:\\"), &cancel).is_err());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs an elevated process and reads the raw system volume (read-only)"]
    fn reads_system_volume_mft() {
        use super::super::diskspace::{ScanMethod, ScanShared};
        use std::sync::Mutex;
        if !crate::pro::winutil::is_elevated() {
            eprintln!("skipped: not elevated");
            return;
        }
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let root = std::path::PathBuf::from(format!("{drive}\\"));
        let vol = eligible(&root).expect("system drive is local NTFS");
        let shared = Mutex::new(ScanShared::default());
        let publish = Publisher::new(&shared, &root);
        scan(&root, &vol, &AtomicBool::new(false), &publish).expect("mft scan");
        let t = shared.lock().unwrap().tree.clone().expect("tree");
        assert!(t.nodes.len() > 1000, "{} folders", t.nodes.len());
        assert!(t.files.len() > t.nodes.len());
        assert_eq!(t.method, ScanMethod::Mft);
    }
}
