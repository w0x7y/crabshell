//! Masks and shadows cast by what a subtree draws, both worked out on the
//! layer the subtree is drawn into.

use std::collections::VecDeque;
use std::path::Path;

use crabshell_ui::{Mask, Rect, Shadow, Style};
use tiny_skia::{FillRule, MaskType, Paint, PathBuilder, Pixmap, PixmapMut, Transform};

use super::{LAYER_MARGIN, Painter, mul};

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

  /// Puts `shadow` under what `layer` holds: the layer's alpha, spread by
  /// whole pixels, blurred, moved by whole pixels and tinted. Every step
  /// works out a pixel from its neighbors alone, in integers where it
  /// sums, so every tile that holds a pixel agrees on it.
  pub(super) fn content_shadow(&self, layer: &mut PixmapMut<'_>, shadow: &Shadow) {
    let scale = self.grid.scale;
    let (width, height) = (layer.width() as usize, layer.height() as usize);
    let (dx, dy, _) = self.shadow_reach(shadow);
    let mut alpha: Vec<u8> = layer
      .data_mut()
      .as_chunks::<4>()
      .0
      .iter()
      .map(|pixel| pixel[3])
      .collect();
    let spread = (shadow.spread * scale).round();
    if spread != 0.0 {
      let reach = spread.abs() as usize;
      each_line(&mut alpha, width, height, |line, out| {
        spread_line(line, out, reach, spread > 0.0);
      });
    }
    let blur = Blur::new(shadow.blur.max(0.0) * scale / 2.0);
    if !matches!(blur, Blur::None) {
      each_line(&mut alpha, width, height, |line, out| blur.line(line, out));
    }
    let color = shadow.color;
    let (dx, dy) = (dx as i64, dy as i64);
    let data = layer.data_mut();
    for y in 0..height {
      let from_y = y as i64 - dy;
      if !(0..height as i64).contains(&from_y) {
        continue;
      }
      for x in 0..width {
        let from_x = x as i64 - dx;
        if !(0..width as i64).contains(&from_x) {
          continue;
        }
        let index = (y * width + x) * 4;
        let mut cast = mul(alpha[from_y as usize * width + from_x as usize], color.a);
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
