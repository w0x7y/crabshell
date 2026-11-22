//! An image's transition: the picture it leaves and the one coming in,
//! blended pixel by pixel as its kind says. Everything is worked out in
//! the picture's own buffer pixels, so every tile draws the same.

use crabshell_ui::ImageTransition;
use tiny_skia::{Pixmap, PixmapMut};

use super::{over, overlap};

/// Draws the premultiplied `picture` at `left`, `top`: rows that are
/// opaque are copied, others blended over. tiny-skia's `draw_pixmap`
/// gives the same at a whole-pixel offset, through a raster pipeline
/// several times slower, which a 1080p wallpaper notices.
pub(super) fn blit(pixmap: &mut PixmapMut<'_>, at: (i32, i32), picture: &Pixmap) {
  let whole = [0, 0, pixmap.width(), pixmap.height()];
  blit_within(pixmap, at, picture, whole);
}

/// [`blit`] of only what lands within `area` of the pixmap: its left,
/// top, right and bottom pixel edges.
pub(super) fn blit_within(
  pixmap: &mut PixmapMut<'_>,
  (left, top): (i32, i32),
  picture: &Pixmap,
  [area_left, area_top, area_right, area_bottom]: [u32; 4],
) {
  let (width, height) = (picture.width(), picture.height());
  let Some((x0, x1, y0, y1)) = overlap(pixmap, left, top, width, height) else {
    return;
  };
  // The picture's pixels that land in the area.
  let from = |edge: u32, at: i32| (i64::from(edge) - i64::from(at)).max(0) as usize;
  let (x0, x1) = (
    x0.max(from(area_left, left)),
    x1.min(from(area_right, left)),
  );
  let (y0, y1) = (y0.max(from(area_top, top)), y1.min(from(area_bottom, top)));
  if x0 >= x1 || y0 >= y1 {
    return;
  }
  let stride = pixmap.width() as usize;
  let data = pixmap.data_mut();
  let source = picture.data();
  for y in y0..y1 {
    let from = (y * width as usize + x0) * 4;
    let row = &source[from..from + (x1 - x0) * 4];
    let at = target(stride, (left, top), x0, y);
    put_row(&mut data[at..at + row.len()], row);
  }
}

/// Draws `to` coming in over `from`, both placed at `left`, `top`, `t` of
/// the way, as `kind` says. A `from` of `None`, or of another size, comes
/// in from nothing.
pub(super) fn draw(
  pixmap: &mut PixmapMut<'_>,
  (left, top): (i32, i32),
  from: Option<&Pixmap>,
  to: &Pixmap,
  t: f32,
  kind: ImageTransition,
) {
  let (width, height) = (to.width(), to.height());
  let Some((x0, x1, y0, y1)) = overlap(pixmap, left, top, width, height) else {
    return;
  };
  let from = from
    .filter(|from| (from.width(), from.height()) == (width, height))
    .map(Pixmap::data);
  let to = to.data();
  let cover = Cover::new(kind, t, width as f32, height as f32);
  let stride = pixmap.width() as usize;
  let data = pixmap.data_mut();
  let columns = x1 - x0;
  let clear = vec![0; columns * 4];
  let (mut old, mut new) = (vec![0; columns * 4], vec![0; columns * 4]);
  let mut amounts = vec![0; columns];
  let mut out = vec![0; columns * 4];
  for y in y0..y1 {
    // The pixels each picture shows in this row, gathered into blocks
    // when pixelating.
    let (old, new): (&[u8], &[u8]) = match cover {
      Cover::Blocks { block, .. } => {
        let row = block_middle(y, block, height as usize) * width as usize;
        let mut x = x0;
        while x < x1 {
          let end = ((x / block + 1) * block).min(x1);
          let index = (row + block_middle(x, block, width as usize)) * 4;
          let span = (x - x0) * 4..(end - x0) * 4;
          for (old, new) in old[span.clone()]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(new[span].as_chunks_mut::<4>().0)
          {
            *new = pixel(to, index);
            *old = from.map_or([0; 4], |from| pixel(from, index));
          }
          x = end;
        }
        (&old, &new)
      }
      _ => {
        let start = (y * width as usize + x0) * 4;
        let span = start..start + columns * 4;
        (
          from.map_or(&clear[..], |from| &from[span.clone()]),
          &to[span],
        )
      }
    };
    match cover.amount(y) {
      Some(amount) => lerp_row(&mut out, old, new, amount),
      None => {
        for (x, amount) in (x0..x1).zip(&mut amounts) {
          *amount = cover.at(x, y);
        }
        for (((out, old), new), &amount) in out
          .as_chunks_mut::<4>()
          .0
          .iter_mut()
          .zip(old.as_chunks::<4>().0)
          .zip(new.as_chunks::<4>().0)
          .zip(&amounts)
        {
          *out = match amount {
            0 => *old,
            255 => *new,
            _ => std::array::from_fn(|i| lerp(old[i], new[i], amount)),
          };
        }
      }
    }
    let at = target(stride, (left, top), x0, y);
    put_row(&mut data[at..at + out.len()], &out);
  }
}

/// The byte where column `x`, row `y` of a picture placed at `left`, `top`
/// lands in a pixmap `stride` pixels wide.
fn target(stride: usize, (left, top): (i32, i32), x: usize, y: usize) -> usize {
  ((top as isize + y as isize) as usize * stride + (left as isize + x as isize) as usize) * 4
}

/// Blends the premultiplied `row` over `target`, copying it whole when it
/// is opaque.
fn put_row(target: &mut [u8], row: &[u8]) {
  // Every alpha byte ANDed together, without stopping early, which
  // vectorizes: a 1080p picture's rows took a millisecond the other way.
  let alpha = row
    .as_chunks::<4>()
    .0
    .iter()
    .fold(u32::MAX, |all, pixel| all & u32::from_le_bytes(*pixel));
  if alpha >> 24 == 255 {
    target.copy_from_slice(row);
    return;
  }
  for (index, pixel) in row.as_chunks::<4>().0.iter().enumerate() {
    match pixel[3] {
      0 => {}
      255 => target[index * 4..index * 4 + 4].copy_from_slice(pixel),
      _ => over(target, index * 4, *pixel),
    }
  }
}

/// `out` as `amount`/255 of the way from `old` to `new`, byte by byte.
fn lerp_row(out: &mut [u8], old: &[u8], new: &[u8], amount: u8) {
  match amount {
    0 => out.copy_from_slice(old),
    255 => out.copy_from_slice(new),
    _ => {
      for ((out, &old), &new) in out.iter_mut().zip(old).zip(new) {
        *out = lerp(old, new, amount);
      }
    }
  }
}

/// The middle pixel of the `block` wide block holding pixel `at`, within
/// `end`.
fn block_middle(at: usize, block: usize, end: usize) -> usize {
  (at / block * block + block / 2).min(end - 1)
}

fn pixel(data: &[u8], index: usize) -> [u8; 4] {
  [
    data[index],
    data[index + 1],
    data[index + 2],
    data[index + 3],
  ]
}

/// `amount`/255 of the way from `old` to `new`, premultiplied channels.
fn lerp(old: u8, new: u8, amount: u8) -> u8 {
  ((u16::from(old) * u16::from(255 - amount) + u16::from(new) * u16::from(amount) + 127) / 255)
    as u8
}

/// How much of the new picture each pixel shows, and which pixel of the
/// pictures it shows.
enum Cover {
  /// The same amount everywhere.
  All(u8),
  /// New behind an edge: a pixel whose center lies `along` the direction
  /// less than `edge` is new, antialiased over a pixel.
  Wipe {
    along: (f32, f32),
    edge: f32,
    middle: (f32, f32),
  },
  /// New inside a circle around the middle.
  Circle { radius: f32, middle: (f32, f32) },
  /// Blocks of `block` pixels, each the color of its middle pixel, the
  /// new picture `amount` of the way in everywhere.
  Blocks { block: usize, amount: u8 },
}

impl Cover {
  fn new(kind: ImageTransition, t: f32, width: f32, height: f32) -> Cover {
    let t = if t.is_nan() { 1.0 } else { t };
    let middle = (width / 2.0, height / 2.0);
    match kind {
      ImageTransition::None | ImageTransition::Crossfade(_) => Cover::All(amount(t)),
      ImageTransition::Wipe(_, angle) => {
        let angle = if angle.is_finite() { angle } else { 90.0 };
        let (sin, cos) = angle.to_radians().sin_cos();
        // Half the box's length along the direction: the edge runs from
        // one end to the other.
        let half = (width * sin.abs() + height * cos.abs()) / 2.0;
        Cover::Wipe {
          along: (sin, -cos),
          edge: -half + t.clamp(0.0, 1.0) * 2.0 * half,
          middle,
        }
      }
      ImageTransition::Circle(_) => {
        // From nothing at 0 to past the corners at 1.
        let reach = width.hypot(height) / 2.0 + 1.0;
        Cover::Circle {
          radius: t.clamp(0.0, 1.0) * reach - 0.5,
          middle,
        }
      }
      ImageTransition::Pixelate(_) => {
        let t = t.clamp(0.0, 1.0);
        // Blocks grow to a twentieth of the longer side halfway, then
        // shrink; the pictures swap through the middle half.
        let largest = (width.max(height) / 20.0).round().max(1.0);
        let grown = 1.0 - (2.0 * t - 1.0).abs();
        Cover::Blocks {
          block: (1.0 + grown * (largest - 1.0)).round() as usize,
          amount: amount((t - 0.25) * 2.0),
        }
      }
    }
  }

  /// The amount of the new picture every pixel of row `y` shows, out
  /// of 255, when it is the same across the row.
  fn amount(&self, y: usize) -> Option<u8> {
    match *self {
      Cover::All(amount) | Cover::Blocks { amount, .. } => Some(amount),
      Cover::Wipe { along, .. } if along.0.abs() < 1e-6 => Some(self.at(0, y)),
      _ => None,
    }
  }

  /// How much of the new picture pixel `x`, `y` shows, out of 255.
  fn at(&self, x: usize, y: usize) -> u8 {
    let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
    match *self {
      Cover::All(amount) | Cover::Blocks { amount, .. } => amount,
      Cover::Wipe {
        along,
        edge,
        middle,
      } => amount(edge - ((cx - middle.0) * along.0 + (cy - middle.1) * along.1) + 0.5),
      Cover::Circle { radius, middle } => {
        let squared = (cx - middle.0).powi(2) + (cy - middle.1).powi(2);
        // Inside or outside by more than half a pixel needs no root.
        if radius > 0.5 && squared <= (radius - 0.5).powi(2) {
          255
        } else if squared >= (radius + 0.5).max(0.0).powi(2) {
          0
        } else {
          amount(radius - squared.sqrt() + 0.5)
        }
      }
    }
  }
}

/// `fraction` of 255, kept within 0 and 255, rounded half up.
fn amount(fraction: f32) -> u8 {
  if fraction >= 1.0 {
    255
  } else if fraction > 0.0 {
    // Truncating after adding a half rounds without a call to `round`.
    (fraction * 255.0 + 0.5) as u8
  } else {
    0
  }
}
