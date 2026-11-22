//! Kept layers: a subtree that only fades or moves is blended in again
//! from the layer the last draw made, and draws what drawing it again
//! would. Anything changing inside draws it again, and a layer is dropped
//! once its node stops fading or moving.

use crabshell_reactive::Signal;
use crabshell_ui::{Color, Row, Stack, Style, Text, Window};

use super::{Buffer, Scene};
use crate::DamageHistory;

const SCALES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

/// What the test windows animate.
#[derive(Clone, Copy)]
struct Knobs {
  opacity: Signal<f32>,
  scale: Signal<f32>,
  shade: Signal<u8>,
  shown: Signal<bool>,
  /// Whether the stack is in a rounded clip, built so.
  clipped: bool,
  /// Whether its text is scaled up by a transform of its own.
  nested: bool,
}

impl Knobs {
  fn new(scene: &Scene) -> Knobs {
    Knobs {
      opacity: scene.cx.signal(0.9),
      scale: scene.cx.signal(1.0),
      shade: scene.cx.signal(40),
      shown: scene.cx.signal(true),
      clipped: false,
      nested: false,
    }
  }
}

/// A 120 by 80 window holding a stack that fades and shrinks as `knobs`
/// say, across tile edges at every scale: a rounded box, text, and a
/// rounded clip around a box whose shade `knobs` set.
fn window(scene: &mut Scene, knobs: Knobs) -> Window {
  let stack = Stack::new()
    .visible(move || knobs.shown.get())
    .style(move || {
      Style::new()
        .left(9.0)
        .top(11.0)
        .width(92.0)
        .height(54.0)
        .opacity(knobs.opacity.get())
        .scale(knobs.scale.get())
    })
    .child(
      Row::new().style(
        Style::new()
          .width(92.0)
          .height(54.0)
          .radius(12.0)
          .background(Color::rgb(200, 120, 40)),
      ),
    )
    .child(
      Text::new("kept").style(Style::new().left(8.0).top(6.0).scale(if knobs.nested {
        2.0
      } else {
        1.0
      })),
    )
    .child(
      Row::new()
        .style(
          Style::new()
            .left(40.0)
            .top(20.0)
            .width(44.0)
            .height(28.0)
            .radius(10.0)
            .clip(true),
        )
        .child(Row::new().style(move || {
          let shade = knobs.shade.get();
          Style::new()
            .width(60.0)
            .height(40.0)
            .background(Color::rgb(shade, 90, 200))
        })),
    );
  let panel = scene
    .panel()
    .style(Style::new().background(Color::rgb(20, 20, 30)));
  let panel = if knobs.clipped {
    // A rounded clip cutting the stack, its layers cut where it is.
    panel.child(
      Row::new()
        .style(
          Style::new()
            .left(4.0)
            .top(30.0)
            .width(70.0)
            .height(46.0)
            .radius(14.0)
            .clip(true),
        )
        .child(stack),
    )
  } else {
    panel.child(stack)
  };
  scene.window(panel, 120.0, 80.0)
}

/// Draws the window's damage into `buffer` and checks it against a full
/// draw from scratch, returning the nodes drawn. A transformed layer kept
/// is drawn in one piece, so it is checked against a draw in one tile,
/// without the seams a layer cut to each tile shows: up to 14/255 where a
/// cut chops a rounded corner.
fn draw_and_compare(
  scene: &mut Scene,
  window: &Window,
  history: &mut DamageHistory,
  buffer: &mut Buffer,
  label: &str,
) -> u64 {
  scene.frame(window);
  let before = scene.renderer.work().1;
  scene.draw(window, history, buffer);
  let drawn = scene.renderer.work().1 - before;
  let view = window.view();
  let transformed = view
    .root()
    .children()
    .any(|node| node.transform().is_some());
  drop(view);
  let fresh = if transformed && scene.renderer.kept_bytes() > 0 {
    let side = crate::draw::TILE_SIDE.replace(4096);
    let fresh = scene.full(window, buffer.scale);
    crate::draw::TILE_SIDE.set(side);
    fresh
  } else {
    scene.full(window, buffer.scale)
  };
  let worst = buffer
    .pixels
    .iter()
    .zip(&fresh.pixels)
    .map(|(a, b)| a.abs_diff(*b))
    .max()
    .unwrap_or(0);
  assert!(worst <= 2, "{label}: off by {worst} from a full draw");
  drawn
}

#[test]
fn a_kept_layer_draws_what_drawing_it_again_would_at_every_scale() {
  for (shrinks, scale) in [false, true]
    .into_iter()
    .flat_map(|shrinks| SCALES.map(|scale| (shrinks, scale)))
  {
    let mut scene = Scene::new();
    let knobs = Knobs::new(&scene);
    let window = window(&mut scene, knobs);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    let mut drawn = Vec::new();
    for step in 1..=5 {
      knobs.opacity.set(1.0 - 0.1 * step as f32);
      if shrinks {
        knobs.scale.set(1.0 - 0.04 * step as f32);
      }
      let label = format!("shrinks {shrinks}, scale {scale}, step {step}");
      drawn.push(draw_and_compare(
        &mut scene,
        &window,
        &mut history,
        &mut buffer,
        &label,
      ));
      if step >= 2 {
        assert!(scene.renderer.kept_bytes() > 0, "{label}: nothing kept");
      }
    }
    // From the third draw the stack's five nodes are blended, not drawn.
    assert!(
      drawn[3] + 5 <= drawn[0] && drawn[4] + 5 <= drawn[0],
      "shrinks {shrinks}, scale {scale}: nodes drawn {drawn:?}"
    );
  }
}

#[test]
fn a_kept_fade_in_a_rounded_clip_draws_what_drawing_it_again_would() {
  for scale in SCALES {
    let mut scene = Scene::new();
    let knobs = Knobs {
      clipped: true,
      ..Knobs::new(&scene)
    };
    let window = window(&mut scene, knobs);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    for opacity in [0.9, 0.8, 0.7, 0.6] {
      knobs.opacity.set(opacity);
      let label = format!("scale {scale}, opacity {opacity}");
      draw_and_compare(&mut scene, &window, &mut history, &mut buffer, &label);
    }
    assert!(scene.renderer.kept_bytes() > 0);
  }
}

#[test]
fn a_moving_layer_with_a_transform_inside_is_drawn_as_usual() {
  for scale in SCALES {
    let mut scene = Scene::new();
    let knobs = Knobs {
      nested: true,
      ..Knobs::new(&scene)
    };
    let window = window(&mut scene, knobs);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    // The text's zoom goes from 2 to 1.75 and 1.5 as the stack shrinks,
    // which a kept layer would leave where it was.
    for (step, shrink) in [1.0, 0.95, 0.85, 0.8, 0.7, 0.65].into_iter().enumerate() {
      knobs.scale.set(shrink);
      let label = format!("scale {scale}, step {step}");
      draw_and_compare(&mut scene, &window, &mut history, &mut buffer, &label);
      assert_eq!(scene.renderer.kept_bytes(), 0, "{label}");
    }
  }
}

#[test]
fn a_change_inside_a_kept_layer_shows() {
  for scale in SCALES {
    let mut scene = Scene::new();
    let knobs = Knobs::new(&scene);
    let window = window(&mut scene, knobs);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    for (step, opacity) in [0.9, 0.8, 0.7].into_iter().enumerate() {
      knobs.opacity.set(opacity);
      knobs.scale.set(1.0 - 0.05 * step as f32);
      let label = format!("scale {scale}, step {step}");
      draw_and_compare(&mut scene, &window, &mut history, &mut buffer, &label);
    }
    knobs.opacity.set(0.6);
    knobs.scale.set(0.85);
    knobs.shade.set(250);
    let label = format!("scale {scale}, the box inside changed");
    draw_and_compare(&mut scene, &window, &mut history, &mut buffer, &label);
  }
}

#[test]
fn a_kept_layer_is_dropped_once_its_node_settles_hides_or_its_window_goes() {
  let mut scene = Scene::new();
  let knobs = Knobs::new(&scene);
  let window = window(&mut scene, knobs);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.5);
  let fade = |scene: &mut Scene, history: &mut DamageHistory, buffer: &mut Buffer| {
    for opacity in [0.9, 0.8, 0.7] {
      knobs.opacity.set(opacity);
      draw_and_compare(scene, &window, history, buffer, "fading");
    }
    assert!(scene.renderer.kept_bytes() > 0);
  };

  fade(&mut scene, &mut history, &mut buffer);
  knobs.opacity.set(1.0);
  draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "opaque");
  assert_eq!(scene.renderer.kept_bytes(), 0, "settled at opacity 1");

  fade(&mut scene, &mut history, &mut buffer);
  knobs.opacity.set(0.0);
  draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "clear");
  assert_eq!(scene.renderer.kept_bytes(), 0, "faded out");

  fade(&mut scene, &mut history, &mut buffer);
  knobs.shown.set(false);
  draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "hidden");
  assert_eq!(scene.renderer.kept_bytes(), 0, "hidden");

  knobs.shown.set(true);
  fade(&mut scene, &mut history, &mut buffer);
  drop(history);
  assert_eq!(scene.renderer.kept_bytes(), 0, "the window's history went");
}

#[test]
fn a_still_translucent_widget_and_a_counting_bar_keep_nothing() {
  let mut scene = Scene::new();
  let count = scene.cx.signal(0_u32);
  let bar = scene
    .panel()
    .style(Style::new().background(Color::rgb(30, 30, 46)))
    .child(
      Row::new()
        .style(
          Style::new()
            .opacity(0.6)
            .background(Color::rgb(90, 90, 120)),
        )
        .child(Text::new(move || format!("count {}", count.get()))),
    );
  let window = scene.window(bar, 200.0, 30.0);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  for n in 1..=4 {
    count.set(n);
    draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "counting");
    assert_eq!(scene.renderer.kept_bytes(), 0);
  }
}

#[test]
fn a_layer_past_the_renderers_budget_is_drawn_as_usual() {
  let mut scene = Scene::new();
  scene.renderer.spend_kept(crate::draw::KEPT_LIMIT - 100);
  let knobs = Knobs::new(&scene);
  let window = window(&mut scene, knobs);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  for opacity in [0.9, 0.8, 0.7] {
    knobs.opacity.set(opacity);
    draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "over");
  }
  assert_eq!(scene.renderer.kept_bytes(), crate::draw::KEPT_LIMIT - 100);
}

#[test]
fn a_still_transform_fading_draws_as_before() {
  let mut scene = Scene::new();
  let knobs = Knobs::new(&scene);
  knobs.scale.set(0.9);
  let window = window(&mut scene, knobs);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.5);
  for opacity in [0.9, 0.8, 0.7] {
    knobs.opacity.set(opacity);
    draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "fading");
    assert_eq!(scene.renderer.kept_bytes(), 0);
  }
}

#[test]
fn a_fading_subtree_with_a_playing_gif_is_drawn_as_usual_and_the_gif_plays() {
  let gif = super::animation::red_then_green("kept");
  let mut scene = Scene::new();
  let opacity = scene.cx.signal(0.9_f32);
  let path = gif.to_path_buf();
  let panel = scene.panel().child(
    Stack::new()
      .style(move || Style::new().opacity(opacity.get()))
      .child(crabshell_ui::Image::new(path.clone()).width(20).height(20)),
  );
  let window = scene.window(panel, 40.0, 40.0);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  for value in [0.9, 0.8, 0.7] {
    opacity.set(value);
    draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "gif");
    assert_eq!(scene.renderer.kept_bytes(), 0);
    let now = std::time::Instant::now();
    assert!(scene.renderer.next_image_frame(now).is_some(), "it plays");
  }
}

/// A 4 by 4 red PNG in the temp dir, removed when the test ends.
fn red_png(name: &str) -> super::Removed {
  let path = std::env::temp_dir().join(format!("crabshell-kept-{}-{name}.png", std::process::id()));
  super::write_png(&path, &[255, 0, 0, 255].repeat(16), 4, 4);
  super::Removed(path)
}

/// Waits for the worker's pictures and repaints the images that wait.
fn take_pictures(scene: &mut Scene, window: &Window) {
  let (sender, woken) = std::sync::mpsc::channel();
  scene.renderer.wake_when_decoded(move || {
    let _ = sender.send(());
  });
  woken
    .recv_timeout(std::time::Duration::from_secs(20))
    .expect("the worker decodes the picture");
  for id in scene.renderer.take_decoded() {
    window.repaint_image(id);
  }
}

#[test]
fn a_picture_in_a_kept_layer_stays_cached_through_a_long_fade_or_move() {
  for moves in [false, true] {
    pictures_stay_cached(moves);
  }
}

/// [`a_picture_in_a_kept_layer_stays_cached_through_a_long_fade_or_move`]
/// for a layer that fades, or with `moves` shrinks.
fn pictures_stay_cached(moves: bool) {
  let png = red_png(if moves { "moved" } else { "cached" });
  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  let opacity = scene.cx.signal(0.9_f32);
  let scale = scene.cx.signal(1.0_f32);
  let shade = scene.cx.signal(0_u8);
  let path = png.to_path_buf();
  let stack = Stack::new()
    .style(move || {
      let style = Style::new().width(300.0).height(300.0);
      if moves {
        style.scale(scale.get())
      } else {
        style.opacity(opacity.get())
      }
    })
    .child(
      crabshell_ui::Image::new(path.clone())
        .width(300)
        .height(300),
    );
  // Past the cache's 128 pictures, each drawn every frame.
  let mut many = Row::new().style(move || {
    Style::new()
      .width(400.0)
      .height(150.0)
      .background(Color::rgb(shade.get(), 0, 0))
  });
  for height in 1..=140_u32 {
    let path = png.to_path_buf();
    many = many.child(crabshell_ui::Image::new(path).width(2).height(height));
  }
  let column = crabshell_ui::Column::new()
    .align(crabshell_ui::Align::Start)
    .child(stack)
    .child(many);
  let window = scene.window(scene.panel().child(column), 400.0, 460.0);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  scene.draw(&window, &mut history, &mut buffer);
  take_pictures(&mut scene, &window);
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);
  for frame in 0..40 {
    opacity.set(0.9 - frame as f32 * 0.001);
    if moves {
      scale.set(0.99 - frame as f32 * 0.001);
    }
    shade.set(frame as u8);
    scene.frame(&window);
    scene.draw(&window, &mut history, &mut buffer);
  }
  assert!(scene.renderer.kept_bytes() > 0, "kept");
  opacity.set(1.0);
  scale.set(1.0);
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(!scene.renderer.decoding(), "decoded again, moves {moves}");
  assert_eq!(
    buffer.pixel(150, 150),
    [255, 0, 0, 255],
    "the picture shows once it settles, moves {moves}"
  );
}

#[test]
fn a_picture_landing_inside_a_kept_layer_shows() {
  let png = red_png("landing");
  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  let opacity = scene.cx.signal(0.9_f32);
  let path = png.to_path_buf();
  let stack = Stack::new()
    .style(move || {
      Style::new()
        .width(300.0)
        .height(300.0)
        .opacity(opacity.get())
        .background(Color::rgb(0, 0, 255))
    })
    .child(
      crabshell_ui::Image::new(path.clone())
        .width(300)
        .height(300),
    );
  let window = scene.window(scene.panel().child(stack), 300.0, 300.0);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  for frame in 0..4 {
    opacity.set(0.9 - frame as f32 * 0.01);
    draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "decoding");
  }
  assert!(scene.renderer.kept_bytes() > 0, "kept while decoding");
  take_pictures(&mut scene, &window);
  opacity.set(0.8);
  draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "landed");
  assert!(
    buffer.pixel(150, 150)[0] > 150,
    "{:?}",
    buffer.pixel(150, 150)
  );
}

#[test]
fn text_shaped_again_inside_a_kept_layer_shows() {
  use crabshell_ui::TextAlign;
  let mut scene = Scene::new();
  let opacity = scene.cx.signal(0.9_f32);
  let end = scene.cx.signal(false);
  let faded = Stack::new()
    .style(move || {
      Style::new()
        .width(200.0)
        .height(40.0)
        .opacity(opacity.get())
    })
    .child(
      Text::new("ab").style(
        Style::new()
          .width(200.0)
          .height(30.0)
          .color(Color::rgb(255, 255, 255)),
      ),
    );
  let outer = Stack::new()
    .style(move || {
      let align = if end.get() {
        TextAlign::End
      } else {
        TextAlign::Start
      };
      Style::new().width(200.0).height(40.0).text_align(align)
    })
    .child(faded);
  let panel = scene
    .panel()
    .style(Style::new().background(Color::rgb(0, 0, 0)))
    .child(outer);
  let window = scene.window(panel, 200.0, 40.0);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  for opacity_now in [0.9, 0.89, 0.88] {
    opacity.set(opacity_now);
    draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "fading");
  }
  assert!(scene.renderer.kept_bytes() > 0, "kept");
  end.set(true);
  opacity.set(0.8);
  draw_and_compare(
    &mut scene,
    &window,
    &mut history,
    &mut buffer,
    "moved to the end",
  );
}

#[test]
fn a_shrinking_kept_layer_past_a_tiles_edge_never_panics() {
  for display_scale in [1.0, 2.0] {
    let mut scene = Scene::new();
    let scale = scene.cx.signal(0.42_f32);
    let box_ = Stack::new()
      .style(move || {
        Style::new()
          .left(20.95)
          .top(0.0)
          .width(40.0)
          .height(100.0)
          .background(Color::rgb(200, 60, 40))
          .scale(scale.get())
      })
      .child(
        Row::new().style(
          Style::new()
            .width(20.0)
            .height(20.0)
            .background(Color::rgb(255, 255, 255)),
        ),
      );
    let window = scene.window(scene.panel().child(box_), 80.0, 100.0);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, display_scale);
    for value in [0.42, 0.41, 0.40] {
      scale.set(value);
      draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "edge");
    }
  }
}

#[test]
fn a_fading_or_moving_gif_image_itself_keeps_playing() {
  let gif = super::animation::red_then_green("kept-self");
  for moves in [false, true] {
    let mut scene = Scene::new();
    let opacity = scene.cx.signal(0.9_f32);
    let scale = scene.cx.signal(0.95_f32);
    let path = gif.to_path_buf();
    let image = crabshell_ui::Image::new(path)
      .width(20)
      .height(20)
      .style(move || {
        if moves {
          Style::new().scale(scale.get())
        } else {
          Style::new().opacity(opacity.get())
        }
      });
    let window = scene.window(scene.panel().child(image), 40.0, 40.0);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, 1.0);
    for step in 0..3 {
      opacity.set(0.9 - step as f32 * 0.1);
      scale.set(0.95 - step as f32 * 0.05);
      draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "gif-self");
      assert_eq!(scene.renderer.kept_bytes(), 0, "moves {moves}");
      let now = std::time::Instant::now();
      assert!(
        scene.renderer.next_image_frame(now).is_some(),
        "it plays, moves {moves}"
      );
    }
  }
}

#[test]
fn a_delayed_image_transition_inside_a_fading_stack_keeps_the_old_picture() {
  use crabshell_ui::{Easing, ImageTransition, Transition, advance_animations};
  use std::time::{Duration, Instant};

  let red = red_png("trans-red");
  let blue_path = std::env::temp_dir().join(format!(
    "crabshell-kept-{}-trans-blue.png",
    std::process::id()
  ));
  super::write_png(&blue_path, &[0, 0, 255, 255].repeat(16), 4, 4);
  let blue = super::Removed(blue_path);

  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  let opacity = scene.cx.signal(0.9_f32);
  let source = scene.cx.signal(red.to_path_buf());
  let stack = Stack::new()
    .style(move || {
      Style::new()
        .width(300.0)
        .height(300.0)
        .opacity(opacity.get())
    })
    .child(
      crabshell_ui::Image::new(move || source.get())
        .width(300)
        .height(300)
        .transition(ImageTransition::crossfade(
          Transition::new(100).delay(500).easing(Easing::Linear),
        )),
    );
  let window = scene.window(scene.panel().child(stack), 300.0, 300.0);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  scene.draw(&window, &mut history, &mut buffer);
  take_pictures(&mut scene, &window);
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);

  source.set(blue.to_path_buf());
  scene.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  scene.draw(&window, &mut history, &mut buffer);
  take_pictures(&mut scene, &window);
  scene.frame(&window);
  // During the 500 ms delay the new picture is ready and t stays 0.
  for frame in 0..4 {
    opacity.set(0.89 - frame as f32 * 0.01);
    advance_animations(start + Duration::from_millis((frame + 1) * 16));
    scene.frame(&window);
    scene.draw(&window, &mut history, &mut buffer);
  }
  // Crossfade halfway at 550 ms, settled at opacity 1.
  opacity.set(1.0);
  advance_animations(start + Duration::from_millis(550));
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(!scene.renderer.decoding(), "old picture stayed cached");
  let pixel = buffer.pixel(150, 150);
  assert!(
    pixel[0] > 100 && pixel[2] > 100 && pixel[3] == 255,
    "blended red and blue, got {pixel:?}"
  );
}

#[test]
fn a_capture_in_a_kept_layer_stays_cached_through_a_long_fade_or_move() {
  use crabshell_ui::{CaptureFrame, CaptureSource, ScreencopyView};
  use std::rc::Rc;

  for moves in [false, true] {
    let mut scene = Scene::new();
    let opacity = scene.cx.signal(0.9_f32);
    let scale = scene.cx.signal(1.0_f32);
    let stack = Stack::new()
      .style(move || {
        let style = Style::new().width(60.0).height(40.0);
        if moves {
          style.scale(scale.get())
        } else {
          style.opacity(opacity.get())
        }
      })
      .child(
        ScreencopyView::new(CaptureSource::Output(String::from("DP-1")))
          .style(Style::new().width(60.0).height(40.0)),
      );
    let window = scene.window(scene.panel().child(stack), 60.0, 40.0);
    let id = window.captures()[0].id;
    window.show_capture(
      id,
      CaptureFrame {
        width: 16,
        height: 16,
        logical_width: 16,
        logical_height: 16,
        rgba: Rc::from([0, 200, 100, 255].repeat(256)),
      },
    );
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, 1.0);
    scene.draw(&window, &mut history, &mut buffer);
    assert_eq!(scene.renderer.cached_captures(), 1);
    for frame in 0..20 {
      opacity.set(0.9 - frame as f32 * 0.01);
      scale.set(0.99 - frame as f32 * 0.01);
      scene.frame(&window);
      scene.draw(&window, &mut history, &mut buffer);
    }
    assert!(scene.renderer.kept_bytes() > 0, "kept, moves {moves}");
    assert_eq!(
      scene.renderer.cached_captures(),
      1,
      "capture stayed cached, moves {moves}"
    );
  }
}

#[test]
fn a_move_from_rest_keeps_its_layer_from_its_first_moving_frame() {
  for scale in SCALES {
    let mut scene = Scene::new();
    let knobs = Knobs::new(&scene);
    knobs.opacity.set(1.0);
    let window = window(&mut scene, knobs);
    let mut history = DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "at rest");
    assert_eq!(scene.renderer.kept_bytes(), 0, "at rest at {scale}");
    for shrunk in [0.95, 0.9] {
      knobs.scale.set(shrunk);
      let label = format!("shrunk to {shrunk} at {scale}");
      draw_and_compare(&mut scene, &window, &mut history, &mut buffer, &label);
      assert!(scene.renderer.kept_bytes() > 0, "{label}: kept");
    }
    // Back at rest it is drawn as usual again.
    knobs.scale.set(1.0);
    draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "settled");
    assert_eq!(scene.renderer.kept_bytes(), 0, "settled at {scale}");
  }
}

#[test]
fn a_widget_that_changes_as_it_starts_moving_is_drawn_again() {
  let mut scene = Scene::new();
  let knobs = Knobs::new(&scene);
  knobs.opacity.set(1.0);
  let window = window(&mut scene, knobs);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.25);
  draw_and_compare(&mut scene, &window, &mut history, &mut buffer, "at rest");
  knobs.scale.set(0.9);
  knobs.shade.set(200);
  draw_and_compare(
    &mut scene,
    &window,
    &mut history,
    &mut buffer,
    "moved and shaded",
  );
  assert_eq!(scene.renderer.kept_bytes(), 0, "its revision moved");
}

#[test]
fn large_buffers_are_kept_spare_zeroed_charged_and_let_go_each_draw() {
  let budget = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
  let held = || budget.load(std::sync::atomic::Ordering::Relaxed);
  let mut kept = crate::draw::Kept::default();
  kept.start(&budget, 0);
  let big = 3 << 20;
  kept.spare(vec![255; big]);
  kept.spare(vec![255; 1000]);
  assert_eq!(kept.spare_buffers(), (1, big), "only a large one is kept");
  assert_eq!(held(), big as u64, "charged to the renderer's budget");
  let reused = kept.buffer(2 << 20);
  assert_eq!(
    (reused.len(), reused.capacity()),
    (2 << 20, 2 << 20),
    "cut to what was asked"
  );
  assert!(reused.iter().all(|&byte| byte == 0), "zeroed");
  assert_eq!(
    (kept.spare_buffers().0, held()),
    (0, 0),
    "taken, no longer charged"
  );
  for _ in 0..8 {
    kept.spare(vec![1; big]);
  }
  assert_eq!(kept.spare_buffers(), (5, 5 * big), "at most 16 MiB");
  kept.end();
  assert_eq!(
    (kept.spare_buffers().0, held()),
    (0, 0),
    "let go as the draw ends"
  );
  budget.store(
    crate::draw::KEPT_LIMIT - 1000,
    std::sync::atomic::Ordering::Relaxed,
  );
  kept.spare(vec![1; big]);
  assert_eq!(kept.spare_buffers().0, 0, "no room left in the budget");
}

#[test]
fn fills_in_one_draw_share_their_scratch_and_none_is_kept_after_it() {
  // Two large clipped boxes that start shrinking in the same draw: the
  // second fill takes the clip layer and masks the first let go.
  let mut scene = Scene::new();
  let scale = scene.cx.signal(1.0);
  let boxes = (0..2).fold(Stack::new(), |stack, index| {
    stack.child(
      Stack::new()
        .style(move || {
          Style::new()
            .left(20.0 + 620.0 * index as f32)
            .top(20.0)
            .width(600.0)
            .height(560.0)
            .scale(scale.get())
        })
        .child(
          Row::new()
            .style(Style::new().radius(16.0).clip(true))
            .child(
              Row::new().style(
                Style::new()
                  .width(600.0)
                  .height(560.0)
                  .background(Color::rgb(90, 140, 200)),
              ),
            ),
        ),
    )
  });
  let window = scene.window(scene.panel().child(boxes), 1260.0, 600.0);
  let mut history = DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  for shrunk in [1.0, 0.95, 0.9] {
    scale.set(shrunk);
    scene.frame(&window);
    scene.draw(&window, &mut history, &mut buffer);
    let spare = history.kept.spare_buffers().0;
    assert_eq!(spare, 0, "nothing spare after the draw at {shrunk}");
  }
  assert!(scene.renderer.kept_bytes() > 0, "both kept");
  assert!(
    history.kept.reused() > 0,
    "the second fill took the first's scratch"
  );
}
