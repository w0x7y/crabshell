//! Captured frames: a compositor's buffer turned into the frame a
//! screencopy view keeps, scaled down so a small view of a large output
//! keeps a small frame.

use std::rc::Rc;

use crabshell_ui::{CaptureFrame, Fit};
use tiny_skia::PixmapRef;

use crate::PixelFormat;
use crate::icons::halve;
use crate::images;

/// How the compositor turned a captured buffer, as `wl_output.transform`
/// says: rotated counter-clockwise by a quarter turn or more, and flipped
/// around the vertical axis before that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Turn {
  /// Quarter turns counter-clockwise, 0 to 3.
  pub quarters: u8,
  /// Flipped around the vertical axis first.
  pub flipped: bool,
}

impl Turn {
  /// Whether the buffer's sides are swapped against what shows.
  fn sideways(self) -> bool {
    self.quarters % 2 == 1
  }
}

/// The frame a view keeps of a `width` by `height` buffer of premultiplied
/// pixels in `format`, which the compositor `turn`ed. With `opaque`, as in
/// `XRGB8888` and `XBGR8888`, the fourth byte is ignored. The frame is
/// turned back upright and scaled down to [`frame_size`] for `cover`.
/// `None` if `pixels` doesn't hold `width * height` pixels.
pub fn capture_frame(
  pixels: &[u8],
  width: u32,
  height: u32,
  format: PixelFormat,
  opaque: bool,
  turn: Turn,
  cover: (u32, u32),
) -> Option<CaptureFrame> {
  let source = PixmapRef::from_bytes(pixels, width, height)?;
  // Scaled while still turned, so the box is turned to match.
  let cover = if turn.sideways() {
    (cover.1, cover.0)
  } else {
    cover
  };
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
  let (width, height, rgba) = upright(kept_width, kept_height, kept.take(), turn);
  Some(CaptureFrame {
    width,
    height,
    rgba: Rc::from(rgba),
  })
}

/// A `width` by `height` image the compositor `turn`ed, turned back.
fn upright(width: u32, height: u32, rgba: Vec<u8>, turn: Turn) -> (u32, u32, Vec<u8>) {
  if turn == Turn::default() {
    return (width, height, rgba);
  }
  let (w, h) = (width as usize, height as usize);
  let (out_w, out_h) = if turn.sideways() { (h, w) } else { (w, h) };
  let mut out = vec![0; rgba.len()];
  for y in 0..out_h {
    for x in 0..out_w {
      // Where the shown pixel is in the buffer: undoing the turn is the
      // turn's inverse, then the flip, as the protocol applies the flip
      // first.
      let (bx, by) = match turn.quarters % 4 {
        0 => (x, y),
        1 => (y, h - 1 - x),
        2 => (w - 1 - x, h - 1 - y),
        _ => (w - 1 - y, x),
      };
      let bx = if turn.flipped { w - 1 - bx } else { bx };
      let (from, to) = ((by * w + bx) * 4, (y * out_w + x) * 4);
      out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
    }
  }
  (out_w as u32, out_h as u32, out)
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
