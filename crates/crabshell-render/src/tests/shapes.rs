//! Rules the golden images show, checked pixel by pixel.

use crabshell_ui::{Align, Color, Row, Stack, Style};

use super::Scene;

const BACKGROUND: Color = Color::rgb(0, 0, 0);

/// `over` at `alpha` out of 255 over `under`, as tiny-skia blends.
fn mix(under: Color, over: Color, alpha: u8) -> [u8; 3] {
  let blend = |a: u8, b: u8| {
    ((u16::from(a) * (255 - u16::from(alpha)) + u16::from(b) * u16::from(alpha) + 127) / 255) as u8
  };
  [
    blend(under.r, over.r),
    blend(under.g, over.g),
    blend(under.b, over.b),
  ]
}

fn close(pixel: [u8; 4], expected: [u8; 3]) -> bool {
  pixel[..3]
    .iter()
    .zip(expected)
    .all(|(a, b)| a.abs_diff(b) <= 2)
}

#[test]
fn opacity_applies_to_a_widget_and_its_children_as_one_layer() {
  let blue = Color::rgb(0, 0, 255);
  let yellow = Color::rgb(255, 255, 0);
  let mut scene = Scene::new();
  let stack = Stack::new()
    .style(Style::new().opacity(0.5))
    .child(Row::new().style(Style::new().background(blue).padding(10, 10)))
    .child(
      Row::new()
        .padding(5, 5)
        .align(Align::Start)
        .child(Row::new().style(Style::new().background(yellow).padding(5, 5))),
    );
  let panel = scene
    .panel()
    .style(Style::new().background(BACKGROUND))
    .child(Row::new().align(Align::Start).child(stack));
  let window = scene.window(panel, 40.0, 20.0);
  let buffer = scene.full(&window, 1.0);
  // Inside the yellow square, the blue under it doesn't show through.
  let pixel = buffer.pixel(10, 10);
  assert!(close(pixel, mix(BACKGROUND, yellow, 128)), "{pixel:?}");
  let pixel = buffer.pixel(2, 2);
  assert!(close(pixel, mix(BACKGROUND, blue, 128)), "{pixel:?}");
}

#[test]
fn edges_snap_to_whole_pixels_at_a_fractional_scale() {
  let red = Color::rgb(255, 0, 0);
  let mut scene = Scene::new();
  // At 1.5, the box's left edge at 5 lands on 7.5 and snaps to 8.
  let panel = scene
    .panel()
    .style(Style::new().background(BACKGROUND))
    .child(
      Row::new()
        .padding(0, 5)
        .align(Align::Start)
        .child(Row::new().style(Style::new().background(red).padding(5, 5))),
    );
  let window = scene.window(panel, 40.0, 20.0);
  let buffer = scene.full(&window, 1.5);
  assert_eq!(buffer.pixel(7, 5), [0, 0, 0, 255]);
  assert_eq!(buffer.pixel(8, 5), [255, 0, 0, 255]);
  // 5 + 10 = 15 logical is 22.5, so the box ends after column 22.
  assert_eq!(buffer.pixel(22, 5), [255, 0, 0, 255]);
  assert_eq!(buffer.pixel(23, 5), [0, 0, 0, 255]);
}

#[test]
fn a_border_is_drawn_inside_the_edge() {
  let border = Color::rgb(0, 255, 0);
  let fill = Color::rgb(0, 0, 255);
  let mut scene = Scene::new();
  let panel = scene
    .panel()
    .style(Style::new().background(BACKGROUND))
    .child(
      Row::new().align(Align::Start).child(
        Row::new().style(
          Style::new()
            .background(fill)
            .border(2.0, border)
            .padding(6, 6),
        ),
      ),
    );
  let window = scene.window(panel, 40.0, 20.0);
  let buffer = scene.full(&window, 1.0);
  // The 12 by 12 box: two columns of border, then the fill.
  assert_eq!(buffer.pixel(0, 6), [0, 255, 0, 255]);
  assert_eq!(buffer.pixel(1, 6), [0, 255, 0, 255]);
  assert_eq!(buffer.pixel(2, 6), [0, 0, 255, 255]);
  assert_eq!(buffer.pixel(11, 6), [0, 255, 0, 255]);
  assert_eq!(buffer.pixel(12, 6), [0, 0, 0, 255]);
}

#[test]
fn a_border_thinner_than_a_pixel_gets_one() {
  let border = Color::rgb(0, 255, 0);
  let fill = Color::rgb(0, 0, 255);
  let mut scene = Scene::new();
  let panel = scene
    .panel()
    .style(Style::new().background(BACKGROUND))
    .child(
      Row::new().align(Align::Start).child(
        Row::new().style(
          Style::new()
            .background(fill)
            .border(0.3, border)
            .padding(6, 6),
        ),
      ),
    );
  let window = scene.window(panel, 40.0, 20.0);
  let buffer = scene.full(&window, 1.0);
  assert_eq!(buffer.pixel(0, 6), [0, 255, 0, 255]);
  assert_eq!(buffer.pixel(1, 6), [0, 0, 255, 255]);
}

#[test]
fn the_window_background_reaches_the_buffers_last_pixel() {
  let red = Color::rgb(255, 0, 0);
  let mut scene = Scene::new();
  // 13 by 10 at 1.25 is 16.25 by 12.5, so the buffer is 17 by 13 and
  // rounding the edges would leave its last column out.
  let panel = scene.panel().style(Style::new().background(red));
  let window = scene.window(panel, 13.0, 10.0);
  let buffer = scene.full(&window, 1.25);
  assert_eq!((buffer.width, buffer.height), (17, 13));
  assert_eq!(buffer.pixel(16, 12), [255, 0, 0, 255]);
  assert_eq!(buffer.pixel(0, 0), [255, 0, 0, 255]);
}

#[test]
fn text_is_drawn_in_its_colors_alpha() {
  use crabshell_ui::Text;

  let mut scene = Scene::new();
  let draw = |scene: &mut Scene, alpha: u8| {
    let panel = scene
      .panel()
      .style(
        Style::new()
          .background(BACKGROUND)
          .color(Color::rgba(255, 255, 255, alpha)),
      )
      .child(Text::new("HHHH"));
    let window = scene.window(panel, 60.0, 20.0);
    let buffer = scene.full(&window, 1.0);
    buffer.pixels.iter().step_by(4).copied().max().unwrap_or(0)
  };
  assert_eq!(draw(&mut scene, 255), 255);
  let brightest = draw(&mut scene, 128);
  assert!((126..=130).contains(&brightest), "{brightest}");
}

#[test]
fn border_widths_round_down_to_whole_pixels() {
  let border = Color::rgb(0, 255, 0);
  let fill = Color::rgb(0, 0, 255);
  for (width, scale, pixels) in [(1.0, 1.5, 1), (1.0, 1.75, 1), (1.5, 1.0, 1), (2.0, 1.5, 3)] {
    let mut scene = Scene::new();
    let panel = scene
      .panel()
      .style(Style::new().background(BACKGROUND))
      .child(
        Row::new().align(Align::Start).child(
          Row::new().style(
            Style::new()
              .background(fill)
              .border(width, border)
              .padding(8, 8),
          ),
        ),
      );
    let window = scene.window(panel, 40.0, 20.0);
    let buffer = scene.full(&window, scale);
    let y = (8.0 * scale) as u32;
    let drawn = (0..buffer.width)
      .take_while(|&x| buffer.pixel(x, y) == [0, 255, 0, 255])
      .count();
    assert_eq!(drawn, pixels, "a {width}-pixel border at {scale}");
  }
}

#[test]
fn an_opacity_layer_covers_only_its_widget() {
  let mut scene = Scene::new();
  // Ten 16 by 8 translucent boxes in a 1920 by 30 bar at scale 2.
  let row = (0..10).fold(Row::new().gap(4).align(Align::Center), |row, _| {
    row.child(
      Row::new().style(
        Style::new()
          .opacity(0.5)
          .background(Color::rgb(200, 0, 0))
          .padding(4, 8),
      ),
    )
  });
  let window = scene.window(scene.panel().child(row), 1920.0, 30.0);
  scene.full(&window, 2.0);
  // Each layer is the box, 32 by 16 pixels, plus a pixel around it; the
  // first box starts at the window's edge, so it has no pixel on its left.
  let (layer_pixels, ..) = scene.renderer.work();
  assert_eq!(layer_pixels, 9 * 34 * 18 + 33 * 18);
}

#[test]
fn a_repaint_skips_widgets_outside_the_damage() {
  use crabshell_ui::Text;

  let mut scene = Scene::new();
  let label = scene.cx.signal(String::from("a"));
  let row = (0..40).fold(
    Row::new().gap(4).child(Text::new(move || label.get())),
    |row, number| row.child(Text::new(number.to_string())),
  );
  let window = scene.window(scene.panel().child(row), 1920.0, 30.0);
  let mut history = crate::DamageHistory::new();
  let mut buffer = super::Buffer::new(&window, 1.0);
  scene.draw(&window, &mut history, &mut buffer);
  let (_, before, _) = scene.renderer.work();
  label.set(String::from("b"));
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);
  let (_, after, _) = scene.renderer.work();
  // The window, the row, the changed text and the neighbors that share its
  // 64-pixel tile; not all 43 nodes.
  assert!(after - before <= 12, "{} nodes drawn", after - before);
}

mod gradients {
  use crabshell_ui::{Align, Color, Gradient, Row, Style};

  use super::super::Scene;
  use super::close;

  const RED: Color = Color::rgb(255, 0, 0);
  const BLUE: Color = Color::rgb(0, 0, 255);

  /// The buffer of a 100 by 20 window filled with `gradient`, at scale 1.
  fn filled(gradient: Gradient) -> super::super::Buffer {
    let mut scene = Scene::new();
    let window = scene.window(
      scene.panel().style(Style::new().gradient(gradient)),
      100.0,
      20.0,
    );
    scene.full(&window, 1.0)
  }

  #[test]
  fn a_linear_gradient_runs_at_its_angle_from_edge_to_edge() {
    let across = filled(Gradient::linear(90.0, RED, BLUE));
    assert!(
      close(across.pixel(0, 10), [254, 0, 1]),
      "{:?}",
      across.pixel(0, 10)
    );
    assert!(
      close(across.pixel(50, 0), [126, 0, 129]),
      "{:?}",
      across.pixel(50, 0)
    );
    assert!(
      close(across.pixel(99, 19), [1, 0, 254]),
      "{:?}",
      across.pixel(99, 19)
    );
    let down = filled(Gradient::linear(180.0, RED, BLUE));
    assert!(
      close(down.pixel(70, 0), [249, 0, 6]),
      "{:?}",
      down.pixel(70, 0)
    );
    assert!(
      close(down.pixel(70, 19), [6, 0, 249]),
      "{:?}",
      down.pixel(70, 19)
    );
  }

  #[test]
  fn a_middle_stop_sits_where_it_says() {
    let green = Color::rgb(0, 255, 0);
    let three = filled(Gradient::linear(90.0, RED, BLUE).stop(0.2, green));
    assert!(
      close(three.pixel(20, 5), [0, 253, 2]),
      "{:?}",
      three.pixel(20, 5)
    );
    assert!(
      close(three.pixel(60, 5), [0, 126, 129]),
      "{:?}",
      three.pixel(60, 5)
    );
  }

  #[test]
  fn a_clear_stop_fades_each_side_out_in_its_own_color() {
    // As CSS: blended with alpha applied, red fades out without darkening
    // toward the clear stop's black.
    let clear = Color::rgba(0, 0, 0, 0);
    let fading = filled(Gradient::linear(90.0, RED, BLUE).stop(0.5, clear));
    let [r, g, b, a] = fading.pixel(25, 5);
    assert!(
      a.abs_diff(124) <= 3 && r.abs_diff(a) <= 2 && g == 0 && b == 0,
      "{:?}",
      [r, g, b, a]
    );
    let [r, g, b, a] = fading.pixel(75, 5);
    assert!(
      a.abs_diff(130) <= 3 && b.abs_diff(a) <= 2 && g == 0 && r == 0,
      "{:?}",
      [r, g, b, a]
    );
  }

  #[test]
  fn a_sweep_gradient_turns_clockwise_from_its_angle_around_the_middle() {
    let window = |gradient| {
      let mut scene = Scene::new();
      let window = scene.window(
        scene.panel().style(Style::new().gradient(gradient)),
        100.0,
        100.0,
      );
      scene.full(&window, 1.0)
    };
    // From 12 o'clock: a quarter turn at 3, half at 6, three quarters at
    // 9, and the end just before 12 again.
    let from_up = window(Gradient::sweep(0.0, RED, BLUE));
    for ((x, y), expected) in [
      ((50, 4), [255, 0, 0]),
      ((95, 49), [192, 0, 63]),
      ((50, 95), [128, 0, 127]),
      ((4, 50), [64, 0, 191]),
      ((49, 4), [0, 0, 255]),
    ] {
      let pixel = from_up.pixel(x, y);
      assert!(close(pixel, expected), "at {x}, {y}: {pixel:?}");
    }
    // From 3 o'clock, a quarter turn is at 6.
    let from_right = window(Gradient::sweep(90.0, RED, BLUE));
    for ((x, y), expected) in [((95, 50), [255, 0, 0]), ((50, 95), [192, 0, 63])] {
      let pixel = from_right.pixel(x, y);
      assert!(close(pixel, expected), "at {x}, {y}: {pixel:?}");
    }
  }

  #[test]
  fn a_radial_gradient_runs_out_from_its_center_to_its_radius() {
    // Centered at 20, 80 of a 100 by 100 box, 40 logical pixels across
    // and so 80 at scale 2; past that the end color holds.
    let glow = Gradient::radial(RED, BLUE).center(0.2, 0.8).radius(40.0);
    for scale in [1.0, 2.0] {
      let buffer = sized(glow, 100.0, 100.0, scale);
      let at = |x: f32, y: f32| buffer.pixel((x * scale) as u32, (y * scale) as u32);
      // Half a pixel from the center is already a little way along.
      let [r, _, b, _] = at(20.0, 80.0);
      assert!(r > 245 && b < 10, "{:?}", at(20.0, 80.0));
      // Halfway out, give or take the half pixel to the pixel's center.
      let halfway = |[r, g, b, _]: [u8; 4]| r.abs_diff(128) <= 8 && g == 0 && b.abs_diff(127) <= 8;
      assert!(halfway(at(40.0, 80.0)), "{:?}", at(40.0, 80.0));
      assert!(halfway(at(20.0, 60.0)), "{:?}", at(20.0, 60.0));
      assert!(close(at(62.0, 80.0), [0, 0, 255]), "{:?}", at(62.0, 80.0));
      assert!(close(at(95.0, 5.0), [0, 0, 255]), "{:?}", at(95.0, 5.0));
    }
    // Without a radius it reaches the corner farthest from the center,
    // 100 right and 80 up.
    let reaching = sized(
      Gradient::radial(RED, BLUE).center(0.0, 1.0),
      100.0,
      80.0,
      1.0,
    );
    assert!(
      close(reaching.pixel(99, 0), [0, 0, 255]),
      "{:?}",
      reaching.pixel(99, 0)
    );
    let halfway = reaching.pixel(50, 40);
    assert!(close(halfway, [128, 0, 127]), "{halfway:?}");
    // A radius of 0 leaves only the end color.
    let none = sized(Gradient::radial(RED, BLUE).radius(0.0), 10.0, 10.0, 1.0);
    assert!(
      close(none.pixel(5, 5), [0, 0, 255]),
      "{:?}",
      none.pixel(5, 5)
    );
    // Even where the center falls on a pixel's center, in an odd box.
    let odd = sized(Gradient::radial(RED, BLUE).radius(0.0), 11.0, 11.0, 1.0);
    assert!(close(odd.pixel(5, 5), [0, 0, 255]), "{:?}", odd.pixel(5, 5));
  }

  #[test]
  fn a_sweep_gradient_turns_around_its_center() {
    // Centered at 25, 75: straight up from there is the start, straight
    // right a quarter turn.
    let sweep = sized(
      Gradient::sweep(0.0, RED, BLUE).center(0.25, 0.75),
      100.0,
      100.0,
      1.0,
    );
    assert!(
      close(sweep.pixel(25, 10), [255, 0, 0]),
      "{:?}",
      sweep.pixel(25, 10)
    );
    assert!(
      close(sweep.pixel(90, 75), [192, 0, 63]),
      "{:?}",
      sweep.pixel(90, 75)
    );
    assert!(
      close(sweep.pixel(25, 95), [128, 0, 127]),
      "{:?}",
      sweep.pixel(25, 95)
    );
  }

  #[test]
  fn a_gradient_of_eight_colors_shows_each() {
    let green = Color::rgb(0, 255, 0);
    let black = Color::rgb(0, 0, 0);
    let stripes = Gradient::linear(90.0, RED, BLUE)
      .stop(1.0 / 7.0, green)
      .stop(2.0 / 7.0, black)
      .stop(3.0 / 7.0, RED)
      .stop(4.0 / 7.0, green)
      .stop(5.0 / 7.0, black)
      .stop(6.0 / 7.0, green);
    let buffer = sized(stripes, 701.0, 4.0, 1.0);
    for (x, expected) in [
      (0, [255, 0, 0]),
      (100, [0, 255, 0]),
      (200, [0, 0, 0]),
      (300, [255, 0, 0]),
      (400, [0, 255, 0]),
      (500, [0, 0, 0]),
      (600, [0, 255, 0]),
      (700, [0, 0, 255]),
    ] {
      let pixel = buffer.pixel(x, 2);
      assert!(close(pixel, expected), "at {x}: {pixel:?}");
    }
  }

  /// A `width` by `height` window filled with `gradient` at `scale`.
  fn sized(gradient: Gradient, width: f32, height: f32, scale: f32) -> super::super::Buffer {
    let mut scene = Scene::new();
    let panel = scene.panel().style(Style::new().gradient(gradient));
    let window = scene.window(panel, width, height);
    scene.full(&window, scale)
  }

  #[test]
  fn a_subtle_gradient_is_dithered_by_under_a_step_so_it_shows_no_bands() {
    // Four levels of red over 256 pixels: undithered, bands 64 wide.
    let from = Color::rgb(16, 0, 0);
    let to = Color::rgb(20, 0, 0);
    let buffer = sized(Gradient::linear(90.0, from, to), 256.0, 8.0, 1.0);
    let exact = |x: u32| 16.0 + 4.0 * (x as f32 + 0.5) / 256.0;
    for x in 0..256 {
      for y in 0..8 {
        let red = f32::from(buffer.pixel(x, y)[0]);
        assert!((red - exact(x)).abs() < 1.0, "at {x}, {y}: {red}");
      }
    }
    // Each 4 by 4 block averages out close to the exact color.
    for block in 0..64 {
      let mean: f32 = (0..4)
        .flat_map(|dx| (0..4).map(move |y| (block * 4 + dx, y)))
        .map(|(x, y)| f32::from(buffer.pixel(x, y)[0]))
        .sum::<f32>()
        / 16.0;
      let middle = exact(block * 4) + 1.5 * 4.0 / 256.0;
      assert!(
        (mean - middle).abs() < 0.15,
        "block {block}: {mean} {middle}"
      );
    }
  }

  #[test]
  fn a_subtle_fade_is_dithered_in_its_alpha_too() {
    // White from alpha 16 to 20 over 256 pixels, on a clear window.
    let from = Color::rgba(255, 255, 255, 16);
    let to = Color::rgba(255, 255, 255, 20);
    let buffer = sized(Gradient::linear(90.0, from, to), 256.0, 8.0, 1.0);
    let exact = |x: u32| 16.0 + 4.0 * (x as f32 + 0.5) / 256.0;
    for x in 0..256 {
      for y in 0..8 {
        let [r, g, b, a] = buffer.pixel(x, y);
        assert!(r <= a && g <= a && b <= a, "premultiplied at {x}, {y}");
        assert!((f32::from(a) - exact(x)).abs() < 1.0, "at {x}, {y}: {a}");
      }
    }
    for block in 0..64 {
      let mean: f32 = (0..4)
        .flat_map(|dx| (0..4).map(move |y| (block * 4 + dx, y)))
        .map(|(x, y)| f32::from(buffer.pixel(x, y)[3]))
        .sum::<f32>()
        / 16.0;
      let middle = exact(block * 4) + 1.5 * 4.0 / 256.0;
      assert!(
        (mean - middle).abs() < 0.15,
        "block {block}: {mean} {middle}"
      );
    }
    // One translucent color stays that color.
    let half = Color::rgba(255, 255, 255, 128);
    let flat = sized(Gradient::linear(90.0, half, half), 64.0, 64.0, 1.0);
    assert!(
      flat
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| *pixel == [128, 128, 128, 128]),
      "a translucent gradient of one color stays that color"
    );
  }

  #[test]
  fn every_gradient_shape_is_dithered_and_a_flat_one_is_not() {
    let (from, to) = (Color::rgb(40, 40, 40), Color::rgb(44, 44, 44));
    for gradient in [
      Gradient::linear(90.0, from, to),
      Gradient::radial(from, to),
      Gradient::sweep(0.0, from, to),
    ] {
      let buffer = sized(gradient, 64.0, 64.0, 1.0);
      // Neighbors differ all over, not only where one level ends: about
      // 8 times a row undithered.
      let changes = (0..64)
        .flat_map(|y| (1..64).map(move |x| (x, y)))
        .filter(|&(x, y)| buffer.pixel(x, y)[0] != buffer.pixel(x - 1, y)[0])
        .count();
      assert!(changes > 1000, "{gradient:?}: {changes}");
    }
    let flat = sized(Gradient::linear(90.0, from, from), 64.0, 64.0, 1.5);
    assert!(
      flat
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| *pixel == [40, 40, 40, 255]),
      "a gradient of one color stays that color"
    );
  }

  /// How long a full draw of the effects example's translucent bar
  /// gradient across a 1920 by 48 bar takes at scales 1 and 2, at best.
  /// Run it in
  /// a release build:
  /// `cargo test --release -p crabshell-render --lib time_the_effects_bar_gradient -- --ignored --nocapture`.
  #[test]
  #[ignore = "a measurement, not a check"]
  fn time_the_effects_bar_gradient() {
    let bar = Gradient::linear(
      90.0,
      Color::rgba(0x1e, 0x1e, 0x2e, 0xd8),
      Color::rgba(0x31, 0x32, 0x44, 0xd8),
    );
    for scale in [1.0, 2.0] {
      let mut scene = Scene::new();
      let panel = scene.panel().style(Style::new().gradient(bar));
      let window = scene.window(panel, 1920.0, 48.0);
      // The fastest of many draws, which other work on the machine
      // slows least.
      let fastest = (0..100)
        .map(|_| {
          let start = std::time::Instant::now();
          scene.full(&window, scale);
          start.elapsed()
        })
        .min()
        .expect("a draw");
      println!(
        "{:.2} ms a full draw at scale {scale}",
        fastest.as_secs_f64() * 1000.0
      );
    }
  }

  #[test]
  fn a_radial_gradient_reaches_the_farthest_corner() {
    let round = filled(Gradient::radial(RED, BLUE));
    assert!(
      close(round.pixel(50, 10), [252, 0, 3]),
      "{:?}",
      round.pixel(50, 10)
    );
    assert!(
      close(round.pixel(0, 0), [3, 0, 252]),
      "{:?}",
      round.pixel(0, 0)
    );
  }

  #[test]
  fn a_gradient_goes_over_the_background_inside_the_corners() {
    let mut scene = Scene::new();
    let window = scene.window(
      scene.panel().child(
        Row::new().align(Align::Start).child(
          Row::new().style(
            Style::new()
              .background(Color::rgb(0, 255, 0))
              .gradient(Gradient::linear(90.0, Color::rgba(0, 0, 255, 0), BLUE))
              .radius(10.0)
              .width(40.0)
              .height(20.0),
          ),
        ),
      ),
      100.0,
      20.0,
    );
    let buffer = scene.full(&window, 1.0);
    // Transparent blue at the left shows the green under it.
    assert!(
      close(buffer.pixel(1, 10), [0, 245, 10]),
      "{:?}",
      buffer.pixel(1, 10)
    );
    // Outside the rounded corner, nothing.
    assert_eq!(buffer.pixel(0, 0)[3], 0);
  }
}

mod shadows {
  use crabshell_ui::{Align, Color, Row, Shadow, Style};

  use super::super::{Buffer, Scene};

  /// A 40 by 40 white box at 20, 10 casting `shadow`, in a clear 120 by
  /// 70 window at scale 1.
  fn cast(shadow: Shadow, radius: f32) -> Buffer {
    cast_on(shadow, radius, Color::rgb(255, 255, 255))
  }

  /// [`cast`] from a box of `background`.
  fn cast_on(shadow: Shadow, radius: f32, background: Color) -> Buffer {
    let mut scene = Scene::new();
    let row = Row::new().padding(10, 20).align(Align::Start).child(
      Row::new().style(
        Style::new()
          .background(background)
          .radius(radius)
          .shadow(shadow)
          .width(40.0)
          .height(40.0),
      ),
    );
    let window = scene.window(scene.panel().child(row), 120.0, 70.0);
    scene.full(&window, 1.0)
  }

  #[test]
  fn a_sharp_shadow_is_the_box_moved_under_it() {
    let black = Color::rgb(0, 0, 0);
    let buffer = cast(Shadow::new(10.0, 10.0, 0.0, black), 0.0);
    assert_eq!(buffer.pixel(30, 30), [255, 255, 255, 255], "the box");
    assert_eq!(buffer.pixel(65, 55), [0, 0, 0, 255], "its shadow");
    assert_eq!(buffer.pixel(65, 15), [0, 0, 0, 0], "above the shadow");
    let grown = cast(Shadow::new(0.0, 0.0, 0.0, black).spread(4.0), 0.0);
    assert_eq!(grown.pixel(63, 30), [0, 0, 0, 255], "spread past the box");
    assert_eq!(grown.pixel(64, 30), [0, 0, 0, 0]);
    // As CSS: a square box's spread keeps square corners, and a rounded
    // one's grow by less than the spread while the radius is smaller.
    assert_eq!(grown.pixel(16, 6), [0, 0, 0, 255], "a square corner");
    let rounded = cast(Shadow::new(0.0, 0.0, 0.0, black).spread(8.0), 2.0);
    // A 10 pixel corner radius would leave this pixel clear; CSS's 6.6
    // covers it.
    assert!(rounded.pixel(14, 4)[3] > 128, "{:?}", rounded.pixel(14, 4));
  }

  #[test]
  fn a_blurred_edge_fades_out_over_one_and_a_half_blurs() {
    // Blur 4 is a Gaussian of deviation 2 around the box's right edge at
    // x 60, across the tile edge at 64.
    let buffer = cast(Shadow::new(0.0, 0.0, 4.0, Color::rgb(0, 0, 0)), 0.0);
    let alpha = |x| i32::from(buffer.pixel(x, 30)[3]);
    assert!((alpha(60) - 102).abs() <= 3, "{}", alpha(60));
    assert!((alpha(62) - 27).abs() <= 3, "{}", alpha(62));
    assert!((alpha(64) - 3).abs() <= 2, "{}", alpha(64));
    assert_eq!(alpha(67), 0);
    // Above the box's top edge, likewise.
    let above = i32::from(buffer.pixel(40, 9)[3]);
    assert!((above - 102).abs() <= 3, "{above}");
  }

  #[test]
  fn a_shadow_follows_the_rounded_corners() {
    let black = Color::rgb(0, 0, 0);
    let square = cast(Shadow::new(0.0, 0.0, 4.0, black), 0.0);
    let round = cast(Shadow::new(0.0, 0.0, 4.0, black), 16.0);
    // Just off the corner, diagonally: the round shadow is fainter.
    let (x, y) = (61, 8);
    assert!(round.pixel(x, y)[3] + 10 < square.pixel(x, y)[3]);
    // Along the straight edge, the same.
    assert!(round.pixel(61, 30)[3].abs_diff(square.pixel(61, 30)[3]) <= 2);
  }

  #[test]
  fn a_cut_out_shadow_is_not_drawn_under_its_box() {
    let black = Color::rgb(0, 0, 0);
    let clear = Color::rgba(255, 255, 255, 0);
    for blur in [0.0, 4.0] {
      let shadow = Shadow::new(10.0, 10.0, blur, black);
      let under = cast_on(shadow, 8.0, clear);
      assert_eq!(under.pixel(50, 40)[3], 255, "blur {blur}: under the box");
      let cut = cast_on(shadow.cut_out(), 8.0, clear);
      assert_eq!(cut.pixel(50, 40), [0, 0, 0, 0], "blur {blur}: cut out");
      assert_eq!(
        cut.pixel(58, 30),
        [0, 0, 0, 0],
        "blur {blur}: near the edge"
      );
      assert_eq!(
        cut.pixel(65, 55),
        under.pixel(65, 55),
        "blur {blur}: outside"
      );
      // Just inside a rounded corner the box doesn't cover, the shadow
      // still shows.
      assert!(
        cut.pixel(58, 49)[3] > 0,
        "blur {blur}: {:?}",
        cut.pixel(58, 49)
      );
    }
  }

  #[test]
  fn an_inset_shadow_is_drawn_inside_the_box_over_its_background() {
    let black = Color::rgb(0, 0, 0);
    let white = [255, 255, 255, 255];
    // Spread 4 shrinks the hole 4 pixels from each edge of the 40 by 40
    // box at 20, 10.
    let ring = cast(Shadow::new(0.0, 0.0, 0.0, black).spread(4.0).inset(), 0.0);
    assert_eq!(ring.pixel(20, 30), [0, 0, 0, 255], "the left edge");
    assert_eq!(ring.pixel(23, 30), [0, 0, 0, 255], "4 pixels in");
    assert_eq!(ring.pixel(24, 30), white, "the hole");
    assert_eq!(ring.pixel(40, 45), white);
    assert_eq!(ring.pixel(40, 46), [0, 0, 0, 255], "4 pixels up");
    assert_eq!(ring.pixel(40, 49), [0, 0, 0, 255], "the bottom edge");
    for (x, y) in [(19, 30), (60, 30), (40, 9), (40, 50)] {
      assert_eq!(ring.pixel(x, y), [0, 0, 0, 0], "outside, at {x}, {y}");
    }
    // Moved down and right, the hole leaves the top and left edges dark.
    let moved = cast(Shadow::new(5.0, 5.0, 0.0, black).inset(), 0.0);
    assert_eq!(moved.pixel(24, 30), [0, 0, 0, 255], "the left");
    assert_eq!(moved.pixel(40, 14), [0, 0, 0, 255], "the top");
    assert_eq!(moved.pixel(25, 30), white);
    assert_eq!(moved.pixel(59, 49), white, "the bottom right");
    assert_eq!(moved.pixel(65, 55), [0, 0, 0, 0], "nothing outside");
  }

  #[test]
  fn a_blurred_inset_shadow_fades_in_from_the_edge_and_follows_the_corners() {
    let black = Color::rgb(0, 0, 0);
    let buffer = cast(Shadow::new(0.0, 0.0, 8.0, black).inset(), 12.0);
    // Darker toward the left edge, white in the middle.
    let red = |x, y| buffer.pixel(x, y)[0];
    assert!(red(20, 30) < red(22, 30) && red(22, 30) < red(26, 30));
    assert_eq!(buffer.pixel(40, 30), [255, 255, 255, 255]);
    // The edge of the hole meets the edge of the box: half dark there.
    assert!((100..=156).contains(&red(20, 30)), "{}", red(20, 30));
    // Cut to the rounded corner, and nothing outside the box.
    assert_eq!(buffer.pixel(20, 10), [0, 0, 0, 0]);
    assert_eq!(buffer.pixel(16, 30), [0, 0, 0, 0]);
    assert_eq!(buffer.pixel(40, 52), [0, 0, 0, 0]);
  }

  #[test]
  fn an_inset_shadow_starts_inside_the_border() {
    let mut scene = Scene::new();
    let row = Row::new().padding(10, 20).align(Align::Start).child(
      Row::new().style(
        Style::new()
          .background(Color::rgb(255, 255, 255))
          .border(3.0, Color::rgba(255, 0, 0, 128))
          .shadow(
            Shadow::new(0.0, 0.0, 0.0, Color::rgb(0, 0, 0))
              .spread(2.0)
              .inset(),
          )
          .width(40.0)
          .height(40.0),
      ),
    );
    let window = scene.window(scene.panel().child(row), 120.0, 70.0);
    let buffer = scene.full(&window, 1.0);
    // The translucent border shows the white background, not the shadow.
    assert_eq!(buffer.pixel(21, 30), [255, 127, 127, 255], "the border");
    assert_eq!(buffer.pixel(24, 30), [0, 0, 0, 255], "the shadow");
    assert_eq!(buffer.pixel(25, 30), [255, 255, 255, 255], "the hole");
  }

  #[test]
  fn a_shadow_with_a_number_that_isnt_finite_is_not_drawn() {
    for blur in [f32::NAN, f32::INFINITY] {
      let buffer = cast(Shadow::new(10.0, 0.0, blur, Color::rgb(0, 0, 0)), 0.0);
      assert_eq!(buffer.pixel(62, 30), [0, 0, 0, 0], "{blur}");
    }
  }
}

/// An opaque background on whole pixels is filled straight into the
/// pixels, as tiny-skia's antialiased path fill gives it, past the
/// pixmap's edges included; anything else is left to the path fill.
#[test]
fn an_opaque_whole_pixel_fill_matches_the_path_fill() {
  let color = Color::rgb(30, 102, 245);
  let rect = |l, t, r, b| tiny_skia::Rect::from_ltrb(l, t, r, b).expect("a rect");
  for area in [
    rect(0.0, 0.0, 64.0, 64.0),
    rect(3.0, 5.0, 20.0, 9.0),
    rect(-10.0, -4.0, 30.0, 80.0),
    rect(50.0, 60.0, 900.0, 700.0),
  ] {
    let mut path = tiny_skia::Pixmap::new(64, 64).expect("a pixmap");
    path.fill(tiny_skia::Color::from_rgba8(9, 9, 9, 255));
    let mut direct = path.clone();
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    let shape = tiny_skia::PathBuilder::from_rect(area);
    path.fill_path(
      &shape,
      &paint,
      tiny_skia::FillRule::Winding,
      tiny_skia::Transform::identity(),
      None,
    );
    assert!(crate::draw::fill_opaque(&mut direct.as_mut(), area, color));
    assert!(direct == path, "{area:?} differs from the path fill");
  }
  let mut pixmap = tiny_skia::Pixmap::new(8, 8).expect("a pixmap");
  let half = rect(0.5, 0.0, 4.0, 4.0);
  assert!(!crate::draw::fill_opaque(&mut pixmap.as_mut(), half, color));
  let whole = rect(0.0, 0.0, 4.0, 4.0);
  let translucent = Color::rgba(30, 102, 245, 128);
  assert!(!crate::draw::fill_opaque(
    &mut pixmap.as_mut(),
    whole,
    translucent
  ));
}
