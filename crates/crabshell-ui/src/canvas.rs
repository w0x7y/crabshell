//! [`Canvas`], a widget a config draws on with paths, and the plain data
//! it records: [`Path`], [`Paint`] and [`Stroke`]. ui only records the
//! drawing; `crabshell-render` draws it.

use std::rc::Rc;

use crate::geometry::{Point, Rect, Size};
use crate::style::{Color, Gradient, Style};
use crate::tree::{Change, NodeId, NodeKind, Tree, borrow};
use crate::widgets::{Common, Element, ElementKind, Widget, visibility_methods};

/// The most path commands and dash lengths one run of a paint closure
/// keeps, so a runaway loop can't eat memory: 16,384 is eight lines
/// across a 2048-pixel-wide graph, a point a pixel, at about 0.5 MiB.
const MOST_COMMANDS: usize = 16_384;

/// The largest number a canvas takes as a coordinate, radius, width or
/// dash, in logical pixels. Past it a number is a mistake, and the
/// renderer's arithmetic would lose it.
const MOST_COORDINATE: f32 = 1.0e6;

/// The most dashes one run of a paint closure may cut its strokes into,
/// as far as their lengths can be told before they are drawn. Each tile
/// a stroke crosses cuts it again.
const MOST_DASHES: f32 = 100_000.0;

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

/// One drawing a paint closure recorded, for the renderer.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasOp {
  /// The inside of a path, by the nonzero winding rule.
  Fill(Path, Paint),
  /// Along a path.
  Stroke(Path, Stroke),
}

/// What a [`Canvas`]'s paint closure draws with. Each run starts with a
/// clean canvas, and what it draws replaces the last run's drawing.
pub struct Pen {
  size: Size,
  ops: Vec<CanvasOp>,
  commands: usize,
  /// How many dashes its strokes are cut into, at most.
  dashes: f32,
  /// Why the first drawing it skipped was skipped.
  skipped: Option<String>,
}

impl Pen {
  fn new(size: Size) -> Pen {
    Pen {
      size,
      ops: Vec::new(),
      commands: 0,
      dashes: 0.0,
      skipped: None,
    }
  }

  /// The canvas's size in logical pixels, from its last layout.
  pub fn size(&self) -> Size {
    self.size
  }

  /// Fills the inside of `path` with `paint`, a [`Color`] or a
  /// [`Gradient`] laid over the canvas's box.
  pub fn fill(&mut self, path: &Path, paint: impl Into<Paint>) {
    let commands = path.commands.len();
    match check_path(path) {
      Ok(()) => self.record(CanvasOp::Fill(path.clone(), paint.into()), commands, 0.0),
      Err(reason) => self.skip(reason),
    }
  }

  /// Draws along `path` with `stroke`.
  pub fn stroke(&mut self, path: &Path, stroke: &Stroke) {
    if stroke.width == 0.0 {
      return;
    }
    let checked = check_path(path).and_then(|()| check_stroke(path, stroke));
    match checked {
      Ok((dash, dashes)) => {
        let commands = path.commands.len() + dash.len();
        let stroke = Stroke {
          dash,
          ..stroke.clone()
        };
        self.record(CanvasOp::Stroke(path.clone(), stroke), commands, dashes);
      }
      Err(reason) => self.skip(reason),
    }
  }

  fn record(&mut self, op: CanvasOp, commands: usize, dashes: f32) {
    if self.commands + commands > MOST_COMMANDS {
      self.skip(format!(
        "more than {MOST_COMMANDS} path commands and dash lengths in one run"
      ));
      return;
    }
    if self.dashes + dashes > MOST_DASHES {
      self.skip(format!("more than {MOST_DASHES} dashes in one run"));
      return;
    }
    self.commands += commands;
    self.dashes += dashes;
    self.ops.push(op);
  }

  fn skip(&mut self, reason: String) {
    self.skipped.get_or_insert(reason);
  }
}

/// Whether `n` is a number a canvas takes.
fn usable(n: f32) -> bool {
  n.is_finite() && n.abs() <= MOST_COORDINATE
}

/// Refuses a path with a number that isn't finite, is past
/// [`MOST_COORDINATE`], or is a negative radius.
fn check_path(path: &Path) -> Result<(), String> {
  for command in &path.commands {
    let (points, numbers): (&[Point], &[f32]) = match command {
      PathCommand::MoveTo(point) | PathCommand::LineTo(point) => (std::slice::from_ref(point), &[]),
      PathCommand::QuadTo { control, to } => (&[*control, *to], &[]),
      PathCommand::CubicTo { first, second, to } => (&[*first, *second, *to], &[]),
      PathCommand::Arc {
        center,
        radius,
        start,
        sweep,
      } => {
        if *radius < 0.0 {
          return Err(format!("an arc with a negative radius, {radius}"));
        }
        (std::slice::from_ref(center), &[*radius, *start, *sweep])
      }
      PathCommand::Close => (&[], &[]),
    };
    let all = points
      .iter()
      .flat_map(|p| [p.x, p.y])
      .chain(numbers.iter().copied());
    if let Some(bad) = all.into_iter().find(|&n| !usable(n)) {
      return Err(format!(
        "a path holding {bad}, which isn't a finite number within {MOST_COORDINATE}"
      ));
    }
  }
  Ok(())
}

/// Refuses a stroke whose width or dashes the renderer can't draw, and
/// gives its dash pattern with an even number of lengths and at most how
/// many dashes it cuts its path into.
fn check_stroke(path: &Path, stroke: &Stroke) -> Result<(Vec<f32>, f32), String> {
  if !(usable(stroke.width) && stroke.width > 0.0) {
    return Err(format!("a stroke {} wide", stroke.width));
  }
  if stroke.dash.is_empty() {
    return Ok((Vec::new(), 0.0));
  }
  if !usable(stroke.dash_offset) || stroke.dash.iter().any(|&n| !(usable(n) && n >= 0.0)) {
    return Err(format!(
      "a dash pattern {:?} at offset {}",
      stroke.dash, stroke.dash_offset
    ));
  }
  let mut dash = stroke.dash.clone();
  if dash.len() % 2 == 1 {
    dash.extend_from_within(..);
  }
  let interval: f32 = dash.iter().sum();
  let dashes = path_length(path) / interval * (dash.len() / 2) as f32;
  if !(interval > 0.0 && dashes.is_finite()) {
    return Err(format!("a dash pattern {:?}", stroke.dash));
  }
  Ok((dash, dashes))
}

/// At least the length of `path`: the lengths of its control polygons
/// and arcs.
fn path_length(path: &Path) -> f32 {
  /// The distance from `at` to `to`, which becomes where the path is.
  fn step(at: &mut Option<Point>, to: Point) -> f32 {
    let from = at.replace(to).unwrap_or(to);
    (to.x - from.x).hypot(to.y - from.y)
  }
  let (mut length, mut at, mut start) = (0.0, None, Point::default());
  for command in &path.commands {
    // As drawn, a path that doesn't start with a move starts a piece at
    // the first point of its first command.
    if at.is_none() {
      let first = match *command {
        PathCommand::MoveTo(point) | PathCommand::LineTo(point) => Some(point),
        PathCommand::QuadTo { control, .. } => Some(control),
        PathCommand::CubicTo { first, .. } => Some(first),
        PathCommand::Arc {
          center,
          radius,
          start,
          ..
        } => {
          let (sin, cos) = start.to_radians().sin_cos();
          Some(Point::new(center.x + radius * cos, center.y + radius * sin))
        }
        PathCommand::Close => None,
      };
      if let Some(first) = first {
        (at, start) = (Some(first), first);
      }
    }
    length += match *command {
      PathCommand::MoveTo(to) => {
        (at, start) = (Some(to), to);
        0.0
      }
      PathCommand::LineTo(to) => step(&mut at, to),
      PathCommand::QuadTo { control, to } => step(&mut at, control) + step(&mut at, to),
      PathCommand::CubicTo { first, second, to } => {
        step(&mut at, first) + step(&mut at, second) + step(&mut at, to)
      }
      PathCommand::Arc {
        center,
        radius,
        start: from,
        sweep,
      } => {
        let point = |degrees: f32| {
          let (sin, cos) = degrees.to_radians().sin_cos();
          Point::new(center.x + radius * cos, center.y + radius * sin)
        };
        let sweep = sweep.clamp(-360.0, 360.0);
        let line = step(&mut at, point(from));
        at = Some(point(from + sweep));
        line + radius * sweep.abs().to_radians()
      }
      PathCommand::Close => step(&mut at, start),
    };
  }
  length
}

/// A box a config draws on with paths, arcs, curves and dashed strokes,
/// for progress rings, graphs and audio visualizers.
///
/// The paint closure gets a [`Pen`] and runs when the canvas is first
/// laid out, when its laid-out size changes, and when a signal it read
/// changes; each run replaces the drawing, and repaints only the
/// canvas's box. Coordinates are logical pixels from the canvas's
/// top-left, and the drawing is cut to its box and rounded corners. Size
/// it with its style's `width` and `height`, or let its parent stretch
/// it, as any box. A panic in the closure is logged with the canvas's
/// path, such as `PanelWindow[DP-1]/Row/Canvas[2].paint`, and the last
/// drawing stays.
///
/// ```
/// use crabshell_ui::{Canvas, Cap, Color, Path, Point, Stroke, Style};
///
/// # let cx = crabshell_ui::Cx::new(crabshell_reactive::Runtime::current().new_owner());
/// let charge = cx.signal(0.6_f32);
/// let ring = Canvas::new(move |pen| {
///     let size = pen.size();
///     let middle = Point::new(size.width / 2.0, size.height / 2.0);
///     let radius = size.width.min(size.height) / 2.0 - 3.0;
///     let arc = Path::new().arc(middle, radius, -90.0, 360.0 * charge.get());
///     pen.stroke(&arc, &Stroke::new(4.0, Color::hex("#a6e3a1")).cap(Cap::Round));
/// })
/// .style(Style::new().width(24.0).height(24.0));
/// ```
pub struct Canvas {
  paint: Box<dyn FnMut(&mut Pen)>,
  common: Common,
}

impl Canvas {
  /// A canvas `paint` draws on.
  pub fn new(paint: impl FnMut(&mut Pen) + 'static) -> Canvas {
    Canvas {
      paint: Box::new(paint),
      common: Common::default(),
    }
  }

  /// Sets the style: its `width` and `height` size the canvas, and its
  /// background and border are drawn under the drawing.
  pub fn style(mut self, style: impl crate::IntoProp<Style>) -> Canvas {
    self.common.style = Some(style.into_prop());
    self
  }

  visibility_methods!();
}

impl Widget for Canvas {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Canvas",
      common: self.common,
      kind: ElementKind::Canvas(self.paint),
      children: Vec::new(),
    }
  }
}

/// What a canvas node holds: the last run's drawing.
#[derive(Default)]
pub(crate) struct CanvasNode {
  pub(crate) ops: Vec<CanvasOp>,
}

impl Tree {
  /// Runs `paint` in an effect under the canvas's owner once the canvas
  /// has a size, and again whenever that size or a signal it read
  /// changes, storing what it drew.
  pub(crate) fn bind_canvas(&mut self, id: NodeId, mut paint: Box<dyn FnMut(&mut Pen)>) {
    let Some(node) = self.node_mut(id) else {
      return;
    };
    let cx = node.cx.clone();
    let path = Rc::clone(&node.path);
    // `None` until the first layout says how large it is.
    let size = cx.signal(None::<Size>);
    node.measure = Some(Rc::new(move |laid: Size| {
      size.try_set(Some(laid));
    }));
    let tree = self.this.clone();
    let label = format!("{path}.paint");
    let mut logged = false;
    cx.effect(move || {
      let Some(size) = size.get() else {
        return;
      };
      let mut pen = Pen::new(size);
      // The user's closure runs before the tree is borrowed.
      paint(&mut pen);
      let Pen { ops, skipped, .. } = pen;
      if let Some(reason) = skipped
        && !std::mem::replace(&mut logged, true)
      {
        tracing::warn!(
          "{path}.paint drew {reason}, so that drawing is skipped; later ones aren't logged"
        );
      }
      let Some(tree) = tree.upgrade() else {
        return;
      };
      borrow(&tree).update(id, |node| match &mut node.kind {
        NodeKind::Canvas(canvas) if canvas.ops != ops => {
          canvas.ops = ops;
          Change::Paint
        }
        _ => Change::None,
      });
    })
    .named(label);
  }
}
