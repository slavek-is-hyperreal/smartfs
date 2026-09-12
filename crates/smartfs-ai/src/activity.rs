use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// @id: 8f234567-cdef-4012-8345-6789abcdef01
/// Activity monitor that detects file write activity and provides debouncing
/// for the background embedding worker (§3.7, ADR-42, FIX-05).
#[derive(Debug)]
pub struct ActivityMonitor {
    last_write: RwLock<Option<Instant>>,
    write_tx: broadcast::Sender<()>,
}

impl ActivityMonitor {
    /// @id: 9a345678-def0-5123-9456-789abcdef012
    /// Create a new activity monitor.
    pub fn new() -> Arc<Self> {
        let (write_tx, _) = broadcast::channel(64);
        Arc::new(Self {
            last_write: RwLock::new(None),
            write_tx,
        })
    }

    /// @id: ab456789-ef01-6234-a567-89abcdef0123
    /// Notify the monitor that a file write or modification has occurred.
    pub fn notify_write(&self) {
        if let Ok(mut lock) = self.last_write.write() {
            *lock = Some(Instant::now());
        }
        let _ = self.write_tx.send(());
    }

    /// @id: bc56789a-f012-7345-b678-9abcdef01234
    /// Check the elapsed duration since the last detected write activity.
    pub fn time_since_last_write(&self) -> Duration {
        if let Ok(lock) = self.last_write.read() {
            if let Some(instant) = *lock {
                return instant.elapsed();
            }
        }
        Duration::from_secs(3600) // No writes yet, completely idle
    }

    /// @id: cd6789ab-0123-8456-c789-abcdef012345
    /// Wait until the file system has been idle (no writes) for at least `debounce_duration`.
    pub async fn wait_for_idle(&self, debounce_duration: Duration) {
        loop {
            let elapsed = self.time_since_last_write();
            if elapsed >= debounce_duration {
                return;
            }
            let wait = debounce_duration.saturating_sub(elapsed);
            let mut rx = self.write_tx.subscribe();
            tokio::select! {
                _ = tokio::time::sleep(wait) => {
                    if self.time_since_last_write() >= debounce_duration {
                        return;
                    }
                }
                _ = rx.recv() => {
                    // Reset debounce wait on new write
                }
            }
        }
    }

    /// @id: de789abc-1234-9567-d89a-bcdef0123456
    /// Asynchronous notification that resolves when a new write activity is detected.
    pub async fn write_activity_detected(&self) {
        let mut rx = self.write_tx.subscribe();
        let _ = rx.recv().await;
    }
}

impl Default for ActivityMonitor {
    fn default() -> Self {
        let (write_tx, _) = broadcast::channel(64);
        Self {
            last_write: RwLock::new(None),
            write_tx,
        }
    }
}
