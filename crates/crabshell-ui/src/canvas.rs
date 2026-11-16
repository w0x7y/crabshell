//! The plain data a canvas draws: [`Path`], [`Paint`] and [`Stroke`].

use crate::geometry::{Point, Rect};
use crate::style::{Color, Gradient};

/// How a [`Canvas`] paints a path: in one color, or in a gradient laid
/// over the canvas's box, as a style's is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
  /// One color.
  Color(Color),
  /// A gradient across the canvas's box.
  Gradient(Gradient),
}

impl From<Color> for Paint {
  fn from(color: Color) -> Paint {
    Paint::Color(color)
  }
}

impl From<Gradient> for Paint {
  fn from(gradient: Gradient) -> Paint {
    Paint::Gradient(gradient)
  }
}

/// One step of a [`Path`], in logical pixels from the canvas's top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCommand {
  /// Starts a new piece at the point.
  MoveTo(Point),
  /// A straight line to the point.
  LineTo(Point),
  /// A quadratic curve to `to`, bent toward `control`.
  QuadTo {
    /// Its control point.
    control: Point,
    /// Where it ends.
    to: Point,
  },
  /// A cubic curve to `to`, leaving toward `first` and arriving from
  /// `second`.
  CubicTo {
    /// The control point it leaves toward.
    first: Point,
    /// The control point it arrives from.
    second: Point,
    /// Where it ends.
    to: Point,
  },
  /// An arc of a circle, from `start` degrees through `sweep` degrees,
  /// clockwise from 3 o'clock, with a line to its start from where the
  /// path is.
  Arc {
    /// The circle's middle.
    center: Point,
    /// The circle's radius.
    radius: f32,
    /// Where it starts, in degrees clockwise from 3 o'clock.
    start: f32,
    /// How far it goes, in degrees: clockwise when positive, at most a
    /// whole turn either way.
    sweep: f32,
  },
  /// A straight line back to where the piece started, closing it.
  Close,
}

/// A shape to fill or stroke on a [`Canvas`], built from lines, curves
/// and arcs in logical pixels from the canvas's top-left corner. It is
/// plain data, so it can be built once outside the paint closure.
///
/// ```
/// use crabshell_ui::{Path, Point, Rect};
///
/// let tick = Path::new()
///     .move_to(Point::new(2.0, 8.0))
///     .line_to(Point::new(6.0, 12.0))
///     .line_to(Point::new(14.0, 4.0));
/// // A quarter of a ring from 12 o'clock, as a progress ring at 25%.
/// let progress = Path::new().arc(Point::new(20.0, 20.0), 16.0, -90.0, 90.0);
/// let card = Path::rounded_rect(Rect::new(0.0, 0.0, 40.0, 40.0), 8.0);
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
#[must_use]
pub struct Path {
  commands: Vec<PathCommand>,
}

impl Path {
  /// An empty path.
  pub fn new() -> Path {
    Path::default()
  }

  /// Starts a new piece at `to`.
  pub fn move_to(self, to: Point) -> Path {
    self.with(PathCommand::MoveTo(to))
  }

  /// Adds a straight line to `to`.
  pub fn line_to(self, to: Point) -> Path {
    self.with(PathCommand::LineTo(to))
  }

  /// Adds a quadratic curve to `to`, bent toward `control`.
  pub fn quad_to(self, control: Point, to: Point) -> Path {
    self.with(PathCommand::QuadTo { control, to })
  }

  /// Adds a cubic curve to `to`, leaving toward `first` and arriving from
  /// `second`.
  pub fn cubic_to(self, first: Point, second: Point, to: Point) -> Path {
    self.with(PathCommand::CubicTo { first, second, to })
  }

  /// Adds an arc of the circle around `center`, from `start` degrees
  /// through `sweep` degrees, clockwise from 3 o'clock, as an HTML
  /// canvas's `arc` goes: a negative sweep runs counterclockwise, and a
  /// sweep past a whole turn is one. A line joins it to where the path
  /// was; on an empty path it starts a piece.
  pub fn arc(self, center: Point, radius: f32, start: f32, sweep: f32) -> Path {
    self.with(PathCommand::Arc {
      center,
      radius,
      start,
      sweep,
    })
  }

  /// Closes the piece with a straight line back to its start.
  pub fn close(self) -> Path {
    self.with(PathCommand::Close)
  }

  /// A rectangle.
  pub fn rect(rect: Rect) -> Path {
    Path::new()
      .move_to(Point::new(rect.x, rect.y))
      .line_to(Point::new(rect.right(), rect.y))
      .line_to(Point::new(rect.right(), rect.bottom()))
      .line_to(Point::new(rect.x, rect.bottom()))
      .close()
  }

  /// A rectangle with circular corners of `radius`, at most half its
  /// shorter side.
  pub fn rounded_rect(rect: Rect, radius: f32) -> Path {
    let r = radius.min(rect.width.min(rect.height) / 2.0);
    if r.is_nan() || r <= 0.0 {
      return Path::rect(rect);
    }
    let (left, top, right, bottom) = (rect.x, rect.y, rect.right(), rect.bottom());
    Path::new()
      .move_to(Point::new(left + r, top))
      .arc(Point::new(right - r, top + r), r, -90.0, 90.0)
      .arc(Point::new(right - r, bottom - r), r, 0.0, 90.0)
      .arc(Point::new(left + r, bottom - r), r, 90.0, 90.0)
      .arc(Point::new(left + r, top + r), r, 180.0, 90.0)
      .close()
  }

  /// A circle around `center`.
  pub fn circle(center: Point, radius: f32) -> Path {
    Path::new()
      .move_to(Point::new(center.x + radius, center.y))
      .arc(center, radius, 0.0, 360.0)
      .close()
  }

  /// An ellipse around `center`, `radius_x` wide and `radius_y` tall on
  /// each side.
  pub fn ellipse(center: Point, radius_x: f32, radius_y: f32) -> Path {
    // How far a cubic curve's control points sit along the tangents to
    // follow a quarter circle closely.
    const KAPPA: f32 = 0.552_284_8;
    let (x, y) = (center.x, center.y);
    let (kx, ky) = (radius_x * KAPPA, radius_y * KAPPA);
    let point = Point::new;
    Path::new()
      .move_to(point(x + radius_x, y))
      .cubic_to(
        point(x + radius_x, y + ky),
        point(x + kx, y + radius_y),
        point(x, y + radius_y),
      )
      .cubic_to(
        point(x - kx, y + radius_y),
        point(x - radius_x, y + ky),
        point(x - radius_x, y),
      )
      .cubic_to(
        point(x - radius_x, y - ky),
        point(x - kx, y - radius_y),
        point(x, y - radius_y),
      )
      .cubic_to(
        point(x + kx, y - radius_y),
        point(x + radius_x, y - ky),
        point(x + radius_x, y),
      )
      .close()
  }

  /// The steps, in order.
  pub fn commands(&self) -> &[PathCommand] {
    &self.commands
  }

  fn with(mut self, command: PathCommand) -> Path {
    self.commands.push(command);
    self
  }
}

/// How a stroke's open ends look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cap {
  /// Cut square at the end. The default.
  #[default]
  Butt,
  /// A half circle past the end.
  Round,
  /// A half square past the end.
  Square,
}

/// How a stroke turns its corners.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Join {
  /// A sharp point, cut flat where it would reach past 4 widths. The
  /// default.
  #[default]
  Miter,
  /// A round corner.
  Round,
  /// A corner cut flat.
  Bevel,
}

/// How a [`Canvas`] draws along a path: its width in logical pixels, its
/// paint, its ends, its corners and its dashes.
///
/// ```
/// use crabshell_ui::{Cap, Color, Join, Stroke};
///
/// let ring = Stroke::new(4.0, Color::hex("#89b4fa")).cap(Cap::Round);
/// let grid = Stroke::new(1.0, Color::rgba(255, 255, 255, 40)).dash(&[4.0, 2.0], 0.0);
/// let line = Stroke::new(2.0, Color::hex("#a6e3a1")).join(Join::Round);
/// ```
#[derive(Clone, Debug, PartialEq)]
#[must_use]
pub struct Stroke {
  /// How wide it is. 0 draws nothing.
  pub width: f32,
  /// What it is painted with.
  pub paint: Paint,
  /// How its open ends look.
  pub cap: Cap,
  /// How it turns corners.
  pub join: Join,
  /// The lengths of its dashes and the gaps between them, in turn, or
  /// empty for a solid line. An odd list is repeated, as an HTML
  /// canvas's `setLineDash` does.
  pub dash: Vec<f32>,
  /// How far into the dash pattern the stroke starts.
  pub dash_offset: f32,
}

impl Stroke {
  /// A solid stroke `width` wide, with butt ends and miter corners.
  pub fn new(width: f32, paint: impl Into<Paint>) -> Stroke {
    Stroke {
      width,
      paint: paint.into(),
      cap: Cap::default(),
      join: Join::default(),
      dash: Vec::new(),
      dash_offset: 0.0,
    }
  }

  /// Sets how its open ends look, dashes' included.
  pub fn cap(mut self, cap: Cap) -> Stroke {
    self.cap = cap;
    self
  }

  /// Sets how it turns corners.
  pub fn join(mut self, join: Join) -> Stroke {
    self.join = join;
    self
  }

  /// Dashes it: `pattern` holds the lengths of dashes and the gaps
  /// between them, in turn, and `offset` is how far into it the stroke
  /// starts.
  pub fn dash(mut self, pattern: &[f32], offset: f32) -> Stroke {
    self.dash = pattern.to_vec();
    self.dash_offset = offset;
    self
  }
}
