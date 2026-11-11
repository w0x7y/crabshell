//! Crabshell's Wayland platform. This first part has the pieces a window's
//! drawing is decided with, apart from any compositor objects: its scale
//! and buffer size, when it may draw, which of its buffers to draw into,
//! and the outputs windows go on.
//!
//! # How it works
//!
//! - Buffers are sized `logical × scale` rounded halfway away from zero, as
//!   `wp_fractional_scale_v1` says, from a scale in 120ths or a whole one.
//! - A window draws after its first configure, once its last frame was
//!   shown, and only when something changed, so an idle window commits
//!   nothing.
//! - Each window keeps two or three buffers, reusing the free one drawn
//!   most recently.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Drawing and the event loop, which come next, use all of these.
#![allow(dead_code, reason = "drawing isn't written yet")]

mod buffers;
mod outputs;
mod pacing;
mod scale;

#[cfg(test)]
mod tests;

pub use outputs::{Mode, OutputInfo};
