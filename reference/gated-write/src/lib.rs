//! `gated-write` plugin library crate — the confirmed write executor lives
//! in [`handler`]; the plugin wiring (confirmation gate + manifest + main)
//! is in `main.rs`.
pub mod handler;
