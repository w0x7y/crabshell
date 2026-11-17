//! Colors, fonts and [`Style`]. Everything here is `Copy` and built with
//! `const fn`s, so a theme is a set of constants.

use crate::animation::{Animate, Transition};
use crate::geometry::{Affine, Point, Rect};
use crate::widgets::Align;

/// An sRGB color with straight (not premultiplied) alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
  /// Red, 0 to 255.
  pub r: u8,
  /// Green, 0 to 255.
  pub g: u8,
  /// Blue, 0 to 255.
  pub b: u8,
  /// Alpha: 0 is transparent, 255 is opaque.
  pub a: u8,
}

impl Color {
  /// An opaque color.
  pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
  }

  /// A color with alpha: 0 is transparent, 255 is opaque.
  pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color { r, g, b, a }
  }

  /// Parses `"#rrggbb"` or `"#rrggbbaa"`, in upper or lower case.
  ///
  /// # Panics
  ///
  /// If `hex` has any other form. In a `const`, that is a compile error:
  ///
  /// ```compile_fail
  /// const BAD: crabshell_ui::Color = crabshell_ui::Color::hex("#12345");
  /// ```
  pub const fn hex(hex: &str) -> Color {
    let bytes = hex.as_bytes();
    if bytes.is_empty() || bytes[0] != b'#' || (bytes.len() != 7 && bytes.len() != 9) {
      panic!("Color::hex takes \"#rrggbb\" or \"#rrggbbaa\"");
    }
    let a = if bytes.len() == 9 {
      hex_byte(bytes[7], bytes[8])
    } else {
      255
    };
    Color {
      r: hex_byte(bytes[1], bytes[2]),
      g: hex_byte(bytes[3], bytes[4]),
      b: hex_byte(bytes[5], bytes[6]),
      a,
    }
  }

  /// An opaque color from a hue in degrees, a saturation and a lightness
  /// from 0 to 1, as CSS's `hsl()`: `Color::hsl(210.0, 0.5, 0.4)` is
  /// `hsl(210 50% 40%)`. The hue wraps around, the others clamp, and a
  /// number that isn't finite gives opaque black.
  pub const fn hsl(hue: f32, saturation: f32, lightness: f32) -> Color {
    Color::hsla(hue, saturation, lightness, 1.0)
  }

  /// [`hsl`](Self::hsl) with an alpha from 0 (transparent) to 1 (opaque).
  pub const fn hsla(hue: f32, saturation: f32, lightness: f32, alpha: f32) -> Color {
    if !(hue.is_finite() && saturation.is_finite() && lightness.is_finite() && alpha.is_finite()) {
      return Color::rgb(0, 0, 0);
    }
    let hue = hue - (hue / 360.0).floor() * 360.0;
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    // CSS Color 4's conversion: the chroma, then where the hue puts it.
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let second = chroma * (1.0 - (sector - (sector / 2.0).floor() * 2.0 - 1.0).abs());
    let (r, g, b) = match sector as u8 {
      0 => (chroma, second, 0.0),
      1 => (second, chroma, 0.0),
      2 => (0.0, chroma, second),
      3 => (0.0, second, chroma),
      4 => (second, 0.0, chroma),
      _ => (chroma, 0.0, second),
    };
    let lowest = lightness - chroma / 2.0;
    Color {
      r: unit_byte(r + lowest),
      g: unit_byte(g + lowest),
      b: unit_byte(b + lowest),
      a: unit_byte(alpha),
    }
  }

  /// The same color with an alpha from 0 (transparent) to 1 (opaque),
  /// clamped. NaN leaves it as it is.
  pub const fn with_alpha(self, alpha: f32) -> Color {
    if alpha.is_nan() {
      return self;
    }
    Color {
      a: unit_byte(alpha),
      ..self
    }
  }

  /// `t` of the way from this color to `other`, from 0 to 1, clamped,
  /// each sRGB channel and the alpha on their own, as a style fade
  /// blends. NaN leaves this color as it is.
  pub const fn mix(self, other: Color, t: f32) -> Color {
    if t.is_nan() {
      return self;
    }
    let t = t.clamp(0.0, 1.0);
    Color {
      r: mix_channel(self.r, other.r, t),
      g: mix_channel(self.g, other.g, t),
      b: mix_channel(self.b, other.b, t),
      a: mix_channel(self.a, other.a, t),
    }
  }

  /// The same color with its HSL lightness raised by `amount`, from 0 to
  /// 1, as Sass's `lighten`: `lighter(0.1)` takes `hsl(210 50% 40%)` to
  /// `hsl(210 50% 50%)`. The amount and the lightness clamp, and NaN
  /// leaves the color as it is.
  pub const fn lighter(self, amount: f32) -> Color {
    self.lightness_by(amount.clamp(0.0, 1.0))
  }

  /// The same color with its HSL lightness lowered by `amount`, as
  /// [`lighter`](Self::lighter) raises it.
  pub const fn darker(self, amount: f32) -> Color {
    self.lightness_by(-amount.clamp(0.0, 1.0))
  }

  /// This color with `change` added to its HSL lightness.
  const fn lightness_by(self, change: f32) -> Color {
    if change.is_nan() {
      return self;
    }
    let (r, g, b) = (
      self.r as f32 / 255.0,
      self.g as f32 / 255.0,
      self.b as f32 / 255.0,
    );
    let (high, low) = (r.max(g).max(b), r.min(g).min(b));
    let lightness = (high + low) / 2.0;
    let chroma = high - low;
    let (hue, saturation) = if chroma == 0.0 {
      (0.0, 0.0)
    } else {
      let hue = if high == r {
        (g - b) / chroma
      } else if high == g {
        (b - r) / chroma + 2.0
      } else {
        (r - g) / chroma + 4.0
      };
      (hue * 60.0, chroma / (1.0 - (2.0 * lightness - 1.0).abs()))
    };
    let lightness = lightness + change;
    Color::hsla(hue, saturation, lightness, self.a as f32 / 255.0)
  }
}

/// `t` of the way from one channel to another, rounded. Closures can't
/// be called in a `const fn`.
const fn mix_channel(from: u8, to: u8, t: f32) -> u8 {
  (from as f32 + (to as f32 - from as f32) * t).round() as u8
}

/// A fraction from 0 to 1, clamped, as a channel from 0 to 255.
const fn unit_byte(value: f32) -> u8 {
  (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

const fn hex_byte(high: u8, low: u8) -> u8 {
  hex_digit(high) * 16 + hex_digit(low)
}

const fn hex_digit(digit: u8) -> u8 {
  match digit {
    b'0'..=b'9' => digit - b'0',
    b'a'..=b'f' => digit - b'a' + 10,
    b'A'..=b'F' => digit - b'A' + 10,
    _ => panic!("Color::hex found a character that isn't a hex digit"),
  }
}

/// A font: family, size, weight, slant, letter spacing and OpenType
/// features.
///
/// The family is resolved through fontconfig, so generic names such as
/// `"sans-serif"` and `"monospace"` work too.
///
/// ```
/// use crabshell_ui::{Font, FontFeature};
///
/// const CLOCK: Font = Font::new("sans-serif", 13.0).features(&[FontFeature::TABULAR_NUMBERS]);
/// const NOTE: Font = Font::new("sans-serif", 11.0).italic().letter_spacing(0.5);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
  /// The family name, such as `"Inter"` or `"sans-serif"`.
  pub family: &'static str,
  /// The size in logical pixels.
  pub size: f32,
  /// The weight, from 100 (thin) through 400 (regular) and 700 (bold) to
  /// 900 (black).
  pub weight: u16,
  /// Whether the italic face is asked for.
  pub italic: bool,
  /// Extra logical pixels after each glyph, at least minus half the
  /// size; a number that isn't finite counts as 0.
  pub letter_spacing: f32,
  /// OpenType features turned on or off, in order.
  pub features: &'static [FontFeature],
}

impl Font {
  /// The font text uses when no ancestor sets one.
  pub const DEFAULT: Font = Font::new("sans-serif", 13.0);

  /// A regular-weight font of `family` at `size` logical pixels.
  pub const fn new(family: &'static str, size: f32) -> Font {
    Font {
      family,
      size,
      weight: 400,
      italic: false,
      letter_spacing: 0.0,
      features: &[],
    }
  }

  /// The same font with another weight, such as 700 for bold.
  pub const fn weight(self, weight: u16) -> Font {
    Font { weight, ..self }
  }

  /// The same font in italic. A family without an italic face gets the
  /// face fontconfig picks, slanted by the renderer if it is upright.
  pub const fn italic(self) -> Font {
    Font {
      italic: true,
      ..self
    }
  }

  /// The same font with `px` extra logical pixels after each glyph, or
  /// less with a negative number, down to minus half the font size.
  pub const fn letter_spacing(self, px: f32) -> Font {
    Font {
      letter_spacing: px,
      ..self
    }
  }

  /// The same font with these OpenType features, such as
  /// [`FontFeature::TABULAR_NUMBERS`] for digits of one width.
  pub const fn features(self, features: &'static [FontFeature]) -> Font {
    Font { features, ..self }
  }
}

/// An OpenType feature: a four-letter tag and its value, 0 for off and
/// 1 for on, or which alternate for features that have several.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontFeature {
  /// The tag, such as `*b"tnum"`.
  pub tag: [u8; 4],
  /// The value.
  pub value: u32,
}

impl FontFeature {
  /// Digits of one width, so a clock's width holds still (`tnum`).
  pub const TABULAR_NUMBERS: FontFeature = FontFeature::new(*b"tnum", 1);
  /// No standard ligatures such as "fi" (`liga` off).
  pub const NO_LIGATURES: FontFeature = FontFeature::new(*b"liga", 0);
  /// Lowercase letters as small capitals (`smcp`).
  pub const SMALL_CAPS: FontFeature = FontFeature::new(*b"smcp", 1);
  /// A zero with a slash or dot (`zero`).
  pub const SLASHED_ZERO: FontFeature = FontFeature::new(*b"zero", 1);

  /// The feature `tag` set to `value`.
  pub const fn new(tag: [u8; 4], value: u32) -> FontFeature {
    FontFeature { tag, value }
  }
}

/// Where each line of text sits in the text's content box. Text one line
/// long and sized to itself looks the same whichever is set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
  /// At the start: the left for left-to-right text. The default.
  #[default]
  Start,
  /// In the middle.
  Center,
  /// At the end: the right for left-to-right text.
  End,
  /// Stretched to both edges by widening the spaces, except a
  /// paragraph's last line, which sits at the start.
  Justify,
}

/// A border drawn inside a widget's edge. It takes no layout space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
  /// The width in logical pixels.
  pub width: f32,
  /// The color.
  pub color: Color,
}

/// How round each corner of a widget's box is: the radius in logical
/// pixels, 0 for a square corner. Each is clamped to half the box's
/// shorter side, so two on one side never overlap.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Radii {
  /// The top-left corner.
  pub top_left: f32,
  /// The top-right corner.
  pub top_right: f32,
  /// The bottom-right corner.
  pub bottom_right: f32,
  /// The bottom-left corner.
  pub bottom_left: f32,
}

impl Radii {
  /// Every corner `radius` round.
  pub const fn all(radius: f32) -> Radii {
    Radii {
      top_left: radius,
      top_right: radius,
      bottom_right: radius,
      bottom_left: radius,
    }
  }

  /// The four, from the top left clockwise.
  pub const fn corners(&self) -> [f32; 4] {
    [
      self.top_left,
      self.top_right,
      self.bottom_right,
      self.bottom_left,
    ]
  }

  /// `t` of the way to `to`, each corner on its own.
  pub(crate) fn lerp(&self, to: &Radii, t: f32) -> Radii {
    Radii {
      top_left: self.top_left.lerp(&to.top_left, t),
      top_right: self.top_right.lerp(&to.top_right, t),
      bottom_right: self.bottom_right.lerp(&to.bottom_right, t),
      bottom_left: self.bottom_left.lerp(&to.bottom_left, t),
    }
  }

  /// Whether `point` is inside `rect` with these corners, each clamped
  /// to half its shorter side, after `inset` is taken off every side and
  /// every corner.
  pub(crate) fn contains(&self, rect: Rect, inset: f32, point: Point) -> bool {
    let most = rect.width.min(rect.height) / 2.0;
    let inner = Rect::new(
      rect.x + inset,
      rect.y + inset,
      rect.width - 2.0 * inset,
      rect.height - 2.0 * inset,
    );
    if !inner.contains(point) {
      return false;
    }
    let [top_left, top_right, bottom_right, bottom_left] = self
      .corners()
      .map(|r| (if r.is_nan() { 0.0 } else { r.clamp(0.0, most) } - inset).max(0.0));
    let (left, top, right, bottom) = (inner.x, inner.y, inner.right(), inner.bottom());
    // Each corner's circle, and the side of its middle the corner is on.
    [
      (top_left, left + top_left, top + top_left, -1.0, -1.0),
      (top_right, right - top_right, top + top_right, 1.0, -1.0),
      (
        bottom_right,
        right - bottom_right,
        bottom - bottom_right,
        1.0,
        1.0,
      ),
      (
        bottom_left,
        left + bottom_left,
        bottom - bottom_left,
        -1.0,
        1.0,
      ),
    ]
    .into_iter()
    .all(|(r, cx, cy, sx, sy)| {
      let (dx, dy) = (point.x - cx, point.y - cy);
      r <= 0.0 || dx * sx <= 0.0 || dy * sy <= 0.0 || dx.hypot(dy) <= r
    })
  }
}

/// Colors that blend into each other across a widget's box, drawn over
/// its background with the same corners.
///
/// ```
/// use crabshell_ui::{Color, Gradient, Style};
///
/// const DUSK: Style = Style::new().gradient(
///     Gradient::linear(90.0, Color::hex("#1e1e2e"), Color::hex("#45475a"))
///         .stop(0.5, Color::hex("#313244")),
/// );
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gradient {
  /// Which way the colors run.
  pub shape: GradientShape,
  /// The first `count` hold each color and where it sits, from 0 at the
  /// start to 1 at the end.
  stops: [(f32, Color); 4],
  count: u8,
}

/// Which way a [`Gradient`]'s colors run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientShape {
  /// Along a line through the middle at this many degrees, as in CSS: 0
  /// runs from the bottom up, 90 from left to right, 180 from the top
  /// down. Its ends meet the corners.
  Linear(f32),
  /// In circles out from the middle to the farthest corner.
  Radial,
  /// Around the middle, clockwise from this many degrees, as CSS's
  /// `conic-gradient(from …)`: 0 starts at 12 o'clock, 90 at 3.
  Sweep(f32),
}

impl Gradient {
  /// A gradient from `from` to `to` along a line at `angle` degrees: 0
  /// runs from the bottom up, 90 from left to right.
  pub const fn linear(angle: f32, from: Color, to: Color) -> Gradient {
    Gradient::new(GradientShape::Linear(angle), from, to)
  }

  /// A gradient from `from` in the middle to `to` at the farthest corner.
  pub const fn radial(from: Color, to: Color) -> Gradient {
    Gradient::new(GradientShape::Radial, from, to)
  }

  /// A gradient from `from` to `to` once around the middle, clockwise
  /// from `angle` degrees: 0 starts at 12 o'clock, 90 at 3, as CSS's
  /// `conic-gradient(from 90deg, …)`. The end meets the start there.
  ///
  /// ```
  /// use crabshell_ui::{Color, Gradient, Style};
  ///
  /// const RING: Style = Style::new().radius(20.0).gradient(
  ///     Gradient::sweep(0.0, Color::hex("#89b4fa"), Color::hex("#89b4fa"))
  ///         .stop(0.5, Color::hex("#f5c2e7")),
  /// );
  /// ```
  pub const fn sweep(angle: f32, from: Color, to: Color) -> Gradient {
    Gradient::new(GradientShape::Sweep(angle), from, to)
  }

  const fn new(shape: GradientShape, from: Color, to: Color) -> Gradient {
    Gradient {
      shape,
      stops: [(0.0, from), (1.0, to), (1.0, to), (1.0, to)],
      count: 2,
    }
  }

  /// Adds `color` at `at`, from 0 to 1, before the end color. Add them
  /// in order. A gradient has at most four colors, so a third stop
  /// panics.
  pub const fn stop(self, at: f32, color: Color) -> Gradient {
    assert!(self.count < 4, "a gradient has at most four colors");
    let mut stops = self.stops;
    let last = self.count as usize - 1;
    stops[last + 1] = stops[last];
    stops[last] = (at, color);
    Gradient {
      stops,
      count: self.count + 1,
      ..self
    }
  }

  /// Each color and where it sits, from the start to the end.
  pub fn stops(&self) -> &[(f32, Color)] {
    &self.stops[..usize::from(self.count)]
  }

  /// `t` of the way from this gradient to `to`, or `None` when their
  /// shapes or numbers of colors differ.
  fn lerp(&self, to: &Gradient, t: f32) -> Option<Gradient> {
    let shape = match (self.shape, to.shape) {
      (GradientShape::Linear(from), GradientShape::Linear(to)) => {
        GradientShape::Linear(from.lerp(&to, t))
      }
      (GradientShape::Radial, GradientShape::Radial) => GradientShape::Radial,
      (GradientShape::Sweep(from), GradientShape::Sweep(to)) => {
        GradientShape::Sweep(from.lerp(&to, t))
      }

      _ => return None,
    };
    if self.count != to.count {
      return None;
    }
    let mut stops = to.stops;
    for (stop, (from, to)) in stops.iter_mut().zip(self.stops.iter().zip(to.stops)) {
      *stop = (from.0.lerp(&to.0, t), from.1.lerp(&to.1, t));
    }
    Some(Gradient {
      shape,
      stops,
      count: to.count,
    })
  }

  /// This gradient with every color transparent.
  fn clear(mut self) -> Gradient {
    for (_, color) in &mut self.stops {
      color.a = 0;
    }
    self
  }
}

/// A shadow cast by a widget's box, as CSS's `box-shadow`: the box's
/// shape moved by `x` and `y`, grown by `spread` and blurred over `blur`
/// logical pixels. Unlike CSS's, it is drawn under the box too, so a
/// translucent background shows it through, unless it is
/// [`cut_out`](Shadow::cut_out).
///
/// ```
/// use crabshell_ui::{Color, Shadow, Style};
///
/// const CARD: Style = Style::new()
///     .background(Color::hex("#1e1e2e"))
///     .radius(8.0)
///     .shadow(Shadow::new(0.0, 4.0, 12.0, Color::rgba(0, 0, 0, 96)));
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
  /// How far right it is moved.
  pub x: f32,
  /// How far down it is moved.
  pub y: f32,
  /// How far its edge is blurred. 0 is a sharp edge.
  pub blur: f32,
  /// How much larger than the box it is on each side, or smaller when
  /// negative.
  pub spread: f32,
  /// Its color.
  pub color: Color,
  /// Whether it is left out under the box, as under translucent glass.
  pub cut_out: bool,
  /// Whether it is cast by what the widget and its children draw,
  /// through their alpha, instead of by its box.
  pub follow_content: bool,
}

impl Shadow {
  /// A shadow moved by `x` and `y` and blurred over `blur`, the box's
  /// size.
  pub const fn new(x: f32, y: f32, blur: f32, color: Color) -> Shadow {
    Shadow {
      x,
      y,
      blur,
      spread: 0.0,
      color,
      cut_out: false,
      follow_content: false,
    }
  }

  /// The same shadow `spread` larger on each side.
  pub const fn spread(self, spread: f32) -> Shadow {
    Shadow { spread, ..self }
  }

  /// The same shadow left out under the box's own shape, so a translucent
  /// background, such as glass the compositor blurs, shows what is
  /// behind it instead of the shadow. CSS's `box-shadow` is always drawn
  /// this way.
  pub const fn cut_out(self) -> Shadow {
    Shadow {
      cut_out: true,
      ..self
    }
  }

  /// The same shadow cast by what the widget and everything inside it
  /// draw, through their alpha, instead of by its box, so text and icons
  /// get a drop shadow, as Qt's `MultiEffect` shadow gives. It is drawn
  /// under them, offset, spread and blurred as a box shadow is, and
  /// after a [`mask`](Style::mask), so it follows the masked shape. Cut
  /// out, it is left out under the content's own alpha.
  ///
  /// ```
  /// use crabshell_ui::{Color, Shadow, Style};
  ///
  /// const LABEL: Style = Style::new()
  ///     .color(Color::hex("#cdd6f4"))
  ///     .shadow(Shadow::new(0.0, 1.0, 3.0, Color::rgba(0, 0, 0, 160)).follow_content());
  /// ```
  pub const fn follow_content(self) -> Shadow {
    Shadow {
      follow_content: true,
      ..self
    }
  }

  /// Whether this shadow is cast by the content and can be: its numbers
  /// are finite and it reaches at most 64 logical pixels past what casts
  /// it, the size of its spread plus one and a half blurs. One reaching
  /// farther is left out whole and the content drawn without it, so the
  /// work of casting it stays bounded.
  pub fn casts_from_content(&self) -> bool {
    let reach = self.spread.abs() + 1.5 * self.blur.max(0.0);
    self.follow_content && [self.x, self.y, reach].iter().all(|n| n.is_finite()) && reach <= 64.0
  }

  /// The area this shadow of `rect` covers: its shape, grown by one and a
  /// half blurs, past which the blur fades below a 255th. A spread that
  /// swallows the box still reaches between the edges it would have, so
  /// a fade from it stays within its start and end. A shadow with a
  /// number that isn't finite is never drawn, so it reaches only `rect`.
  pub(crate) fn reach(&self, rect: Rect) -> Rect {
    let grow = self.spread + 1.5 * self.blur.max(0.0);
    if ![self.x, self.y, grow].iter().all(|n| n.is_finite()) {
      return rect;
    }
    let (left, right) = (rect.x + self.x - grow, rect.right() + self.x + grow);
    let (top, bottom) = (rect.y + self.y - grow, rect.bottom() + self.y + grow);
    Rect::new(
      left.min(right),
      top.min(bottom),
      (right - left).abs(),
      (bottom - top).abs(),
    )
  }

  /// `t` of the way to `to`.
  fn lerp(&self, to: &Shadow, t: f32) -> Shadow {
    Shadow {
      x: self.x.lerp(&to.x, t),
      y: self.y.lerp(&to.y, t),
      blur: self.blur.lerp(&to.blur, t),
      spread: self.spread.lerp(&to.spread, t),
      color: self.color.lerp(&to.color, t),
      // Not numbers: they change at once.
      cut_out: to.cut_out,
      follow_content: to.follow_content,
    }
  }

  /// This shadow, transparent.
  fn clear(self) -> Shadow {
    Shadow {
      color: Color::rgba(self.color.r, self.color.g, self.color.b, 0),
      ..self
    }
  }
}

/// Where a widget and everything inside it show, as Qt's `OpacityMask`:
/// what they draw is kept only where the mask is opaque, its alpha
/// multiplying theirs, and nothing shows outside the widget's box. Set it
/// with [`Style::mask`].
///
/// ```
/// use crabshell_ui::{Color, Gradient, Mask, Style};
///
/// const AVATAR: Style = Style::new().mask(Mask::ellipse());
/// // A list that fades out over its last fifth.
/// const FADING: Style = Style::new().mask(Mask::gradient(
///     Gradient::linear(180.0, Color::rgb(0, 0, 0), Color::rgba(0, 0, 0, 0))
///         .stop(0.8, Color::rgb(0, 0, 0)),
/// ));
/// const STAR: Style = Style::new().mask(Mask::image("/usr/share/my-shell/star.png"));
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mask {
  /// An ellipse filling the box.
  Ellipse,
  /// A gradient's alpha, laid over the box as a background's is.
  Gradient(Gradient),
  /// A file's alpha, stretched over the box. It is read through the
  /// [`Image`](crate::Image) loaders and their caps; one that is missing
  /// or refused is logged once and the widget is drawn unmasked.
  Image(&'static str),
}

impl Mask {
  /// An ellipse filling the box, for round avatars and album art.
  pub const fn ellipse() -> Mask {
    Mask::Ellipse
  }

  /// `gradient`'s alpha laid over the box, for lists and strips that fade
  /// out at an edge. Only the colors' alpha counts.
  pub const fn gradient(gradient: Gradient) -> Mask {
    Mask::Gradient(gradient)
  }

  /// The alpha of the image file at `path`, stretched over the box.
  ///
  /// The path is `&'static str`, so a style holding it can be a `const`.
  /// For a path made at run time, leak it once, outside any closure: a
  /// `String::leak` inside a `move || Style` binding leaks again each
  /// time the binding runs.
  ///
  /// ```
  /// use crabshell_ui::{Mask, Stack, Style};
  ///
  /// let home = std::env::var("HOME").unwrap_or_default();
  /// // Once, here, not in the closure below.
  /// let face: &'static str = format!("{home}/.face").leak();
  /// let avatar = Stack::new().style(move || Style::new().mask(Mask::image(face)));
  /// ```
  pub const fn image(path: &'static str) -> Mask {
    Mask::Image(path)
  }
}

/// Space between a widget's edge and its content, in logical pixels, on
/// each side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Padding {
  /// Above the content.
  pub top: u32,
  /// Right of the content.
  pub right: u32,
  /// Below the content.
  pub bottom: u32,
  /// Left of the content.
  pub left: u32,
}

/// How a widget looks. Every field is optional, and an unset field falls back
/// to the widget's default: no background, black text in
/// [`Font::DEFAULT`], no padding, square corners, no border, fully opaque.
///
/// [`Text`](crate::Text) takes `color`, `font`, `text_align`,
/// `line_height`, `underline` and `strikethrough` from the nearest widget
/// that sets each, starting with itself and going up. Nothing else
/// inherits.
///
/// `font`, `text_align`, `line_height`, `padding`, `clip`, the sizes, the
/// flex fields, the alignment, the absolute position and `span` change the
/// layout, at once; the other fields only change how the widget is
/// painted. A number that isn't finite counts as unset.
///
/// `translate`, `scale`, `rotate` and `origin` move the widget and
/// everything inside it where it is drawn and where the pointer finds it,
/// without moving anything else: the layout, and popups anchored to the
/// widget, keep its box where it was laid out.
///
/// ```
/// use crabshell_ui::{Color, Font, Style};
///
/// const BAR: Style = Style::new()
///     .background(Color::rgb(0x1e, 0x1e, 0x2e))
///     .color(Color::hex("#cdd6f4"))
///     .font(Font::new("Inter", 13.0));
/// const WS: Style = Style::new().padding(2, 8).radius(4.0);
/// const WS_FOCUSED: Style = WS.background(Color::rgb(0x45, 0x47, 0x5a));
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
  /// Fills the widget's rectangle.
  pub background: Option<Color>,
  /// Colors blending across the widget's rectangle, over the background.
  pub gradient: Option<Gradient>,
  /// A shadow under the widget's box.
  pub shadow: Option<Shadow>,
  /// The text color, inherited by [`Text`](crate::Text).
  pub color: Option<Color>,
  /// The text font, inherited by [`Text`](crate::Text).
  pub font: Option<Font>,
  /// Where text lines sit in their box, inherited by
  /// [`Text`](crate::Text).
  pub text_align: Option<TextAlign>,
  /// The height of a line of text, as a multiple of the font size,
  /// inherited by [`Text`](crate::Text). 1.2 by default.
  pub line_height: Option<f32>,
  /// Whether text is underlined, inherited by [`Text`](crate::Text).
  pub underline: Option<bool>,
  /// Whether text is struck through, inherited by [`Text`](crate::Text).
  pub strikethrough: Option<bool>,
  /// Space between the edge and the content.
  pub padding: Option<Padding>,
  /// How round each corner is, in logical pixels.
  pub radius: Option<Radii>,
  /// A border inside the edge.
  pub border: Option<Border>,
  /// Dashes for the border, `on` and `off` logical pixels along its
  /// middle line, or a solid border when unset. See
  /// [`border_dash`](Self::border_dash).
  pub border_dash: Option<(f32, f32)>,
  /// From 0.0 (invisible) to 1.0 (opaque), applied to the widget and
  /// everything inside it.
  pub opacity: Option<f32>,
  /// Where the widget and everything inside it show.
  pub mask: Option<Mask>,
  /// Whether the children are cut to the inside of the border, following
  /// the rounded corners, like CSS's `overflow: hidden`.
  pub clip: Option<bool>,
  /// The width in logical pixels, instead of what the content needs.
  pub width: Option<f32>,
  /// The height in logical pixels, instead of what the content needs.
  pub height: Option<f32>,
  /// The least width in logical pixels.
  pub min_width: Option<f32>,
  /// The least height in logical pixels.
  pub min_height: Option<f32>,
  /// The most width in logical pixels.
  pub max_width: Option<f32>,
  /// The most height in logical pixels.
  pub max_height: Option<f32>,
  /// How much of a row's or column's free space it takes, against its
  /// siblings', as CSS's `flex-grow`. 0 by default.
  pub grow: Option<f32>,
  /// How much it gives up when a row or column is too small, against its
  /// siblings', as CSS's `flex-shrink`. 1 by default.
  pub shrink: Option<f32>,
  /// Its size along a row or column before growing or shrinking, in
  /// logical pixels, as CSS's `flex-basis`.
  pub basis: Option<f32>,
  /// How it is placed across its row's or column's direction, or
  /// vertically in a stack's or grid's cell, instead of the parent's
  /// `align`.
  pub align_self: Option<Align>,
  /// How it is placed horizontally in a stack's or grid's cell. Rows and
  /// columns ignore it, as CSS's flexbox does.
  pub justify_self: Option<Align>,
  /// Logical pixels from its parent's left edge, inside the border.
  /// Any of `left`, `top`, `right` and `bottom` takes the widget out of
  /// its parent's flow, as CSS's `position: absolute`: it is placed from
  /// those edges and no longer sizes its parent.
  pub left: Option<f32>,
  /// Logical pixels from its parent's top edge, as `left`.
  pub top: Option<f32>,
  /// Logical pixels from its parent's right edge, as `left`.
  pub right: Option<f32>,
  /// Logical pixels from its parent's bottom edge, as `left`.
  pub bottom: Option<f32>,
  /// The order it is drawn and found by the pointer among its siblings,
  /// lowest first; siblings of equal `z` keep their order, later on top.
  /// 0 by default. It changes no layout.
  pub z: Option<i32>,
  /// The columns and rows of a [`Grid`](crate::Grid) it takes, each at
  /// least 1.
  pub span: Option<(u32, u32)>,
  /// How far the widget is moved, right and down, in logical pixels,
  /// after it is scaled and turned.
  pub translate: Option<Point>,
  /// How many times its size it is drawn, around `origin`. 0 draws
  /// nothing, and a negative scale also turns it upside down.
  pub scale: Option<f32>,
  /// How many degrees it is turned clockwise, around `origin`.
  pub rotate: Option<f32>,
  /// The point it is scaled and turned around, as fractions of its box:
  /// `(0, 0)` is the top-left corner, and the default `(0.5, 0.5)` the
  /// middle.
  pub origin: Option<Point>,
  /// How a change to the paint fields fades in: background, gradient,
  /// shadow, color, border, radius, opacity and the transform. Other
  /// fields change at once.
  pub transition: Option<Transition>,
  /// How a button's style fades back to its own once no hover, pressed
  /// or focus style is laid over it; `transition` when unset.
  pub transition_out: Option<Transition>,
}

impl Style {
  /// A style that sets nothing.
  pub const fn new() -> Style {
    Style {
      background: None,
      gradient: None,
      shadow: None,
      color: None,
      font: None,
      text_align: None,
      line_height: None,
      underline: None,
      strikethrough: None,
      padding: None,
      radius: None,
      border: None,
      border_dash: None,
      opacity: None,
      mask: None,
      clip: None,
      width: None,
      height: None,
      min_width: None,
      min_height: None,
      max_width: None,
      max_height: None,
      grow: None,
      shrink: None,
      basis: None,
      align_self: None,
      justify_self: None,
      left: None,
      top: None,
      right: None,
      bottom: None,
      z: None,
      span: None,
      translate: None,
      scale: None,
      rotate: None,
      origin: None,
      transition: None,
      transition_out: None,
    }
  }

  /// Sets the background color.
  pub const fn background(self, color: Color) -> Style {
    Style {
      background: Some(color),
      ..self
    }
  }

  /// Sets a gradient, drawn over the background.
  pub const fn gradient(self, gradient: Gradient) -> Style {
    Style {
      gradient: Some(gradient),
      ..self
    }
  }

  /// Sets a shadow under the box.
  pub const fn shadow(self, shadow: Shadow) -> Style {
    Style {
      shadow: Some(shadow),
      ..self
    }
  }

  /// Sets the text color.
  pub const fn color(self, color: Color) -> Style {
    Style {
      color: Some(color),
      ..self
    }
  }

  /// Sets the text font.
  pub const fn font(self, font: Font) -> Style {
    Style {
      font: Some(font),
      ..self
    }
  }

  /// Sets where text lines sit in their box. It shows on text given a
  /// width, a `grow` or more than one line.
  pub const fn text_align(self, align: TextAlign) -> Style {
    Style {
      text_align: Some(align),
      ..self
    }
  }

  /// Sets the height of a line of text as a multiple of the font size,
  /// rounded to whole logical pixels. A number that isn't finite or
  /// isn't above 0 counts as unset.
  pub const fn line_height(self, multiple: f32) -> Style {
    Style {
      line_height: Some(multiple),
      ..self
    }
  }

  /// Sets whether text is underlined, in its color.
  pub const fn underline(self, on: bool) -> Style {
    Style {
      underline: Some(on),
      ..self
    }
  }

  /// Sets whether text is struck through, in its color.
  pub const fn strikethrough(self, on: bool) -> Style {
    Style {
      strikethrough: Some(on),
      ..self
    }
  }

  /// Sets the padding: `vertical` above and below, `horizontal` left and
  /// right, in logical pixels.
  pub const fn padding(self, vertical: u32, horizontal: u32) -> Style {
    Style {
      padding: Some(Padding {
        top: vertical,
        right: horizontal,
        bottom: vertical,
        left: horizontal,
      }),
      ..self
    }
  }

  /// Sets the padding above the content, in logical pixels, keeping the
  /// other sides this style sets, or 0 where it sets none. As with every
  /// field, [`merge`](Self::merge) takes the whole padding of the style on
  /// top, so a hover style built from `Style::new()` sets the rest to 0.
  ///
  /// ```
  /// use crabshell_ui::Style;
  ///
  /// const PILL: Style = Style::new().padding(2, 8).padding_left(12);
  /// ```
  pub const fn padding_top(self, top: u32) -> Style {
    let mut padding = self.sides();
    padding.top = top;
    Style {
      padding: Some(padding),
      ..self
    }
  }

  /// Sets the padding right of the content, keeping the other sides, as
  /// [`padding_top`](Self::padding_top) does.
  pub const fn padding_right(self, right: u32) -> Style {
    let mut padding = self.sides();
    padding.right = right;
    Style {
      padding: Some(padding),
      ..self
    }
  }

  /// Sets the padding below the content, keeping the other sides, as
  /// [`padding_top`](Self::padding_top) does.
  pub const fn padding_bottom(self, bottom: u32) -> Style {
    let mut padding = self.sides();
    padding.bottom = bottom;
    Style {
      padding: Some(padding),
      ..self
    }
  }

  /// Sets the padding left of the content, keeping the other sides, as
  /// [`padding_top`](Self::padding_top) does.
  pub const fn padding_left(self, left: u32) -> Style {
    let mut padding = self.sides();
    padding.left = left;
    Style {
      padding: Some(padding),
      ..self
    }
  }

  /// The padding this style sets, or 0 on every side.
  const fn sides(self) -> Padding {
    match self.padding {
      Some(padding) => padding,
      None => Padding {
        top: 0,
        right: 0,
        bottom: 0,
        left: 0,
      },
    }
  }

  /// Rounds every corner by `radius` logical pixels.
  pub const fn radius(self, radius: f32) -> Style {
    Style {
      radius: Some(Radii::all(radius)),
      ..self
    }
  }

  /// Rounds each corner on its own, in logical pixels, as CSS's
  /// `border-radius` with four values. Each is clamped to half the box's
  /// shorter side. Everything that follows the corners follows each one:
  /// the background, gradient, border, shadow, a clip and the scroll
  /// bar's cut.
  ///
  /// ```
  /// use crabshell_ui::Style;
  ///
  /// // A tab, round on top only.
  /// const TAB: Style = Style::new().radii(8.0, 8.0, 0.0, 0.0);
  /// ```
  pub const fn radii(
    self,
    top_left: f32,
    top_right: f32,
    bottom_right: f32,
    bottom_left: f32,
  ) -> Style {
    Style {
      radius: Some(Radii {
        top_left,
        top_right,
        bottom_right,
        bottom_left,
      }),
      ..self
    }
  }

  /// Sets a border of `width` logical pixels.
  pub const fn border(self, width: f32, color: Color) -> Style {
    Style {
      border: Some(Border { width, color }),
      ..self
    }
  }

  /// Dashes the border: `on` logical pixels drawn, `off` left out, along
  /// the middle of the border, so the dashes follow its corners. An `on`
  /// of 0 draws round dots the border's width across, `off` apart from
  /// middle to middle. The dashes are stretched or squeezed a little so a
  /// whole number of them goes around. A number that isn't finite, a
  /// negative one, an `off` of 0 or a pattern shorter than a pixel draws
  /// the border solid. It is a field of its own, so a hover style can
  /// dash a border the style under it sets, and with no border it draws
  /// nothing. A fade takes the end style's dash at once.
  ///
  /// ```
  /// use crabshell_ui::{Color, Style};
  ///
  /// const DROP_ZONE: Style = Style::new()
  ///     .border(2.0, Color::hex("#89b4fa"))
  ///     .border_dash(6.0, 4.0)
  ///     .radius(8.0);
  /// const DOTTED: Style = DROP_ZONE.border_dash(0.0, 5.0);
  /// ```
  pub const fn border_dash(self, on: f32, off: f32) -> Style {
    Style {
      border_dash: Some((on, off)),
      ..self
    }
  }

  /// Sets the opacity, from 0.0 to 1.0.
  pub const fn opacity(self, opacity: f32) -> Style {
    Style {
      opacity: Some(opacity),
      ..self
    }
  }

  /// Keeps the widget and everything inside it only where `mask` is
  /// opaque, as Qt's `OpacityMask`. They are drawn into one layer, the
  /// one opacity and a transform use, so stacking them costs no extra
  /// layer, and a clip still cuts inside it. A fade takes the end
  /// style's mask at once.
  pub const fn mask(self, mask: Mask) -> Style {
    Style {
      mask: Some(mask),
      ..self
    }
  }

  /// Sets whether the children are cut to the inside of the border,
  /// following the rounded corners. Nothing is cut by default.
  pub const fn clip(self, clip: bool) -> Style {
    Style {
      clip: Some(clip),
      ..self
    }
  }

  /// Sets the width in logical pixels, instead of what the content needs.
  pub const fn width(self, width: f32) -> Style {
    Style {
      width: Some(width),
      ..self
    }
  }

  /// Sets the height in logical pixels, instead of what the content
  /// needs.
  pub const fn height(self, height: f32) -> Style {
    Style {
      height: Some(height),
      ..self
    }
  }

  /// Sets the least width in logical pixels.
  pub const fn min_width(self, width: f32) -> Style {
    Style {
      min_width: Some(width),
      ..self
    }
  }

  /// Sets the least height in logical pixels.
  pub const fn min_height(self, height: f32) -> Style {
    Style {
      min_height: Some(height),
      ..self
    }
  }

  /// Sets the most width in logical pixels.
  pub const fn max_width(self, width: f32) -> Style {
    Style {
      max_width: Some(width),
      ..self
    }
  }

  /// Sets the most height in logical pixels.
  pub const fn max_height(self, height: f32) -> Style {
    Style {
      max_height: Some(height),
      ..self
    }
  }

  /// Sets how much of a row's or column's free space the widget takes,
  /// against its siblings': two children with a grow of 1 and 3 take a
  /// quarter and three quarters. A [`Spacer`](crate::Spacer) grows by 1.
  ///
  /// ```
  /// use crabshell_ui::Style;
  ///
  /// const FILL: Style = Style::new().grow(1.0).basis(0.0);
  /// ```
  pub const fn grow(self, grow: f32) -> Style {
    Style {
      grow: Some(grow),
      ..self
    }
  }

  /// Sets how much the widget gives up, against its siblings', when its
  /// row or column is too small for them all. 0 keeps its size.
  pub const fn shrink(self, shrink: f32) -> Style {
    Style {
      shrink: Some(shrink),
      ..self
    }
  }

  /// Sets the widget's size along its row or column, in logical pixels,
  /// before it grows or shrinks.
  pub const fn basis(self, basis: f32) -> Style {
    Style {
      basis: Some(basis),
      ..self
    }
  }

  /// Sets how the widget is placed across its row's or column's
  /// direction, or vertically in a stack's or grid's cell, instead of
  /// the parent's `align`.
  pub const fn align_self(self, align: Align) -> Style {
    Style {
      align_self: Some(align),
      ..self
    }
  }

  /// Sets how the widget is placed horizontally in a stack's or grid's
  /// cell. Rows and columns ignore it.
  pub const fn justify_self(self, justify: Align) -> Style {
    Style {
      justify_self: Some(justify),
      ..self
    }
  }

  /// Places the widget `left` logical pixels from its parent's left edge,
  /// inside the border, out of the parent's flow.
  ///
  /// ```
  /// use crabshell_ui::Style;
  ///
  /// // A badge in a stack's top-right corner.
  /// const BADGE: Style = Style::new().top(2.0).right(2.0);
  /// ```
  pub const fn left(self, left: f32) -> Style {
    Style {
      left: Some(left),
      ..self
    }
  }

  /// Places the widget `top` logical pixels from its parent's top edge,
  /// as [`left`](Self::left) does.
  pub const fn top(self, top: f32) -> Style {
    Style {
      top: Some(top),
      ..self
    }
  }

  /// Places the widget `right` logical pixels from its parent's right
  /// edge, as [`left`](Self::left) does.
  pub const fn right(self, right: f32) -> Style {
    Style {
      right: Some(right),
      ..self
    }
  }

  /// Places the widget `bottom` logical pixels from its parent's bottom
  /// edge, as [`left`](Self::left) does.
  pub const fn bottom(self, bottom: f32) -> Style {
    Style {
      bottom: Some(bottom),
      ..self
    }
  }

  /// Sets the order the widget is drawn and hit in among its siblings,
  /// lowest first. Equal ones keep their order.
  pub const fn z(self, z: i32) -> Style {
    Style { z: Some(z), ..self }
  }

  /// Makes the widget take `columns` columns and `rows` rows of its
  /// [`Grid`](crate::Grid). 0 counts as 1.
  pub const fn span(self, columns: u32, rows: u32) -> Style {
    Style {
      span: Some((columns, rows)),
      ..self
    }
  }

  /// Moves the widget and everything inside it `x` logical pixels right
  /// and `y` down where it is drawn, after it is scaled and turned. The
  /// layout doesn't move.
  ///
  /// ```
  /// use crabshell_ui::Style;
  ///
  /// const HIDDEN: Style = Style::new().translate(0.0, -40.0);
  /// ```
  pub const fn translate(self, x: f32, y: f32) -> Style {
    Style {
      translate: Some(Point::new(x, y)),
      ..self
    }
  }

  /// Draws the widget and everything inside it `scale` times its size,
  /// around its [`origin`](Self::origin), without changing the layout.
  /// 0 draws nothing and is never under the pointer; a negative scale
  /// also turns it upside down. A number that isn't finite is ignored.
  pub const fn scale(self, scale: f32) -> Style {
    Style {
      scale: Some(scale),
      ..self
    }
  }

  /// Turns the widget and everything inside it `degrees` clockwise,
  /// around its [`origin`](Self::origin), without changing the layout. A
  /// number that isn't finite is ignored.
  pub const fn rotate(self, degrees: f32) -> Style {
    Style {
      rotate: Some(degrees),
      ..self
    }
  }

  /// Sets the point the widget is scaled and turned around, as fractions
  /// of its box: `(0.0, 0.0)` is the top-left corner, `(1.0, 1.0)` the
  /// bottom-right. The default is the middle, `(0.5, 0.5)`.
  pub const fn origin(self, x: f32, y: f32) -> Style {
    Style {
      origin: Some(Point::new(x, y)),
      ..self
    }
  }

  /// Sets how a change to the paint fields fades in, as when a hover
  /// style takes over: background, gradient, shadow, color, border,
  /// radius, opacity, translate, scale, rotate and origin move over
  /// `transition`. The style being changed to says how.
  pub const fn transition(self, transition: Transition) -> Style {
    Style {
      transition: Some(transition),
      ..self
    }
  }

  /// Sets how a button's style fades back to its own, once no hover,
  /// pressed or focus style is laid over it, so the way out can take a
  /// different curve from the way in. Without it the way back takes
  /// `transition` too. The style being changed to says how.
  pub const fn transition_out(self, transition: Transition) -> Style {
    Style {
      transition_out: Some(transition),
      ..self
    }
  }

  /// This style with every field that `over` sets replaced by its value.
  /// Buttons use it to lay their hover and pressed styles over the normal
  /// one.
  pub const fn merge(self, over: Style) -> Style {
    Style {
      background: or(over.background, self.background),
      gradient: or(over.gradient, self.gradient),
      shadow: or(over.shadow, self.shadow),
      color: or(over.color, self.color),
      font: or(over.font, self.font),
      text_align: or(over.text_align, self.text_align),
      line_height: or(over.line_height, self.line_height),
      underline: or(over.underline, self.underline),
      strikethrough: or(over.strikethrough, self.strikethrough),
      padding: or(over.padding, self.padding),
      radius: or(over.radius, self.radius),
      border: or(over.border, self.border),
      border_dash: or(over.border_dash, self.border_dash),
      opacity: or(over.opacity, self.opacity),
      mask: or(over.mask, self.mask),
      clip: or(over.clip, self.clip),
      width: or(over.width, self.width),
      height: or(over.height, self.height),
      min_width: or(over.min_width, self.min_width),
      min_height: or(over.min_height, self.min_height),
      max_width: or(over.max_width, self.max_width),
      max_height: or(over.max_height, self.max_height),
      grow: or(over.grow, self.grow),
      shrink: or(over.shrink, self.shrink),
      basis: or(over.basis, self.basis),
      align_self: or(over.align_self, self.align_self),
      justify_self: or(over.justify_self, self.justify_self),
      left: or(over.left, self.left),
      top: or(over.top, self.top),
      right: or(over.right, self.right),
      bottom: or(over.bottom, self.bottom),
      z: or(over.z, self.z),
      span: or(over.span, self.span),
      translate: or(over.translate, self.translate),
      scale: or(over.scale, self.scale),
      rotate: or(over.rotate, self.rotate),
      origin: or(over.origin, self.origin),
      transition: or(over.transition, self.transition),
      transition_out: or(over.transition_out, self.transition_out),
    }
  }

  /// Whether the children are cut to the widget's box.
  pub fn clips(&self) -> bool {
    self.clip == Some(true)
  }

  /// Whether the two styles lay out the same: same font, text alignment
  /// and line height, padding, sizes, flex fields, alignment, position
  /// and span, and clipping, which changes how far the children's
  /// painting reaches.
  pub(crate) fn same_layout(&self, other: &Style) -> bool {
    self.font == other.font
      && self.text_align == other.text_align
      && self.line_height == other.line_height
      && self.padding == other.padding
      && self.width == other.width
      && self.height == other.height
      && self.min_width == other.min_width
      && self.min_height == other.min_height
      && self.max_width == other.max_width
      && self.max_height == other.max_height
      && self.grow == other.grow
      && self.shrink == other.shrink
      && self.basis == other.basis
      && self.align_self == other.align_self
      && self.justify_self == other.justify_self
      && self.left == other.left
      && self.top == other.top
      && self.right == other.right
      && self.bottom == other.bottom
      && self.span == other.span
      && self.clips() == other.clips()
  }

  /// Whether any of `left`, `top`, `right` and `bottom` is set to a
  /// finite number, which takes the widget out of its parent's flow.
  pub(crate) fn absolute(&self) -> bool {
    [self.left, self.top, self.right, self.bottom]
      .into_iter()
      .flatten()
      .any(f32::is_finite)
  }

  /// Whether the two styles move the widget the same way where it is
  /// drawn.
  pub(crate) fn same_transform(&self, other: &Style) -> bool {
    self.translate == other.translate
      && self.scale == other.scale
      && self.rotate == other.rotate
      && self.origin == other.origin
  }

  /// Where this style moves a widget laid out at `rect`, from its own
  /// coordinates into its parent's, or `None` when it doesn't. It is
  /// scaled and turned around its origin, then translated. A number that
  /// isn't finite counts as unset.
  pub(crate) fn transform(&self, rect: Rect) -> Option<Affine> {
    let finite = |value: f32, default: f32| if value.is_finite() { value } else { default };
    let translate = self.translate.unwrap_or_default();
    let (x, y) = (finite(translate.x, 0.0), finite(translate.y, 0.0));
    let scale = finite(self.scale.unwrap_or(1.0), 1.0);
    let degrees = finite(self.rotate.unwrap_or(0.0), 0.0).rem_euclid(360.0);
    if x == 0.0 && y == 0.0 && scale == 1.0 && degrees == 0.0 {
      return None;
    }
    // Quarter turns are exact, so a chevron turned over stays crisp.
    let (sin, cos) = match degrees {
      90.0 => (1.0, 0.0),
      180.0 => (0.0, -1.0),
      270.0 => (-1.0, 0.0),
      _ => degrees.to_radians().sin_cos(),
    };
    let origin = self.origin.unwrap_or(Point::new(0.5, 0.5));
    let ox = rect.x + finite(origin.x, 0.5) * rect.width;
    let oy = rect.y + finite(origin.y, 0.5) * rect.height;
    // A point p goes to origin + turn(scale(p - origin)) + translate.
    let (sx, ky, kx, sy) = (cos * scale, sin * scale, -sin * scale, cos * scale);
    // One too large for f32 maps every area to an empty one, so it is
    // drawn nowhere and never hit.
    Some(Affine {
      sx,
      ky,
      kx,
      sy,
      tx: ox + x - (sx * ox + kx * oy),
      ty: oy + y - (ky * ox + sy * oy),
    })
  }
}

/// `Option::or`, which isn't a `const fn` on stable Rust.
const fn or<T: Copy>(first: Option<T>, second: Option<T>) -> Option<T> {
  match first {
    Some(value) => Some(value),
    None => second,
  }
}

impl Style {
  /// The paint fields `t` of the way from `self` to `to`; every other
  /// field is `to`'s. A field only one side sets fades from or to nothing:
  /// a color from or to its transparent self, a border from or to 0 wide.
  /// A text color only one sets changes at once, as the other comes
  /// from an ancestor. Gradients of different shapes or numbers of colors
  /// change at once. An unset translate is 0, scale 1, rotate 0 and
  /// origin the middle.
  pub(crate) fn lerp(&self, to: &Style, t: f32) -> Style {
    let color = |from: Option<Color>, to: Option<Color>| match (from, to) {
      (Some(from), Some(to)) => Some(from.lerp(&to, t)),
      (None, Some(to)) => Some(Color::rgba(to.r, to.g, to.b, 0).lerp(&to, t)),
      (Some(from), None) => Some(from.lerp(&Color::rgba(from.r, from.g, from.b, 0), t)),
      (None, None) => None,
    };
    let border = match (self.border, to.border) {
      (None, None) => None,
      (from, to) => {
        let from = from.or(to.map(|b| Border {
          width: 0.0,
          color: Color::rgba(b.color.r, b.color.g, b.color.b, 0),
        }));
        let to = to.or(from.map(|b| Border {
          width: 0.0,
          color: Color::rgba(b.color.r, b.color.g, b.color.b, 0),
        }));
        match (from, to) {
          (Some(from), Some(to)) => Some(Border {
            width: from.width.lerp(&to.width, t),
            color: from.color.lerp(&to.color, t),
          }),

          _ => None,
        }
      }
    };
    let number = |from: Option<f32>, to: Option<f32>, none: f32| match (from, to) {
      (None, None) => None,
      (from, to) => Some(from.unwrap_or(none).lerp(&to.unwrap_or(none), t)),
    };
    let point = |from: Option<Point>, to: Option<Point>, none: Point| match (from, to) {
      (None, None) => None,
      (from, to) => Some(from.unwrap_or(none).lerp(&to.unwrap_or(none), t)),
    };
    let gradient = match (self.gradient, to.gradient) {
      (Some(from), Some(end)) => from.lerp(&end, t).or(to.gradient),
      (None, Some(end)) => end.clear().lerp(&end, t),
      (Some(from), None) => from.lerp(&from.clear(), t),
      (None, None) => None,
    };
    let shadow = match (self.shadow, to.shadow) {
      (Some(from), Some(end)) => Some(from.lerp(&end, t)),
      (None, Some(end)) => Some(end.clear().lerp(&end, t)),
      (Some(from), None) => Some(from.lerp(&from.clear(), t)),
      (None, None) => None,
    };
    Style {
      background: color(self.background, to.background),
      gradient,
      shadow,
      color: match (self.color, to.color) {
        (Some(from), Some(to)) => Some(from.lerp(&to, t)),
        _ => to.color,
      },
      border,
      radius: match (self.radius, to.radius) {
        (None, None) => None,
        (from, to) => Some(from.unwrap_or_default().lerp(&to.unwrap_or_default(), t)),
      },
      opacity: number(self.opacity, to.opacity, 1.0),
      translate: point(self.translate, to.translate, Point::default()),
      scale: number(self.scale, to.scale, 1.0),
      rotate: number(self.rotate, to.rotate, 0.0),
      origin: point(self.origin, to.origin, Point::new(0.5, 0.5)),
      ..*to
    }
  }
}
