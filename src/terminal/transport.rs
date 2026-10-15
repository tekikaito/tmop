//! Transport interface between a terminal emulator and a remote process.

use std::sync::Arc;

/// Lifecycle of the remote side of a terminal session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportState {
    /// The remote process is being set up.
    Starting,
    /// Bytes flow in both directions.
    Running,
    /// The remote process has ended; the text is shown to the user
    /// (for example "exit code 0" or "container stopped").
    Ended(String),
}

type Feed = Arc<dyn Fn(&[u8]) + Send + Sync>;
type OnChange = Arc<dyn Fn() + Send + Sync>;

/// Where a transport delivers output bytes and change notifications.
#[derive(Clone)]
pub struct Sink {
    feed: Feed,
    on_change: OnChange,
}

impl Sink {
    /// Creates a sink from a byte handler and a change handler.
    pub fn new(
        feed: impl Fn(&[u8]) + Send + Sync + 'static,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self { feed: Arc::new(feed), on_change: Arc::new(on_change) }
    }

    /// Delivers output bytes, then signals the change.
    pub fn feed(&self, bytes: &[u8]) {
        (self.feed)(bytes);
        self.notify();
    }

    /// Signals a change without delivering bytes (state changes).
    pub fn notify(&self) {
        (self.on_change)();
    }
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sink")
    }
}

/// A bidirectional byte channel to a remote process with a TTY.
///
/// Output flows through the [`Sink`] given to the transport at creation;
/// input and control flow through this trait.
pub trait Transport: Send {
    /// Sends input bytes to the remote process (fire and forget).
    fn write(&self, bytes: &[u8]);
    /// Tells the remote side the new terminal size in character cells.
    fn resize(&self, cols: u16, rows: u16);
    /// Current lifecycle state.
    fn state(&self) -> TransportState;
    /// Ends the session; the state becomes `Ended` once the remote side is gone.
    fn close(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[test]
    fn feed_appends_bytes_and_notifies_once() {
        let bytes = Arc::new(Mutex::new(Vec::<u8>::new()));
        let changes = Arc::new(AtomicUsize::new(0));
        let sink = {
            let bytes = Arc::clone(&bytes);
            let changes = Arc::clone(&changes);
            Sink::new(
                move |chunk| bytes.lock().unwrap().extend_from_slice(chunk),
                move || {
                    changes.fetch_add(1, Ordering::SeqCst);
                },
            )
        };
        sink.feed(b"ab");
        assert_eq!(*bytes.lock().unwrap(), b"ab");
        assert_eq!(changes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn notify_signals_without_bytes() {
        let bytes = Arc::new(Mutex::new(Vec::<u8>::new()));
        let changes = Arc::new(AtomicUsize::new(0));
        let sink = {
            let bytes = Arc::clone(&bytes);
            let changes = Arc::clone(&changes);
            Sink::new(
                move |chunk| bytes.lock().unwrap().extend_from_slice(chunk),
                move || {
                    changes.fetch_add(1, Ordering::SeqCst);
                },
            )
        };
        sink.notify();
        assert!(bytes.lock().unwrap().is_empty());
        assert_eq!(changes.load(Ordering::SeqCst), 1);
    }
}
