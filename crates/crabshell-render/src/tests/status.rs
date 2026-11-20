//! What a draw tells an image's `status`: Ready once it is drawn from its
//! picture, Failed for a file that can't be read or is past a limit, and
//! nothing while a picture is decoded off the thread.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crabshell_reactive::Signal;
use crabshell_ui::{
  Align, Content, Easing, Fit, Image, ImageStatus, ImageTransition, Row, Transition, Window,
  image_status_changed,
};

use super::{Buffer, Scene, capture_logs, fixture};

fn id(window: &Window) -> u64 {
  match super::node_at(window.view().root(), &[0, 0]).content() {
    Content::Image(image) => image.id,
    _ => panic!("no image"),
  }
}

/// A window of one 40 by 20 image showing `source`, with `status`.
fn window(
  scene: &mut Scene,
  source: Signal<PathBuf>,
  status: Signal<ImageStatus>,
  transition: ImageTransition,
) -> Window {
  let image = Image::new(move || source.get())
    .width(40)
    .height(20)
    .transition(transition)
    .status(status);
  let row = Row::new().align(Align::Start).child(image);
  scene.window(scene.panel().child(row), 40.0, 20.0)
}

#[test]
fn a_draw_says_ready_or_failed_and_a_new_source_waits_for_the_next() {
  for transition in [
    ImageTransition::None,
    ImageTransition::crossfade(Transition::new(200).easing(Easing::Linear)),
  ] {
    let mut scene = Scene::new();
    let source = scene.cx.signal(fixture("stripes.png"));
    let status = scene.cx.signal(ImageStatus::Empty);
    let window = window(&mut scene, source, status, transition);
    assert_eq!(status.get(), ImageStatus::Loading);
    let mut history = crate::DamageHistory::new();
    let mut buffer = Buffer::new(&window, 1.0);
    scene.draw(&window, &mut history, &mut buffer);
    assert!(image_status_changed(), "{transition:?}");
    assert_eq!(status.get(), ImageStatus::Ready);
    // Another draw of it changes nothing.
    scene.full(&window, 2.0);
    assert!(!image_status_changed());
    // A status change alone repaints nothing.
    scene.frame(&window);
    assert!(!window.has_damage());

    capture_logs(|| {
      for bad in ["/nonexistent/picture.png", "too-wide.png", "too-many.png"] {
        source.set(fixture(bad));
        scene.frame(&window);
        assert_eq!(status.get(), ImageStatus::Loading, "{bad}");
        scene.draw(&window, &mut history, &mut buffer);
        assert_eq!(status.get(), ImageStatus::Failed, "{bad} {transition:?}");
      }
    });
    source.set(fixture("bands.png"));
    scene.frame(&window);
    assert_eq!(status.get(), ImageStatus::Loading);
    scene.draw(&window, &mut history, &mut buffer);
    assert_eq!(status.get(), ImageStatus::Ready);
  }
}

#[test]
fn a_picture_decoded_off_the_thread_is_ready_once_drawn() {
  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  let source = scene.cx.signal(fixture("stripes.png"));
  let status = scene.cx.signal(ImageStatus::Empty);
  let image = Image::new(move || source.get())
    .width(300)
    .height(300)
    .fit(Fit::Fill)
    .status(status);
  let row = Row::new().align(Align::Start).child(image);
  let window = scene.window(scene.panel().child(row), 300.0, 300.0);
  let mut history = crate::DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(scene.renderer.decoding());
  assert_eq!(status.get(), ImageStatus::Loading);
  assert!(!image_status_changed());

  let (sender, woken) = mpsc::channel();
  scene.renderer.wake_when_decoded(move || {
    let _ = sender.send(());
  });
  woken
    .recv_timeout(Duration::from_secs(20))
    .expect("the worker wakes the loop");
  assert_eq!(scene.renderer.take_decoded(), [id(&window)]);
  assert_eq!(status.get(), ImageStatus::Loading, "until it is drawn");
  assert!(window.repaint_image(id(&window)));
  scene.draw(&window, &mut history, &mut buffer);
  assert_eq!(status.get(), ImageStatus::Ready);
  assert!(image_status_changed());
}
