//! Images drawn by copying their rows, and an image's transition.

use tiny_skia::{Pixmap, PixmapMut};

use super::{over, overlap};

/// Draws the premultiplied `picture` at `left`, `top`: rows that are
/// opaque are copied, others blended over. tiny-skia's `draw_pixmap`
/// gives the same at a whole-pixel offset, through a raster pipeline
/// several times slower, which a 1080p wallpaper notices.
pub(super) fn blit(pixmap: &mut PixmapMut<'_>, (left, top): (i32, i32), picture: &Pixmap) {
  let (width, height) = (picture.width(), picture.height());
  let Some((x0, x1, y0, y1)) = overlap(pixmap, left, top, width, height) else {
    return;
  };
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

/// The byte where column `x`, row `y` of a picture placed at `left`, `top`
/// lands in a pixmap `stride` pixels wide.
fn target(stride: usize, (left, top): (i32, i32), x: usize, y: usize) -> usize {
  ((top as isize + y as isize) as usize * stride + (left as isize + x as isize) as usize) * 4
}

/// Blends the premultiplied `row` over `target`, copying it whole when it
/// is opaque.
fn put_row(target: &mut [u8], row: &[u8]) {
  if row.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255) {
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
