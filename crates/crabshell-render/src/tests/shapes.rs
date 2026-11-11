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
  let (layer_pixels, _) = scene.renderer.work();
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
  let (_, before) = scene.renderer.work();
  label.set(String::from("b"));
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);
  let (_, after) = scene.renderer.work();
  // The window, the row, the changed text and a neighbor or two whose
  // ink allowance reaches it; not all 43 nodes.
  assert!(after - before <= 6, "{} nodes drawn", after - before);
}
