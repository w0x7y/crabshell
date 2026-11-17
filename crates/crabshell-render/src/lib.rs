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
//! Damage arrives in logical pixels and becomes buffer pixels, rounded
//! outward. A buffer that skipped frames also repaints what changed in
//! them, and one more than half dirty repaints whole. Damaged areas are
//! drawn in fixed 64-pixel tiles, so a repaint matches a full draw. Edges
//! snap to whole pixels, text is drawn as grayscale masks, and icons are
//! cached by source and size. [`capture_frame`] and [`quantize_colors`]
//! serve screencopy views and wallpaper themes. The design doc's
//! [Rendering](https://github.com/w0x7y/crabshell/blob/main/docs/design.md#rendering)
//! section has the full rules.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod animation;
mod capture;
mod colors;
mod damage;
mod draw;
mod filters;
mod icons;
mod images;
mod theme;
mod worker;

#[cfg(test)]
mod tests;

pub use capture::{Turn, capture_frame, frame_size};
pub use colors::quantize_colors;
pub use damage::{BufferAge, DamageHistory, PixelRect};
pub use draw::{Canvas, CanvasError, PixelFormat, Renderer};
pub use icons::DEFAULT_THEME as DEFAULT_ICON_THEME;
