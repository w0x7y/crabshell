//! Masks.

use crabshell_ui::{Align, Color, Gradient, Mask, Row, Stack, Style, Text, Window};

use super::{Buffer, Scene, capture_logs, fixture};
use crate::DamageHistory;

const WHITE: Color = Color::rgb(255, 255, 255);
const BLACK: Color = Color::rgb(0, 0, 0);

/// A 40 by 40 box at 20, 10 with `style`, in a clear 120 by 70 window,
/// holding a white square `side` wide at its top left.
fn window(scene: &mut Scene, style: Style, side: f32) -> Window {
  let inside = Row::new().style(Style::new().background(WHITE).width(side).height(side));
  let drawn = Stack::new()
    .style(style.width(40.0).height(40.0))
    .child(inside);
  let row = Row::new().padding(10, 20).align(Align::Start).child(drawn);
  scene.window(scene.panel().child(row), 120.0, 70.0)
}

/// [`window`] drawn whole at scale 1.
fn boxed(style: Style, side: f32) -> Buffer {
  let mut scene = Scene::new();
  let window = window(&mut scene, style, side);
  scene.full(&window, 1.0)
}

/// A path in `tests/fixtures` that lives as long as a `Mask::image` needs.
fn leaked(name: &str) -> &'static str {
  fixture(name).display().to_string().leak()
}

#[test]
fn an_ellipse_mask_keeps_only_the_inside_of_the_box() {
  // A child larger than the box is cut to it too.
  let buffer = boxed(Style::new().mask(Mask::ellipse()), 60.0);
  assert_eq!(buffer.pixel(40, 30), [255, 255, 255, 255], "the middle");
  for (x, y) in [(21, 11), (58, 11), (58, 48), (21, 48), (65, 30), (40, 55)] {
    assert_eq!(buffer.pixel(x, y)[3], 0, "outside the ellipse at {x}, {y}");
  }
  // Just inside each edge's middle.
  for (x, y) in [(40, 11), (21, 30), (58, 30), (40, 48)] {
    assert_eq!(buffer.pixel(x, y)[3], 255, "inside at {x}, {y}");
  }
}

#[test]
fn a_gradient_mask_fades_by_its_alpha_alone() {
  // White down to clear red: only the alpha counts.
  let fade = Gradient::linear(180.0, WHITE, Color::rgba(255, 0, 0, 0));
  let buffer = boxed(Style::new().mask(Mask::gradient(fade)), 40.0);
  let alpha = |y| buffer.pixel(40, y)[3];
  assert!(alpha(10) > 245, "{}", alpha(10));
  assert!(alpha(30).abs_diff(128) <= 8, "{}", alpha(30));
  assert!(alpha(49) < 10, "{}", alpha(49));
  let [r, g, b, a] = buffer.pixel(40, 30);
  assert_eq!((r, g, b), (a, a, a), "still white, premultiplied");
}

#[test]
fn an_image_mask_uses_the_files_alpha_stretched_over_the_box() {
  let disc = leaked("disc.png");
  let masked = boxed(Style::new().mask(Mask::image(disc)), 40.0);
  let mut scene = Scene::new();
  let row = Row::new().padding(10, 20).align(Align::Start).child(
    crabshell_ui::Image::new(fixture("disc.png"))
      .width(40)
      .height(40),
  );
  let window = scene.window(scene.panel().child(row), 120.0, 70.0);
  let image = scene.full(&window, 1.0);
  // The white square's alpha is the stretched picture's.
  let mut shown = 0;
  for y in 10..50 {
    for x in 20..60 {
      let alpha = image.pixel(x, y)[3];
      assert!(
        masked.pixel(x, y)[3].abs_diff(alpha) <= 1,
        "at {x}, {y}: {:?} for {alpha}",
        masked.pixel(x, y)
      );
      shown += u32::from(alpha > 0);
    }
  }
  assert!(shown > 100 && shown < 1500, "{shown} pixels shown");
}

#[test]
fn a_missing_mask_image_draws_unmasked_and_logs_once() {
  let plain = boxed(Style::new(), 40.0);
  let logs = capture_logs(|| {
    let mut scene = Scene::new();
    let missing = Style::new().mask(Mask::image("/nonexistent/crabshell/mask.png"));
    let window = window(&mut scene, missing, 40.0);
    for _ in 0..3 {
      assert!(scene.full(&window, 1.0).pixels == plain.pixels);
    }
  });
  assert_eq!(logs.matches("mask.png").count(), 1, "{logs}");
}

#[test]
fn a_mask_shares_one_layer_with_opacity_and_a_transform() {
  let layers = |style: Style| {
    let mut scene = Scene::new();
    let window = window(&mut scene, style, 40.0);
    scene.full(&window, 1.0);
    scene.renderer.work().0
  };
  let turned = Style::new().opacity(0.5).rotate(10.0);
  assert_eq!(layers(turned.mask(Mask::ellipse())), layers(turned));
  let faded = Style::new().opacity(0.5);
  assert_eq!(layers(faded.mask(Mask::ellipse())), layers(faded));
  assert!(layers(Style::new().mask(Mask::ellipse())) > 0);
}

#[test]
fn a_clip_with_its_own_corners_cuts_inside_a_mask() {
  let fade = Gradient::linear(90.0, WHITE, WHITE);
  let clipped = Style::new()
    .mask(Mask::gradient(fade))
    .radii(16.0, 0.0, 0.0, 0.0)
    .clip(true);
  let buffer = boxed(clipped, 60.0);
  assert_eq!(buffer.pixel(21, 11)[3], 0, "the round corner");
  assert_eq!(buffer.pixel(58, 11)[3], 255, "a square one");
  assert_eq!(buffer.pixel(61, 30)[3], 0, "past the box");
}

/// A window with an ellipse-masked turned card, a list faded at its
/// bottom by a gradient, an image-masked box and text; `on` changes the
/// card's mask and the text's color.
fn effects(scene: &mut Scene, on: crabshell_reactive::Signal<bool>) -> Window {
  let star = leaked("shapes.svg");
  let fade = Gradient::linear(180.0, BLACK, Color::rgba(0, 0, 0, 0)).stop(0.6, BLACK);
  let row = |text: &'static str| Text::new(text);
  let panel = scene
    .panel()
    .style(Style::new().background(Color::hex("#45475a")))
    .child(
      Row::new()
        .padding(8, 10)
        .gap(12)
        .align(Align::Start)
        .child(
          Stack::new()
            .style(move || {
              let card = Style::new()
                .background(Color::hex("#89b4fa"))
                .width(36.0)
                .height(30.0)
                .rotate(20.0)
                .transition(crabshell_ui::Transition::new(100));
              if on.get() {
                card.mask(Mask::ellipse()).opacity(0.8)
              } else {
                card
              }
            })
            .child(row("ab")),
        )
        .child(
          crabshell_ui::Column::new()
            .style(Style::new().mask(Mask::gradient(fade)).height(40.0))
            .child(row("one"))
            .child(row("two"))
            .child(row("three")),
        )
        .child(
          Stack::new().style(
            Style::new()
              .gradient(Gradient::sweep(0.0, WHITE, Color::hex("#f38ba8")))
              .mask(Mask::image(star))
              .width(30.0)
              .height(30.0),
          ),
        )
        .child(Row::new().child(Text::new("shadow").style(move || {
          let color = if on.get() {
            WHITE
          } else {
            Color::hex("#f9e2af")
          };
          Style::new().color(color)
        }))),
    );
  scene.window(panel, 260.0, 60.0)
}

#[test]
fn masks_repaint_like_drawing_everything() {
  use crate::{Canvas, PixelFormat};

  for scale in [1.0, 1.5, 2.0] {
    let mut scene = Scene::new();
    let on = scene.cx.signal(false);
    let window = effects(&mut scene, on);
    let full = scene.full(&window, scale);
    // A repaint whose right edge cuts through every one, a tile column
    // after another.
    for right in (6..260).step_by(17) {
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
    // The text's color, the card's mask and opacity change: what is
    // repainted over the last frame matches a full draw, mid-fade too.
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    scene.draw(&window, &mut history, &mut buffer);
    on.set(true);
    scene.frame(&window);
    let start = std::time::Instant::now();
    for ms in [0, 50] {
      crabshell_ui::advance_animations(start + std::time::Duration::from_millis(ms));
      scene.frame(&window);
      scene.draw(&window, &mut history, &mut buffer);
      let now = scene.full(&window, scale);
      assert!(
        buffer.pixels == now.pixels,
        "{ms} ms into the fade at {scale}"
      );
    }
    assert!(buffer.pixels != full.pixels, "it changed");
  }
}

#[test]
fn a_mask_changes_at_once_in_a_fade() {
  let mut scene = Scene::new();
  let on = scene.cx.signal(false);
  let window = effects(&mut scene, on);
  on.set(true);
  scene.frame(&window);
  crabshell_ui::advance_animations(std::time::Instant::now());
  scene.frame(&window);
  let style = super::node_at(window.view().root(), &[0, 0]).style();
  assert_eq!(style.mask, Some(Mask::Ellipse), "from the fade's start");
  assert!(
    style.opacity.is_some_and(|o| o > 0.95),
    "opacity still fades"
  );
}
