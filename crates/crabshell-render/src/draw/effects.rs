//! Masks, worked out on the layer a subtree is drawn into.

use std::path::Path;

use crabshell_ui::{Fit, ImageFilters, ImageView, Mask, Rect};
use tiny_skia::{FillRule, MaskType, Paint, PathBuilder, Pixmap, PixmapMut, Transform};

use super::Painter;

/// The id every mask image is drawn under. They never play, so they share
/// it; image ids count up from 0.
const MASK_IMAGE: u64 = u64::MAX;

impl Painter<'_> {
  /// Keeps `layer` only where `mask`, laid over `rect`, is opaque, its
  /// alpha multiplying the layer's; nothing shows outside `rect`. A mask
  /// image that can't be drawn leaves the layer as it is, and the image
  /// cache logs that once.
  pub(super) fn mask(&mut self, layer: &mut PixmapMut<'_>, rect: Rect, mask: &Mask) {
    let (width, height) = (layer.width(), layer.height());
    let Some(area) = self.grid.rect(rect) else {
      layer.fill(tiny_skia::Color::TRANSPARENT);
      return;
    };
    let Some(mut cut) = tiny_skia::Mask::new(width, height) else {
      return;
    };
    match mask {
      Mask::Ellipse => {
        let Some(oval) = PathBuilder::from_oval(area) else {
          return;
        };
        cut.fill_path(&oval, FillRule::Winding, true, Transform::identity());
      }
      Mask::Gradient(gradient) => {
        let (Some(shader), Some(mut painted)) =
          (self.shader(gradient, area), Pixmap::new(width, height))
        else {
          return;
        };
        let paint = Paint {
          shader,
          ..Paint::default()
        };
        painted.fill_rect(area, &paint, Transform::identity(), None);
        cut = tiny_skia::Mask::from_pixmap(painted.as_ref(), MaskType::Alpha);
      }
      Mask::Image(path) => {
        // The box's edges are whole pixels of the layer.
        let (left, top) = (area.left() as i64, area.top() as i64);
        let (side_x, side_y) = (area.width() as u32, area.height() as u32);
        // Unfiltered and paused, so a GIF stays on its first frame and
        // never wakes the loop.
        let image = ImageView {
          id: MASK_IMAGE,
          path: Path::new(path),
          rect,
          fit: Fit::Fill,
          filters: ImageFilters::default(),
          playing: false,
        };
        let scale = self.grid.scale;
        let Some(picture) = self.icons.image(&image, (side_x, side_y), scale) else {
          return;
        };
        let stride = width as i64;
        let data = cut.data_mut();
        for (row, line) in picture.data().chunks_exact(side_x as usize * 4).enumerate() {
          let y = top + row as i64;
          if !(0..i64::from(height)).contains(&y) {
            continue;
          }
          for (column, pixel) in line.as_chunks::<4>().0.iter().enumerate() {
            let x = left + column as i64;
            if (0..stride).contains(&x) {
              data[(y * stride + x) as usize] = pixel[3];
            }
          }
        }
      }
    }
    layer.apply_mask(&cut);
  }
}
