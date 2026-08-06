//! macOS communication backend using the portable vsock muxer.

/// macOS communication backend.
pub type MacosCommsBackend = super::muxer::MuxerCommsBackend;

#[cfg(test)]
#[path = "macos_test.rs"]
mod tests;
