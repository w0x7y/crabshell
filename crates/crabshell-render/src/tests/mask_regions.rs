//! A widget's `mask_region`: what it keeps, its edges, and repaints.

use crabshell_ui::{Align, Color, Gradient, Image, Region, Row, Stack, Style, Window};

use super::{Buffer, Scene, fixture};
use crate::DamageHistory;

const WHITE: Color = Color::rgb(255, 255, 255);

/// A 40 by 40 box at 20, 10 in a clear 120 by 70 window, holding a white
/// square 60 wide, past the box, and masked by `region`.
fn masked(region: Region) -> Buffer {
  let mut scene = Scene::new();
  let inside = Row::new().style(Style::new().background(WHITE).width(60.0).height(60.0));
  let drawn = Stack::new()
    .style(Style::new().width(40.0).height(40.0))
    .mask_region(Some(region))
    .child(inside);
  let row = Row::new().padding(10, 20).align(Align::Start).child(drawn);
  let window = scene.window(scene.panel().child(row), 120.0, 70.0);
  scene.full(&window, 1.0)
}

#[test]
fn a_region_keeps_the_widget_only_inside_it_and_its_box() {
  // Reaching past the box, with a round-cornered hole in the middle.
  let hole = Region::rect(10, 10, 20, 20).radius(6.0);
  let buffer = masked(Region::rect(-20, -20, 100, 100).subtract(hole));
  assert_eq!(buffer.pixel(22, 12), [255; 4], "kept inside the region");
  assert_eq!(buffer.pixel(40, 30)[3], 0, "the hole is clear");
  assert_eq!(buffer.pixel(65, 30)[3], 0, "nothing past the box");
  assert_eq!(buffer.pixel(30, 55)[3], 0, "below the box neither");
  // The hole's top-left corner, at 30, 20, is cut antialiased.
  let corner = buffer.pixel(31, 21)[3];
  assert!(0 < corner && corner < 255, "an edge pixel {corner}");
}

#[test]
fn each_join_keeps_what_its_name_says() {
  let left = || Region::rect(0, 0, 24, 40);
  let right = Region::rect(16, 0, 24, 40);
  // Columns 20 to 28 are in both, 32 in only the left one and 52 in only
  // the right one; 60 is in neither.
  for (region, kept) in [
    (left().union(right.clone()), [true, true, true]),
    (left().subtract(right.clone()), [false, true, false]),
    (left().intersect(right.clone()), [true, false, false]),
    (left().xor(right.clone()), [false, true, true]),
  ] {
    let buffer = masked(region.clone());
    for (x, kept) in [40, 30, 50].into_iter().zip(kept) {
      let alpha = buffer.pixel(x, 30)[3];
      assert_eq!(alpha == 255, kept, "{region:?} at {x}: {alpha}");
      assert!(alpha == 255 || alpha == 0, "{region:?} at {x}: {alpha}");
    }
  }
  assert_eq!(masked(Region::empty()).pixel(40, 30)[3], 0);
}

/// An image cut by a rect minus two rounded rects, a colored stack cut by
/// an ellipse, and a turned, faded stack cut by a region larger than its
/// box with an ellipse taken out, which `inset` grows.
pub(super) fn regions(
  scene: &Scene,
  inset: crabshell_reactive::Signal<i32>,
) -> crabshell_ui::PanelWindow {
  let windows = Region::rect(0, 0, 48, 48)
    .subtract(Region::rect(4, 4, 18, 40).radius(6.0))
    .subtract(Region::rect(26, 8, 18, 32).radii(10.0, 2.0, 10.0, 2.0));
  scene
    .panel()
    .style(Style::new().background(Color::hex("#1e1e2e")))
    .child(
      Row::new()
        .padding(6, 10)
        .gap(14)
        .align(Align::Start)
        .child(
          Image::new(fixture("stripes.png"))
            .width(48)
            .height(48)
            .mask_region(Some(windows)),
        )
        .child(
          Stack::new()
            .style(
              Style::new()
                .gradient(Gradient::linear(
                  90.0,
                  Color::hex("#89b4fa"),
                  Color::hex("#f38ba8"),
                ))
                .width(44.0)
                .height(44.0),
            )
            .mask_region(Some(Region::ellipse(0, 0, 44, 44))),
        )
        .child(
          Stack::new()
            .style(
              Style::new()
                .background(Color::hex("#a6e3a1"))
                .width(44.0)
                .height(44.0)
                .rotate(-12.0)
                .opacity(0.9),
            )
            .mask_region(move || {
              let inset = inset.get();
              let side = (44 - 2 * inset).max(0) as u32;
              Some(
                Region::rect(-10, -10, 64, 64).subtract(Region::ellipse(inset, inset, side, side)),
              )
            }),
        ),
    )
}

#[test]
fn a_region_repaints_like_drawing_everything_at_every_scale() {
  use crate::{Canvas, PixelFormat};

  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    let mut scene = Scene::new();
    let inset = scene.cx.signal(10);
    let panel = regions(&scene, inset);
    let window: Window = scene.window(panel, 200.0, 60.0);
    let full = scene.full(&window, scale);
    // A repaint whose right edge cuts through every one, a tile column
    // after another.
    for right in (6..200).step_by(13) {
      let mut history = DamageHistory::new();
      let mut buffer = Buffer::new(&window, scale);
      for damage in [
        Vec::new(),
        vec![crabshell_ui::Rect::new(0.0, 0.0, right as f32, 60.0)],
      ] {
        scene
          .renderer
          .draw(
            &window.view(),
            &damage,
            &mut scene.text,
            &mut history,
            Canvas {
              pixels: &mut buffer.pixels,
              width: buffer.width,
              height: buffer.height,
              scale,
              format: PixelFormat::Abgr8888,
              age: &mut buffer.age,
            },
          )
          .expect("the test's canvas fits");
      }
      assert!(
        buffer.pixels == full.pixels,
        "a repaint to x {right} at scale {scale} differs from a full draw"
      );
    }
    // A new region repaints what it changes, as a full draw has it.
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    scene.draw(&window, &mut history, &mut buffer);
    inset.set(4);
    scene.frame(&window);
    scene.draw(&window, &mut history, &mut buffer);
    let now = scene.full(&window, scale);
    assert!(buffer.pixels == now.pixels, "after the change at {scale}");
    assert!(buffer.pixels != full.pixels, "it changed at {scale}");
  }
}

/// How long a full draw of a 1920 by 1080 image takes at scales 1 and 2:
/// masked by a rect minus two rounded rects, as one frame of a bloom is,
/// by the whole box, and unmasked. Run it in a release build:
/// `cargo test --release -p crabshell-render --lib time_mask_regions -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn time_mask_regions() {
  let holes = Region::rect(0, 0, 1920, 1080)
    .subtract(Region::rect(200, 150, 700, 500).radius(24.0))
    .subtract(Region::rect(1000, 300, 720, 600).radius(24.0));
  for (what, region) in [
    ("with two holes", Some(holes)),
    ("masked whole", Some(Region::rect(0, 0, 1920, 1080))),
    ("unmasked", None),
  ] {
    for scale in [1.0, 2.0] {
      let mut scene = Scene::new();
      let image = Image::new(fixture("stripes.png"))
        .width(1920)
        .height(1080)
        .mask_region(region.clone());
      let window = scene.window(scene.panel().child(image), 1920.0, 1080.0);
      scene.full(&window, scale);
      let draws = 20;
      let start = std::time::Instant::now();
      for _ in 0..draws {
        scene.full(&window, scale);
      }
      println!(
        "{what}: {:.2} ms a full draw at scale {scale}",
        start.elapsed().as_secs_f64() * 1000.0 / f64::from(draws)
      );
    }
  }
}
