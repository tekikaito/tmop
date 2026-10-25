//! Process backend for targets without one: an empty list and actions that report so. The
//! Linux and macOS backends replace it with the same API.

use super::{Priority, ProcessRow, Totals};
use std::collections::HashMap;

fn unsupported() -> String {
    format!("the process list is not implemented on {}", crate::platform::os_name())
}

#[derive(Default)]
pub struct Enumerator;

impl Enumerator {
    pub fn new() -> Self {
        Self
    }

    pub fn collect(&mut self, _logical_cpus: usize, _gpu_by_pid: &HashMap<u32, f32>) -> Result<(Vec<ProcessRow>, Totals), String> {
        Err(unsupported())
    }
}

/// The action primitives [`super::actions::run`] drives.
pub(super) mod sys {
    use super::*;

    pub fn terminate(_pid: u32) -> Result<(), String> {
        Err(unsupported())
    }

    pub fn suspend(_pid: u32) -> Result<(), String> {
        Err(unsupported())
    }

    pub fn resume(_pid: u32) -> Result<(), String> {
        Err(unsupported())
    }

    pub fn set_priority(_pid: u32, _p: Priority) -> Result<(), String> {
        Err(unsupported())
    }

    pub fn set_efficiency(_pid: u32, _on: bool) -> Result<(), String> {
        Err(unsupported())
    }

    /// Selects the image in the file manager.
    pub fn open_location(path: &str) -> Result<(), String> {
        crate::platform::reveal(std::path::Path::new(path))
    }
}
