//! Cross-platform interrupt event abstraction.
//!
//! On Linux this wraps `EventFd` (eventfd2 syscall).
//! On macOS this would use `kqueue` with `EVFILT_USER`.
//! On Windows this would use `CreateEvent`.

/// Raw OS handle for interrupt events.
///
/// On Unix this is a file descriptor (`RawFd`).
/// On Windows this would be a `RawHandle`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub type RawEventHandle = std::os::fd::RawFd;

#[cfg(target_os = "windows")]
pub type RawEventHandle = std::os::windows::io::RawHandle;

/// Cross-platform interrupt event abstraction.
///
/// On Linux this wraps `EventFd` (eventfd2 syscall).
/// On macOS this would use `kqueue` with `EVFILT_USER`.
/// On Windows this would use `CreateEvent`.
pub trait InterruptEvent: Send + Sync {
    /// Triggers the interrupt event (writes 1 to eventfd on Linux).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the trigger fails.
    fn trigger(&self) -> Result<(), std::io::Error>;

    /// Returns the raw OS handle for use with KVM irqfd / kqueue / etc.
    fn as_raw(&self) -> RawEventHandle;
}

/// Mock interrupt event for testing — records trigger count, no OS resources.
#[cfg(test)]
pub struct MockInterruptEvent {
    /// Number of times [`InterruptEvent::trigger`] has been called.
    pub trigger_count: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
impl MockInterruptEvent {
    /// Creates a new mock with zero trigger count.
    #[must_use]
    pub fn new() -> Self {
        Self {
            trigger_count: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

#[cfg(test)]
impl InterruptEvent for MockInterruptEvent {
    fn trigger(&self) -> Result<(), std::io::Error> {
        self.trigger_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn as_raw(&self) -> RawEventHandle {
        -1 // Sentinel value — no real OS resource.
    }
}
#[cfg(test)]
#[path = "event_test.rs"]
mod tests;
