//! Reusable terminal component: VT emulation, key encoding and the transport
//! abstraction that connects an emulator to a remote process.

pub mod emulator;
pub mod keys;
pub mod transport;

use std::sync::{Arc, Mutex};

pub use emulator::{CellColor, CellInfo, CursorInfo, CursorShape, Emulator, SelectionKind};
pub use keys::{encode_key, encode_paste, encode_text, KeyModes};
pub use transport::{Sink, Transport, TransportState};

/// Sink that feeds output bytes into `emulator`, then calls `on_change`.
pub fn sink_for(
    emulator: &Arc<Mutex<Emulator>>,
    on_change: impl Fn() + Send + Sync + 'static,
) -> Sink {
    let emulator = Arc::clone(emulator);
    Sink::new(
        move |bytes| emulator.lock().unwrap_or_else(|e| e.into_inner()).feed(bytes),
        on_change,
    )
}
