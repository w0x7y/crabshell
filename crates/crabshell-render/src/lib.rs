//! Draws Crabshell windows on the CPU. This first part plans what each
//! frame repaints: only what changed, in each buffer.
//!
//! The platform keeps a [`DamageHistory`] per window and a [`BufferAge`]
//! per buffer.
//!
//! # How it works
//!
//! - Damage arrives in logical pixels and becomes buffer pixels, rounded
//!   outward. Overlapping areas are merged, a buffer that skipped frames
//!   also repaints what changed in them, and when more than half of the
//!   buffer is dirty, all of it is repainted.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod damage;

#[cfg(test)]
mod tests;

pub use damage::{BufferAge, DamageHistory, PixelRect};
