//! Points, sizes and rectangles. Every unit is a logical pixel; the renderer
//! multiplies by the output's scale.

/// A point in logical pixels, measured from the window's top-left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
  /// Distance from the left edge.
  pub x: f32,
  /// Distance from the top edge.
  pub y: f32,
}

impl Point {
  /// A point at `x`, `y`.
  pub const fn new(x: f32, y: f32) -> Point {
    Point { x, y }
  }
}

/// A width and height in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
  /// The width.
  pub width: f32,
  /// The height.
  pub height: f32,
}

impl Size {
  /// A size of `width` by `height`.
  pub const fn new(width: f32, height: f32) -> Size {
    Size { width, height }
  }
}

/// A rectangle in logical pixels: its top-left corner and its size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
  /// The left edge.
  pub x: f32,
  /// The top edge.
  pub y: f32,
  /// The width.
  pub width: f32,
  /// The height.
  pub height: f32,
}

impl Rect {
  /// A rectangle with its top-left corner at `x`, `y`.
  pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
      x,
      y,
      width,
      height,
    }
  }

  /// The right edge.
  pub fn right(&self) -> f32 {
    self.x + self.width
  }

  /// The bottom edge.
  pub fn bottom(&self) -> f32 {
    self.y + self.height
  }

  /// Whether the rectangle covers no area.
  pub fn is_empty(&self) -> bool {
    self.width <= 0.0 || self.height <= 0.0
  }

  /// Whether `point` lies inside. The left and top edges count as inside,
  /// the right and bottom edges don't, so two rectangles that touch never
  /// both contain a point.
  pub fn contains(&self, point: Point) -> bool {
    point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
  }

  /// The smallest rectangle that covers both. An empty rectangle adds
  /// nothing.
  pub fn union(&self, other: Rect) -> Rect {
    if other.is_empty() {
      return *self;
    }
    if self.is_empty() {
      return other;
    }
    let x = self.x.min(other.x);
    let y = self.y.min(other.y);
    Rect::new(
      x,
      y,
      self.right().max(other.right()) - x,
      self.bottom().max(other.bottom()) - y,
    )
  }

  /// The area both cover, or an empty rectangle if they don't overlap.
  pub fn intersect(&self, other: Rect) -> Rect {
    let x = self.x.max(other.x);
    let y = self.y.max(other.y);
    let right = self.right().min(other.right());
    let bottom = self.bottom().min(other.bottom());
    if right > x && bottom > y {
      Rect::new(x, y, right - x, bottom - y)
    } else {
      Rect::default()
    }
  }
}

impl Rect {
  /// This rectangle grown by `by` on every side. An empty one stays as
  /// it is.
  pub(crate) fn outset(&self, by: f32) -> Rect {
    if self.is_empty() {
      return *self;
    }
    Rect::new(
      self.x - by,
      self.y - by,
      self.width + 2.0 * by,
      self.height + 2.0 * by,
    )
  }
}

/// A 2D affine transform, in the order tiny-skia's `Transform::from_row`
/// takes: a point `(x, y)` goes to `(sx·x + kx·y + tx, ky·x + sy·y + ty)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
  /// How far x moves along x.
  pub sx: f32,
  /// How far x moves along y.
  pub ky: f32,
  /// How far y moves along x.
  pub kx: f32,
  /// How far y moves along y.
  pub sy: f32,
  /// Added to x.
  pub tx: f32,
  /// Added to y.
  pub ty: f32,
}

impl Affine {
  /// Where `point` goes.
  pub(crate) fn map(&self, point: Point) -> Point {
    Point::new(
      self.sx * point.x + self.kx * point.y + self.tx,
      self.ky * point.x + self.sy * point.y + self.ty,
    )
  }

  /// The smallest rectangle covering where `rect` goes, or an empty one
  /// if that isn't made of finite numbers.
  pub(crate) fn map_rect(&self, rect: Rect) -> Rect {
    let corners = [
      Point::new(rect.x, rect.y),
      Point::new(rect.right(), rect.y),
      Point::new(rect.x, rect.bottom()),
      Point::new(rect.right(), rect.bottom()),
    ]
    .map(|corner| self.map(corner));
    let (mut left, mut top) = (f32::INFINITY, f32::INFINITY);
    let (mut right, mut bottom) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for corner in corners {
      left = left.min(corner.x);
      top = top.min(corner.y);
      right = right.max(corner.x);
      bottom = bottom.max(corner.y);
    }
    let mapped = Rect::new(left, top, right - left, bottom - top);
    if [mapped.x, mapped.y, mapped.width, mapped.height]
      .iter()
      .all(|n| n.is_finite())
    {
      mapped
    } else {
      Rect::default()
    }
  }

  /// `inner`, then this transform.
  pub(crate) fn after(&self, inner: &Affine) -> Affine {
    let at = self.map(Point::new(inner.tx, inner.ty));
    Affine {
      sx: self.sx * inner.sx + self.kx * inner.ky,
      ky: self.ky * inner.sx + self.sy * inner.ky,
      kx: self.sx * inner.kx + self.kx * inner.sy,
      sy: self.ky * inner.kx + self.sy * inner.sy,
      tx: at.x,
      ty: at.y,
    }
  }

  /// How many times its own resolution a layer drawn through this
  /// transform, from the window down, is drawn at, so what it scales up
  /// stays sharp: the most it stretches a length, rounded up to a quarter
  /// and kept from 1 to 4. A scale-down draws at its own size, and an
  /// animated scale-up changes it only every quarter.
  ///
  /// ```
  /// use crabshell_ui::Affine;
  ///
  /// let scale = |s: f32| Affine { sx: s, ky: 0.0, kx: 0.0, sy: s, tx: 0.0, ty: 0.0 };
  /// assert_eq!(scale(3.0).layer_zoom(), 3.0);
  /// assert_eq!(scale(1.1).layer_zoom(), 1.25);
  /// assert_eq!(scale(0.5).layer_zoom(), 1.0);
  /// assert_eq!(scale(9.0).layer_zoom(), 4.0);
  /// ```
  pub fn layer_zoom(&self) -> f32 {
    let (a, b, c, d) = (self.sx, self.kx, self.ky, self.sy);
    // The larger singular value of the 2 by 2 part.
    let sum = a * a + b * b + c * c + d * d;
    let det = a * d - b * c;
    let stretch = ((sum + (sum * sum - 4.0 * det * det).max(0.0).sqrt()) / 2.0).sqrt();
    // A turn's rounding leaves a hair past 1, which shouldn't count.
    let quarters = (stretch * 4.0 - 1e-3).ceil() / 4.0;
    if quarters.is_nan() {
      1.0
    } else {
      quarters.clamp(1.0, 4.0)
    }
  }

  /// The transform that undoes this one, or `None` when it squashes the
  /// plane flat, as a scale of 0 does.
  pub(crate) fn invert(&self) -> Option<Affine> {
    let det = self.sx * self.sy - self.kx * self.ky;
    if det == 0.0 || !det.is_finite() {
      return None;
    }
    let (sx, ky, kx, sy) = (self.sy / det, -self.ky / det, -self.kx / det, self.sx / det);
    Some(Affine {
      sx,
      ky,
      kx,
      sy,
      tx: -(sx * self.tx + kx * self.ty),
      ty: -(ky * self.tx + sy * self.ty),
    })
  }
}
