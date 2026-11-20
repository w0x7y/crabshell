//! `Image::status`: Empty and Loading from the source, Ready and Failed
//! from the renderer's reports, written only on a change and repainting
//! nothing.

use std::path::{Path, PathBuf};

use super::{Counter, Harness};
use crate::image::watched_images;
use crate::{
  Content, Image, ImageStatus, Row, Window, image_sources_changed, image_status,
  image_status_changed,
};

fn id(window: &Window, index: usize) -> u64 {
  match super::node_at(window.view().root(), &[0, index]).content() {
    Content::Image(image) => image.id,
    _ => panic!("no image at {index}"),
  }
}

#[test]
fn the_source_says_empty_or_loading_and_the_renderer_ready_or_failed() {
  let mut h = Harness::new();
  let source = h.cx.signal("a.png");
  let (status, empty) = (
    h.cx.signal(ImageStatus::Ready),
    h.cx.signal(ImageStatus::Ready),
  );
  let writes = Counter::default();
  let w = writes.clone();
  h.cx.effect(move || {
    status.get();
    w.hit();
  });
  let row = Row::new()
    .child(Image::new(move || PathBuf::from(source.get())).status(status))
    .child(Image::new("").status(empty));
  let window = h.window(h.panel().child(row), 200.0, 64.0);
  assert_eq!(status.get(), ImageStatus::Loading);
  assert_eq!(empty.get(), ImageStatus::Empty);
  let image = id(&window, 0);

  image_status(image, Path::new("a.png"), ImageStatus::Ready);
  assert!(image_status_changed());
  assert!(!image_status_changed(), "taken");
  h.frame(&window);
  assert_eq!(status.get(), ImageStatus::Ready);
  assert_eq!(writes.get(), 3, "the first run, Loading and Ready");
  // The same report again, every draw, writes nothing.
  image_status(image, Path::new("a.png"), ImageStatus::Ready);
  assert!(!image_status_changed());
  // An empty source keeps Empty whatever is drawn.
  image_status(id(&window, 1), Path::new(""), ImageStatus::Failed);
  assert_eq!(empty.get(), ImageStatus::Empty);

  // A new source goes back to Loading, and a late report about the old
  // one is dropped.
  source.set("b.png");
  h.frame(&window);
  assert_eq!(status.get(), ImageStatus::Loading);
  image_status(image, Path::new("a.png"), ImageStatus::Ready);
  assert_eq!(status.get(), ImageStatus::Loading);
  image_status(image, Path::new("b.png"), ImageStatus::Failed);
  h.frame(&window);
  assert_eq!(status.get(), ImageStatus::Failed);
  source.set("");
  h.frame(&window);
  assert_eq!(status.get(), ImageStatus::Empty);
  let _ = image_status_changed();
}

#[test]
fn a_status_change_repaints_nothing_by_itself() {
  let mut h = Harness::new();
  let status = h.cx.signal(ImageStatus::Empty);
  let window = h.window(
    h.panel()
      .child(Row::new().child(Image::new("a.png").status(status))),
    100.0,
    64.0,
  );
  window.take_damage();
  image_status(id(&window, 0), Path::new("a.png"), ImageStatus::Ready);
  h.frame(&window);
  assert_eq!(status.get(), ImageStatus::Ready);
  assert!(!window.has_damage());
  assert!(!window.needs_layout());
  let _ = image_status_changed();
}

#[test]
fn an_image_that_goes_is_no_longer_watched() {
  let mut h = Harness::new();
  let before = watched_images();
  let status = h.cx.signal(ImageStatus::Empty);
  let shown = h.cx.signal(true);
  let row = Row::new().children_keyed(
    move || if shown.get() { vec![()] } else { Vec::new() },
    |_| (),
    move |_, _| Image::new("a.png").status(status),
  );
  let window = h.window(h.panel().child(row), 100.0, 64.0);
  let image = id(&window, 0);
  assert_eq!(watched_images(), before + 1);
  shown.set(false);
  h.frame(&window);
  assert_eq!(watched_images(), before);
  image_status(image, Path::new("a.png"), ImageStatus::Ready);
  assert_eq!(status.get(), ImageStatus::Loading, "the last it was told");
  assert!(!image_status_changed());
}

#[test]
fn a_new_source_asks_the_platform_to_load_it() {
  let mut h = Harness::new();
  let source = h.cx.signal("a.png");
  let status = h.cx.signal(ImageStatus::Empty);
  let plain = h.cx.signal("c.png");
  let row = Row::new()
    .child(Image::new(move || PathBuf::from(source.get())).status(status))
    .child(Image::new(move || PathBuf::from(plain.get())));
  let window = h.window(h.panel().child(row), 200.0, 64.0);
  assert!(image_sources_changed());
  assert!(!image_sources_changed(), "taken");
  plain.set("d.png");
  h.frame(&window);
  assert!(!image_sources_changed(), "no status waits on it");
  source.set("");
  h.frame(&window);
  assert!(!image_sources_changed(), "an empty source loads nothing");
  source.set("b.png");
  h.frame(&window);
  assert!(image_sources_changed());
}
