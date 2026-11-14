//! Colors, fonts and styles.

use crate::{Border, Color, Font, Gradient, GradientShape, Padding, Style};

#[test]
fn hex_parses_both_forms_in_either_case() {
  assert_eq!(Color::hex("#1e1E2e"), Color::rgb(0x1e, 0x1e, 0x2e));
  assert_eq!(Color::hex("#CDD6F480"), Color::rgba(0xcd, 0xd6, 0xf4, 0x80));
}

#[test]
fn hex_works_in_a_const() {
  const THEME: Color = Color::hex("#45475a");
  assert_eq!(THEME, Color::rgb(0x45, 0x47, 0x5a));
}

#[test]
#[should_panic(expected = "Color::hex takes")]
fn hex_rejects_a_missing_hash() {
  Color::hex("1e1e2e");
}

#[test]
#[should_panic(expected = "isn't a hex digit")]
fn hex_rejects_other_characters() {
  Color::hex("#1e1e2g");
}

#[test]
fn builders_set_their_fields_and_nothing_else() {
  const WS: Style = Style::new().padding(2, 8).radius(4.0);
  assert_eq!(
    WS,
    Style {
      padding: Some(Padding {
        vertical: 2,
        horizontal: 8
      }),
      radius: Some(4.0),
      ..Style::new()
    }
  );
  let bordered = Style::new().border(1.5, Color::rgb(1, 2, 3)).opacity(0.5);
  assert_eq!(
    bordered.border,
    Some(Border {
      width: 1.5,
      color: Color::rgb(1, 2, 3)
    })
  );
  assert_eq!(bordered.opacity, Some(0.5));
  assert_eq!(bordered.background, None);
}

#[test]
fn merge_takes_the_fields_the_top_style_sets() {
  let base = Style::new()
    .background(Color::rgb(1, 1, 1))
    .color(Color::rgb(2, 2, 2))
    .padding(2, 8);
  let hover = Style::new().background(Color::rgb(9, 9, 9));
  let merged = base.merge(hover);
  assert_eq!(merged.background, Some(Color::rgb(9, 9, 9)));
  assert_eq!(merged.color, Some(Color::rgb(2, 2, 2)));
  assert_eq!(merged.padding, base.padding);
  assert_eq!(Style::new().merge(base), base);
}

#[test]
fn only_font_and_padding_affect_layout() {
  let base = Style::new().padding(2, 8).font(Font::new("Inter", 13.0));
  assert!(
    base.same_layout(
      &base
        .background(Color::rgb(1, 2, 3))
        .radius(9.0)
        .opacity(0.1)
    )
  );
  assert!(!base.same_layout(&base.padding(3, 8)));
  assert!(!base.same_layout(&base.font(Font::new("Inter", 14.0))));
  assert!(!base.same_layout(&base.font(Font::new("Inter", 13.0).weight(700))));
}

#[test]
fn font_defaults_to_regular_weight() {
  const BOLD: Font = Font::new("Inter", 13.0).weight(700);
  assert_eq!(Font::new("Inter", 13.0).weight, 400);
  assert_eq!(BOLD.weight, 700);
  assert_eq!(BOLD.family, "Inter");
}

#[test]
fn a_style_blends_its_paint_fields_and_takes_the_rest_from_the_end() {
  let from = Style::new()
    .background(Color::rgb(0, 0, 0))
    .radius(0.0)
    .font(Font::new("a", 10.0));
  let to = Style::new()
    .border(4.0, Color::rgb(200, 0, 0))
    .opacity(0.0)
    .font(Font::new("b", 12.0));
  let half = from.lerp(&to, 0.5);
  // A background only one side sets fades to its transparent self.
  assert_eq!(half.background, Some(Color::rgba(0, 0, 0, 128)));
  let border = half.border.expect("a border on the way");
  assert_eq!(
    (border.width, border.color),
    (2.0, Color::rgba(200, 0, 0, 128))
  );
  assert_eq!(half.radius, Some(0.0));
  assert_eq!(half.opacity, Some(0.5), "unset opacity is 1");
  assert_eq!(
    half.font,
    Some(Font::new("b", 12.0)),
    "the rest is the end's"
  );
  assert_eq!(
    from.lerp(&to, 1.0).background,
    Some(Color::rgba(0, 0, 0, 0))
  );
}

#[test]
fn a_gradient_keeps_its_ends_and_the_stops_between_in_order() {
  const RED: Color = Color::rgb(255, 0, 0);
  const BLUE: Color = Color::rgb(0, 0, 255);
  const SKY: Style = Style::new().gradient(
    Gradient::linear(90.0, RED, BLUE)
      .stop(0.25, Color::rgb(0, 255, 0))
      .stop(0.75, Color::rgb(9, 9, 9)),
  );
  let gradient = SKY.gradient.expect("set by the builder");
  assert_eq!(gradient.shape, GradientShape::Linear(90.0));
  assert_eq!(
    gradient.stops(),
    [
      (0.0, RED),
      (0.25, Color::rgb(0, 255, 0)),
      (0.75, Color::rgb(9, 9, 9)),
      (1.0, BLUE)
    ]
  );
  assert_eq!(
    Gradient::radial(RED, BLUE).stops(),
    [(0.0, RED), (1.0, BLUE)]
  );
  assert_eq!(Style::new().merge(SKY).gradient, Some(gradient));
  assert!(Style::new().same_layout(&SKY), "a gradient is paint only");
}

#[test]
#[should_panic(expected = "at most four colors")]
fn a_fifth_gradient_color_is_refused() {
  let black = Color::rgb(0, 0, 0);
  let _ = Gradient::radial(black, black)
    .stop(0.2, black)
    .stop(0.4, black)
    .stop(0.6, black);
}

#[test]
fn gradients_blend_when_they_have_the_same_shape_and_colors() {
  let black = Color::rgb(0, 0, 0);
  let white = Color::rgb(200, 200, 200);
  let from = Style::new().gradient(Gradient::linear(0.0, black, black));
  let to = Style::new().gradient(Gradient::linear(90.0, white, black));
  let half = from.lerp(&to, 0.5).gradient.expect("both set one");
  assert_eq!(half.shape, GradientShape::Linear(45.0));
  assert_eq!(half.stops()[0], (0.0, Color::rgb(100, 100, 100)));
  // One that appears fades in from transparent.
  let appearing = Style::new().lerp(&to, 0.5).gradient.expect("on its way");
  assert_eq!(appearing.stops()[0].1, Color::rgba(200, 200, 200, 128));
  // Different shapes, or a different number of colors, switch at once.
  let radial = Style::new().gradient(Gradient::radial(black, white));
  assert_eq!(from.lerp(&radial, 0.5).gradient, radial.gradient);
  let three = Style::new().gradient(Gradient::linear(0.0, black, white).stop(0.5, black));
  assert_eq!(from.lerp(&three, 0.5).gradient, three.gradient);
}
