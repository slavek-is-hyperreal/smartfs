//! smartfs-daemon — FUSE daemon entrypoint (`smartfsd`).
//!
//! Provides CLI argument parsing, startup gates, and life-cycle management
//! for running SmartFS as a live FUSE mount.

pub mod args;
pub mod exit;
pub mod startup;

pub use args::DaemonArgs;
