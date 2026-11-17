//! Image transitions: what each kind draws partway, that tiles and
//! repaints match a full draw through clips, opacity and transforms, the
//! old picture kept for a source that can't be loaded, and the picture
//! left dropped from the cache as soon as a transition ends.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crabshell_ui::{
  Align, Color, Easing, Fit, Image, ImageFilters, ImageTransition, ImageView, Mask, PanelWindow,
  Rect, Row, Stack, Style, Transition, Window, advance_animations,
};

use super::{Buffer, Removed, Scene, assert_golden, capture_logs, fixture, write_png};
use crate::icons::{Drawn, Icons};

const LINEAR: Transition = Transition::new(100).easing(Easing::Linear);

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// A directory for `test`'s pictures, removed when it ends.
fn dir(test: &str) -> Removed {
  let path = std::env::temp_dir().join(format!(
    "crabshell-transition-{}-{test}",
    std::process::id()
  ));
  std::fs::create_dir_all(&path).expect("a directory for the pictures");
  Removed(path)
}

/// Writes a `width` by `height` PNG at `dir/name` whose pixel at `x`, `y`
/// is `color(x, y)`, and returns its path.
fn picture(
  dir: &Path,
  name: &str,
  (width, height): (u32, u32),
  color: impl Fn(u32, u32) -> [u8; 4],
) -> PathBuf {
  let path = dir.join(name);
  let rgba: Vec<u8> = (0..width * height)
    .flat_map(|index| color(index % width, index / width))
    .collect();
  write_png(&path, &rgba, width, height);
  path
}

/// A window of `width` by `height` showing `panel`, whose images read
/// `next`: switched on, they move to their second source over their
/// transitions, and the clock is driven to `t` of the way along, linear.
/// The clock ends after the closure `f` runs.
fn partway(
  scene: &mut Scene,
  (width, height): (f32, f32),
  t: f32,
  panel: impl FnOnce(crabshell_reactive::Signal<bool>, PanelWindow) -> PanelWindow,
  f: impl FnOnce(&mut Scene, &Window),
) {
  let next = scene.cx.signal(false);
  let panel = panel(next, scene.panel());
  let window = scene.window(panel, width, height);
  next.set(true);
  scene.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_secs_f32(0.1 * t));
  scene.frame(&window);
  f(scene, &window);
  drop(window);
  assert!(
    !advance_animations(start + Duration::from_secs(10)),
    "the transitions end"
  );
}

/// An image of `from` that turns to `to` over `transition`.
fn switching(
  next: crabshell_reactive::Signal<bool>,
  from: &Path,
  to: &Path,
  transition: ImageTransition,
) -> Image {
  let (from, to) = (from.to_owned(), to.to_owned());
  Image::new(move || if next.get() { to.clone() } else { from.clone() }).transition(transition)
}

/// A pixel's place and the color it should show.
type Expected = ((u32, u32), [u8; 4]);

#[test]
fn each_kind_draws_the_new_picture_partway_in() {
  let dir = dir("kinds");
  let red = picture(&dir, "red.png", (8, 8), |_, _| RED);
  let blue = picture(&dir, "blue.png", (8, 8), |_, _| BLUE);
  let cases: [(ImageTransition, &[Expected]); 5] = [
    (
      ImageTransition::crossfade(LINEAR),
      &[((20, 20), [128, 0, 128, 255]), ((0, 0), [128, 0, 128, 255])],
    ),
    // Toward the right: the new picture comes in from the left.
    (
      ImageTransition::wipe(LINEAR, 90.0),
      &[((5, 20), BLUE), ((35, 20), RED), ((5, 0), BLUE)],
    ),
    // Toward the top: it comes in from the bottom.
    (
      ImageTransition::wipe(LINEAR, 0.0),
      &[((20, 35), BLUE), ((20, 5), RED)],
    ),
    (
      ImageTransition::wipe(LINEAR, f32::NAN),
      &[((5, 20), BLUE), ((35, 20), RED)],
    ),
    // Halfway, its radius is half the way to past the corners.
    (
      ImageTransition::circle(LINEAR),
      &[
        ((20, 20), BLUE),
        ((20, 8), BLUE),
        ((20, 2), RED),
        ((1, 1), RED),
      ],
    ),
  ];
  for (kind, pixels) in cases {
    let mut scene = Scene::new();
    partway(
      &mut scene,
      (40.0, 40.0),
      0.5,
      |next, panel| {
        panel.child(
          switching(next, &red, &blue, kind)
            .width(40)
            .height(40)
            .fit(Fit::Fill),
        )
      },
      |scene, window| {
        let buffer = scene.full(window, 1.0);
        for &((x, y), expected) in pixels {
          let got = buffer.pixel(x, y);
          assert!(
            got.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
            "{kind:?} at {x}, {y}: {got:?}, not {expected:?}"
          );
        }
      },
    );
  }
}

#[test]
fn the_ends_show_one_picture_whole() {
  let dir = dir("ends");
  let red = picture(&dir, "red.png", (8, 8), |_, _| RED);
  let blue = picture(&dir, "blue.png", (8, 8), |_, _| BLUE);
  let kinds = [
    ImageTransition::crossfade(LINEAR),
    ImageTransition::wipe(LINEAR, 30.0),
    ImageTransition::circle(LINEAR),
    ImageTransition::pixelate(LINEAR),
  ];
  for kind in kinds {
    for (t, expected) in [(0.0, RED), (0.999, BLUE)] {
      let mut scene = Scene::new();
      partway(
        &mut scene,
        (37.0, 23.0),
        t,
        |next, panel| panel.child(switching(next, &red, &blue, kind).width(37).height(23)),
        |scene, window| {
          let buffer = scene.full(window, 1.0);
          for (index, pixel) in buffer.pixels.as_chunks::<4>().0.iter().enumerate() {
            assert!(
              pixel.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
              "{kind:?} at {t}: pixel {index} is {pixel:?}"
            );
          }
        },
      );
    }
  }
}

#[test]
fn pixelating_grows_blocks_to_a_twentieth_of_the_longer_side_halfway() {
  let dir = dir("blocks");
  // Each column its own red, the same in both pictures.
  let column = |x: u32, _| [x as u8, 0, 0, 255];
  let from = picture(&dir, "from.png", (200, 20), column);
  let to = picture(&dir, "to.png", (200, 20), column);
  for (t, block) in [(0.0, 1), (0.25, 6), (0.5, 10), (0.75, 6)] {
    let mut scene = Scene::new();
    partway(
      &mut scene,
      (200.0, 20.0),
      t,
      |next, panel| {
        panel.child(
          switching(next, &from, &to, ImageTransition::pixelate(LINEAR))
            .width(200)
            .height(20),
        )
      },
      |scene, window| {
        let buffer = scene.full(window, 1.0);
        for x in [0, 13, 57, 199] {
          let sample = (x / block * block + block / 2).min(199);
          assert_eq!(
            buffer.pixel(x, 10)[0],
            sample as u8,
            "at {t}, column {x} shows its block's middle"
          );
        }
      },
    );
  }
}

/// Four transitions halfway, then a wipe inside a clipped, rounded,
/// turned and translucent box, as they draw at `next`.
fn halfway_panel(next: crabshell_reactive::Signal<bool>, panel: PanelWindow) -> PanelWindow {
  let (from, to) = (fixture("stripes.png"), fixture("bands.png"));
  let image = |transition| {
    switching(next, &from, &to, transition)
      .width(80)
      .height(48)
      .fit(Fit::Fill)
  };
  panel
    .style(Style::new().background(Color::hex("#1e1e2e")))
    .child(
      Row::new()
        .padding(5, 5)
        .gap(5)
        .align(Align::Start)
        .child(image(ImageTransition::crossfade(LINEAR)))
        .child(image(ImageTransition::wipe(LINEAR, 45.0)))
        .child(image(ImageTransition::circle(LINEAR)))
        .child(image(ImageTransition::pixelate(LINEAR)))
        .child(
          Stack::new()
            .style(
              Style::new()
                .radius(12.0)
                .clip(true)
                .rotate(10.0)
                .opacity(0.8)
                .width(80.0)
                .height(48.0),
            )
            .child(image(ImageTransition::wipe(LINEAR, 90.0)).blur(1.0)),
        ),
    )
}

const HALFWAY_SIZE: (f32, f32) = (430.0, 58.0);

/// Every kind halfway between the stripes and the bands, with the clock
/// driven by hand.
#[test]
fn image_transitions() {
  for (scale, suffix) in [(1.0, "1x"), (1.5, "1.5x"), (2.0, "2x")] {
    let mut scene = Scene::new();
    partway(
      &mut scene,
      HALFWAY_SIZE,
      0.5,
      halfway_panel,
      |scene, window| {
        let buffer = scene.full(window, scale);
        assert_golden(&format!("image_transitions@{suffix}"), &buffer);
      },
    );
  }
}

#[test]
fn repainting_part_of_a_transition_matches_drawing_everything() {
  for scale in [1.0, 1.5, 2.0] {
    let mut scene = Scene::new();
    partway(
      &mut scene,
      HALFWAY_SIZE,
      0.5,
      halfway_panel,
      |scene, window| {
        let full = scene.full(window, scale);
        // Damage strips that cut through the pictures at odd places.
        for (x, y) in [(7.0, 4.0), (100.5, 21.0), (230.0, 30.0), (390.0, 9.5)] {
          let mut history = crate::DamageHistory::new();
          let mut buffer = Buffer::new(window, scale);
          scene.draw(window, &mut history, &mut buffer);
          let damage = Rect::new(x, y, 23.0, 13.0);
          super::repaint::repaint(scene, window, &[damage], &mut history, &mut buffer);
          assert!(
            buffer.pixels == full.pixels,
            "a repaint at {x}, {y} at scale {scale} differs from a full draw"
          );
        }
      },
    );
  }
}

#[test]
fn a_transition_in_an_ellipse_masked_box_repaints_as_a_full_draw() {
  let (from, to) = (fixture("stripes.png"), fixture("bands.png"));
  for scale in [1.0, 1.5, 2.0] {
    let mut scene = Scene::new();
    partway(
      &mut scene,
      (100.0, 70.0),
      0.5,
      |next, panel| {
        panel.child(
          Row::new().padding(5, 5).align(Align::Start).child(
            Stack::new()
              .style(Style::new().mask(Mask::ellipse()).width(90.0).height(60.0))
              .child(
                switching(next, &from, &to, ImageTransition::circle(LINEAR))
                  .width(90)
                  .height(60)
                  .fit(Fit::Fill),
              ),
          ),
        )
      },
      |scene, window| {
        let full = scene.full(window, scale);
        // The ellipse cuts the corners, and the circle's rim crosses it.
        assert_eq!(full.pixel(6, 6)[3], 0, "outside the ellipse at {scale}");
        for (x, y) in [(4.0, 3.0), (40.5, 30.0), (70.0, 50.5), (60.0, 8.0)] {
          let mut history = crate::DamageHistory::new();
          let mut buffer = Buffer::new(window, scale);
          scene.draw(window, &mut history, &mut buffer);
          let damage = Rect::new(x, y, 17.0, 11.0);
          super::repaint::repaint(scene, window, &[damage], &mut history, &mut buffer);
          assert!(
            buffer.pixels == full.pixels,
            "a repaint at {x}, {y} at scale {scale} differs from a full draw"
          );
        }
      },
    );
  }
}

#[test]
fn each_tick_repaints_to_what_a_full_draw_shows() {
  for scale in [1.0, 1.5, 2.0] {
    let mut scene = Scene::new();
    let next = scene.cx.signal(false);
    let panel = halfway_panel(next, scene.panel());
    let window = scene.window(panel, HALFWAY_SIZE.0, HALFWAY_SIZE.1);
    let mut history = crate::DamageHistory::new();
    let mut buffer = Buffer::new(&window, scale);
    scene.draw(&window, &mut history, &mut buffer);
    next.set(true);
    scene.frame(&window);
    let start = Instant::now();
    for ms in [0, 20, 45, 70, 100] {
      let running = advance_animations(start + Duration::from_millis(ms));
      scene.frame(&window);
      scene.draw(&window, &mut history, &mut buffer);
      let full = scene.full(&window, scale);
      assert!(
        buffer.pixels == full.pixels,
        "at {ms} ms at scale {scale} the repaint differs from a full draw"
      );
      assert_eq!(running, ms < 100);
    }
  }
}

fn icons() -> Icons {
  Icons::with_themes(crate::theme::Themes::new(Vec::new()))
}

/// What an `Image` of `path` with a crossfade hands the renderer, leaving
/// `leaving` partway.
fn view<'a>(path: &'a Path, leaving: Option<(&'a Path, f32)>) -> ImageView<'a> {
  ImageView {
    id: 1,
    path,
    rect: Rect::new(0.0, 0.0, 1.0, 1.0),
    fit: Fit::Fill,
    filters: ImageFilters::default(),
    playing: true,
    transition: ImageTransition::crossfade(LINEAR),
    leaving,
  }
}

/// The red of the pixel at `x`, `y` of `pixmap`.
fn red_at(pixmap: &tiny_skia::Pixmap, x: u32, y: u32) -> u8 {
  pixmap.pixel(x, y).expect("inside").red()
}

#[test]
fn two_1080p_pictures_are_held_while_a_crossfade_runs_and_one_once_it_ends() {
  let dir = dir("held");
  let size = (1920, 1080);
  let bytes = 1920 * 1080 * 4;
  // Small files, scaled to the size drawn, which is what is kept.
  let a = picture(&dir, "a.png", (192, 108), |_, _| RED);
  let b = picture(&dir, "b.png", (192, 108), |_, _| BLUE);
  let mut icons = icons();
  icons.image(&view(&a, None), size, 1.0);
  icons.end_frame();
  assert_eq!(icons.cached_bytes(), bytes);
  for t in [0.0, 0.5, 0.9] {
    // Several tiles a draw, as the renderer asks.
    for _ in 0..3 {
      assert!(matches!(
        icons.image(&view(&b, Some((&a, t))), size, 1.0),
        Some(Drawn::Two { from: Some(_), .. })
      ));
    }
    icons.end_frame();
    assert_eq!(icons.cached_bytes(), 2 * bytes, "both, about 16 MB");
  }
  icons.image(&view(&b, None), size, 1.0);
  assert_eq!(icons.cached_bytes(), bytes, "the old one goes at once");
  icons.end_frame();
  assert_eq!(icons.cached(), 1);
}

#[test]
fn a_picture_left_that_another_image_still_draws_stays_cached() {
  let dir = dir("shared");
  let a = picture(&dir, "a.png", (4, 4), |_, _| RED);
  let b = picture(&dir, "b.png", (4, 4), |_, _| BLUE);
  let mut icons = icons();
  icons.image(&view(&a, None), (4, 4), 1.0);
  icons.end_frame();
  icons.image(&view(&b, Some((&a, 0.5))), (4, 4), 1.0);
  // Another image's transition leaves the same picture too.
  let other = ImageView {
    id: 2,
    ..view(&b, Some((&a, 0.2)))
  };
  icons.image(&other, (4, 4), 1.0);
  icons.end_frame();
  icons.image(&view(&b, None), (4, 4), 1.0);
  assert_eq!(icons.cached(), 2, "still left by image 2");
  icons.image(
    &ImageView {
      id: 2,
      ..view(&b, None)
    },
    (4, 4),
    1.0,
  );
  assert_eq!(icons.cached(), 1, "and dropped once that ends");
}

#[test]
fn a_source_that_cant_load_keeps_the_old_picture_with_no_transition() {
  let dir = dir("missing");
  let a = picture(&dir, "a.png", (4, 4), |_, _| RED);
  let c = picture(&dir, "c.png", (4, 4), |_, _| BLUE);
  let missing = dir.join("missing.png");
  let mut icons = icons();
  icons.image(&view(&a, None), (4, 4), 1.0);
  icons.end_frame();
  let logs = capture_logs(|| {
    for t in [0.0, 0.5, 0.9] {
      match icons.image(&view(&missing, Some((&a, t))), (4, 4), 1.0) {
        Some(Drawn::One(pixmap)) => assert_eq!(red_at(pixmap, 1, 1), 255),
        _ => panic!("the old picture, alone"),
      }
      icons.end_frame();
    }
    match icons.image(&view(&missing, None), (4, 4), 1.0) {
      Some(Drawn::One(pixmap)) => assert_eq!(red_at(pixmap, 1, 1), 255, "kept after"),
      _ => panic!("the old picture"),
    }
    icons.end_frame();
  });
  assert_eq!(logs.matches("couldn't draw").count(), 1, "{logs}");
  assert_eq!(icons.cached(), 2, "the old picture and the failure");

  // A good one then comes in from the old picture, not the failure.
  match icons.image(&view(&c, Some((&missing, 0.5))), (4, 4), 1.0) {
    Some(Drawn::Two {
      from: Some(from),
      to,
      t,
    }) => {
      assert_eq!((red_at(from, 1, 1), red_at(to, 1, 1), t), (255, 0, 0.5));
    }
    _ => panic!("a transition from the old picture"),
  }
  icons.end_frame();
  icons.image(&view(&c, None), (4, 4), 1.0);
  icons.end_frame();
  match icons.image(&view(&c, None), (4, 4), 1.0) {
    Some(Drawn::One(pixmap)) => assert_eq!(red_at(pixmap, 1, 1), 0),
    _ => panic!("the new picture"),
  }
}

#[test]
fn without_a_transition_or_an_old_picture_a_source_that_cant_load_draws_nothing() {
  let dir = dir("nothing");
  let a = picture(&dir, "a.png", (4, 4), |_, _| RED);
  let missing = dir.join("missing.png");
  let mut icons = icons();
  let plain = |path| ImageView {
    transition: ImageTransition::None,
    ..view(path, None)
  };
  icons.image(&plain(&a), (4, 4), 1.0);
  assert!(icons.image(&plain(&missing), (4, 4), 1.0).is_none());

  // The first source failing has nothing to keep, and a good one then
  // comes in from nothing.
  let mut icons = self::icons();
  assert!(icons.image(&view(&missing, None), (4, 4), 1.0).is_none());
  assert!(matches!(
    icons.image(&view(&a, Some((&missing, 0.5))), (4, 4), 1.0),
    Some(Drawn::Two { from: None, .. })
  ));
}

#[test]
fn both_pictures_go_through_the_same_fit_and_filters() {
  let dir = dir("filters");
  let a = picture(&dir, "a.png", (8, 4), |_, _| RED);
  let b = picture(&dir, "b.png", (8, 4), |_, _| BLUE);
  let mut icons = icons();
  let dim = |path, leaving| ImageView {
    fit: Fit::Contain,
    filters: ImageFilters {
      brightness: 0.5,
      ..ImageFilters::default()
    },
    ..view(path, leaving)
  };
  icons.image(&dim(&a, None), (8, 8), 1.0);
  match icons.image(&dim(&b, Some((&a, 0.5))), (8, 8), 1.0) {
    Some(Drawn::Two {
      from: Some(from),
      to,
      ..
    }) => {
      for pixmap in [from, to] {
        assert_eq!(pixmap.pixel(4, 0).expect("inside").alpha(), 0, "contained");
        let middle = pixmap.pixel(4, 4).expect("inside");
        assert_eq!(middle.red().max(middle.blue()), 128, "dimmed");
      }
    }
    _ => panic!("a transition"),
  }
}

/// How long a frame of a 1920 by 1080 crossfade takes to draw at scale 1,
/// both pictures cached, the whole window repainted. Run it in a release
/// build:
/// `cargo test --release -p crabshell-render --lib time_a_1080p_crossfade -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn time_a_1080p_crossfade() {
  let dir = dir("timing");
  let size = (1920, 1080);
  let gradient = |shift: u32| move |x: u32, y: u32| [(x >> shift) as u8, (y >> 2) as u8, 128, 255];
  let a = picture(&dir, "a.jpg.png", size, gradient(3));
  let b = picture(&dir, "b.png", size, gradient(4));
  for kind in [
    ImageTransition::crossfade(Transition::new(1000)),
    ImageTransition::wipe(Transition::new(1000), 45.0),
    ImageTransition::circle(Transition::new(1000)),
    ImageTransition::pixelate(Transition::new(1000)),
  ] {
    let mut scene = Scene::new();
    let next = scene.cx.signal(false);
    let image = switching(next, &a, &b, kind).width(1920).height(1080);
    let panel = scene.panel().child(image);
    let window = scene.window(panel, 1920.0, 1080.0);
    let mut history = crate::DamageHistory::new();
    let mut buffer = Buffer::new(&window, 1.0);
    scene.draw(&window, &mut history, &mut buffer);
    next.set(true);
    scene.frame(&window);
    let start = Instant::now();
    advance_animations(start);
    // The first frame decodes the new picture.
    scene.draw(&window, &mut history, &mut buffer);
    let frames = 30;
    let mut total = Duration::ZERO;
    for frame in 1..=frames {
      advance_animations(start + Duration::from_millis(frame * 30));
      scene.frame(&window);
      let started = Instant::now();
      scene.draw(&window, &mut history, &mut buffer);
      total += started.elapsed();
    }
    println!(
      "a 1920x1080 {kind:?} frame takes {:.2} ms on average over {frames} frames",
      total.as_secs_f64() * 1000.0 / frames as f64
    );
    drop(window);
    advance_animations(start + Duration::from_secs(10));
  }
}

#[test]
fn a_picture_a_new_transition_replaces_is_dropped_at_once() {
  let dir = dir("replaced");
  let a = picture(&dir, "a.png", (4, 4), |_, _| RED);
  let b = picture(&dir, "b.png", (4, 4), |_, _| BLUE);
  let c = picture(&dir, "c.png", (4, 4), |_, _| RED);
  let mut icons = icons();
  icons.image(&view(&a, None), (4, 4), 1.0);
  icons.end_frame();
  icons.image(&view(&b, Some((&a, 0.5))), (4, 4), 1.0);
  icons.end_frame();
  assert_eq!(icons.cached(), 2);
  // A new source mid-way: it starts again from the one coming in, and
  // the first picture is no longer drawn.
  icons.image(&view(&c, Some((&b, 0.0))), (4, 4), 1.0);
  assert_eq!(icons.cached(), 2, "b and c, not a");
  icons.end_frame();
  icons.image(&view(&c, None), (4, 4), 1.0);
  assert_eq!(icons.cached(), 1);
}

/// A window drawn the way the platform draws it: its own history and
/// buffer, damage only.
struct Drawing {
  window: Window,
  history: crate::DamageHistory,
  buffer: Buffer,
}

impl Drawing {
  /// A window of `child`, drawn once.
  fn new(scene: &mut Scene, child: impl crabshell_ui::Widget) -> Drawing {
    let panel = scene.panel().child(child);
    let window = scene.window(panel, 40.0, 20.0);
    let history = crate::DamageHistory::new();
    let buffer = Buffer::new(&window, 1.0);
    let mut drawn = Drawing {
      window,
      history,
      buffer,
    };
    drawn.draw(scene);
    drawn
  }

  fn draw(&mut self, scene: &mut Scene) {
    scene.frame(&self.window);
    scene.draw(&self.window, &mut self.history, &mut self.buffer);
  }
}

/// Two 4 by 4 pictures, red and blue, in `test`'s directory.
fn pair(test: &str) -> (Removed, PathBuf, PathBuf) {
  let dir = dir(test);
  let a = picture(&dir, "a.png", (4, 4), |_, _| RED);
  let b = picture(&dir, "b.png", (4, 4), |_, _| BLUE);
  (dir, a, b)
}

/// Runs the clock `ms` into transitions started at `start`.
fn tick(start: Instant, ms: u64) {
  advance_animations(start + Duration::from_millis(ms));
}

/// Starts `next`'s transitions and draws `window` halfway through them,
/// both pictures cached, and returns when they started.
fn halfway(
  scene: &mut Scene,
  next: crabshell_reactive::Signal<bool>,
  window: &mut Drawing,
) -> Instant {
  next.set(true);
  scene.frame(&window.window);
  let start = Instant::now();
  tick(start, 0);
  tick(start, 50);
  window.draw(scene);
  assert_eq!(scene.renderer.cached_icons(), 2, "both pictures, halfway");
  start
}

#[test]
fn an_image_hidden_or_under_opacity_0_mid_transition_lets_the_old_picture_go() {
  let (_dir, a, b) = pair("hidden");
  for opacity in [false, true] {
    let mut scene = Scene::new();
    let next = scene.cx.signal(false);
    let shown = scene.cx.signal(true);
    let image = switching(next, &a, &b, ImageTransition::crossfade(LINEAR))
      .width(20)
      .height(20);
    let mut window = if opacity {
      let style = move || Style::new().opacity(if shown.get() { 1.0 } else { 0.0 });
      Drawing::new(&mut scene, Stack::new().style(style).child(image))
    } else {
      Drawing::new(
        &mut scene,
        Row::new().child(image.visible(move || shown.get())),
      )
    };
    let start = halfway(&mut scene, next, &mut window);
    shown.set(false);
    window.draw(&mut scene);
    assert_eq!(scene.renderer.cached_icons(), 1, "opacity 0: {opacity}");
    tick(start, 1000);
  }
}

#[test]
fn an_image_removed_or_its_window_closed_mid_transition_lets_the_old_picture_go() {
  let (_dir, a, b) = pair("removed");
  // Removed from a list.
  let mut scene = Scene::new();
  let next = scene.cx.signal(false);
  let items = scene.cx.signal(vec![1]);
  let (from, to) = (a.clone(), b.clone());
  let list = Row::new().children_keyed(
    move || items.get(),
    |item| *item,
    move |_cx, _item| {
      switching(next, &from, &to, ImageTransition::crossfade(LINEAR))
        .width(20)
        .height(20)
    },
  );
  let mut window = Drawing::new(&mut scene, list);
  let start = halfway(&mut scene, next, &mut window);
  items.set(Vec::new());
  window.draw(&mut scene);
  assert_eq!(scene.renderer.cached_icons(), 1, "the new picture, aging");
  tick(start, 1000);

  // A window closed: the next draw of another window lets go of it.
  let mut scene = Scene::new();
  let next = scene.cx.signal(false);
  let image = switching(next, &a, &b, ImageTransition::crossfade(LINEAR))
    .width(20)
    .height(20);
  let mut closing = Drawing::new(&mut scene, image);
  let other = Image::new(b.clone()).width(20).height(20);
  let mut other = Drawing::new(&mut scene, other);
  let start = halfway(&mut scene, next, &mut closing);
  drop(closing);
  other.draw(&mut scene);
  assert_eq!(scene.renderer.cached_icons(), 1, "only b");
  tick(start, 1000);
}

#[test]
fn a_transition_switched_to_none_mid_run_lets_the_old_picture_go() {
  let (_dir, a, b) = pair("none");
  let mut scene = Scene::new();
  let next = scene.cx.signal(false);
  let kind = scene.cx.signal(ImageTransition::crossfade(LINEAR));
  let image = switching(next, &a, &b, ImageTransition::None)
    .transition(move || kind.get())
    .width(20)
    .height(20);
  let mut window = Drawing::new(&mut scene, image);
  let start = halfway(&mut scene, next, &mut window);
  kind.set(ImageTransition::None);
  window.draw(&mut scene);
  assert_eq!(scene.renderer.cached_icons(), 1);
  tick(start, 1000);
}

#[test]
fn two_windows_mid_transition_keep_their_pictures_between_each_other_s_draws() {
  let (dir, a, b) = pair("two");
  let c = picture(&dir, "c.png", (4, 4), |_, _| [0, 255, 0, 255]);
  let mut scene = Scene::new();
  let next = scene.cx.signal(false);
  // Each leaves a picture of its own, so neither keeps the other's.
  let image = |from: &Path| {
    switching(next, from, &b, ImageTransition::crossfade(LINEAR))
      .width(20)
      .height(20)
  };
  let mut first = Drawing::new(&mut scene, image(&a));
  let mut second = Drawing::new(&mut scene, image(&c));
  next.set(true);
  scene.runtime.flush();
  let start = Instant::now();
  for ms in [0, 20, 40] {
    tick(start, ms);
    first.draw(&mut scene);
    second.draw(&mut scene);
  }
  let decoded = scene.renderer.rasterized_icons();
  for ms in [60, 80] {
    tick(start, ms);
    first.draw(&mut scene);
    second.draw(&mut scene);
  }
  assert_eq!(
    scene.renderer.rasterized_icons(),
    decoded,
    "nothing decoded again"
  );
  tick(start, 100);
  first.draw(&mut scene);
  second.draw(&mut scene);
  assert_eq!(scene.renderer.cached_icons(), 1, "both ended");
}

#[test]
fn a_picture_left_that_another_window_shows_stays_cached() {
  let (_dir, a, b) = pair("shown");
  let mut scene = Scene::new();
  let next = scene.cx.signal(false);
  let image = switching(next, &a, &b, ImageTransition::crossfade(LINEAR))
    .width(20)
    .height(20);
  let mut switching_window = Drawing::new(&mut scene, image);
  let still = Image::new(a.clone()).width(20).height(20);
  let mut still = Drawing::new(&mut scene, still);
  let start = halfway(&mut scene, next, &mut switching_window);
  tick(start, 100);
  switching_window.draw(&mut scene);
  assert_eq!(
    scene.renderer.cached_icons(),
    2,
    "a stays: the other window shows it"
  );
  let decoded = scene.renderer.rasterized_icons();
  scene.full(&still.window, 1.0);
  still.draw(&mut scene);
  assert_eq!(scene.renderer.rasterized_icons(), decoded);
}

#[test]
fn a_picture_a_transition_leaves_that_a_mask_uses_is_decoded_once() {
  let (_dir, a, b) = pair("mask");
  let mask: &'static str = Box::leak(a.to_str().expect("a UTF-8 path").into());
  let mut scene = Scene::new();
  let next = scene.cx.signal(false);
  let image = switching(next, &a, &b, ImageTransition::crossfade(LINEAR))
    .width(20)
    .height(20)
    .fit(Fit::Fill);
  let masked = Stack::new()
    .style(
      Style::new()
        .mask(Mask::image(mask))
        .width(20.0)
        .height(20.0),
    )
    .child(image);
  let mut window = Drawing::new(&mut scene, masked);
  assert_eq!(scene.renderer.rasterized_icons(), 1, "one decode of a");
  let start = halfway(&mut scene, next, &mut window);
  let decoded = scene.renderer.rasterized_icons();
  assert_eq!(decoded, 2, "and one of b");
  // The transition ends and lets go of a, which the mask still uses.
  tick(start, 100);
  window.draw(&mut scene);
  scene.full(&window.window, 1.0);
  assert_eq!(scene.renderer.rasterized_icons(), decoded, "a stays cached");
}
