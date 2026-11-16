//! Drawing a [`Canvas`](crabshell_ui::Canvas)'s recorded paths: each one
//! scaled from the canvas's logical pixels onto the pixel grid, cut to
//! the canvas's box, in the order the paint closure drew them.

use crabshell_ui::{CanvasOp, CanvasView, Cap, Join, Paint, Path, PathCommand, Point, Style};
use tiny_skia::{FillRule, LineCap, LineJoin, PathBuilder, PixmapMut, StrokeDash, Transform};

use super::{Painter, visible};

impl Painter<'_> {
  /// `canvas`'s drawing, through a mask of its box with `style`'s
  /// corners. A path or dash pattern tiny-skia refuses is left out.
  pub(super) fn canvas(&self, pixmap: &mut PixmapMut<'_>, canvas: &CanvasView<'_>, style: &Style) {
    if canvas.ops.is_empty() {
      return;
    }
    let Some((area, _)) = self.box_shape(canvas.rect, style) else {
      return;
    };
    if !visible(pixmap, area) {
      return;
    }
    let Some(mask) = self.box_mask(pixmap, canvas.rect, style) else {
      return;
    };
    let scale = self.grid.scale;
    // From the canvas's logical pixels to this pixmap's, its corner on
    // the pixel its box starts at.
    let at = Transform::from_row(scale, 0.0, 0.0, scale, area.left(), area.top());
    for op in canvas.ops {
      let (path, paint) = match op {
        CanvasOp::Fill(path, paint)
        | CanvasOp::Stroke(path, crabshell_ui::Stroke { paint, .. }) => (path, paint),
      };
      let (Some(path), Some(paint)) = (build(path), self.paint(paint, area, at)) else {
        continue;
      };
      match op {
        CanvasOp::Fill(..) => {
          pixmap.fill_path(&path, &paint, FillRule::Winding, at, Some(&mask));
        }
        CanvasOp::Stroke(_, stroke) => {
          let Some(stroke) = tiny_stroke(stroke) else {
            continue;
          };
          pixmap.stroke_path(&path, &paint, &stroke, at, Some(&mask));
        }
      }
    }
  }

  /// `paint` as tiny-skia's. A gradient is laid over `area`, the box in
  /// this pixmap's pixels, as a style's is, and undoes `at`, which
  /// tiny-skia applies to shaders as well as paths.
  fn paint(
    &self,
    paint: &Paint,
    area: tiny_skia::Rect,
    at: Transform,
  ) -> Option<tiny_skia::Paint<'static>> {
    let mut out = tiny_skia::Paint {
      anti_alias: true,
      ..tiny_skia::Paint::default()
    };
    match paint {
      Paint::Color(color) => out.set_color_rgba8(color.r, color.g, color.b, color.a),
      Paint::Gradient(gradient) => {
        let mut shader = self.shader(gradient, area)?;
        shader.transform(at.invert()?);
        out.shader = shader;
      }
    }
    Some(out)
  }
}

/// `stroke` as tiny-skia's, or `None` for a dash pattern it refuses.
fn tiny_stroke(stroke: &crabshell_ui::Stroke) -> Option<tiny_skia::Stroke> {
  let dash = if stroke.dash.is_empty() {
    None
  } else {
    Some(StrokeDash::new(stroke.dash.clone(), stroke.dash_offset)?)
  };
  Some(tiny_skia::Stroke {
    width: stroke.width,
    line_cap: match stroke.cap {
      Cap::Butt => LineCap::Butt,
      Cap::Round => LineCap::Round,
      Cap::Square => LineCap::Square,
    },
    line_join: match stroke.join {
      Join::Miter => LineJoin::Miter,
      Join::Round => LineJoin::Round,
      Join::Bevel => LineJoin::Bevel,
    },
    dash,
    ..tiny_skia::Stroke::default()
  })
}

/// `path` as tiny-skia's, or `None` when it draws nothing. As on an HTML
/// canvas, a line or curve with no piece started starts one at its first
/// point.
fn build(path: &Path) -> Option<tiny_skia::Path> {
  let mut builder = PathBuilder::new();
  let start = |builder: &mut PathBuilder, at: Point| {
    if builder.is_empty() {
      builder.move_to(at.x, at.y);
    }
  };
  for command in path.commands() {
    match *command {
      PathCommand::MoveTo(to) => builder.move_to(to.x, to.y),
      PathCommand::LineTo(to) => {
        start(&mut builder, to);
        builder.line_to(to.x, to.y);
      }
      PathCommand::QuadTo { control, to } => {
        start(&mut builder, control);
        builder.quad_to(control.x, control.y, to.x, to.y);
      }
      PathCommand::CubicTo { first, second, to } => {
        start(&mut builder, first);
        builder.cubic_to(first.x, first.y, second.x, second.y, to.x, to.y);
      }
      PathCommand::Arc {
        center,
        radius,
        start,
        sweep,
      } => arc(&mut builder, center, radius, start, sweep),
      PathCommand::Close => builder.close(),
    }
  }
  builder.finish()
}

/// Adds an arc of the circle around `center` from `start` degrees through
/// `sweep`, clockwise from 3 o'clock, as cubic curves of at most a
/// quarter turn each, after a line from where the path is.
fn arc(builder: &mut PathBuilder, center: Point, radius: f32, start: f32, sweep: f32) {
  let point = |degrees: f32| {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (center.x + radius * cos, center.y + radius * sin)
  };
  let (x, y) = point(start);
  if builder.is_empty() {
    builder.move_to(x, y);
  } else if builder.last_point() != Some(tiny_skia::Point::from_xy(x, y)) {
    builder.line_to(x, y);
  }
  let sweep = sweep.clamp(-360.0, 360.0);
  if sweep == 0.0 || radius == 0.0 {
    return;
  }
  let parts = (sweep.abs() / 90.0).ceil();
  let step = sweep / parts;
  // How far along each end's tangent the control points sit for a cubic
  // to follow an arc of `step` closely.
  let reach = 4.0 / 3.0 * (step.to_radians() / 4.0).tan() * radius;
  for part in 0..parts as u32 {
    let from = start + step * part as f32;
    let to = start + step * (part + 1) as f32;
    let (x0, y0) = point(from);
    let (x1, y1) = point(to);
    let (sin0, cos0) = from.to_radians().sin_cos();
    let (sin1, cos1) = to.to_radians().sin_cos();
    builder.cubic_to(
      x0 - reach * sin0,
      y0 + reach * cos0,
      x1 + reach * sin1,
      y1 - reach * cos1,
      x1,
      y1,
    );
  }
}
