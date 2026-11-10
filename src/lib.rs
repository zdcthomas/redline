//! redline core: the event log, the anchors, the CLI and the TUI. Front
//! ends in other crates (the native app) use `store`, `anchor` and `ops`.

pub mod anchor;
pub mod cli;
pub mod config;
pub mod ops;
pub mod store;
pub mod tui;
