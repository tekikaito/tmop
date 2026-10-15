//! Docker support: host contexts, plain data types shared with the UI, and the
//! engine connection factory.
//!
//! * [`context`] reads the Docker CLI configuration (`~/.docker`) and lists the
//!   hosts that can be used;
//! * [`model`] holds engine data as plain Rust types, so no `bollard` type leaks
//!   into the UI;
//! * [`client`] turns an [`context::Endpoint`] into a connected `bollard::Docker`.

pub mod client;
pub mod context;
pub mod model;

/// Maximum number of containers that keep their own stats history.
pub const MAX_TRACKED: usize = 50;
