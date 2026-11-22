//! Masks and shadows cast by what a subtree draws, both worked out on the
//! layer the subtree is drawn into.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::Path;

use crabshell_ui::{Mask, NodeView, Rect, Region, RegionOp, RegionShape, Shadow, Style};
use tiny_skia::{FillRule, MaskType, Paint, PathBuilder, Pixmap, PixmapMut, Transform};

use super::{Corners, Grid, LAYER_MARGIN, Painter, fit, mul, rounded_rect, straight_inside};

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

  /// How much of a `width` by `height` layer `region`, laid out from
  /// `rect`'s top-left corner, and `rect` itself cover: each shape filled
  /// antialiased, then joined to the coverage so far. A shape that misses
  /// the layer or covers all of it isn't filled, so most tiles of a large
  /// region cost no mask.
  pub(super) fn region_cover(&self, width: u32, height: u32, rect: Rect, region: &Region) -> Cover {
    let in_box = self.grid.rect(rect).map_or(Cover::Clear, |area| {
      shape_cover(width, height, area, Some([0.0; 4]))
    });
    if matches!(in_box, Cover::Clear) {
      return Cover::Clear;
    }
    join(
      in_box,
      self.shapes_cover(width, height, rect, region),
      RegionOp::Intersect,
    )
  }

  /// [`region_cover`](Self::region_cover) without the box.
  fn shapes_cover(&self, width: u32, height: u32, rect: Rect, region: &Region) -> Cover {
    let area = |[left, top, right, bottom]: [i32; 4]| {
      let (left, top) = (rect.x + left as f32, rect.y + top as f32);
      let (right, bottom) = (rect.x + right as f32, rect.y + bottom as f32);
      self
        .grid
        .rect(Rect::new(left, top, right - left, bottom - top))
    };
    let mut cover = match region.shape() {
      RegionShape::Empty => Cover::Clear,
      RegionShape::Rect(bounds, radii) => area(bounds).map_or(Cover::Clear, |area| {
        let corners = fit(radii.map(|radius| radius * self.grid.scale), area);
        shape_cover(width, height, area, Some(corners))
      }),
      RegionShape::Ellipse(bounds) => {
        area(bounds).map_or(Cover::Clear, |area| shape_cover(width, height, area, None))
      }
    };
    for (op, other) in region.joined() {
      cover = join(cover, self.shapes_cover(width, height, rect, other), op);
    }
    cover
  }

  /// How far, in buffer pixels, `shadow` reaches back from where it is
  /// drawn to the content that casts it: its offset across and down, and
  /// how far its spread and blur reach, plus the layer's margin.
  pub(super) fn shadow_reach(&self, shadow: &Shadow) -> (f32, f32, f32) {
    let scale = self.grid.scale;
    let spread = (shadow.spread * scale).round().abs();
    let blur = Reduced::new(shadow.blur.max(0.0) * scale).reach() as f32;
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
    inner.layer_contents(&mut layer.as_mut(), node, &node.style(), None, None);
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
    let blur = Reduced::new(shadow.blur.max(0.0) * scale);
    let layer = (left, top, width, height);
    if blur.factor == 1 {
      self.work.blurred += alpha.len() as u64;
      if !matches!(blur.blur, Blur::None) {
        each_line(&mut alpha, width, height, |line, out| {
          blur.blur.line(line, out)
        });
      }
      let grid = blur.small_grid((left, top), (width, height));
      return Some(Cast::new(layer, grid, 1, alpha));
    }
    let grid = blur.small_grid((left, top), (width, height));
    self.work.blurred += (grid.width * grid.height) as u64;
    let blocks = blur.apply(&alpha, (left, top), width, &grid);
    Some(Cast::new(layer, grid, blur.factor, blocks))
  }
}

/// The side of a content shadow's cells, in pixels, at least.
const CELL: i64 = 256;

/// A content shadow's blur, worked out at a `factor`th of the painter's
/// resolution each way past 8 buffer pixels, and a quarter past 16: a
/// wide blur loses no detail it shows, and costs a quarter or a
/// sixteenth.
struct Reduced {
  blur: Blur,
  factor: usize,
}

/// Where a reduced blur's grid of blocks lies over a layer: its corner
/// in the painter's pixels, and its size in blocks.
#[derive(Clone, Copy)]
struct SmallGrid {
  left: i64,
  top: i64,
  width: usize,
  height: usize,
}

impl Reduced {
  /// The blur of a shadow blurred by `blur` buffer pixels, a Gaussian of
  /// half that.
  fn new(blur: f32) -> Reduced {
    let factor = if blur >= 16.0 {
      4
    } else if blur >= 8.0 {
      2
    } else {
      1
    };
    // Averaging the blocks and stretching them back blur by a
    // deviation of half a block between them, taken off the blur's.
    let sigma = blur / 2.0;
    let reduced = match factor {
      1 => sigma,
      _ => (sigma * sigma - (factor * factor) as f32 / 4.0)
        .max(0.0)
        .sqrt(),
    };
    Reduced {
      blur: Blur::new(reduced / factor as f32),
      factor,
    }
  }

  /// How many of the painter's pixels away a pixel's value reaches,
  /// with a block and a block's stretch more when reduced.
  fn reach(&self) -> usize {
    match self.factor {
      1 => self.blur.reach(),
      factor => (self.blur.reach() + 2).saturating_mul(factor),
    }
  }

  /// The blocks over a `width` by `height` layer at `left`, `top`, laid
  /// from the painter's origin, so every layer blocks alike.
  fn small_grid(&self, (left, top): (i64, i64), (width, height): (usize, usize)) -> SmallGrid {
    let factor = self.factor as i64;
    let (x, y) = (
      left.div_euclid(factor) * factor,
      top.div_euclid(factor) * factor,
    );
    SmallGrid {
      left: x,
      top: y,
      width: ((left - x) as usize + width).div_ceil(self.factor),
      height: ((top - y) as usize + height).div_ceil(self.factor),
    }
  }

  /// `alpha`, `width` wide with its corner at `left`, `top`, averaged
  /// over the blocks of `small` and blurred there.
  fn apply(
    &self,
    alpha: &[u8],
    (left, top): (i64, i64),
    width: usize,
    small: &SmallGrid,
  ) -> Vec<u8> {
    let factor = self.factor;
    let (dx, dy) = ((left - small.left) as usize, (top - small.top) as usize);
    let mut sums = vec![0_u32; small.width * small.height];
    for (y, line) in alpha.chunks_exact(width).enumerate() {
      let row = &mut sums[(dy + y) / factor * small.width..][..small.width];
      // The first block starts `dx` pixels before the layer.
      let (first, rest) = line.split_at((factor - dx).min(width));
      let blocks = std::iter::once(first).chain(rest.chunks(factor));
      for (sum, block) in row.iter_mut().zip(blocks) {
        *sum += block.iter().map(|&value| u32::from(value)).sum::<u32>();
      }
    }
    let area = (factor * factor) as u32;
    let mut blocks: Vec<u8> = sums
      .iter()
      .map(|&sum| ((sum + area / 2) / area) as u8)
      .collect();
    each_line(&mut blocks, small.width, small.height, |line, out| {
      self.blur.line(line, out);
    });
    blocks
  }
}

/// A cell of a content shadow: the alpha of what casts it, spread and
/// blurred, over its layer's pixels, or blocks of `factor` of them a side
/// laid over the layer by `grid`, stretched back bilinearly as read.
pub(super) struct Cast {
  /// The layer's corner and size, in its painter's pixels.
  left: i64,
  top: i64,
  width: i64,
  height: i64,
  grid: SmallGrid,
  factor: usize,
  values: Vec<u8>,
}

impl Cast {
  fn new(
    (left, top, width, height): (i64, i64, usize, usize),
    grid: SmallGrid,
    factor: usize,
    values: Vec<u8>,
  ) -> Cast {
    Cast {
      left,
      top,
      width: width as i64,
      height: height as i64,
      grid,
      factor,
      values,
    }
  }

  /// Where column `x` of the painter's pixels reads, `None` off the
  /// layer.
  fn column(&self, x: i64) -> Option<Between> {
    (self.left..self.left + self.width)
      .contains(&x)
      .then(|| self.between((x - self.grid.left) as usize, self.grid.width))
  }

  /// Where row `y` reads, likewise.
  fn row(&self, y: i64) -> Option<Between> {
    (self.top..self.top + self.height)
      .contains(&y)
      .then(|| self.between((y - self.grid.top) as usize, self.grid.height))
  }

  /// The alpha at `row` and `column`, mixed from the four values around.
  fn at(&self, row: Between, column: Between) -> u8 {
    let width = self.grid.width;
    let at = |row: usize, column: usize| u32::from(self.values[row * width + column]);
    let mix = |row| at(row, column.0) * (256 - column.2) + at(row, column.1) * column.2;
    let value = mix(row.0) * (256 - row.2) + mix(row.1) * row.2;
    ((value + (1 << 15)) >> 16) as u8
  }

  /// Where pixel `at` lies between the middles of `count` values: the
  /// one before, the one after and how far toward it, in 256ths.
  fn between(&self, at: usize, count: usize) -> Between {
    let place = ((2 * at + 1) * 256 / self.factor).saturating_sub(256) / 2;
    let before = (place / 256).min(count - 1);
    let after = (before + 1).min(count - 1);
    (before, after, (place % 256) as u32)
  }
}

/// A place between two values of a cast: the one before, the one after,
/// and how far toward it, in 256ths.
type Between = (usize, usize, u32);

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
  let reads: Vec<_> = columns.clone().map(|x| cast.column(x - dx)).collect();
  for y in rows {
    let Some(row) = cast.row(y - dy) else {
      continue;
    };
    for (x, column) in columns.clone().zip(&reads) {
      let Some(column) = *column else {
        continue;
      };
      let alpha = cast.at(row, column);
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

/// How much of a layer a region covers.
pub(super) enum Cover {
  /// None of it.
  Clear,
  /// All of it.
  Whole,
  /// Each pixel's coverage.
  Part(tiny_skia::Mask),
}

impl Cover {
  /// The coverage of each of a `width` by `height` layer's pixels.
  fn mask(self, width: u32, height: u32) -> Option<tiny_skia::Mask> {
    let mut mask = match self {
      Cover::Part(mask) => return Some(mask),
      _ => tiny_skia::Mask::new(width, height)?,
    };
    if matches!(self, Cover::Whole) {
      mask.data_mut().fill(255);
    }
    Some(mask)
  }
}

/// How much of a `width` by `height` layer `area` covers, as a rectangle
/// with `corners` or, with none, as the ellipse filling it. It is filled
/// only when an edge or a corner crosses the layer.
fn shape_cover(width: u32, height: u32, area: tiny_skia::Rect, corners: Option<Corners>) -> Cover {
  let (w, h) = (width as f32, height as f32);
  if area.right() <= 0.0 || area.left() >= w || area.bottom() <= 0.0 || area.top() >= h {
    return Cover::Clear;
  }
  let inside = area.left() <= 0.0 && area.top() <= 0.0 && area.right() >= w && area.bottom() >= h;
  let whole = match corners {
    // No corner's square reaches the layer.
    Some([top_left, top_right, bottom_right, bottom_left]) => {
      let (left, top) = (area.left(), area.top());
      let (right, bottom) = (area.right() - w, area.bottom() - h);
      inside
        && (left + top_left <= 0.0 || top + top_left <= 0.0)
        && (right - top_right >= 0.0 || top + top_right <= 0.0)
        && (right - bottom_right >= 0.0 || bottom - bottom_right >= 0.0)
        && (left + bottom_left <= 0.0 || bottom - bottom_left >= 0.0)
    }
    // Every corner of the layer, a pixel out, is inside the ellipse.
    None => {
      let (rx, ry) = (area.width() / 2.0, area.height() / 2.0);
      let (cx, cy) = (area.left() + rx, area.top() + ry);
      [
        (-1.0, -1.0),
        (w + 1.0, -1.0),
        (-1.0, h + 1.0),
        (w + 1.0, h + 1.0),
      ]
      .iter()
      .all(|&(x, y)| ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) < 1.0)
    }
  };
  if whole {
    return Cover::Whole;
  }
  if let Some([left, top, right, bottom]) =
    corners.and_then(|corners| straight_inside((width, height), area, corners))
  {
    // The path would fill that rectangle whole and nothing else.
    let Some(mut mask) = tiny_skia::Mask::new(width, height) else {
      return Cover::Clear;
    };
    let (columns, stride) = (left as usize..right as usize, width as usize);
    for row in mask
      .data_mut()
      .chunks_exact_mut(stride)
      .take(bottom as usize)
      .skip(top as usize)
    {
      row[columns.clone()].fill(255);
    }
    return Cover::Part(mask);
  }
  let path = match corners {
    Some(corners) => {
      let mut path = PathBuilder::new();
      rounded_rect(&mut path, area, corners);
      path.finish()
    }
    None => PathBuilder::from_oval(area),
  };
  let (Some(path), Some(mut mask)) = (path, tiny_skia::Mask::new(width, height)) else {
    return Cover::Clear;
  };
  mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
  Cover::Part(mask)
}

/// Joins `b`'s coverage to `a`'s as `op` joins regions, each pixel as a
/// fraction covered, so edges stay antialiased.
fn join(a: Cover, b: Cover, op: RegionOp) -> Cover {
  use Cover::{Clear, Part, Whole};
  let (a, b) = match (op, a, b) {
    (RegionOp::Union, Whole, _) | (RegionOp::Union, _, Whole) => return Whole,
    (RegionOp::Union | RegionOp::Xor, Clear, other)
    | (RegionOp::Union | RegionOp::Xor | RegionOp::Subtract, other, Clear)
    | (RegionOp::Intersect, Whole, other)
    | (RegionOp::Intersect, other, Whole) => return other,
    (RegionOp::Subtract | RegionOp::Intersect, Clear, _)
    | (RegionOp::Subtract, _, Whole)
    | (RegionOp::Intersect, _, Clear)
    | (RegionOp::Xor, Whole, Whole) => return Clear,
    (_, Part(a), b) => {
      let (width, height) = (a.width(), a.height());
      (Some(a), b.mask(width, height))
    }
    (_, a, Part(b)) => {
      let (width, height) = (b.width(), b.height());
      (a.mask(width, height), Some(b))
    }
  };
  let (Some(mut a), Some(b)) = (a, b) else {
    return Clear;
  };
  for (a, &b) in a.data_mut().iter_mut().zip(b.data()) {
    let both = mul(*a, b);
    *a = match op {
      RegionOp::Union => (*a - both).saturating_add(b),
      RegionOp::Subtract => mul(*a, 255 - b),
      RegionOp::Intersect => both,
      RegionOp::Xor => (*a - both).saturating_add(b - both),
    };
  }
  Part(a)
}
