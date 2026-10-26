//! USB Crash Cart setup: the embedded payload, the install sequence and its window.
#![forbid(unsafe_code)]

#[path = "../../viewer/src/chrome.rs"]
mod chrome;
pub mod install;
mod render;
pub mod system;
pub mod ui;
