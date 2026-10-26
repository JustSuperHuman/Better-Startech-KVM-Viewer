//! Platform-independent packet, input, paste, geometry, and frame validation.
//!
//! Packet encodings follow the approved bridge contract; they are not a claim
//! that keyboard or mouse behavior has been validated on physical hardware.

#![forbid(unsafe_code)]

pub mod geometry;
pub mod input;
pub mod ipc;
pub mod pacing;
pub mod paste;
pub mod protocol;
