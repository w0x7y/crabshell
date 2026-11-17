//! An image's transition to a new source: what the view hands the
//! renderer as it runs on the animation clock, what each tick repaints,
//! and that it stops when it ends, is replaced or its window goes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{Harness, capture_logs, node_at, rect_at};
use crate::{
  Content, Easing, Image, ImageTransition, Row, Transition, Window, advance_animations, animating,
};

const LINEAR: Transition = Transition::new(100).easing(Easing::Linear);

/// The source the image at `path` shows, and the one it leaves with how
/// far along, while a transition runs.
fn shown_at(window: &Window, path: &[usize]) -> (PathBuf, Option<(PathBuf, f32)>) {
  let view = window.view();
  match node_at(view.root(), path).content() {
    Content::Image(image) => (
      image.path.to_owned(),
      image.leaving.map(|(source, t)| (source.to_owned(), t)),
    ),
    _ => panic!("no image at {path:?}"),
  }
}

fn path(name: &str) -> PathBuf {
  Path::new(name).to_owned()
}

#[test]
fn without_a_transition_a_new_source_shows_at_once() {
  let mut h = Harness::new();
  let source = h.cx.signal(path("a.png"));
  let image = Image::new(move || source.get());
  let window = h.window(h.panel().child(Row::new().child(image)), 100.0, 64.0);
  source.set(path("b.png"));
  h.frame(&window);
  assert_eq!(shown_at(&window, &[0, 0]), (path("b.png"), None));
  assert!(!animating());
}

#[test]
fn a_new_source_comes_in_over_its_transition_repainting_only_the_image() {
  let mut h = Harness::new();
  let source = h.cx.signal(path("a.png"));
  let row = Row::new()
    .child(Image::new("still.png").width(20).height(20))
    .child(
      Image::new(move || source.get())
        .width(40)
        .height(30)
        .transition(ImageTransition::crossfade(LINEAR)),
    );
  let window = h.window(h.panel().child(row), 100.0, 64.0);
  let rect = rect_at(&window, &[0, 1]);
  window.take_damage();
  assert!(!animating(), "nothing runs before a change");

  source.set(path("b.png"));
  h.frame(&window);
  assert_eq!(
    shown_at(&window, &[0, 1]),
    (path("b.png"), Some((path("a.png"), 0.0)))
  );
  assert_eq!(window.take_damage(), vec![rect]);
  assert!(!window.needs_layout());
  assert!(animating());

  let start = Instant::now();
  assert!(advance_animations(start), "its time 0");
  window.take_damage();
  assert!(advance_animations(start + Duration::from_millis(50)));
  let (_, leaving) = shown_at(&window, &[0, 1]);
  let t = leaving.expect("still running").1;
  assert!((t - 0.5).abs() < 1e-4, "halfway: {t}");
  assert_eq!(window.take_damage(), vec![rect], "the image's box only");
  assert!(!window.needs_layout());

  // At its end the old picture goes, with one last repaint, and the
  // clock stops.
  assert!(!advance_animations(start + Duration::from_millis(100)));
  assert_eq!(shown_at(&window, &[0, 1]), (path("b.png"), None));
  assert_eq!(window.take_damage(), vec![rect]);
  assert!(!animating());
}

#[test]
fn each_kind_reaches_the_view_and_none_is_the_default() {
  let mut h = Harness::new();
  let kinds = [
    ImageTransition::none(),
    ImageTransition::crossfade(LINEAR),
    ImageTransition::wipe(LINEAR, 45.0),
    ImageTransition::circle(LINEAR),
    ImageTransition::pixelate(LINEAR),
  ];
  let mut row = Row::new().child(Image::new("a.png"));
  for kind in kinds {
    row = row.child(Image::new("a.png").transition(kind));
  }
  let window = h.window(h.panel().child(row), 600.0, 64.0);
  let view = window.view();
  let transition_at = |index| match node_at(view.root(), &[0, index]).content() {
    Content::Image(image) => image.transition,
    _ => panic!("no image"),
  };
  assert_eq!(transition_at(0), ImageTransition::None);
  for (index, kind) in kinds.into_iter().enumerate() {
    assert_eq!(transition_at(index + 1), kind);
  }
  assert_eq!(
    ImageTransition::wipe(LINEAR, 45.0).transition(),
    Some(LINEAR)
  );
  assert_eq!(ImageTransition::none().transition(), None);
}

#[test]
fn a_new_source_mid_transition_leaves_the_picture_most_of_the_box_shows() {
  // Before halfway, most of the box shows the picture leaving; from
  // halfway on, the one coming in.
  for (ms, left) in [(40, "a.png"), (60, "b.png")] {
    let mut h = Harness::new();
    let source = h.cx.signal(path("a.png"));
    let image = Image::new(move || source.get()).transition(ImageTransition::circle(LINEAR));
    let window = h.window(h.panel().child(Row::new().child(image)), 100.0, 64.0);
    source.set(path("b.png"));
    h.frame(&window);
    let start = Instant::now();
    advance_animations(start);
    advance_animations(start + Duration::from_millis(ms));

    source.set(path("c.png"));
    h.frame(&window);
    assert_eq!(
      shown_at(&window, &[0, 0]),
      (path("c.png"), Some((path(left), 0.0))),
      "at {ms} ms"
    );
    // The replaced one stops; the new one counts from the clock's last
    // tick, so it moves at the next.
    assert!(advance_animations(start + Duration::from_millis(ms + 50)));
    let (_, leaving) = shown_at(&window, &[0, 0]);
    let t = leaving.expect("the new one runs").1;
    assert!((t - 0.5).abs() < 1e-4, "{t}");
    assert!(!advance_animations(start + Duration::from_millis(ms + 100)));
    assert_eq!(shown_at(&window, &[0, 0]), (path("c.png"), None));
  }
}

#[test]
fn going_back_early_to_the_picture_still_shown_shows_it_at_once() {
  let mut h = Harness::new();
  let source = h.cx.signal(path("a.png"));
  let image = Image::new(move || source.get()).transition(ImageTransition::crossfade(LINEAR));
  let window = h.window(h.panel().child(Row::new().child(image)), 100.0, 64.0);
  source.set(path("b.png"));
  h.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(10));
  source.set(path("a.png"));
  h.frame(&window);
  assert_eq!(shown_at(&window, &[0, 0]), (path("a.png"), None));
  assert!(!advance_animations(start + Duration::from_millis(20)));
}

#[test]
fn a_delay_shows_the_old_picture_and_repaints_nothing_until_it_moves() {
  let mut h = Harness::new();
  let source = h.cx.signal(path("a.png"));
  let image =
    Image::new(move || source.get()).transition(ImageTransition::pixelate(LINEAR.delay(50)));
  let window = h.window(h.panel().child(Row::new().child(image)), 100.0, 64.0);
  source.set(path("b.png"));
  h.frame(&window);
  window.take_damage();
  let start = Instant::now();
  advance_animations(start);
  assert!(advance_animations(start + Duration::from_millis(40)));
  assert!(!window.has_damage());
  assert_eq!(shown_at(&window, &[0, 0]).1, Some((path("a.png"), 0.0)));
  advance_animations(start + Duration::from_millis(100));
  assert!(window.has_damage());
  assert!(!advance_animations(start + Duration::from_millis(150)));
  assert_eq!(shown_at(&window, &[0, 0]).1, None);
}

#[test]
fn an_instant_transition_a_hidden_image_or_the_first_source_shows_at_once() {
  let mut h = Harness::new();
  let source = h.cx.signal(path("a.png"));
  let shown = h.cx.signal(true);
  let row = Row::new()
    .child(
      Image::new(move || source.get()).transition(ImageTransition::crossfade(Transition::new(0))),
    )
    .child(
      Image::new(move || source.get())
        .transition(ImageTransition::crossfade(LINEAR))
        .visible(move || shown.get()),
    );
  let window = h.window(h.panel().child(row), 200.0, 64.0);
  assert_eq!(shown_at(&window, &[0, 0]).1, None, "the first source");
  shown.set(false);
  h.frame(&window);
  source.set(path("b.png"));
  h.frame(&window);
  assert_eq!(shown_at(&window, &[0, 0]), (path("b.png"), None));
  assert!(!animating(), "a 0 ms one and a hidden one run nothing");
}

#[test]
fn a_transition_stops_when_its_window_goes() {
  let mut h = Harness::new();
  let source = h.cx.signal(path("a.png"));
  let image = Image::new(move || source.get()).transition(ImageTransition::crossfade(LINEAR));
  let window = h.window(h.panel().child(Row::new().child(image)), 100.0, 64.0);
  source.set(path("b.png"));
  h.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  drop(window);
  assert!(!advance_animations(start + Duration::from_millis(10)));
}

#[test]
fn a_panicking_transition_is_logged_with_its_path_and_the_rest_keeps_working() {
  let mut h = Harness::new();
  let fail = h.cx.signal(false);
  let source = h.cx.signal(path("a.png"));
  let row = Row::new()
    .child(Image::new("a.png").transition(move || {
      assert!(!fail.get(), "transition failure");
      ImageTransition::crossfade(LINEAR)
    }))
    .child(Image::new(move || source.get()).transition(ImageTransition::wipe(LINEAR, 90.0)));
  let window = h.window(h.panel().child(row), 200.0, 64.0);
  let logs = capture_logs(|| {
    fail.set(true);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect PanelWindow[DP-1]/Row/Image[0].transition panicked and is disabled"),
    "{logs}"
  );
  let view = window.view();
  match node_at(view.root(), &[0, 0]).content() {
    Content::Image(image) => assert_eq!(
      image.transition,
      ImageTransition::crossfade(LINEAR),
      "keeps its last value"
    ),
    _ => panic!("no image"),
  }
  drop(view);
  source.set(path("b.png"));
  h.frame(&window);
  assert_eq!(
    shown_at(&window, &[0, 1]),
    (path("b.png"), Some((path("a.png"), 0.0)))
  );
}

#[test]
fn a_dropped_image_or_window_is_reported_once_for_the_renderer() {
  let mut h = Harness::new();
  let shown = h.cx.signal(vec![1]);
  let list = Row::new().child(Image::new("kept.png")).children_keyed(
    move || shown.get(),
    |item| *item,
    |_cx, _item| Image::new("a.png"),
  );
  let window = h.window(h.panel().child(list), 200.0, 64.0);
  let id = |path: &[usize]| {
    let view = window.view();
    match node_at(view.root(), path).content() {
      Content::Image(image) => image.id,
      _ => panic!("no image"),
    }
  };
  let (kept, removed) = (id(&[0, 0]), id(&[0, 1]));
  crate::dropped_images();
  shown.set(Vec::new());
  h.frame(&window);
  assert_eq!(crate::dropped_images(), vec![removed]);
  assert!(crate::dropped_images().is_empty(), "once");
  drop(window);
  assert_eq!(crate::dropped_images(), vec![kept]);
}

#[test]
fn a_source_the_renderer_cant_load_ends_its_transition_at_once() {
  let mut h = Harness::new();
  let source = h.cx.signal(path("a.png"));
  let image = Image::new(move || source.get())
    .width(40)
    .height(30)
    .transition(ImageTransition::crossfade(LINEAR));
  let window = h.window(h.panel().child(Row::new().child(image)), 100.0, 64.0);
  let rect = rect_at(&window, &[0, 0]);
  let id = match node_at(window.view().root(), &[0, 0]).content() {
    Content::Image(image) => image.id,
    _ => panic!("no image"),
  };
  source.set(path("broken.png"));
  h.frame(&window);
  let start = Instant::now();
  assert!(advance_animations(start));
  window.take_damage();

  // A report for another image or another source changes nothing.
  crate::image_source_failed(id + 1, Path::new("broken.png"));
  crate::image_source_failed(id, Path::new("a.png"));
  assert!(advance_animations(start + Duration::from_millis(10)));
  assert!(shown_at(&window, &[0, 0]).1.is_some(), "still running");

  crate::image_source_failed(id, Path::new("broken.png"));
  crate::image_source_failed(id, Path::new("broken.png"));
  assert!(
    !advance_animations(start + Duration::from_millis(20)),
    "it ends"
  );
  assert_eq!(shown_at(&window, &[0, 0]), (path("broken.png"), None));
  assert_eq!(window.take_damage(), vec![rect], "repainted once");
  assert!(!animating());

  // The report was used up: the next transition runs its time.
  source.set(path("c.png"));
  h.frame(&window);
  assert!(advance_animations(start + Duration::from_millis(30)));
  assert!(advance_animations(start + Duration::from_millis(80)));
  assert!(!advance_animations(start + Duration::from_millis(500)));
}

#[test]
fn a_report_about_an_image_dropped_is_let_go() {
  let mut h = Harness::new();
  let window = h.window(h.panel().child(Image::new("a.png")), 100.0, 64.0);
  let id = match node_at(window.view().root(), &[0]).content() {
    Content::Image(image) => image.id,
    _ => panic!("no image"),
  };
  crate::image_source_failed(id, Path::new("broken.png"));
  assert_eq!(crate::image::failed_reports(), 1);
  drop(window);
  assert_eq!(crate::image::failed_reports(), 0);
}

#[test]
fn a_transition_waits_at_its_start_while_its_new_picture_is_decoded() {
  let mut h = Harness::new();
  let source = h.cx.signal(path("a.png"));
  let image = Image::new(move || source.get())
    .width(40)
    .height(30)
    .transition(ImageTransition::crossfade(LINEAR));
  let window = h.window(h.panel().child(Row::new().child(image)), 100.0, 64.0);
  let id = match node_at(window.view().root(), &[0, 0]).content() {
    Content::Image(image) => image.id,
    _ => panic!("no image"),
  };
  source.set(path("b.png"));
  h.frame(&window);
  crate::image_source_decoding(id, Path::new("b.png"));
  let start = Instant::now();
  let ms = Duration::from_millis;
  for at in [0, 50, 500] {
    assert!(advance_animations(start + ms(at)), "it waits at {at} ms");
    let (_, leaving) = shown_at(&window, &[0, 0]);
    assert_eq!(leaving.expect("still running").1, 0.0, "at its start");
  }
  // The picture comes in: the clock starts at the next tick.
  assert!(window.repaint_image(id));
  assert!(advance_animations(start + ms(600)));
  assert!(advance_animations(start + ms(650)));
  let t = shown_at(&window, &[0, 0]).1.expect("still running").1;
  assert!((t - 0.5).abs() < 1e-4, "halfway: {t}");
  assert!(!advance_animations(start + ms(700)), "and it ends");

  // A report about another source, or one an image dropped, holds
  // nothing.
  crate::image_source_decoding(id, Path::new("c.png"));
  source.set(path("a.png"));
  h.frame(&window);
  assert!(advance_animations(start + ms(800)));
  assert!(!advance_animations(start + ms(1000)), "ran its time");
  drop(window);
  assert_eq!(crate::image::decoding_reports(), 0);
}
