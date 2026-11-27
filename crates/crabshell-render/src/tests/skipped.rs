//! Images a draw skips, at opacity 0 or cut out by their mask region, load
//! their pictures at the size a draw gives them, status or not, so a
//! later reveal draws them on its first frame. Hidden ones load nothing.

use std::sync::mpsc;
use std::time::Duration;

use crabshell_reactive::Signal;
use crabshell_ui::{Align, Column, Content, Fit, Image, Region, Row, ScrollView, Style};

use super::{Buffer, Scene, fixture};

fn shows_pixels(buffer: &Buffer) -> bool {
  buffer
    .pixels
    .iter()
    .skip(3)
    .step_by(4)
    .any(|&alpha| alpha > 0)
}

fn opacity(shown: Signal<bool>) -> impl Fn() -> f32 + 'static {
  move || if shown.get() { 1.0 } else { 0.0 }
}

type Skip = fn(Image, Signal<bool>) -> Row;

/// The ways a draw skips an image until `shown`.
const SKIPS: [(&str, Skip); 4] = [
  ("opacity", |image, shown| {
    let opacity = opacity(shown);
    Row::new().child(image.style(move || Style::new().opacity(opacity())))
  }),
  ("an ancestor's opacity", |image, shown| {
    let opacity = opacity(shown);
    Row::new().child(
      Row::new()
        .style(move || Style::new().opacity(opacity()))
        .child(image),
    )
  }),
  ("mask region", |image, shown| {
    Row::new().child(image.mask_region(move || (!shown.get()).then(Region::empty)))
  }),
  ("a scaled ancestor's opacity", |image, shown| {
    let opacity = opacity(shown);
    Row::new().child(
      Row::new()
        .style(move || Style::new().opacity(opacity()).scale(2.0).origin(0.0, 0.0))
        .child(image),
    )
  }),
];

#[test]
fn a_skipped_image_loads_at_its_size_and_is_drawn_on_its_reveal() {
  for (name, skip) in SKIPS {
    let mut scene = Scene::new();
    let shown = scene.cx.signal(false);
    // Over several tiles, and no status.
    let image = Image::new(fixture("stripes.png"))
      .width(100)
      .height(70)
      .fit(Fit::Fill);
    let row = skip(image, shown).align(Align::Start);
    let window = scene.window(scene.panel().child(row), 200.0, 140.0);
    let mut history = crate::DamageHistory::new();
    let mut buffer = Buffer::new(&window, 1.0);
    scene.draw(&window, &mut history, &mut buffer);
    assert!(!shows_pixels(&buffer), "{name}: nothing drawn");
    assert_eq!(scene.renderer.rasterized_icons(), 1, "{name}: loaded");
    assert_eq!(scene.renderer.image_lookups(), 1, "{name}: once a frame");
    scene.frame(&window);
    assert!(!window.has_damage(), "{name}: idle");
    scene.full(&window, 1.0);
    assert_eq!(scene.renderer.rasterized_icons(), 1, "{name}: kept");

    shown.set(true);
    scene.frame(&window);
    scene.draw(&window, &mut history, &mut buffer);
    assert!(shows_pixels(&buffer), "{name}: drawn on its reveal");
    assert!(!scene.renderer.waits_for_pictures(), "{name}");
    assert_eq!(
      scene.renderer.rasterized_icons(),
      1,
      "{name}: at the size it is drawn at"
    );
  }
}

#[test]
fn a_hidden_image_without_a_status_loads_nothing() {
  let mut scene = Scene::new();
  let image = Image::new(fixture("stripes.png"))
    .width(40)
    .height(20)
    .visible(false);
  let row = Row::new().align(Align::Start).child(image);
  let window = scene.window(scene.panel().child(row), 40.0, 20.0);
  scene.full(&window, 1.0);
  assert_eq!(scene.renderer.rasterized_icons(), 0);
}

#[test]
fn a_large_skipped_image_is_decoded_once_on_the_worker() {
  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  let shown = scene.cx.signal(false);
  let opacity = opacity(shown);
  let image = Image::new(fixture("stripes.png"))
    .width(300)
    .height(300)
    .fit(Fit::Fill)
    .style(move || Style::new().opacity(opacity()));
  let row = Row::new().align(Align::Start).child(image);
  let window = scene.window(scene.panel().child(row), 300.0, 300.0);
  let id = match super::node_at(window.view().root(), &[0, 0]).content() {
    Content::Image(image) => image.id,
    _ => panic!("no image"),
  };
  let mut history = crate::DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(scene.renderer.decoding(), "sent to the worker undrawn");
  assert!(!scene.renderer.waits_for_pictures(), "nothing shown waits");

  let (sender, woken) = mpsc::channel();
  scene.renderer.wake_when_decoded(move || {
    let _ = sender.send(());
  });
  woken
    .recv_timeout(Duration::from_secs(20))
    .expect("the worker wakes the loop");
  assert_eq!(scene.renderer.take_decoded(), [id]);
  assert!(!scene.renderer.decoding(), "no new job");
  assert!(!window.repaint_image(id), "nothing to show for it");
  scene.frame(&window);
  assert!(!window.has_damage(), "idle");

  shown.set(true);
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(shows_pixels(&buffer), "drawn on its reveal");
  assert!(!scene.renderer.waits_for_pictures());
  assert!(!scene.renderer.decoding());
  assert_eq!(scene.renderer.decoded_off_thread(), 1, "decoded once");
}

/// A window whose scale changes while an image is skipped, as one on a
/// fractional output does once it hears its scale, reveals the picture
/// it loaded at the old scale, stretched, until the new size decodes.
#[test]
fn a_skipped_image_revealed_at_a_new_scale_shows_its_picture_while_that_decodes() {
  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  let shown = scene.cx.signal(false);
  let opacity = opacity(shown);
  let image = Image::new(fixture("stripes.png"))
    .width(300)
    .height(300)
    .fit(Fit::Fill)
    .style(move || Style::new().opacity(opacity()));
  let row = Row::new().align(Align::Start).child(image);
  let window = scene.window(scene.panel().child(row), 300.0, 300.0);
  let mut history = crate::DamageHistory::new();
  let mut buffer = Buffer::new(&window, 2.0);
  scene.draw(&window, &mut history, &mut buffer);
  let (sender, woken) = mpsc::channel();
  scene.renderer.wake_when_decoded(move || {
    let _ = sender.send(());
  });
  woken
    .recv_timeout(Duration::from_secs(20))
    .expect("the worker wakes the loop");
  scene.renderer.take_decoded();

  shown.set(true);
  scene.frame(&window);
  let mut history = crate::DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.5);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(scene.renderer.decoding(), "the new size decodes");
  assert!(shows_pixels(&buffer), "the old size shows meanwhile");
}

#[test]
fn a_skipped_subtree_is_looked_through_once_a_draw_and_not_without_images() {
  for with_image in [false, true] {
    let mut scene = Scene::new();
    let row = || Row::new().style(Style::new().width(100.0).height(1.0));
    let rows = (0..1000).fold(Column::new(), |column, _| column.child(row()));
    let mut row = Row::new()
      .align(Align::Start)
      .child(rows.style(Style::new().opacity(0.0)));
    if with_image {
      row = row.child(Image::new(fixture("stripes.png")).width(40).height(20));
    }
    let window = scene.window(scene.panel().child(row), 1920.0, 1080.0);
    scene.full(&window, 1.0);
    let looked = scene.renderer.skipped_nodes();
    match with_image {
      false => assert_eq!(looked, 0, "a window without images"),
      true => assert_eq!(looked, 1001, "the column and its rows, once"),
    }
  }
}

#[test]
fn a_skipped_scroll_view_or_one_inside_a_scroll_view_loads_only_what_it_shows() {
  for transparent_view in [true, false] {
    let mut scene = Scene::new();
    let shown = scene.cx.signal(false);
    let opacity = opacity(shown);
    // Each of its own height, so each is a picture of its own.
    let images = (0..20).fold(Column::new(), |column, at| {
      column.child(
        Image::new(fixture("stripes.png"))
          .width(100)
          .height(40 + at),
      )
    });
    let view = match transparent_view {
      true => ScrollView::new(images)
        .style(move || Style::new().width(100.0).height(50.0).opacity(opacity())),
      false => ScrollView::new(images.style(move || Style::new().opacity(opacity())))
        .style(Style::new().width(100.0).height(50.0)),
    };
    let row = Row::new().align(Align::Start).child(view);
    let window = scene.window(scene.panel().child(row), 200.0, 100.0);
    scene.full(&window, 1.0);
    let name = if transparent_view {
      "the view"
    } else {
      "inside it"
    };
    assert_eq!(
      scene.renderer.rasterized_icons(),
      2,
      "{name}: the two it shows"
    );
    shown.set(true);
    scene.frame(&window);
    scene.full(&window, 1.0);
    assert_eq!(
      scene.renderer.rasterized_icons(),
      2,
      "{name}: what a reveal draws"
    );
  }
}
