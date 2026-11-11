//! Crabshell's Wayland platform. This part draws a window into its layer
//! surface's shared-memory buffers, at the compositor's scale and pace,
//! and describes the outputs windows go on; the event loop comes next.
//!
//! # How it works
//!
//! - A layer surface's settings come from the window's description;
//!   settings layer-shell would refuse, such as a width of 0 without both
//!   side anchors, are caught before they are sent.
//! - Each window has two or three ARGB8888 buffers from sctk's `SlotPool`,
//!   reused once released, sized `logical × scale` rounded halfway away
//!   from zero as `wp_fractional_scale_v1` says, with a damage
//!   history per window and an age per buffer. Every successful draw is
//!   committed, reporting its rectangles with `damage_buffer`.
//! - Each commit asks for a frame callback. A window draws only once it
//!   arrives and something changed; with nothing changed nothing is
//!   committed, so an idle window gets no events.
//! - Scale comes from `wp_fractional_scale_v1` with `wp_viewporter` when
//!   the compositor has both, otherwise from whole scales and
//!   `set_buffer_scale`. A new scale repaints the whole window.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// The event loop, which comes next, uses all of these.
#![allow(dead_code, reason = "the event loop isn't written yet")]

mod buffers;
mod outputs;
mod pacing;
mod scale;
mod surface;

#[cfg(test)]
mod tests;

pub use outputs::{Mode, OutputInfo};
