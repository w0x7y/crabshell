//! Draws Crabshell windows on the CPU: backgrounds, rounded corners,
//! borders and opacity with tiny-skia, glyphs from cosmic-text, and icons
//! from SVG, PNG or raw pixels. Only what changed is repainted.
//!
//! The platform keeps one [`Renderer`], a [`DamageHistory`] per window and
//! a [`BufferAge`] per buffer. Each frame it hands the renderer the window's
//! view and damage and a [`Canvas`] over the buffer's memory, and reports
//! the returned rectangles to the compositor:
//!
//! ```
//! use crabshell_reactive::Runtime;
//! use crabshell_render::{BufferAge, Canvas, DamageHistory, PixelFormat, Renderer};
//! use crabshell_ui::prelude::*;
//!
//! let runtime = Runtime::current();
//! let cx = Cx::new(runtime.new_owner());
//! let bar = PanelWindow::new(&Output::new("DP-1"))
//!     .style(Style::new().background(Color::rgb(0x1e, 0x1e, 0x2e)))
//!     .child(Text::new("hello"));
//! let window = Window::new(&cx, bar);
//! let mut text = TextSystem::new();
//! window.set_size(Size::new(200.0, 30.0));
//! runtime.flush();
//! window.layout(&mut text);
//!
//! let mut renderer = Renderer::new();
//! let mut history = DamageHistory::new();
//! let mut age = BufferAge::default();
//! let scale = 1.5;
//! let (width, height) = (300, 45); // 200 x 30 logical pixels at 1.5
//! let mut buffer = vec![0; width * height * 4];
//! let damage = window.take_damage();
//! let report = renderer.draw(
//!     &window.view(),
//!     &damage,
//!     &mut text,
//!     &mut history,
//!     Canvas {
//!         pixels: &mut buffer,
//!         width: width as u32,
//!         height: height as u32,
//!         scale,
//!         format: PixelFormat::Argb8888,
//!         age: &mut age,
//!     },
//! )?;
//! assert_eq!(report.len(), 1); // a new buffer is drawn whole
//! # Ok::<(), crabshell_render::CanvasError>(())
//! ```
//!
//! # How it works
//!
//! - Damage arrives in logical pixels and becomes buffer pixels, rounded
//!   outward. Overlapping areas are merged, a buffer that skipped frames
//!   also repaints what changed in them, and when more than half of the
//!   buffer is dirty, all of it is repainted.
//! - Each area is drawn from scratch into an RGBA pixmap of its own, which
//!   clips the drawing, and copied into the buffer in the buffer's byte
//!   order.
//! - Rectangles snap to whole pixels, so edges stay sharp at fractional
//!   scales. A border is drawn inside the edge. Opacity applies to a widget
//!   and everything inside it as one layer.
//! - Text is drawn from the cosmic-text buffer layout measured, as
//!   grayscale masks, with the baseline on a whole pixel. It isn't clipped:
//!   the window's damage covers text that overflows its widget.
//! - Icons are rasterized at their size in pixels and cached by source and
//!   that size.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod damage;
mod draw;
mod icons;

#[cfg(test)]
mod tests;

pub use damage::{BufferAge, DamageHistory, PixelRect};
pub use draw::{Canvas, CanvasError, PixelFormat, Renderer};
