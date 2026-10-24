//! State of the advanced and inventory pages (Power & Freq, Flight Recorder, Connections,
//! Installed Apps, Drivers, Disk Space); each page lives in its own module.

use super::table::TableState;
use crate::pro::apps::App as InstalledApp;
use crate::pro::connections::Conn;
use crate::pro::diskspace::{DirFile, Scanner, Tree, Volume};
use crate::pro::drivers::Service;
use crate::pro::power::PowerInfo;
use crate::pro::recorder::{Record, RecordingInfo};
use crate::pro::Loader;
use std::path::PathBuf;
use std::sync::Arc;

/// UI state of the advanced and inventory pages.
#[derive(Default)]
pub struct ProState {
    pub power: Loader<PowerInfo>,
    pub conns: Loader<Vec<Conn>>,
    pub conns_table: TableState,
    pub conns_view: usize,
    pub apps: Loader<Vec<InstalledApp>>,
    pub apps_table: TableState,
    pub drivers: Loader<Vec<Service>>,
    pub drivers_table: TableState,
    pub volumes: Loader<Vec<Volume>>,
    pub scanner: Scanner,
    pub disk_root: String,
    pub disk_tree: Option<(Arc<Tree>, u64)>,
    pub disk_cursor: u32,
    pub disk_tab: usize,
    /// Files tab sort column; direction in `disk_files_asc` (false = descending).
    pub disk_files_sort: FileCol,
    pub disk_files_asc: bool,
    /// Folder the Files tab listing was requested for (None = nothing requested yet).
    pub disk_files_for: Option<PathBuf>,
    pub disk_files: Loader<Vec<DirFile>>,
    /// Sorted index order of `disk_files.ok()`, valid for the (col, asc) it was built with.
    pub disk_files_order: Option<(FileCol, bool, Vec<usize>)>,
    /// Files tab "Include subfolders": on, and the sorted index order (file indices, biggest size) of the
    /// last finished sort with the key it was requested for (`disk_sub_key`) and the key shown (`disk_sub_ready`).
    pub disk_files_sub: bool,
    pub disk_sub_order: Loader<(Vec<u32>, u64)>,
    pub disk_sub_key: Option<SubKey>,
    pub disk_sub_ready: Option<SubKey>,
    /// Largest-files indices into `Tree::files` for (tree generation, cursor).
    pub disk_largest: Option<(u64, u32, Vec<usize>)>,
    pub disk_confirm_stop: bool,
    /// Deletions applied to the scan tree (part of the tree version, see `disk_epoch`).
    pub disk_edits: u64,
    pub del: super::disk_delete::DiskDeleteUi,
    pub disk_mode: super::disk_cleanup::DiskMode,
    pub clean: super::disk_cleanup::CleanupUi,
    pub recordings: Vec<RecordingInfo>,
    pub recordings_listed: bool,
    pub playback: Option<(PathBuf, Vec<Record>)>,
    pub rec_dir_text: String,
    pub rec_limit: usize,
    pub inv: super::inventory_pages::InventoryState,
    pub cpu_details: Option<crate::pro::inventory::CpuDetails>,
}

/// (scan generation, folder, sort column, ascending) a sorted listing was built for.
pub type SubKey = (u64, u32, FileCol, bool);

impl ProState {
    /// Version of the scan tree shown: scan generation and the deletions applied to it.
    pub fn disk_epoch(&self) -> u64 {
        self.disk_tree.as_ref().map(|(_, g)| *g).unwrap_or(0) << 20 | (self.disk_edits & 0xF_FFFF)
    }
}

/// Column of the Disk Space Files tab (in-memory only, never saved).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileCol {
    Name,
    #[default]
    Size,
    Modified,
}


/// Recording length limits of the Flight Recorder page: label and seconds.
pub(super) const LIMITS: [(&str, Option<u64>); 6] = [("Until stopped", None), ("1 minute", Some(60)), ("5 minutes", Some(300)), ("15 minutes", Some(900)), ("1 hour", Some(3600)), ("8 hours", Some(28800))];
