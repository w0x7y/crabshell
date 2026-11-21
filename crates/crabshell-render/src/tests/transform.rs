//! Transformed widgets: drawn through a layer, repainted like a full draw,
//! and without seams where tiles meet.

use std::time::Instant;

use crabshell_ui::{Align, Color, Rect, Row, Stack, Style, Text, Window};

use super::{Buffer, Scene, node_at};
use crate::DamageHistory;

const CARD: Style = Style::new()
  .background(Color::hex("#45475a"))
  .border(2.0, Color::hex("#fab387"))
  .radius(6.0)
  .color(Color::hex("#cdd6f4"))
  .padding(4, 8);

/// A window with a card turned by `degrees`, also moving it, and a
/// faded stack turned inside it, over a translucent background.
fn turning(scene: &mut Scene, degrees: crabshell_reactive::Signal<f32>) -> Window {
  let panel = scene
    .panel()
    .style(Style::new().background(Color::rgba(30, 30, 46, 200)))
    .child(
      Row::new().padding(20, 150).align(Align::Start).child(
        Row::new()
          .style(move || {
            let degrees = degrees.get();
            CARD.rotate(degrees).translate(degrees / 40.0, 0.0)
          })
          .gap(4)
          .child(Text::new("turning"))
          .child(
            Stack::new()
              .style(move || Style::new().opacity(0.6).rotate(-2.0 * degrees.get()))
              .child(
                Row::new().style(Style::new().background(Color::hex("#89b4fa")).padding(6, 6)),
              ),
          ),
      ),
    );
  scene.window(panel, 400.0, 80.0)
}

#[test]
fn turning_repaints_like_drawing_everything_and_only_where_it_was_and_is() {
  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    let mut scene = Scene::new();
    let degrees = scene.cx.signal(0.0_f32);
    let window = turning(&mut scene, degrees);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    scene.draw(&window, &mut history, &mut buffer);
    for step in [15.0, 40.0, 95.0, 180.0, 333.3] {
      let old = node_at(window.view().root(), &[0, 0]).bounds();
      degrees.set(step);
      scene.frame(&window);
      let new = node_at(window.view().root(), &[0, 0]).bounds();
      let report = scene.draw(&window, &mut history, &mut buffer);
      let full = scene.full(&window, scale);
      assert!(
        buffer.pixels == full.pixels,
        "{step} degrees at {scale}: the repaint differs from a full draw"
      );
      // Damage rounds outward and adds a pixel for antialiasing.
      let both = old.union(new);
      let left = (both.x * scale).floor() - 1.0;
      let right = (both.right() * scale).ceil() + 1.0;
      let top = ((both.y * scale).floor() - 1.0).max(0.0);
      let bottom = (both.bottom() * scale).ceil() + 1.0;
      assert!(!report.is_empty());
      for rect in &report {
        assert!(
          rect.x as f32 >= left
            && (rect.x + rect.width) as f32 <= right
            && rect.y as f32 >= top
            && (rect.y + rect.height) as f32 <= bottom,
          "{step} degrees at {scale}: {rect:?} is outside {both:?}"
        );
      }
    }
  }
}

#[test]
fn a_turned_box_shows_no_seams_where_tiles_meet() {
  let color = [0x89, 0xb4, 0xfa, 255];
  let mut scene = Scene::new();
  let panel = scene.panel().child(
    Row::new().padding(30, 30).align(Align::Start).child(
      Row::new().style(
        Style::new()
          .background(Color::rgb(color[0], color[1], color[2]))
          .width(200.0)
          .height(60.0)
          .rotate(10.0),
      ),
    ),
  );
  let window = scene.window(panel, 260.0, 120.0);
  for scale in [1.0, 1.5, 2.0] {
    let buffer = scene.full(&window, scale);
    // The box's middle, (130, 60), turned back: anything 2 pixels inside
    // its edges is its color alone.
    let (sin, cos) = 10.0_f32.to_radians().sin_cos();
    let mut inside = 0;
    for y in 0..buffer.height {
      for x in 0..buffer.width {
        let (dx, dy) = (
          (x as f32 + 0.5) / scale - 130.0,
          (y as f32 + 0.5) / scale - 60.0,
        );
        let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
        if u.abs() < 100.0 - 2.0 && v.abs() < 30.0 - 2.0 {
          inside += 1;
          assert_eq!(buffer.pixel(x, y), color, "at ({x}, {y}) at {scale}");
        }
      }
    }
    assert!(inside > 10_000, "{inside}");
  }
}

#[test]
fn layers_are_made_only_for_a_transform() {
  let mut scene = Scene::new();
  let turned = scene.cx.signal(false);
  let panel = scene.panel().child(
    Row::new().align(Align::Start).child(
      Row::new()
        .style(move || {
          let card = CARD.rotate(if turned.get() { 1.0 } else { 0.0 });
          // Unset and identity transforms alike draw straight in.
          card.scale(1.0).translate(0.0, 0.0).origin(0.0, 0.0)
        })
        .child(Text::new("plain")),
    ),
  );
  let window = scene.window(panel, 300.0, 100.0);
  scene.full(&window, 2.0);
  assert_eq!(scene.renderer.work().0, 0, "no layer without a transform");
  turned.set(true);
  scene.frame(&window);
  scene.full(&window, 1.0);
  // Only the card's own area and margins, not the 64-pixel tiles.
  let bounds = node_at(window.view().root(), &[0, 0]).bounds();
  let area = (bounds.width * bounds.height) as u64;
  let layers = scene.renderer.work().0;
  assert!(
    layers > 0 && layers < area * 3 / 2,
    "{layers} pixels for {area}"
  );
}

/// How long one frame of a turning 200 by 60 card with text takes to draw
/// at scale 2. Run it in a release build:
/// `cargo test --release -p crabshell-render --lib time_a_turning_card -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn time_a_turning_card() {
  let mut scene = Scene::new();
  let degrees = scene.cx.signal(0.0_f32);
  let panel = scene
    .panel()
    .style(Style::new().background(Color::hex("#1e1e2e")))
    .child(
      Row::new().padding(70, 70).align(Align::Start).child(
        Row::new()
          .style(move || CARD.width(200.0).height(60.0).rotate(degrees.get()))
          .child(Text::new("a turning card")),
      ),
    );
  let window = scene.window(panel, 340.0, 200.0);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 2.0);
  scene.draw(&window, &mut history, &mut buffer);
  let frames = 360;
  let mut drawing = std::time::Duration::ZERO;
  for frame in 1..=frames {
    degrees.set(frame as f32);
    scene.frame(&window);
    let start = Instant::now();
    scene.draw(&window, &mut history, &mut buffer);
    drawing += start.elapsed();
  }
  let bounds: Rect = node_at(window.view().root(), &[0, 0]).bounds();
  println!(
    "a frame of the turning card at scale 2 takes {:.3} ms on average over {frames} frames ({bounds:?})",
    drawing.as_secs_f64() * 1000.0 / f64::from(frames)
  );
}

/// A 300 by 90 window with "Sharp" in `font` at its corner, scaled by
/// `scale` from there.
fn sharp_text(scene: &mut Scene, size: f32, scale: f32) -> Window {
  let text = Stack::new()
    .style(Style::new().scale(scale).origin(0.0, 0.0))
    .child(
      Text::new("Sharp").style(
        Style::new()
          .font(crabshell_ui::Font::new("sans-serif", size))
          .color(Color::rgb(0, 0, 0)),
      ),
    );
  let row = Row::new().align(Align::Start).child(text);
  scene.window(scene.panel().child(row), 300.0, 90.0)
}

#[test]
fn text_scaled_up_draws_as_sharp_as_text_that_size() {
  for scale in [1.0, 2.0] {
    let mut scene = Scene::new();
    let scaled = sharp_text(&mut scene, 13.0, 3.0);
    let scaled = scene.full(&scaled, scale);
    let large = sharp_text(&mut scene, 39.0, 1.0);
    let large = scene.full(&large, scale);
    // Edges a blurry upscale would smear: pixels between clear and
    // solid. Glyphs placed at three times the size differ by a little.
    let soft = |buffer: &Buffer| {
      buffer
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| (24..232).contains(&pixel[3]))
        .count() as f32
    };
    let (soft_scaled, soft_large) = (soft(&scaled), soft(&large));
    assert!(
      soft_scaled < soft_large * 1.25,
      "at scale {scale}: {soft_scaled} soft pixels, {soft_large} drawn at that size"
    );
  }
}

/// A widget moved by part of a pixel lands where tiny-skia's nearest
/// sampling puts a moved layer, its edge on the pixel whose middle the
/// moved edge passes, at its opacity.
#[test]
fn a_widget_moved_by_part_of_a_pixel_lands_on_the_nearest_pixel() {
  for (scale, moved, edge) in [
    (1.0, 0.4, 10),
    (1.0, 0.6, 11),
    (2.0, 0.3, 21),
    (2.0, -0.4, 19),
  ] {
    let mut scene = Scene::new();
    let panel = scene.panel().child(
      Row::new().padding(0, 10).align(Align::Start).child(
        Row::new().style(
          Style::new()
            .width(20.0)
            .height(10.0)
            .background(Color::rgb(255, 0, 0))
            .translate(moved, 0.0)
            .opacity(0.6),
        ),
      ),
    );
    let window = scene.window(panel, 64.0, 10.0);
    let drawn = scene.full(&window, scale);
    let y = 5;
    assert_eq!(drawn.pixel(edge - 1, y), [0; 4], "moved {moved} at {scale}");
    assert_eq!(
      drawn.pixel(edge, y),
      [153, 0, 0, 153],
      "moved {moved} at {scale}"
    );
  }
}

#[test]
fn a_scaled_layer_blends_as_tiny_skias_bilinear_pipeline_does() {
  use tiny_skia::{FilterQuality, IntSize, Pixmap, PixmapPaint, Transform};
  // Premultiplied pixels of every kind, a clear margin round them.
  let (width, height) = (37u32, 23u32);
  let pixels: Vec<u8> = (0..width * height)
    .flat_map(|at| {
      let (x, y) = (at % width, at / width);
      let edge = x < 2 || y < 2 || x >= width - 2 || y >= height - 2;
      let alpha = if edge { 0 } else { (at * 37 % 256) as u8 };
      let channel = |n: u32| ((at * n % 256) as u8).min(alpha);
      [channel(11), channel(53), channel(97), alpha]
    })
    .collect();
  let layer =
    Pixmap::from_vec(pixels, IntSize::from_wh(width, height).expect("a size")).expect("a layer");
  let under: Vec<u8> = (0..64u32 * 48)
    .flat_map(|at| [(at % 251) as u8, (at % 7 * 30) as u8, 200, 255])
    .collect();
  let mut worst = 0;
  for (scale, x, y) in [
    (0.8, 3.3, 4.7),
    (0.95, -5.25, 1.0),
    (1.3, 10.5, -3.6),
    (0.37, 20.0, 20.0),
    (1.75, 0.1, 0.9),
  ] {
    for opacity in [1.0, 0.61, 0.07] {
      let at = Transform::from_row(scale, 0.0, 0.0, scale * 1.1, x, y);
      let mut ours = under.clone();
      let mut theirs = under.clone();
      let mut pixmap = tiny_skia::PixmapMut::from_bytes(&mut ours, 64, 48).expect("a pixmap");
      crate::draw::blend_transformed(&mut pixmap, &layer, at, opacity);
      let mut pixmap = tiny_skia::PixmapMut::from_bytes(&mut theirs, 64, 48).expect("a pixmap");
      let paint = PixmapPaint {
        opacity,
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
      };
      pixmap.draw_pixmap(0, 0, layer.as_ref(), &paint, at, None);
      let differs = ours.iter().zip(&theirs).map(|(a, b)| a.abs_diff(*b));
      worst = worst.max(differs.max().unwrap_or(0));
    }
  }
  assert!(worst <= 1, "off by {worst}");
}
