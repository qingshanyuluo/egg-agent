//! egg-agent — a minimal OpenAI-compatible SWE agent in your terminal.
//!
//! This lib crate exists so integration tests in `tests/` can import and
//! exercise the public API. The binary entry-point lives in `main.rs`.

pub mod agent;
pub mod app;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod editor;
pub mod file_search;
pub mod gfx;
pub mod llm;
pub mod memory;
pub mod plugin;
pub mod session;
pub mod tools;
pub mod types;
pub mod ui;
