//! Colors, fonts and [`Style`]. Everything here is `Copy` and built with
//! `const fn`s, so a theme is a set of constants.

use crate::animation::{Animate, Transition};

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

/// A font: family, size and weight.
///
/// The family is resolved through fontconfig, so generic names such as
/// `"sans-serif"` and `"monospace"` work too.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
  /// The family name, such as `"Inter"` or `"sans-serif"`.
  pub family: &'static str,
  /// The size in logical pixels.
  pub size: f32,
  /// The weight, from 100 (thin) through 400 (regular) and 700 (bold) to
  /// 900 (black).
  pub weight: u16,
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
    }
  }

  /// The same font with another weight, such as 700 for bold.
  pub const fn weight(self, weight: u16) -> Font {
    Font { weight, ..self }
  }
}

/// A border drawn inside a widget's edge. It takes no layout space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
  /// The width in logical pixels.
  pub width: f32,
  /// The color.
  pub color: Color,
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

/// Space between a widget's edge and its content, in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Padding {
  /// Above and below the content.
  pub vertical: u32,
  /// Left and right of the content.
  pub horizontal: u32,
}

/// How a widget looks. Every field is optional, and an unset field falls back
/// to the widget's default: no background, black text in
/// [`Font::DEFAULT`], no padding, square corners, no border, fully opaque.
///
/// [`Text`](crate::Text) takes `color` and `font` from the nearest widget
/// that sets them, starting with itself and going up. Nothing else inherits.
///
/// `font`, `padding`, `clip`, `width` and `height` change the layout; the
/// other fields only change how the widget is painted.
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
  /// The text color, inherited by [`Text`](crate::Text).
  pub color: Option<Color>,
  /// The text font, inherited by [`Text`](crate::Text).
  pub font: Option<Font>,
  /// Space between the edge and the content.
  pub padding: Option<Padding>,
  /// The corner radius in logical pixels.
  pub radius: Option<f32>,
  /// A border inside the edge.
  pub border: Option<Border>,
  /// From 0.0 (invisible) to 1.0 (opaque), applied to the widget and
  /// everything inside it.
  pub opacity: Option<f32>,
  /// Whether the children are cut to the inside of the border, following
  /// the rounded corners, like CSS's `overflow: hidden`.
  pub clip: Option<bool>,
  /// The width in logical pixels, instead of what the content needs.
  pub width: Option<f32>,
  /// The height in logical pixels, instead of what the content needs.
  pub height: Option<f32>,
  /// How a change to the paint fields fades in: background, gradient,
  /// color, border, radius and opacity. Other fields change at once.
  pub transition: Option<Transition>,
}

impl Style {
  /// A style that sets nothing.
  pub const fn new() -> Style {
    Style {
      background: None,
      gradient: None,
      color: None,
      font: None,
      padding: None,
      radius: None,
      border: None,
      opacity: None,
      clip: None,
      width: None,
      height: None,
      transition: None,
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

  /// Sets the padding: `vertical` above and below, `horizontal` left and
  /// right, in logical pixels.
  pub const fn padding(self, vertical: u32, horizontal: u32) -> Style {
    Style {
      padding: Some(Padding {
        vertical,
        horizontal,
      }),
      ..self
    }
  }

  /// Sets the corner radius in logical pixels.
  pub const fn radius(self, radius: f32) -> Style {
    Style {
      radius: Some(radius),
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

  /// Sets the opacity, from 0.0 to 1.0.
  pub const fn opacity(self, opacity: f32) -> Style {
    Style {
      opacity: Some(opacity),
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

  /// Sets how a change to the paint fields fades in, as when a hover
  /// style takes over: background, gradient, color, border, radius and opacity move
  /// over `transition`. The style being changed to says how.
  pub const fn transition(self, transition: Transition) -> Style {
    Style {
      transition: Some(transition),
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
      color: or(over.color, self.color),
      font: or(over.font, self.font),
      padding: or(over.padding, self.padding),
      radius: or(over.radius, self.radius),
      border: or(over.border, self.border),
      opacity: or(over.opacity, self.opacity),
      clip: or(over.clip, self.clip),
      width: or(over.width, self.width),
      height: or(over.height, self.height),
      transition: or(over.transition, self.transition),
    }
  }

  /// Whether the children are cut to the widget's box.
  pub fn clips(&self) -> bool {
    self.clip == Some(true)
  }

  /// Whether the two styles lay out the same: same font, padding, size
  /// and clipping, which changes how far the children's painting reaches.
  pub(crate) fn same_layout(&self, other: &Style) -> bool {
    self.font == other.font
      && self.padding == other.padding
      && self.width == other.width
      && self.height == other.height
      && self.clips() == other.clips()
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
  /// A text color only one sets changes at the end, as the other comes
  /// from an ancestor. Gradients of different shapes or numbers of colors
  /// change at once.
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
    let gradient = match (self.gradient, to.gradient) {
      (Some(from), Some(end)) => from.lerp(&end, t).or(to.gradient),
      (None, Some(end)) => end.clear().lerp(&end, t),
      (Some(from), None) => from.lerp(&from.clear(), t),
      (None, None) => None,
    };
    Style {
      background: color(self.background, to.background),
      gradient,
      color: match (self.color, to.color) {
        (Some(from), Some(to)) => Some(from.lerp(&to, t)),
        _ => to.color,
      },
      border,
      radius: number(self.radius, to.radius, 0.0),
      opacity: number(self.opacity, to.opacity, 1.0),
      ..*to
    }
  }
}
