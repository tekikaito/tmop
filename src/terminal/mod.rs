//! Reusable terminal component: VT emulation, key encoding and the transport
//! abstraction that connects an emulator to a remote process.

pub mod transport;

pub use transport::{Sink, Transport, TransportState};
