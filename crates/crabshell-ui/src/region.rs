//! [`Region`], the part of a window that takes pointer input.

/// An area of a window in logical pixels, built from rectangles, rounded
/// rectangles and ellipses joined with [`union`](Self::union),
/// [`subtract`](Self::subtract), [`intersect`](Self::intersect) and
/// [`xor`](Self::xor), applied in the order they are called.
///
/// [`PanelWindow::mask`](crate::PanelWindow::mask) takes one: pointer
/// input outside it goes to whatever is below the window.
///
/// ```
/// use crabshell_ui::Region;
///
/// // A 200 by 30 bar that only takes clicks at its two rounded ends.
/// let ends = Region::rect(0, 0, 40, 30)
///     .radius(8.0)
///     .union(Region::rect(160, 0, 40, 30).radius(8.0));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
  shape: RegionShape,
  ops: Vec<(RegionOp, Region)>,
}

/// The shape a [`Region`] starts from, for a renderer that draws one:
/// see [`Region::shape`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegionShape {
  /// No area.
  Empty,
  /// A rectangle as `[left, top, right, bottom]`, with its corner radii
  /// clockwise from the top left as given, before
  /// [`Radii::fit`](crate::Radii::fit).
  Rect([i32; 4], [f32; 4]),
  /// The ellipse filling `[left, top, right, bottom]`.
  Ellipse([i32; 4]),
}

/// How a region is joined to the one before it: see [`Region::joined`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegionOp {
  /// [`Region::union`].
  Union,
  /// [`Region::subtract`].
  Subtract,
  /// [`Region::intersect`].
  Intersect,
  /// [`Region::xor`].
  Xor,
}

/// One row's covered columns: sorted, disjoint, half-open `(start, end)`.
type Spans = Vec<(i32, i32)>;

impl Region {
  /// No area at all: a window masked with it lets every click through.
  pub fn empty() -> Region {
    Region {
      shape: RegionShape::Empty,
      ops: Vec::new(),
    }
  }

  /// The rectangle at `x`, `y`, relative to the window's top left.
  pub fn rect(x: i32, y: i32, width: u32, height: u32) -> Region {
    Region {
      shape: RegionShape::Rect(bounds(x, y, width, height), [0.0; 4]),
      ops: Vec::new(),
    }
  }

  /// The ellipse that fills the rectangle at `x`, `y`.
  pub fn ellipse(x: i32, y: i32, width: u32, height: u32) -> Region {
    Region {
      shape: RegionShape::Ellipse(bounds(x, y, width, height)),
      ops: Vec::new(),
    }
  }

  /// Rounds every corner of a [`rect`](Self::rect). It does nothing to
  /// other shapes.
  pub fn radius(self, radius: f32) -> Region {
    self.radii(radius, radius, radius, radius)
  }

  /// Rounds each corner of a [`rect`](Self::rect) on its own, fitted to
  /// it as a widget's are ([`Radii::fit`](crate::Radii::fit)). It does
  /// nothing to other shapes.
  pub fn radii(
    mut self,
    top_left: f32,
    top_right: f32,
    bottom_right: f32,
    bottom_left: f32,
  ) -> Region {
    if let RegionShape::Rect(_, radii) = &mut self.shape {
      *radii = [top_left, top_right, bottom_right, bottom_left];
    }
    self
  }

  /// Adds `other`'s area.
  pub fn union(self, other: Region) -> Region {
    self.then(RegionOp::Union, other)
  }

  /// Removes `other`'s area.
  pub fn subtract(self, other: Region) -> Region {
    self.then(RegionOp::Subtract, other)
  }

  /// Keeps only the area also in `other`.
  pub fn intersect(self, other: Region) -> Region {
    self.then(RegionOp::Intersect, other)
  }

  /// Keeps the area in exactly one of the two.
  pub fn xor(self, other: Region) -> Region {
    self.then(RegionOp::Xor, other)
  }

  fn then(mut self, op: RegionOp, other: Region) -> Region {
    self.ops.push((op, other));
    self
  }

  /// The shape the region starts from, for a renderer.
  pub fn shape(&self) -> RegionShape {
    self.shape
  }

  /// Each region joined to [`shape`](Self::shape) and how, in the order
  /// the calls made them, for a renderer.
  pub fn joined(&self) -> impl Iterator<Item = (RegionOp, &Region)> {
    self.ops.iter().map(|(op, region)| (*op, region))
  }

  /// The region inside a `width` by `height` window as rectangles
  /// `[x, y, width, height]`, which don't overlap. Curves are followed to
  /// the nearest whole pixel at each row's center.
  pub fn rects(&self, width: u32, height: u32) -> Vec<[i32; 4]> {
    let width = i32::try_from(width).unwrap_or(i32::MAX);
    let height = i32::try_from(height).unwrap_or(i32::MAX);
    let mut rects = Vec::new();
    // Rows with the same spans as the row above grow its rectangles.
    let mut band: Option<(i32, Spans)> = None;
    for y in 0..=height {
      let spans = if y < height {
        clip(self.spans(y as f32 + 0.5), width)
      } else {
        Spans::new()
      };
      if band.as_ref().is_some_and(|(_, above)| *above == spans) {
        continue;
      }
      if let Some((top, above)) = band.take() {
        rects.extend(
          above
            .iter()
            .map(|&(start, end)| [start, top, end - start, y - top]),
        );
      }
      band = Some((y, spans));
    }
    rects
  }

  /// The columns covered on the row whose center is at `y`.
  fn spans(&self, y: f32) -> Spans {
    let mut spans = match self.shape {
      RegionShape::Empty => Spans::new(),
      RegionShape::Rect(rect, radii) => rect_spans(rect, radii, y),
      RegionShape::Ellipse(rect) => ellipse_spans(rect, y),
    };
    for (op, other) in &self.ops {
      let keep = match op {
        RegionOp::Union => |a, b| a || b,
        RegionOp::Subtract => |a, b: bool| a && !b,
        RegionOp::Intersect => |a, b| a && b,
        RegionOp::Xor => |a, b| a != b,
      };
      spans = combine(&spans, &other.spans(y), keep);
    }
    spans
  }
}

/// `[left, top, right, bottom]`, saturating instead of overflowing.
fn bounds(x: i32, y: i32, width: u32, height: u32) -> [i32; 4] {
  let side = |side: u32| i32::try_from(side).unwrap_or(i32::MAX);
  [
    x,
    y,
    x.saturating_add(side(width)),
    y.saturating_add(side(height)),
  ]
}

fn rect_spans([left, top, right, bottom]: [i32; 4], radii: [f32; 4], y: f32) -> Spans {
  if y < top as f32 || y >= bottom as f32 {
    return Spans::new();
  }
  let (width, height) = (right as f32 - left as f32, bottom as f32 - top as f32);
  let [top_left, top_right, bottom_right, bottom_left] =
    crate::style::fit_corners(radii, width, height);
  // How far a corner of radius `r` cuts into a row `dy` inside its edge.
  let inset = |r: f32, dy: f32| {
    if dy >= r {
      0.0
    } else {
      r - (r * r - (r - dy) * (r - dy)).sqrt()
    }
  };
  let (from_top, from_bottom) = (y - top as f32, bottom as f32 - y);
  let left_inset = inset(top_left, from_top).max(inset(bottom_left, from_bottom));
  let right_inset = inset(top_right, from_top).max(inset(bottom_right, from_bottom));
  span(left as f32 + left_inset, right as f32 - right_inset)
}

fn ellipse_spans([left, top, right, bottom]: [i32; 4], y: f32) -> Spans {
  let rx = (right as f32 - left as f32) / 2.0;
  let ry = (bottom as f32 - top as f32) / 2.0;
  if rx <= 0.0 || ry <= 0.0 {
    return Spans::new();
  }
  let t = (y - (top as f32 + ry)) / ry;
  if t.abs() >= 1.0 {
    return Spans::new();
  }
  let (cx, dx) = (left as f32 + rx, rx * (1.0 - t * t).sqrt());
  span(cx - dx, cx + dx)
}

/// The columns from `start` to `end`, rounded to whole pixels.
fn span(start: f32, end: f32) -> Spans {
  let (start, end) = (start.round() as i32, end.round() as i32);
  if start < end {
    vec![(start, end)]
  } else {
    Spans::new()
  }
}

/// `spans` cut to the columns from 0 to `width`.
fn clip(spans: Spans, width: i32) -> Spans {
  spans
    .into_iter()
    .map(|(start, end)| (start.max(0), end.min(width)))
    .filter(|(start, end)| start < end)
    .collect()
}

/// The columns where `keep` holds for whether they are in `a` and in `b`.
fn combine(a: &[(i32, i32)], b: &[(i32, i32)], keep: fn(bool, bool) -> bool) -> Spans {
  let mut edges: Vec<i32> = a
    .iter()
    .chain(b)
    .flat_map(|&(start, end)| [start, end])
    .collect();
  edges.sort_unstable();
  edges.dedup();
  let inside =
    |spans: &[(i32, i32)], x: i32| spans.iter().any(|&(start, end)| start <= x && x < end);
  let mut out = Spans::new();
  for pair in edges.windows(2) {
    let (start, end) = (pair[0], pair[1]);
    if !keep(inside(a, start), inside(b, start)) {
      continue;
    }
    match out.last_mut() {
      Some(last) if last.1 == start => last.1 = end,
      _ => out.push((start, end)),
    }
  }
  out
}
