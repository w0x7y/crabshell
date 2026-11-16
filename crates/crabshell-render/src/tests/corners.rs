//! A radius per corner, followed by everything that follows the corners.

use crabshell_ui::{Align, Color, Row, Shadow, Style};

use super::{Buffer, Scene};

const WHITE: Color = Color::rgb(255, 255, 255);
const BLACK: Color = Color::rgb(0, 0, 0);

/// A 40 by 40 box at 20, 10 drawn with `style` in a clear 120 by 70
/// window at scale 1, with `inside` filling it.
fn boxed(style: Style, inside: Option<Style>) -> Buffer {
  let mut scene = Scene::new();
  let mut drawn = Row::new()
    .align(Align::Start)
    .style(style.width(40.0).height(40.0));
  if let Some(inside) = inside {
    drawn = drawn.child(Row::new().style(inside.width(40.0).height(40.0)));
  }
  let row = Row::new().padding(10, 20).align(Align::Start).child(drawn);
  let window = scene.window(scene.panel().child(row), 120.0, 70.0);
  scene.full(&window, 1.0)
}

/// Just inside each corner of the box, from the top left clockwise.
const CORNERS: [(u32, u32); 4] = [(21, 11), (58, 11), (58, 48), (21, 48)];

/// Radii with only corner `index`, from the top left clockwise, `r`
/// round.
fn one(index: usize, r: f32) -> Style {
  let mut corners = [0.0; 4];
  corners[index] = r;
  let [a, b, c, d] = corners;
  Style::new().radii(a, b, c, d)
}

#[test]
fn each_corner_is_rounded_on_its_own() {
  for index in 0..4 {
    let buffer = boxed(one(index, 16.0).background(WHITE), None);
    for (corner, &(x, y)) in CORNERS.iter().enumerate() {
      let alpha = buffer.pixel(x, y)[3];
      if corner == index {
        assert_eq!(alpha, 0, "corner {corner} of a box round at {index}");
      } else {
        assert_eq!(alpha, 255, "corner {corner} of a box round at {index}");
      }
    }
  }
}

#[test]
fn a_corner_past_half_the_shorter_side_is_clamped_to_it() {
  let past = boxed(one(0, 100.0).background(WHITE), None);
  let half = boxed(one(0, 20.0).background(WHITE), None);
  assert!(past.pixels == half.pixels);
  // Its neighbors stay square.
  assert_eq!(past.pixel(58, 11)[3], 255);
  assert_eq!(past.pixel(21, 48)[3], 255);
}

#[test]
fn a_border_follows_each_corner() {
  // A 4 pixel border round only at the bottom right: its inside edge
  // curves there, and is square at the top left.
  let buffer = boxed(one(2, 16.0).background(BLACK).border(4.0, WHITE), None);
  assert_eq!(buffer.pixel(24, 14), [0, 0, 0, 255], "inside, top left");
  assert_eq!(buffer.pixel(23, 23), [255, 255, 255, 255], "the border");
  // Where a square inside corner would leave black at the bottom right,
  // 13.4 pixels from the corner's middle, between the border's curves.
  assert_eq!(buffer.pixel(53, 43), [255, 255, 255, 255]);
  // The inside curve is the corner less the border, 12 round about the
  // same middle, so 10.6 pixels from it is inside.
  assert_eq!(buffer.pixel(51, 41), [0, 0, 0, 255]);
}

#[test]
fn a_clip_cuts_its_children_at_each_corner() {
  let buffer = boxed(
    one(1, 16.0).clip(true),
    Some(Style::new().background(WHITE)),
  );
  assert_eq!(buffer.pixel(58, 11)[3], 0, "past the round corner");
  for (x, y) in [(21, 11), (58, 48), (21, 48)] {
    assert_eq!(buffer.pixel(x, y)[3], 255, "in a square corner at {x}, {y}");
  }
}

#[test]
fn a_sharp_shadow_grows_each_corner_by_the_spread() {
  let shadow = Shadow::new(0.0, 0.0, 0.0, BLACK).spread(4.0);
  let mixed = boxed(one(0, 10.0).background(WHITE).shadow(shadow), None);
  let round = boxed(
    Style::new().radius(10.0).background(WHITE).shadow(shadow),
    None,
  );
  let square = boxed(Style::new().background(WHITE).shadow(shadow), None);
  // Each corner's shadow is the one of a box with that radius all round.
  for (x, y) in [(17, 7), (18, 8), (19, 9)] {
    assert_eq!(mixed.pixel(x, y), round.pixel(x, y), "top left at {x}, {y}");
  }
  for (x, y) in [(62, 7), (61, 8), (62, 52), (17, 52)] {
    assert_eq!(mixed.pixel(x, y), square.pixel(x, y), "at {x}, {y}");
  }
  assert_eq!(mixed.pixel(16, 6)[3], 0);
  assert_eq!(mixed.pixel(63, 6)[3], 255);
}

#[test]
fn a_blurred_shadow_follows_each_corner() {
  let shadow = Shadow::new(0.0, 0.0, 8.0, BLACK);
  let tab = boxed(
    Style::new()
      .radii(16.0, 16.0, 0.0, 0.0)
      .background(WHITE)
      .shadow(shadow),
    None,
  );
  let round = boxed(
    Style::new().radius(16.0).background(WHITE).shadow(shadow),
    None,
  );
  let square = boxed(Style::new().background(WHITE).shadow(shadow), None);
  let near = |a: &Buffer, b: &Buffer, x, y| a.pixel(x, y)[3].abs_diff(b.pixel(x, y)[3]) <= 1;
  // Off each top corner diagonally, as round; off each bottom one, as
  // square, and the two differ there.
  for (x, y) in [(17, 7), (14, 4), (62, 7), (65, 4)] {
    assert!(near(&tab, &round, x, y), "top at {x}, {y}");
  }
  for (x, y) in [(17, 52), (14, 55), (62, 52), (65, 55)] {
    assert!(near(&tab, &square, x, y), "bottom at {x}, {y}");
    assert!(
      !near(&round, &square, x, y),
      "the corners differ at {x}, {y}"
    );
  }
  // Above the middle of the top edge, which the round corners narrow, as
  // round, not square.
  assert!(near(&tab, &round, 40, 7));
  assert!(!near(&tab, &square, 40, 7));
  // Below the square bottom edge, as square.
  assert!(near(&tab, &square, 40, 52));
}

#[test]
fn a_cut_out_shadow_is_cut_by_each_corner() {
  let clear = Color::rgba(255, 255, 255, 0);
  for blur in [0.0, 4.0] {
    let shadow = Shadow::new(0.0, 0.0, blur, BLACK).spread(2.0).cut_out();
    let buffer = boxed(one(3, 16.0).background(clear).shadow(shadow), None);
    assert_eq!(buffer.pixel(40, 30)[3], 0, "blur {blur}: under the box");
    assert_eq!(buffer.pixel(58, 48)[3], 0, "blur {blur}: a square corner");
    // Past the box's round corner, inside the shadow's, it shows.
    assert!(buffer.pixel(23, 46)[3] > 0, "blur {blur}: the round corner");
  }
}

/// How long a full draw of an 1880 pixel wide box with a 12 pixel blur
/// takes at scale 2, with one radius, a radius per corner and no shadow.
/// Run it in a release build:
/// `cargo test --release -p crabshell-render --lib time_a_shadow_per_corner -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn time_a_shadow_per_corner() {
  let shadow = Shadow::new(0.0, 4.0, 12.0, Color::rgba(0, 0, 0, 160));
  let card = Style::new().background(Color::hex("#45475a"));
  for (what, style) in [
    ("one radius", card.radius(12.0).shadow(shadow)),
    (
      "a radius per corner",
      card.radii(0.0, 8.0, 16.0, 24.0).shadow(shadow),
    ),
    ("no shadow", card.radii(0.0, 8.0, 16.0, 24.0)),
  ] {
    let mut scene = Scene::new();
    let row = Row::new()
      .padding(20, 20)
      .align(Align::Start)
      .child(Row::new().style(style.width(1880.0).height(40.0)));
    let window = scene.window(scene.panel().child(row), 1920.0, 80.0);
    let draws = 20;
    let start = std::time::Instant::now();
    for _ in 0..draws {
      scene.full(&window, 2.0);
    }
    println!(
      "{what}: {:.2} ms a full draw at scale 2",
      start.elapsed().as_secs_f64() * 1000.0 / f64::from(draws)
    );
  }
}
