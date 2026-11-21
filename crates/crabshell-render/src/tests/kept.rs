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
