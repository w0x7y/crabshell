//! A style's or a canvas's gradient worked out for each pixel and
//! dithered, so a subtle one across a wide bar shows no steps.

use crabshell_ui::{Color, Gradient, GradientShape};
use tiny_skia::{
  FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Pattern, Pixmap, PixmapMut, Point,
  RadialGradient, Shader, SpreadMode, SweepGradient, Transform,
};

use super::{Painter, mul, over};

/// A 4 by 4 ordered dither: where in a step each pixel of a block rounds
/// up, in sixteenths.
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Where a gradient's colors run, in buffer pixels.
enum Laid {
  /// From `start` to `end`.
  Linear(Point, Point),
  /// Out from the middle to `reach`.
  Radial(Point, f32),
  /// Clockwise around the middle from this many degrees past 12 o'clock.
  Sweep(Point, f32),
}

/// A gradient's colors on a pixmap of their own, placed at `x`, `y` in
/// the pixmap they are drawn into.
pub(super) struct Ramp {
  pixels: Pixmap,
  x: f32,
  y: f32,
}

impl Ramp {
  /// Fills `path`, in the pixmap's pixels, with these colors. It blends
  /// them through a mask of the path itself, which is faster than
  /// tiny-skia's pattern shader. The mask covers the whole pixmap, so the
  /// path is cut only where a fill would cut it.
  pub(super) fn fill(&self, pixmap: &mut PixmapMut<'_>, path: &tiny_skia::Path) {
    let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) else {
      return;
    };
    mask.fill_path(path, FillRule::Winding, true, Transform::identity());
    let (width, stride) = (self.pixels.width() as usize, pixmap.width() as usize);
    let (x, y) = (self.x as usize, self.y as usize);
    let data = pixmap.data_mut();
    for (row, colors) in self.pixels.data().chunks_exact(width * 4).enumerate() {
      let start = (y + row) * stride + x;
      let covered = &mask.data()[start..start + width];
      for (column, (&color, &coverage)) in colors.as_chunks::<4>().0.iter().zip(covered).enumerate()
      {
        let source = match coverage {
          0 => continue,
          255 => color,
          _ => color.map(|channel| mul(channel, coverage)),
        };
        over(data, (start + column) * 4, source);
      }
    }
  }

  /// The colors as a shader, each pixel where it was worked out for.
  pub(super) fn shader(&self) -> Shader<'_> {
    Pattern::new(
      self.pixels.as_ref(),
      SpreadMode::Pad,
      FilterQuality::Nearest,
      1.0,
      Transform::from_translate(self.x, self.y),
    )
  }
}

impl Painter<'_> {
  /// `gradient` across `area`, a box in this pixmap's pixels, over the
  /// part of it inside a `width` by `height` pixmap. Each pixel's color is
  /// worked out in buffer pixels and dithered by less than a step, by its
  /// place in the buffer, so every tile works out the same color for it.
  pub(super) fn ramp(
    &self,
    gradient: &Gradient,
    area: tiny_skia::Rect,
    (width, height): (u32, u32),
  ) -> Option<Ramp> {
    let x0 = area.left().floor().max(0.0) as u32;
    let y0 = area.top().floor().max(0.0) as u32;
    let x1 = (area.right().ceil().max(0.0) as u32).min(width);
    let y1 = (area.bottom().ceil().max(0.0) as u32).min(height);
    let mut pixels = Pixmap::new(x1.checked_sub(x0)?, y1.checked_sub(y0)?)?;
    let laid = self.lay_out(gradient.shape, area);
    let stops = even(gradient);
    // The buffer pixel the ramp starts at; tiles start on whole pixels.
    let (left, top) = (
      (self.grid.left + x0 as f32).round() as i64,
      (self.grid.top + y0 as f32).round() as i64,
    );
    let columns = pixels.width() as usize;
    for (row, line) in pixels.data_mut().chunks_exact_mut(columns * 4).enumerate() {
      let y = top + row as i64;
      let cells = BAYER[y.rem_euclid(4) as usize];
      for (column, pixel) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = left + column as i64;
        let t = laid.at(Point::from_xy(x as f32 + 0.5, y as f32 + 0.5));
        let [r, g, b, a] = color_at(&stops, t);
        // Rounding moved by under half a step either way: a sixteenth
        // and a half past the floor, from the pixel's cell.
        let cell = f32::from(cells[x.rem_euclid(4) as usize]);
        let dither = (cell + 0.5) / 16.0;
        // Never negative, so `as` truncates as `floor` would, faster.
        let alpha = (a * 255.0 + 0.5) as u32;
        let channel = |value: f32| ((value * a * 255.0 + dither) as u32).min(alpha) as u8;
        *pixel = [channel(r), channel(g), channel(b), alpha as u8];
      }
    }
    Some(Ramp {
      pixels,
      x: x0 as f32,
      y: y0 as f32,
    })
  }

  /// `gradient` across `area`, a box in this pixmap's pixels, as
  /// tiny-skia's undithered shader, for a mask. It is laid out in buffer
  /// pixels and moved onto the pixmap, so every tile works out the same
  /// value for a pixel.
  pub(super) fn shader(
    &self,
    gradient: &Gradient,
    area: tiny_skia::Rect,
  ) -> Option<Shader<'static>> {
    let stops = stops(gradient)
      .into_iter()
      .map(|(at, color)| {
        let color = tiny_skia::Color::from_rgba8(color.r, color.g, color.b, color.a);
        GradientStop::new(at, color)
      })
      .collect();
    let moved = Transform::from_translate(-self.grid.left, -self.grid.top);
    match self.lay_out(gradient.shape, area) {
      Laid::Linear(start, end) => LinearGradient::new(start, end, stops, SpreadMode::Pad, moved),
      Laid::Radial(middle, reach) => {
        RadialGradient::new(middle, 0.0, middle, reach, stops, SpreadMode::Pad, moved)
      }
      Laid::Sweep(middle, angle) => {
        // tiny-skia's start at 3 o'clock, and it skips a start angle
        // other than 0 when the sweep is a whole turn, so it is turned.
        let turn = Transform::from_rotate_at(angle - 90.0, middle.x, middle.y);
        let at = moved.pre_concat(turn);
        SweepGradient::new(middle, 0.0, 360.0, stops, SpreadMode::Pad, at)
      }
    }
  }

  /// Where `shape`'s colors run across `area`, a box in this pixmap's
  /// pixels, in buffer pixels.
  fn lay_out(&self, shape: GradientShape, area: tiny_skia::Rect) -> Laid {
    let (left, top) = (area.left() + self.grid.left, area.top() + self.grid.top);
    let (width, height) = (area.width(), area.height());
    let middle = Point::from_xy(left + width / 2.0, top + height / 2.0);
    match shape {
      GradientShape::Linear(angle) => {
        // As CSS: 0 degrees points up, and the line is long enough for
        // its ends to meet the corners.
        let (sin, cos) = angle.to_radians().sin_cos();
        let half = (width * sin.abs() + height * cos.abs()) / 2.0;
        let (dx, dy) = (sin * half, -cos * half);
        let start = Point::from_xy(middle.x - dx, middle.y - dy);
        let end = Point::from_xy(middle.x + dx, middle.y + dy);
        Laid::Linear(start, end)
      }
      GradientShape::Radial => Laid::Radial(middle, (width * width + height * height).sqrt() / 2.0),
      // As CSS: 0 degrees points up and the colors go clockwise.
      GradientShape::Sweep(angle) => Laid::Sweep(middle, angle),
    }
  }
}

impl Laid {
  /// How far along the colors `point` is, from 0 to 1.
  fn at(&self, point: Point) -> f32 {
    let t = match *self {
      Laid::Linear(start, end) => {
        let (dx, dy) = (end.x - start.x, end.y - start.y);
        ((point.x - start.x) * dx + (point.y - start.y) * dy) / (dx * dx + dy * dy)
      }
      Laid::Radial(middle, reach) => middle.distance(point) / reach,
      Laid::Sweep(middle, angle) => {
        // Clockwise from 12 o'clock, with y down.
        let turned = (point.x - middle.x).atan2(middle.y - point.y).to_degrees();
        ((turned - angle) / 360.0).rem_euclid(1.0)
      }
    };
    if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) }
  }
}

/// `gradient`'s stops as tiny-skia takes them. tiny-skia blends colors
/// before their alpha, so a clear stop would fade through its own color;
/// as in CSS, each side of one fades out in its neighbor's.
fn stops(gradient: &Gradient) -> Vec<(f32, Color)> {
  let given = gradient.stops();
  let mut stops = Vec::with_capacity(given.len() * 2);
  for (index, &(at, color)) in given.iter().enumerate() {
    if color.a > 0 {
      stops.push((at, color));
      continue;
    }
    for neighbor in [index.checked_sub(1), Some(index + 1)]
      .into_iter()
      .flatten()
    {
      if let Some(&(_, near)) = given.get(neighbor) {
        stops.push((at, Color::rgba(near.r, near.g, near.b, 0)));
      }
    }
  }
  stops
}

/// [`stops`] from 0 to 1 in order, as tiny-skia evens them out, each
/// color's channels from 0 to 1, alpha not applied.
fn even(gradient: &Gradient) -> Vec<(f32, [f32; 4])> {
  let channels = |color: Color| [color.r, color.g, color.b, color.a].map(|c| f32::from(c) / 255.0);
  let given = stops(gradient);
  let mut stops = Vec::with_capacity(given.len() + 2);
  let mut last = 0.0;
  for &(at, color) in &given {
    let at = if at.is_nan() {
      0.0
    } else {
      at.clamp(last, 1.0)
    };
    if stops.is_empty() && at > 0.0 {
      stops.push((0.0, channels(color)));
    }
    stops.push((at, channels(color)));
    last = at;
  }
  if let Some(&(at, color)) = stops.last()
    && at < 1.0
  {
    stops.push((1.0, color));
  }
  stops
}

/// The color `t` of the way along `stops`, blended before its alpha is
/// applied, as tiny-skia does.
fn color_at(stops: &[(f32, [f32; 4])], t: f32) -> [f32; 4] {
  for pair in stops.windows(2) {
    let ((from, a), (to, b)) = (pair[0], pair[1]);
    if t < to {
      let f = ((t - from) / (to - from)).clamp(0.0, 1.0);
      return std::array::from_fn(|channel| a[channel] + (b[channel] - a[channel]) * f);
    }
  }
  stops.last().map_or([0.0; 4], |&(_, color)| color)
}
