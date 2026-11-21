//! Large blurred shadows drawn from a small one worked out once: the same
//! as working each pixel out, at every scale, and kept only while drawn.

use crabshell_reactive::Signal;
use crabshell_ui::{Color, Row, Shadow, Stack, Style, Window};

use super::{Buffer, Scene};
use crate::DamageHistory;

const SCALES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

/// A 300 by 220 window of large shadows: round, of four corners, cut out,
/// spread and moved by fractions, and one of a box growing by `grow`.
fn shadows(scene: &mut Scene, grow: Signal<f32>) -> Window {
  let boxed = |left: f32, top: f32, width: f32, height: f32, radii: [f32; 4], shadow: Shadow| {
    Row::new().style(
      Style::new()
        .left(left)
        .top(top)
        .width(width)
        .height(height)
        .radii(radii[0], radii[1], radii[2], radii[3])
        .background(Color::rgba(200, 210, 255, 60))
        .shadow(shadow),
    )
  };
  let ink = Color::rgba(0, 0, 0, 160);
  let stack = Stack::new()
    .child(boxed(
      12.0,
      10.0,
      120.0,
      90.0,
      [10.0; 4],
      Shadow::new(0.0, 8.0, 24.0, ink).cut_out(),
    ))
    .child(boxed(
      160.0,
      14.0,
      118.0,
      84.0,
      [0.0, 18.0, 4.0, 11.0],
      Shadow::new(2.3, -1.7, 10.0, Color::rgba(120, 20, 60, 200)).spread(3.4),
    ))
    .child(Row::new().style(move || {
      let grow = grow.get();
      Style::new()
        .left(40.0 - 10.0 * grow)
        .top(120.0 - 6.0 * grow)
        .width(150.0 + 20.0 * grow)
        .height(70.0 + 12.0 * grow)
        .radius(10.0)
        .shadow(Shadow::new(0.0, 8.0, 16.0, ink).cut_out())
        .opacity(0.4 + 0.5 * grow)
    }));
  let panel = scene
    .panel()
    .style(Style::new().background(Color::rgb(235, 235, 240)))
    .child(stack);
  scene.window(panel, 300.0, 220.0)
}

#[test]
fn a_shadow_from_a_patch_draws_what_working_out_each_pixel_does() {
  for scale in SCALES {
    let mut scene = Scene::new();
    let grow = scene.cx.signal(0.5);
    let window = shadows(&mut scene, grow);
    let patched = scene.full(&window, scale);
    assert_eq!(scene.renderer.kept_bytes(), 0, "a full draw keeps nothing");
    crate::draw::PATCHES.set(false);
    let exact = scene.full(&window, scale);
    crate::draw::PATCHES.set(true);
    let worst = patched
      .pixels
      .iter()
      .zip(&exact.pixels)
      .map(|(a, b)| a.abs_diff(*b))
      .max();
    assert!(worst <= Some(2), "off by {worst:?} at scale {scale}");
    assert_ne!(patched.pixels, Buffer::new(&window, scale).pixels);
  }
}

#[test]
fn a_growing_shadow_keeps_its_patch_until_a_draw_leaves_it() {
  let mut scene = Scene::new();
  let grow = scene.cx.signal(0.0);
  let window = shadows(&mut scene, grow);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.5);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(
    scene.renderer.kept_bytes() > 0,
    "the three shadows' patches"
  );
  let mut held = None;
  for step in 1..=4 {
    grow.set(0.2 * step as f32);
    scene.frame(&window);
    scene.draw(&window, &mut history, &mut buffer);
    // Only the growing one is drawn again, from one patch.
    let bytes = scene.renderer.kept_bytes();
    assert!(
      bytes > 0 && held.is_none_or(|held| held == bytes),
      "step {step}: {bytes}"
    );
    held = Some(bytes);
    let fresh = scene.full(&window, 1.5);
    assert_eq!(buffer.pixels, fresh.pixels, "step {step}");
  }
  // A draw that repaints nothing uses none.
  scene.draw(&window, &mut history, &mut buffer);
  assert_eq!(scene.renderer.kept_bytes(), 0);
}
