//! Colors, fonts and styles.

use crate::{Border, Color, Font, Gradient, GradientShape, Padding, Radii, Rect, Shadow, Style};

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
        top: 2,
        right: 8,
        bottom: 2,
        left: 8
      }),
      radius: Some(Radii::all(4.0)),
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
fn each_side_sets_its_own_padding_and_keeps_the_others() {
  const PILL: Style = Style::new()
    .padding(2, 8)
    .padding_left(12)
    .padding_bottom(5);
  assert_eq!(
    PILL.padding,
    Some(Padding {
      top: 2,
      right: 8,
      bottom: 5,
      left: 12
    })
  );
  // On a style with no padding, the other sides are 0.
  const TOP: Style = Style::new().padding_top(3);
  const RIGHT: Style = Style::new().padding_right(4);
  let sides = |style: Style| style.padding.map(|p| (p.top, p.right, p.bottom, p.left));
  assert_eq!(sides(TOP), Some((3, 0, 0, 0)));
  assert_eq!(sides(RIGHT), Some((0, 4, 0, 0)));
  // A later `padding` sets every side again.
  assert_eq!(sides(PILL.padding(1, 1)), Some((1, 1, 1, 1)));
  assert!(!PILL.same_layout(&PILL.padding_left(13)));
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
  assert_eq!(half.radius, Some(Radii::all(0.0)));
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

#[test]
fn a_shadow_reaches_past_the_box_by_its_offset_spread_and_blur() {
  const LIFTED: Style =
    Style::new().shadow(Shadow::new(2.0, 4.0, 8.0, Color::rgba(0, 0, 0, 128)).spread(1.0));
  let shadow = LIFTED.shadow.expect("set by the builder");
  assert_eq!(
    (shadow.x, shadow.y, shadow.blur, shadow.spread),
    (2.0, 4.0, 8.0, 1.0)
  );
  // Moved by 2, 4, grown by the spread and one and a half blurs.
  let reach = shadow.reach(Rect::new(10.0, 10.0, 20.0, 10.0));
  assert_eq!(reach, Rect::new(-1.0, 1.0, 46.0, 36.0));
  assert_eq!(Style::new().merge(LIFTED).shadow, Some(shadow));
  // A spread that swallows the box still reaches between its edges, so a
  // fade from it to another shadow stays inside the two.
  let gone = Shadow::new(-100.0, 0.0, 0.0, Color::rgb(0, 0, 0)).spread(-50.0);
  assert_eq!(
    gone.reach(Rect::new(10.0, 10.0, 20.0, 20.0)),
    Rect::new(-120.0, -20.0, 80.0, 80.0)
  );
  // A shadow nobody can draw reaches nowhere past the box.
  let box_ = Rect::new(10.0, 10.0, 20.0, 10.0);
  assert_eq!(
    Shadow::new(f32::NAN, 0.0, 1.0, Color::rgb(0, 0, 0)).reach(box_),
    box_
  );
}

#[test]
fn shadows_blend_and_fade_in_from_clear() {
  let gray = Color::rgba(0, 0, 0, 200);
  let from = Style::new().shadow(Shadow::new(0.0, 0.0, 2.0, gray));
  let to = Style::new().shadow(Shadow::new(0.0, 4.0, 10.0, gray).spread(2.0));
  let half = from.lerp(&to, 0.5).shadow.expect("both set one");
  assert_eq!((half.y, half.blur, half.spread), (2.0, 6.0, 1.0));
  let appearing = Style::new().lerp(&to, 0.5).shadow.expect("on its way");
  assert_eq!(
    (appearing.y, appearing.color),
    (4.0, Color::rgba(0, 0, 0, 100))
  );
  let going = to.lerp(&Style::new(), 0.75).shadow.expect("on its way out");
  assert_eq!(going.color, Color::rgba(0, 0, 0, 50));
}

#[test]
fn merge_takes_each_place_field_from_the_style_on_top() {
  const BASE: Style = Style::new()
    .grow(1.0)
    .min_width(10.0)
    .left(4.0)
    .z(2)
    .span(2, 1);
  const OVER: Style = Style::new()
    .grow(2.0)
    .max_height(8.0)
    .align_self(crate::Align::End)
    .top(1.0);
  let merged = BASE.merge(OVER);
  assert_eq!(merged.grow, Some(2.0));
  assert_eq!(merged.min_width, Some(10.0));
  assert_eq!(merged.max_height, Some(8.0));
  assert_eq!(merged.align_self, Some(crate::Align::End));
  assert_eq!((merged.left, merged.top), (Some(4.0), Some(1.0)));
  assert_eq!(merged.z, Some(2));
  assert_eq!(merged.span, Some((2, 1)));
}

#[test]
fn place_fields_change_the_layout_and_z_only_the_paint() {
  let base = Style::new();
  for other in [
    base.grow(1.0),
    base.shrink(0.0),
    base.basis(4.0),
    base.min_width(1.0),
    base.min_height(1.0),
    base.max_width(1.0),
    base.max_height(1.0),
    base.align_self(crate::Align::Center),
    base.justify_self(crate::Align::Center),
    base.left(1.0),
    base.top(1.0),
    base.right(1.0),
    base.bottom(1.0),
    base.span(2, 2),
  ] {
    assert!(!base.same_layout(&other), "{other:?}");
  }
  assert!(base.same_layout(&base.z(3)));
}

#[test]
fn hsl_gives_the_colors_css_does() {
  // Each against what CSS's `hsl()` gives.
  const STEEL: Color = Color::hsl(210.0, 0.5, 0.4);
  assert_eq!(STEEL, Color::rgb(51, 102, 153));
  assert_eq!(Color::hsl(0.0, 1.0, 0.5), Color::rgb(255, 0, 0));
  assert_eq!(Color::hsl(120.0, 1.0, 0.25), Color::rgb(0, 128, 0));
  assert_eq!(Color::hsl(300.0, 0.76, 0.72), Color::rgb(238, 129, 238));
  assert_eq!(Color::hsl(30.0, 1.0, 0.5), Color::rgb(255, 128, 0));
  assert_eq!(Color::hsl(0.0, 0.0, 0.5), Color::rgb(128, 128, 128));
  // A hue wraps around; the others clamp.
  assert_eq!(Color::hsl(570.0, 0.5, 0.4), STEEL);
  assert_eq!(Color::hsl(-150.0, 0.5, 0.4), STEEL);
  assert_eq!(Color::hsl(10.0, 2.0, 1.5), Color::rgb(255, 255, 255));
  assert_eq!(Color::hsl(10.0, -1.0, 0.5), Color::rgb(128, 128, 128));
  assert_eq!(
    Color::hsla(210.0, 0.5, 0.4, 0.5),
    Color::rgba(51, 102, 153, 128)
  );
  assert_eq!(Color::hsla(210.0, 0.5, 0.4, 7.0).a, 255);
  // Not a number anywhere gives opaque black.
  for (h, s, l, a) in [
    (f32::NAN, 0.5, 0.5, 1.0),
    (0.0, f32::NAN, 0.5, 1.0),
    (0.0, 0.5, f32::NAN, 1.0),
    (0.0, 0.5, 0.5, f32::NAN),
    (f32::INFINITY, 0.5, 0.5, 1.0),
  ] {
    assert_eq!(
      Color::hsla(h, s, l, a),
      Color::rgb(0, 0, 0),
      "{h} {s} {l} {a}"
    );
  }
}

#[test]
fn lighter_and_darker_move_the_hsl_lightness() {
  const STEEL: Color = Color::rgb(51, 102, 153);
  const LIGHT: Color = STEEL.lighter(0.1);
  // hsl(210, 50%, 50%) and hsl(210, 50%, 30%).
  assert_eq!(LIGHT, Color::rgb(64, 128, 191));
  assert_eq!(STEEL.darker(0.1), Color::rgb(38, 77, 115));
  // They keep the alpha, clamp the lightness and the amount, and a NaN
  // leaves the color as it was.
  let clear = Color::rgba(51, 102, 153, 40);
  assert_eq!(clear.lighter(0.1), Color::rgba(64, 128, 191, 40));
  assert_eq!(STEEL.lighter(5.0), Color::rgb(255, 255, 255));
  assert_eq!(STEEL.darker(5.0), Color::rgb(0, 0, 0));
  assert_eq!(STEEL.lighter(-1.0), STEEL);
  assert_eq!(STEEL.darker(f32::NAN), STEEL);
  // Going through HSL and back keeps every color.
  for color in [
    Color::rgb(0x1e, 0x1e, 0x2e),
    Color::rgb(0xcd, 0xd6, 0xf4),
    Color::rgb(0xf3, 0x8b, 0xa8),
    Color::rgb(255, 0, 255),
    Color::rgb(1, 2, 3),
    Color::rgb(254, 255, 253),
  ] {
    assert_eq!(color.lighter(0.0), color);
  }
}

#[test]
fn with_alpha_and_mix_work_on_srgb_channels() {
  const BASE: Color = Color::hex("#1e1e2e");
  const VEIL: Color = BASE.with_alpha(0.5);
  assert_eq!(VEIL, Color::rgba(0x1e, 0x1e, 0x2e, 128));
  assert_eq!(BASE.with_alpha(2.0).a, 255);
  assert_eq!(BASE.with_alpha(-1.0).a, 0);
  assert_eq!(VEIL.with_alpha(f32::NAN), VEIL);
  // Each channel on its own, alpha included, as a style fade blends.
  const RED: Color = Color::rgb(255, 0, 0);
  const HALF: Color = RED.mix(Color::rgba(0, 0, 255, 0), 0.5);
  assert_eq!(HALF, Color::rgba(128, 0, 128, 128));
  assert_eq!(RED.mix(BASE, 0.0), RED);
  assert_eq!(RED.mix(BASE, 1.0), BASE);
  assert_eq!(RED.mix(BASE, 9.0), BASE);
  assert_eq!(RED.mix(BASE, -1.0), RED);
  assert_eq!(RED.mix(BASE, f32::NAN), RED);
  assert_eq!(
    Color::rgb(0, 100, 200).mix(Color::rgb(100, 0, 0), 0.25),
    Color::rgb(25, 75, 150)
  );
}

#[test]
fn radii_sets_each_corner_and_radius_all_four() {
  const TAB: Style = Style::new().radii(8.0, 8.0, 0.0, 2.0);
  assert_eq!(
    TAB.radius,
    Some(Radii {
      top_left: 8.0,
      top_right: 8.0,
      bottom_right: 0.0,
      bottom_left: 2.0,
    })
  );
  assert_eq!(Style::new().radius(3.0).radius, Some(Radii::all(3.0)));
  assert_eq!(
    Style::new().radius(3.0).radius,
    Style::new().radii(3.0, 3.0, 3.0, 3.0).radius
  );
  // The style on top takes all four corners.
  assert_eq!(
    TAB.merge(Style::new().radius(1.0)).radius,
    Some(Radii::all(1.0))
  );
  assert_eq!(Style::new().radius(1.0).merge(TAB).radius, TAB.radius);
  assert!(Style::new().same_layout(&TAB), "corners are paint only");
}

#[test]
fn a_fade_moves_each_corner_from_where_it_was() {
  let round = Style::new().radius(4.0);
  let tab = Style::new().radii(0.0, 8.0, 16.0, 4.0);
  let half = round.lerp(&tab, 0.5).radius.expect("both set corners");
  assert_eq!(
    [
      half.top_left,
      half.top_right,
      half.bottom_right,
      half.bottom_left
    ],
    [2.0, 6.0, 10.0, 4.0]
  );
  // Corners only one side sets fade from or to square ones.
  let appearing = Style::new().lerp(&tab, 0.25).radius.expect("on its way");
  assert_eq!(
    appearing,
    Radii::all(0.0).lerp(&tab.radius.expect("set"), 0.25)
  );
  assert_eq!(appearing.bottom_right, 4.0);
  let going = tab
    .lerp(&Style::new(), 0.75)
    .radius
    .expect("on its way out");
  assert_eq!(going.bottom_right, 4.0);
}

#[test]
fn a_sweep_gradient_blends_with_another_sweep_only() {
  const RED: Color = Color::rgb(255, 0, 0);
  const BLUE: Color = Color::rgb(0, 0, 255);
  const RING: Gradient = Gradient::sweep(45.0, RED, BLUE).stop(0.5, RED);
  assert_eq!(RING.shape, GradientShape::Sweep(45.0));
  assert_eq!(RING.stops(), [(0.0, RED), (0.5, RED), (1.0, BLUE)]);
  let from = Style::new().gradient(Gradient::sweep(0.0, RED, BLUE));
  let to = Style::new().gradient(Gradient::sweep(90.0, BLUE, BLUE));
  let half = from.lerp(&to, 0.5).gradient.expect("both set one");
  assert_eq!(half.shape, GradientShape::Sweep(45.0));
  assert_eq!(half.stops()[0], (0.0, Color::rgb(128, 0, 128)));
  let linear = Style::new().gradient(Gradient::linear(0.0, RED, BLUE));
  assert_eq!(from.lerp(&linear, 0.5).gradient, linear.gradient);
  assert_eq!(linear.lerp(&from, 0.5).gradient, from.gradient);
}
