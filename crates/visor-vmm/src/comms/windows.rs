//! Windows communication backend stub.
//!
//! This is a placeholder implementation. All methods return
//! [`CommsError::Unsupported`] until Windows Hyper-V socket support
//! is implemented.

use super::backend::{AsyncStream, CommsBackend, CommsError};

/// Windows communication backend (stub).
pub struct WindowsCommsBackend;

impl CommsBackend for WindowsCommsBackend {
    async fn connect(&self, _cid: u32, _port: u32) -> Result<Box<dyn AsyncStream>, CommsError> {
        Err(CommsError::Unsupported)
    }
}

#[cfg(test)]
#[path = "windows_test.rs"]
mod tests;
