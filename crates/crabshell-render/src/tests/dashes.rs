//! Dashed and dotted borders.

use crabshell_ui::{Align, Color, Row, Style};

use super::{Buffer, Scene};
use crate::draw::fit_dash;

const WHITE: Color = Color::rgb(255, 255, 255);

/// A 100 by 20 box at 10, 10 with `style`, in a clear 120 by 40 window
/// at `scale`.
fn bordered(style: Style, scale: f32) -> Buffer {
  let mut scene = Scene::new();
  let row = Row::new()
    .padding(10, 10)
    .align(Align::Start)
    .child(Row::new().style(style.width(100.0).height(20.0)));
  let window = scene.window(scene.panel().child(row), 120.0, 40.0);
  scene.full(&window, scale)
}

/// The runs of covered pixels, at least half opaque, along row `y` from
/// `from` to `to`, as their lengths.
fn runs(buffer: &Buffer, y: u32, from: u32, to: u32) -> Vec<u32> {
  let mut runs = Vec::new();
  let mut length = 0;
  for x in from..to {
    if buffer.pixel(x, y)[3] >= 128 {
      length += 1;
    } else if length > 0 {
      runs.push(length);
      length = 0;
    }
  }
  if length > 0 {
    runs.push(length);
  }
  runs
}

#[test]
fn a_dash_is_fitted_so_whole_ones_go_around() {
  // 236 pixels round, a period of 10: 24 dashes, each squeezed a little.
  let (on, off) = fit_dash(236.0, 6.0, 4.0).expect("a pattern");
  assert!((on + off - 236.0 / 24.0).abs() < 1e-4, "{on} {off}");
  assert!((on / off - 1.5).abs() < 1e-4, "keeps its shape");
  // Dots keep a length of 0.
  assert_eq!(fit_dash(100.0, 0.0, 4.0), Some((0.0, 4.0)));
  // A box shorter than one period still gets one.
  assert_eq!(fit_dash(2.5, 6.0, 4.0), Some((1.5, 1.0)));
  // Patterns that can't be drawn.
  for (on, off) in [
    (f32::NAN, 4.0),
    (4.0, f32::NAN),
    (f32::INFINITY, 4.0),
    (-1.0, 4.0),
    (4.0, 0.0),
    (0.0, 0.0),
    (0.3, 0.3),
  ] {
    assert_eq!(fit_dash(100.0, on, off), None, "{on} {off}");
  }
}

#[test]
fn a_dashed_border_leaves_gaps_along_its_middle() {
  let style = Style::new().border(2.0, WHITE);
  let solid = bordered(style, 1.0);
  let dashed = bordered(style.border_dash(6.0, 4.0), 1.0);
  // Along the top edge, inside the corners.
  assert_eq!(runs(&solid, 10, 12, 108), [96]);
  let dashes = runs(&dashed, 10, 12, 108);
  assert!((9..=11).contains(&dashes.len()), "{dashes:?}");
  // Each whole one about 6 long, squeezed by 236 / 240.
  for &length in &dashes[1..dashes.len() - 1] {
    assert!((5..=7).contains(&length), "{dashes:?}");
  }
  // The inside is untouched.
  assert_eq!(dashed.pixel(60, 20)[3], 0);
}

#[test]
fn a_dotted_border_draws_round_dots() {
  let dotted = bordered(Style::new().border(4.0, WHITE).border_dash(0.0, 8.0), 1.0);
  // Along the border's middle line on the top edge, dots 4 across, 8
  // apart, with a clear gap between them.
  let dots = runs(&dotted, 12, 14, 106);
  assert!((10..=12).contains(&dots.len()), "{dots:?}");
  for &length in &dots[1..dots.len() - 1] {
    assert!((3..=5).contains(&length), "{dots:?}");
  }
  // Round: a dot's rows are narrower at its top than through its middle.
  let middle_row = runs(&dotted, 12, 14, 106)[2];
  let top_row = runs(&dotted, 10, 14, 106)[2];
  assert!(top_row < middle_row, "{top_row} {middle_row}");
}

#[test]
fn a_dash_that_cant_be_drawn_draws_the_border_solid() {
  let style = Style::new().border(2.0, WHITE).radius(6.0);
  let solid = bordered(style, 1.5);
  for (on, off) in [(f32::NAN, 4.0), (4.0, 0.0), (-2.0, 4.0), (0.2, 0.2)] {
    let buffer = bordered(style.border_dash(on, off), 1.5);
    assert!(buffer.pixels == solid.pixels, "{on} {off}");
  }
}

#[test]
fn dashes_are_in_logical_pixels_at_every_scale() {
  let style = Style::new().border(2.0, WHITE).border_dash(6.0, 4.0);
  let count = |scale: f32| {
    let buffer = bordered(style, scale);
    let y = (10.0 * scale) as u32;
    let edge = (12.0 * scale) as u32;
    runs(&buffer, y, edge, (108.0 * scale) as u32).len()
  };
  let at_one = count(1.0);
  for scale in [1.5, 2.0] {
    assert!(count(scale).abs_diff(at_one) <= 1, "at {scale}");
  }
}

#[test]
fn a_dashed_border_of_no_width_draws_nothing() {
  for width in [0.0, -1.0] {
    let buffer = bordered(Style::new().border(width, WHITE).border_dash(4.0, 2.0), 1.0);
    assert!(buffer.pixels.iter().all(|&byte| byte == 0), "{width}");
  }
}

#[test]
fn a_dash_longer_than_a_tile_is_drawn_in_every_tile_it_crosses() {
  // Two dashes around a 300 by 20 box, the first along the whole top
  // edge, past both ends of the tiles from 64 to 256.
  let mut scene = Scene::new();
  let row = Row::new().padding(10, 10).align(Align::Start).child(
    Row::new().style(
      Style::new()
        .border(2.0, WHITE)
        .border_dash(250.0, 10.0)
        .width(300.0)
        .height(20.0),
    ),
  );
  let window = scene.window(scene.panel().child(row), 320.0, 40.0);
  let buffer = scene.full(&window, 1.0);
  assert_eq!(runs(&buffer, 10, 12, 308), [296]);
}

/// How long a full draw of an 1880 pixel wide box with a dotted border,
/// a dashed one and a solid one takes at scale 2. Each tile the border
/// crosses dashes its whole line. Run it in a release build:
/// `cargo test --release -p crabshell-render --lib time_a_dashed_bar -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn time_a_dashed_bar() {
  let card = Style::new().border(2.0, WHITE).radius(8.0);
  for (what, style) in [
    ("dotted", card.border_dash(0.0, 6.0)),
    ("dashed", card.border_dash(6.0, 4.0)),
    ("solid", card),
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
