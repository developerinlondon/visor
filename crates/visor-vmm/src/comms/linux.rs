//! Linux communication backend using the portable vsock muxer.

/// Linux communication backend.
///
/// Linux runs the same userspace vsock muxer protocol as other platforms so
/// the runtime speaks to the VMM through a stable trait boundary instead of
/// depending on kernel `AF_VSOCK` integration details.
pub type LinuxCommsBackend = super::muxer::MuxerCommsBackend;

#[cfg(test)]
#[path = "linux_test.rs"]
mod tests;
