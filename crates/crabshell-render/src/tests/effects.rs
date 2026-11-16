//! Masks, and shadows cast by what a widget draws.

use crabshell_ui::{Align, Color, Gradient, Mask, Row, Shadow, Stack, Style, Text, Window};

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
fn a_missing_mask_image_logs_once_whatever_size_its_box_takes() {
  let logs = capture_logs(|| {
    let mut scene = Scene::new();
    let width = scene.cx.signal(40.0_f32);
    let missing = Mask::image("/nonexistent/crabshell/growing.png");
    let drawn = Stack::new().style(move || {
      Style::new()
        .mask(missing)
        .background(WHITE)
        .width(width.get())
        .height(20.0)
    });
    let row = Row::new().padding(10, 20).align(Align::Start).child(drawn);
    let window = scene.window(scene.panel().child(row), 120.0, 70.0);
    for step in 0..5 {
      width.set(40.0 + step as f32);
      scene.frame(&window);
      scene.full(&window, 1.0);
    }
  });
  assert_eq!(logs.matches("growing.png").count(), 1, "{logs}");
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

#[test]
fn a_content_shadow_follows_what_is_drawn_not_the_box() {
  // A 10 pixel square in the box casts a sharp shadow 20 to the right.
  let cast = Shadow::new(20.0, 0.0, 0.0, BLACK);
  let buffer = boxed(Style::new().shadow(cast.follow_content()), 10.0);
  assert_eq!(buffer.pixel(25, 15), [255, 255, 255, 255], "the square");
  assert_eq!(buffer.pixel(45, 15), [0, 0, 0, 255], "its shadow");
  assert_eq!(buffer.pixel(45, 25)[3], 0, "below the square's shadow");
  assert_eq!(buffer.pixel(32, 15)[3], 0, "between them");
  // A box shadow would fill the box moved right.
  let boxy = boxed(Style::new().shadow(cast), 10.0);
  assert_eq!(boxy.pixel(45, 25), [0, 0, 0, 255]);
}

#[test]
fn a_content_shadow_is_drawn_under_its_content_and_spread_by_whole_pixels() {
  let cast = |spread| {
    Shadow::new(0.0, 0.0, 0.0, Color::rgb(255, 0, 0))
      .spread(spread)
      .follow_content()
  };
  let grown = boxed(Style::new().shadow(cast(2.0)), 10.0);
  assert_eq!(
    grown.pixel(25, 15),
    [255, 255, 255, 255],
    "under the square"
  );
  assert_eq!(grown.pixel(31, 15), [255, 0, 0, 255], "2 past its edge");
  assert_eq!(grown.pixel(32, 15)[3], 0, "3 past");
  // A negative spread shrinks it inside the square, where it is hidden.
  let shrunk = boxed(Style::new().shadow(cast(-2.0)), 10.0);
  assert!(shrunk.pixels == boxed(Style::new(), 10.0).pixels);
}

#[test]
fn a_content_shadow_cast_across_a_tile_edge_is_whole_on_both_sides() {
  // The square spans 20 to 30, so its shadow 39 to the right centers on
  // the first tile edge, at 64, or at 65 moved a pixel.
  for (moved, middle) in [(0.0, 64), (1.0, 65)] {
    for blur in [0.0, 3.0, 9.0] {
      let shadow = Shadow::new(39.0, 0.0, blur, BLACK).follow_content();
      let style = Style::new().shadow(shadow).translate(moved, 0.0);
      let buffer = boxed(style, 10.0);
      assert!(buffer.pixel(middle, 15)[3] > 100, "{moved} {blur}");
      for step in 0..20 {
        let (left, right) = (
          buffer.pixel(middle - 1 - step, 15),
          buffer.pixel(middle + step, 15),
        );
        assert!(
          left[3].abs_diff(right[3]) <= 1,
          "moved {moved}, blur {blur}: {left:?} and {right:?} {step} from the middle"
        );
      }
    }
  }
}

#[test]
fn a_blurred_content_shadow_falls_off_as_a_box_shadow_does() {
  // A square filling the box casts what the box itself would.
  for blur in [2.0, 3.0, 8.0, 13.0] {
    let shadow = Shadow::new(0.0, 4.0, blur, BLACK);
    let clear = Style::new().background(Color::rgba(0, 0, 0, 0));
    let content = boxed(clear.shadow(shadow.follow_content()), 40.0);
    let boxy = boxed(clear.shadow(shadow), 40.0);
    let mut worst = 0;
    for y in 0..70 {
      for x in 0..120 {
        worst = worst.max(content.pixel(x, y)[3].abs_diff(boxy.pixel(x, y)[3]));
      }
    }
    assert!(worst <= 10, "blur {blur}: off by {worst}");
  }
}

#[test]
fn a_cut_out_content_shadow_is_left_out_by_the_contents_alpha() {
  let cast = |half: u8, cut: bool| {
    let shadow = Shadow::new(0.0, 0.0, 6.0, BLACK).follow_content();
    let shadow = if cut { shadow.cut_out() } else { shadow };
    let mut scene = Scene::new();
    let square = Style::new()
      .background(Color::rgba(255, 255, 255, half))
      .width(10.0)
      .height(10.0);
    let row = Row::new().padding(10, 20).align(Align::Start).child(
      Stack::new()
        .style(Style::new().shadow(shadow).width(40.0).height(40.0))
        .child(Row::new().style(square)),
    );
    let window = scene.window(scene.panel().child(row), 120.0, 70.0);
    scene.full(&window, 1.0)
  };
  let (cut, kept) = (cast(255, true), cast(255, false));
  assert!(cut.pixels == kept.pixels, "opaque content hides it anyway");
  let (cut, kept) = (cast(128, true), cast(128, false));
  // Under translucent content, it is multiplied by what shows through.
  let (under_cut, under_kept) = (cut.pixel(25, 15)[3], kept.pixel(25, 15)[3]);
  assert!(
    under_cut > 128 && under_cut < under_kept,
    "{under_cut} {under_kept}"
  );
  assert_eq!(cut.pixel(32, 15), kept.pixel(32, 15), "beside it");
  assert!(cut.pixel(32, 15)[3] > 0);
}

#[test]
fn a_content_shadow_is_cast_by_the_masked_shape() {
  let shadow = Shadow::new(10.0, 10.0, 0.0, BLACK).follow_content();
  let buffer = boxed(Style::new().mask(Mask::ellipse()).shadow(shadow), 40.0);
  // The ellipse's shadow, not the box's: clear off its corner.
  assert_eq!(buffer.pixel(68, 58)[3], 0, "past the ellipse's corner");
  assert_eq!(buffer.pixel(50, 58), [0, 0, 0, 255], "under its bottom");
}

#[test]
fn a_content_shadow_too_large_to_draw_leaves_the_content() {
  for blur in [4000.0, 1e30, f32::MAX] {
    let huge = Shadow::new(0.0, 0.0, blur, BLACK).follow_content();
    for style in [Style::new(), Style::new().translate(1.0, 0.0)] {
      let buffer = boxed(style.shadow(huge), 10.0);
      assert!(
        buffer.pixels == boxed(style, 10.0).pixels,
        "blur {blur}, {style:?}"
      );
    }
  }
}

/// A window with an ellipse-masked turned card, a list faded at its
/// bottom by a gradient, an image-masked box and text in a row that
/// casts its shadow; `on` changes the card's mask and the text's color.
fn effects(scene: &mut Scene, on: crabshell_reactive::Signal<bool>) -> Window {
  let star = leaked("shapes.svg");
  let fade = Gradient::linear(180.0, BLACK, Color::rgba(0, 0, 0, 0)).stop(0.6, BLACK);
  let shadow = Shadow::new(1.0, 2.0, 3.0, Color::rgba(0, 0, 0, 200));
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
                .shadow(shadow.follow_content())
                .transition(crabshell_ui::Transition::new(100));
              if on.get() {
                card.mask(Mask::ellipse()).opacity(0.8)
              } else {
                card
              }
            })
            // A content shadow inside another.
            .child(
              row("ab")
                .style(Style::new().shadow(Shadow::new(2.0, 2.0, 2.0, WHITE).follow_content())),
            ),
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
        .child(
          // Its shadow reaches past the text's own ink box.
          Row::new()
            .style(
              Style::new().shadow(
                Shadow::new(16.0, 14.0, 4.0, BLACK)
                  .spread(1.0)
                  .follow_content(),
              ),
            )
            .child(Text::new("shadow").style(move || {
              let color = if on.get() {
                WHITE
              } else {
                Color::hex("#f9e2af")
              };
              Style::new().color(color)
            })),
        ),
    );
  scene.window(panel, 260.0, 60.0)
}

#[test]
fn masks_and_content_shadows_repaint_like_drawing_everything() {
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

/// How long a full draw of a 13 pixel line of text and a 64 pixel icon,
/// each with a content shadow, takes at scale 2, and without. Run it in a
/// release build:
/// `cargo test --release -p crabshell-render --lib time_content_shadows -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn time_content_shadows() {
  let shadow = Shadow::new(0.0, 2.0, 6.0, Color::rgba(0, 0, 0, 160)).follow_content();
  for (what, style) in [
    ("with a content shadow", Style::new().shadow(shadow)),
    ("without", Style::new()),
  ] {
    for name in ["13 px text", "64 px icon"] {
      let mut scene = Scene::new();
      let row = Row::new().padding(8, 8).align(Align::Start);
      let row = if name == "13 px text" {
        row.child(Text::new("Wednesday 16 November  14:05").style(style))
      } else {
        row.child(
          crabshell_ui::Icon::new(fixture("shapes.svg").as_path())
            .size(64)
            .style(style),
        )
      };
      let window = scene.window(scene.panel().child(row), 300.0, 80.0);
      let draws = 50;
      let start = std::time::Instant::now();
      for _ in 0..draws {
        scene.full(&window, 2.0);
      }
      println!(
        "{name} {what}: {:.3} ms a full draw at scale 2",
        start.elapsed().as_secs_f64() * 1000.0 / f64::from(draws)
      );
    }
  }
  let mut scene = Scene::new();
  let window = card(&mut scene);
  let draws = 10;
  let start = std::time::Instant::now();
  for _ in 0..draws {
    scene.full(&window, 2.0);
  }
  println!(
    "400 by 300 card with a blur of 40: {:.3} ms a full draw at scale 2",
    start.elapsed().as_secs_f64() * 1000.0 / f64::from(draws)
  );
}

/// A 400 by 300 card at 60, 60 casting a content shadow blurred by 40,
/// in a 520 by 420 window.
fn card(scene: &mut Scene) -> Window {
  let shadow = Shadow::new(0.0, 8.0, 40.0, Color::rgba(0, 0, 0, 160)).follow_content();
  let card = Stack::new().style(
    Style::new()
      .background(Color::hex("#45475a"))
      .radius(12.0)
      .shadow(shadow)
      .width(400.0)
      .height(300.0),
  );
  let row = Row::new().padding(60, 60).align(Align::Start).child(card);
  scene.window(scene.panel().child(row), 520.0, 420.0)
}

#[test]
fn a_content_shadow_is_cast_once_a_draw_over_the_damage_it_reaches() {
  let mut scene = Scene::new();
  let window = card(&mut scene);
  scene.full(&window, 2.0);
  // What casting it once blurs: the card at scale 2 with the shadow's
  // reach, 1.5 blurs and the offset, each side, and as far again for
  // what casts that, plus a few pixels of margin.
  let reach = 2.0 * (1.5 * 40.0 + 8.0) + 4.0;
  let once = ((800.0 + 4.0 * reach) * (600.0 + 4.0 * reach)) as u64;
  // Cells overlap by the reach, so a full draw blurs a little more.
  let whole = scene.renderer.work().2;
  assert!(
    whole > 0 && whole <= once * 3 / 2,
    "{whole} pixels for {once}"
  );
  // A repaint of a corner blurs only the cell it is in.
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 2.0);
  scene.draw(&window, &mut history, &mut buffer);
  let before = scene.renderer.work().2;
  let corner = crabshell_ui::Rect::new(60.0, 60.0, 10.0, 10.0);
  scene
    .renderer
    .draw(
      &window.view(),
      &[corner],
      &mut scene.text,
      &mut history,
      crate::Canvas {
        pixels: &mut buffer.pixels,
        width: buffer.width,
        height: buffer.height,
        scale: 2.0,
        format: crate::PixelFormat::Abgr8888,
        age: &mut buffer.age,
      },
    )
    .expect("the test's canvas fits");
  let blurred = scene.renderer.work().2 - before;
  assert!(
    blurred > 0 && blurred < whole / 4,
    "{blurred} pixels for a corner, {whole} for all"
  );
  assert!(buffer.pixels == scene.full(&window, 2.0).pixels);
}

#[test]
fn a_content_shadow_is_drawn_whole_up_to_64_pixels_of_reach_and_not_past() {
  for scale in [1.0, 1.5, 2.0] {
    let drawn = |blur: f32| {
      let mut scene = Scene::new();
      let shadow = Shadow::new(0.0, 0.0, blur, BLACK).follow_content();
      let window = window(&mut scene, Style::new().shadow(shadow), 10.0);
      scene.full(&window, scale)
    };
    let plain = {
      let mut scene = Scene::new();
      let window = window(&mut scene, Style::new(), 10.0);
      scene.full(&window, scale)
    };
    assert!(drawn(42.0).pixels != plain.pixels, "63 pixels at {scale}");
    assert!(drawn(43.0).pixels == plain.pixels, "64.5 pixels at {scale}");
  }
}

#[test]
fn a_large_shadow_is_cast_in_cells_that_agree_with_a_full_draw() {
  use crate::{Canvas, PixelFormat};

  let mut scene = Scene::new();
  let shadow = Shadow::new(3.0, 0.0, 6.0, BLACK).follow_content();
  let card = Stack::new().style(
    Style::new()
      .background(Color::hex("#45475a"))
      .radius(30.0)
      .shadow(shadow)
      .width(600.0)
      .height(600.0),
  );
  let row = Row::new().padding(20, 20).align(Align::Start).child(card);
  let window = scene.window(scene.panel().child(row), 640.0, 640.0);
  let full = scene.full(&window, 2.0);
  // Repaints down the round left edge, each casting the cells it needs,
  // agree with the full draw, which cast them all.
  for y in (0..640).step_by(70) {
    let mut history = DamageHistory::new();
    let mut buffer = full.clone();
    buffer.age = crate::BufferAge::default();
    for damage in [
      Vec::new(),
      vec![crabshell_ui::Rect::new(0.0, y as f32, 40.0, 8.0)],
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
            scale: 2.0,
            format: PixelFormat::Abgr8888,
            age: &mut buffer.age,
          },
        )
        .expect("the test's canvas fits");
    }
    assert!(buffer.pixels == full.pixels, "a repaint at y {y}");
  }
}
