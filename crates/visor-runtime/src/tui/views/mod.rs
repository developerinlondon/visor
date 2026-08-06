//! TUI view rendering functions.
//!
//! Each view is a standalone module that renders into a ratatui [`Frame`].
//! Views are stateless — they read from [`App`] and produce widgets.

pub mod confirm;
pub mod create_vm;
pub mod dashboard;
pub mod vm_detail;
