//! Crabshell's colors, fonts and styles, and the geometry types layout uses.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod geometry;
mod style;

#[cfg(test)]
mod tests;

pub use geometry::{Point, Rect, Size};
pub use style::{Border, Color, Font, Padding, Style};
