//! Masks and shadows cast by what a subtree draws, both worked out on the
//! layer the subtree is drawn into.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::Path;

use crabshell_ui::{Mask, NodeView, Rect, Shadow, Style};
use tiny_skia::{FillRule, MaskType, Paint, PathBuilder, Pixmap, PixmapMut, Transform};

use super::{Grid, LAYER_MARGIN, Painter, mul};

impl Painter<'_> {
  /// Keeps `layer` only where `mask`, laid over `rect`, is opaque, its
  /// alpha multiplying the layer's; nothing shows outside `rect`. A mask
  /// image that can't be drawn leaves the layer as it is, and the image
  /// cache logs that once for its path.
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
        let scale = self.grid.scale;
        let Some(picture) = self.icons.mask(Path::new(path), (side_x, side_y), scale) else {
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

  /// How far, in buffer pixels, `shadow` reaches back from where it is
  /// drawn to the content that casts it: its offset across and down, and
  /// how far its spread and blur reach, plus the layer's margin.
  pub(super) fn shadow_reach(&self, shadow: &Shadow) -> (f32, f32, f32) {
    let scale = self.grid.scale;
    let spread = (shadow.spread * scale).round().abs();
    let blur = Blur::new(shadow.blur.max(0.0) * scale / 2.0).reach() as f32;
    let offset = |by: f32| (by * scale).round();
    (
      offset(shadow.x),
      offset(shadow.y),
      spread + blur + LAYER_MARGIN,
    )
  }

  /// Puts `node`'s content `shadow` under what `layer` holds. The shadow
  /// is cast in cells of a grid laid from its corner, each at most once a
  /// draw and only when a layer needs it, so a draw casts it over the
  /// part it repaints. A cell's layer starts at the same pixel in every
  /// draw, so its content is drawn the same, and every step after works
  /// a pixel out from its neighbors alone, in integers where it sums.
  pub(super) fn content_shadow(
    &mut self,
    layer: &mut PixmapMut<'_>,
    node: NodeView<'_>,
    shadow: &Shadow,
  ) {
    let (dx, dy, reach) = self.shadow_reach(shadow);
    let (dx, dy, reach) = (dx as i64, dy as i64, reach as i64);
    let scale = self.grid.scale;
    let bounds = node.inner_bounds();
    // Where it can show, in this painter's pixels.
    let x0 = (bounds.x * scale).floor() as i64 - 1;
    let y0 = (bounds.y * scale).floor() as i64 - 1;
    let x1 = (bounds.right() * scale).ceil() as i64 + 1;
    let y1 = (bounds.bottom() * scale).ceil() as i64 + 1;
    let (left, top) = (self.grid.left as i64, self.grid.top as i64);
    let (width, height) = (layer.width() as usize, layer.height() as usize);
    let columns = left.max(x0)..(left + width as i64).min(x1);
    let rows = top.max(y0)..(top + height as i64).min(y1);
    if columns.is_empty() || rows.is_empty() {
      return;
    }
    // At least four reaches, so a cell's layer is mostly the cell.
    let side = (4 * reach).max(CELL);
    let path = node.path();
    for row in (rows.start - y0) / side..=(rows.end - 1 - y0) / side {
      for column in (columns.start - x0) / side..=(columns.end - 1 - x0) / side {
        let (cell_x, cell_y) = (x0 + column * side, y0 + row * side);
        let key = (path.to_owned(), column, row);
        if !self.casts.contains_key(&key) {
          let cell_width = side.min(x1 - cell_x) + 2 * reach;
          let cell_height = side.min(y1 - cell_y) + 2 * reach;
          let at = (cell_x - dx - reach, cell_y - dy - reach);
          let Some(cast) = self.cast_cell(node, shadow, at, (cell_width, cell_height)) else {
            continue;
          };
          self.casts.insert(key.clone(), cast);
        }
        let Some(cast) = self.casts.get(&key) else {
          continue;
        };
        let xs = columns.start.max(cell_x)..columns.end.min(cell_x + side);
        let ys = rows.start.max(cell_y)..rows.end.min(cell_y + side);
        blend_cast(layer, (left, top), cast, (xs, ys), (dx, dy), shadow);
      }
    }
  }

  /// `node`'s subtree drawn over `width` by `height` pixels at `left`,
  /// `top`, masked, and its alpha spread and blurred as `shadow` says.
  fn cast_cell(
    &mut self,
    node: NodeView<'_>,
    shadow: &Shadow,
    (left, top): (i64, i64),
    (width, height): (i64, i64),
  ) -> Option<Cast> {
    let mut layer = Pixmap::new(u32::try_from(width).ok()?, u32::try_from(height).ok()?)?;
    self.work.layer_pixels += layer.width() as u64 * layer.height() as u64;
    let mut inner = self.on(Grid {
      left: left as f32,
      top: top as f32,
      ..self.grid
    });
    inner.layer_contents(&mut layer.as_mut(), node, &node.style(), None);
    let (width, height) = (width as usize, height as usize);
    let mut alpha: Vec<u8> = layer
      .data()
      .as_chunks::<4>()
      .0
      .iter()
      .map(|pixel| pixel[3])
      .collect();
    let scale = self.grid.scale;
    let spread = (shadow.spread * scale).round();
    if spread != 0.0 {
      let reach = spread.abs() as usize;
      each_line(&mut alpha, width, height, |line, out| {
        spread_line(line, out, reach, spread > 0.0);
      });
    }
    let blur = Blur::new(shadow.blur.max(0.0) * scale / 2.0);
    self.work.blurred += alpha.len() as u64;
    if !matches!(blur, Blur::None) {
      each_line(&mut alpha, width, height, |line, out| blur.line(line, out));
    }
    Some(Cast {
      left,
      top,
      width,
      alpha,
    })
  }
}

/// The side of a content shadow's cells, in pixels, at least.
const CELL: i64 = 256;

/// A cell of a content shadow: the alpha of what casts it, spread and
/// blurred, in its painter's pixels.
pub(super) struct Cast {
  /// Where `alpha` starts.
  left: i64,
  top: i64,
  /// How many pixels a row of `alpha` holds.
  width: usize,
  alpha: Vec<u8>,
}

/// Puts the shadow `cast` holds under `layer`, whose corner is at
/// `corner`, over the painter's pixels `columns` by `rows`: each pixel's
/// alpha moved by `dx`, `dy` and tinted.
fn blend_cast(
  layer: &mut PixmapMut<'_>,
  (left, top): (i64, i64),
  cast: &Cast,
  (columns, rows): (Range<i64>, Range<i64>),
  (dx, dy): (i64, i64),
  shadow: &Shadow,
) {
  let width = layer.width() as usize;
  let color = shadow.color;
  let data = layer.data_mut();
  for y in rows {
    let from_y = y - dy - cast.top;
    let Some(line) = usize::try_from(from_y).ok().and_then(|from_y| {
      cast
        .alpha
        .get(from_y * cast.width..(from_y + 1) * cast.width)
    }) else {
      continue;
    };
    for x in columns.clone() {
      let Some(&alpha) = usize::try_from(x - dx - cast.left)
        .ok()
        .and_then(|from_x| line.get(from_x))
      else {
        continue;
      };
      let index = ((y - top) as usize * width + (x - left) as usize) * 4;
      let mut cast = mul(alpha, color.a);
      if shadow.cut_out {
        cast = mul(cast, 255 - data[index + 3]);
      }
      if cast == 0 {
        continue;
      }
      // Under what is there: it shows only where the content lets it.
      let rest = 255 - data[index + 3];
      let source = [
        mul(color.r, cast),
        mul(color.g, cast),
        mul(color.b, cast),
        cast,
      ];
      for (channel, value) in data[index..index + 4].iter_mut().zip(source) {
        *channel = channel.saturating_add(mul(value, rest));
      }
    }
  }
}

/// The shadow `style` casts through what it and its children draw, if it
/// casts one that shows.
pub(super) fn cast_by_content(style: &Style) -> Option<Shadow> {
  let shadow = style.shadow?;
  (shadow.casts_from_content() && shadow.color.a > 0).then_some(shadow)
}

/// Runs `op` on every row of `values`, then on every column, each time
/// from a copy of the line into the line itself.
fn each_line(values: &mut [u8], width: usize, height: usize, op: impl Fn(&[u8], &mut [u8])) {
  let mut line = Vec::with_capacity(width.max(height));
  let mut out = vec![0; width.max(height)];
  for row in values.chunks_exact_mut(width) {
    line.clear();
    line.extend_from_slice(row);
    op(&line, &mut out[..width]);
    row.copy_from_slice(&out[..width]);
  }
  for column in 0..width {
    line.clear();
    line.extend((0..height).map(|row| values[row * width + column]));
    op(&line, &mut out[..height]);
    for (row, &value) in out[..height].iter().enumerate() {
      values[row * width + column] = value;
    }
  }
}

/// Each value of `line` as the largest, or with `grow` false the
/// smallest, within `reach` of it, counting past either end as 0.
fn spread_line(line: &[u8], out: &mut [u8], reach: usize, grow: bool) {
  let length = line.len();
  // Indexes whose values could still be the extreme of a later window.
  let mut window: VecDeque<usize> = VecDeque::new();
  let mut next = 0;
  for (index, value) in out.iter_mut().enumerate().take(length) {
    let end = index.saturating_add(reach).min(length - 1);
    while next <= end {
      while window.back().is_some_and(|&back| {
        if grow {
          line[next] >= line[back]
        } else {
          line[next] <= line[back]
        }
      }) {
        window.pop_back();
      }
      window.push_back(next);
      next += 1;
    }
    while window
      .front()
      .is_some_and(|&front| front.saturating_add(reach) < index)
    {
      window.pop_front();
    }
    let past_an_end = index < reach || index.saturating_add(reach) >= length;
    *value = match window.front() {
      Some(_) if !grow && past_an_end => 0,
      Some(&front) => line[front],
      None => 0,
    };
  }
}

/// A Gaussian blur of one line, as SVG's `feGaussianBlur` works it out:
/// below a deviation of 2 pixels by its own weights, and from 2 up by
/// three box blurs, which cost the same whatever the length.
enum Blur {
  /// Too small to show.
  None,
  /// Weights from the farthest left to the farthest right.
  Weights(Vec<f32>),
  /// How far each box reaches left and right.
  Boxes([(usize, usize); 3]),
}

impl Blur {
  /// A blur of deviation `sigma` pixels.
  fn new(sigma: f32) -> Blur {
    // NaN too.
    if sigma.is_nan() || sigma < 0.25 {
      return Blur::None;
    }
    if sigma < 2.0 {
      let reach = (3.0 * sigma).ceil() as i32;
      let weights: Vec<f32> = (-reach..=reach)
        .map(|at| (-(at * at) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
      let total: f32 = weights.iter().sum();
      return Blur::Weights(weights.into_iter().map(|weight| weight / total).collect());
    }
    // `as` saturates, so a blur no layer could hold stays in range.
    let side = (sigma * 3.0 * (2.0 * std::f32::consts::PI).sqrt() / 4.0 + 0.5).floor() as usize;
    let half = side / 2;
    if side % 2 == 1 {
      Blur::Boxes([(half, half); 3])
    } else {
      // Two boxes off the middle by half a pixel each way, then one a
      // pixel longer around it.
      Blur::Boxes([(half, half - 1), (half - 1, half), (half, half)])
    }
  }

  /// How many pixels away a pixel's value reaches.
  fn reach(&self) -> usize {
    match self {
      Blur::None => 0,
      Blur::Weights(weights) => weights.len() / 2,
      Blur::Boxes(boxes) => boxes.iter().fold(0, |sum: usize, &(left, right)| {
        sum.saturating_add(left.max(right))
      }),
    }
  }

  /// `line` blurred into `out`, counting past either end as 0.
  fn line(&self, line: &[u8], out: &mut [u8]) {
    match self {
      Blur::None => out.copy_from_slice(line),
      Blur::Weights(weights) => {
        let reach = weights.len() / 2;
        for (index, value) in out.iter_mut().enumerate() {
          let mut sum = 0.0;
          for (at, weight) in weights.iter().enumerate() {
            if let Some(&near) = (index + at).checked_sub(reach).and_then(|i| line.get(i)) {
              sum += weight * f32::from(near);
            }
          }
          *value = sum.round().min(255.0) as u8;
        }
      }
      Blur::Boxes(boxes) => {
        let mut from = line.to_vec();
        for &(left, right) in boxes {
          box_line(&from, out, left, right);
          from.copy_from_slice(out);
        }
      }
    }
  }
}

/// Each value of `line` as the rounded mean of the box from `left` before
/// it to `right` after it, counting past either end as 0. The sum runs in
/// integers, so it never drifts.
fn box_line(line: &[u8], out: &mut [u8], left: usize, right: usize) {
  let side = (left + right + 1) as u64;
  let at = |index: usize| line.get(index).map_or(0, |&value| u64::from(value));
  let mut sum: u64 = (0..=right).map(at).sum();
  for (index, value) in out.iter_mut().enumerate() {
    *value = ((sum + side / 2) / side) as u8;
    sum += at(index + right + 1);
    if let Some(gone) = index.checked_sub(left) {
      sum -= at(gone);
    }
  }
}
