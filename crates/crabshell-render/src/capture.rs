//! Captured frames: a compositor's buffer turned into the frame a
//! screencopy view keeps, scaled down so a small view of a large output
//! keeps a small frame.

use std::rc::Rc;

use crabshell_ui::{CaptureFrame, Fit};
use tiny_skia::PixmapRef;

use crate::PixelFormat;
use crate::icons::halve;
use crate::images;

/// The frame a view keeps of a `width` by `height` buffer of premultiplied
/// pixels in `format`. With `opaque`, as in `XRGB8888` and `XBGR8888`, the
/// fourth byte is ignored. The frame is scaled down to [`frame_size`] for
/// `cover`. `None` if `pixels` doesn't hold `width * height` pixels.
pub fn capture_frame(
  pixels: &[u8],
  width: u32,
  height: u32,
  format: PixelFormat,
  opaque: bool,
  cover: (u32, u32),
) -> Option<CaptureFrame> {
  let source = PixmapRef::from_bytes(pixels, width, height)?;
  let (kept_width, kept_height) = frame_size((width, height), cover);
  // Halving averages, so the fourth byte can be filled in after it, in
  // a quarter of the pixels; bicubic scaling would clamp to it first.
  let mut first = if kept_width * 2 <= width && kept_height * 2 <= height {
    halve(source).ok()?
  } else {
    source.to_owned()
  };
  if opaque {
    for pixel in first.data_mut().as_chunks_mut::<4>().0 {
      pixel[3] = 255;
    }
  }
  // Scaling treats the four channels alike, so the pixels are swapped
  // into RGBA only once they are few.
  let mut kept = if (first.width(), first.height()) == (kept_width, kept_height) {
    first
  } else {
    images::scale(first.as_ref(), kept_width, kept_height, Fit::Fill).ok()?
  };
  if format == PixelFormat::Argb8888 {
    for pixel in kept.data_mut().as_chunks_mut::<4>().0 {
      pixel.swap(0, 2);
    }
  }
  Some(CaptureFrame {
    width: kept_width,
    height: kept_height,
    rgba: Rc::from(kept.take()),
  })
}

/// The size a `source` sized frame is kept at to show in a box of
/// `cover` pixels: scaled down, keeping its shape, to the smallest size
/// that still covers the box, so any fit draws it without scaling up. It
/// is never scaled up, and keeps at least one pixel a side.
pub fn frame_size(source: (u32, u32), cover: (u32, u32)) -> (u32, u32) {
  let across = f64::from(cover.0) / f64::from(source.0.max(1));
  let down = f64::from(cover.1) / f64::from(source.1.max(1));
  let factor = across.max(down).min(1.0);
  // `as` saturates, and the factor is at most 1, so this stays in range.
  let side = |length: u32| ((f64::from(length) * factor).round() as u32).max(1);
  (side(source.0), side(source.1))
}
