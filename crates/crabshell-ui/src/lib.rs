//! Crabshell's styles, and the text system that loads fonts through
//! fontconfig and measures text.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod geometry;
mod style;
#[allow(dead_code, reason = "only layout, which comes later, measures text")]
mod text;

#[cfg(test)]
mod tests;

pub use geometry::{Point, Rect, Size};
pub use style::{Border, Color, Font, Padding, Style};
pub use text::TextSystem;
