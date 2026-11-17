//! Decoding off the loop's thread: what a box draws while its picture is
//! decoded, the repaint once it comes in, the small pictures decoded at
//! once, failures, a mask taking over a job, and the worker ending.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crabshell_ui::{Align, Content, Fit, Image, Mask, Row, Stack, Style, Window};

use super::{Buffer, Scene, capture_logs, fixture};

/// A scene whose renderer decodes off the thread.
fn scene() -> Scene {
  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  scene
}

/// Waits for the worker to say a picture came in, and takes them.
fn wait(scene: &mut Scene) -> Vec<u64> {
  let (sender, woken) = mpsc::channel();
  scene.renderer.wake_when_decoded(move || {
    let _ = sender.send(());
  });
  woken
    .recv_timeout(Duration::from_secs(20))
    .expect("the worker wakes the loop");
  scene.renderer.take_decoded()
}

/// Waits for the worker thread to end, as it does once no job is left.
fn idle(scene: &Scene) {
  let deadline = std::time::Instant::now() + Duration::from_secs(20);
  while scene.renderer.decoder_running() {
    assert!(std::time::Instant::now() < deadline, "the worker ends");
    std::thread::sleep(Duration::from_millis(1));
  }
}

/// The id of the image at `path` under the window's root.
fn image_id(window: &Window, path: &[usize]) -> u64 {
  match super::node_at(window.view().root(), path).content() {
    Content::Image(image) => image.id,
    _ => panic!("no image at {path:?}"),
  }
}

/// A 300 by 300 window showing `source`, 90,000 pixels: past what is
/// decoded at once.
fn large(scene: &mut Scene, source: crabshell_reactive::Signal<&'static str>) -> Window {
  let image = Image::new(move || fixture(source.get()))
    .width(300)
    .height(300)
    .fit(Fit::Fill);
  let panel = scene
    .panel()
    .child(Row::new().align(Align::Start).child(image));
  scene.window(panel, 300.0, 300.0)
}

#[test]
fn a_large_picture_comes_in_from_the_worker_and_repaints_its_box() {
  let mut scene = scene();
  let source = scene.cx.signal("stripes.png");
  let window = large(&mut scene, source);
  let mut history = crate::DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  scene.draw(&window, &mut history, &mut buffer);
  assert_eq!(buffer.pixel(150, 150)[3], 0, "nothing until it comes in");
  assert!(scene.renderer.decoding());

  let id = image_id(&window, &[0, 0]);
  assert_eq!(wait(&mut scene), [id]);
  assert!(!scene.renderer.decoding());
  assert!(window.repaint_image(id));
  scene.draw(&window, &mut history, &mut buffer);
  let decoded_here = Scene::new().full(&window, 1.0);
  assert!(
    buffer.pixels == decoded_here.pixels,
    "as a draw decoding at once"
  );

  // A new source: the old picture stays until the new one comes in.
  source.set("bands.png");
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(buffer.pixels == decoded_here.pixels, "the old picture");
  assert_eq!(wait(&mut scene), [id]);
  assert!(window.repaint_image(id));
  scene.draw(&window, &mut history, &mut buffer);
  assert!(buffer.pixels == Scene::new().full(&window, 1.0).pixels);
}

#[test]
fn a_small_picture_is_decoded_at_once() {
  let mut scene = scene();
  let image = Image::new(fixture("stripes.png")).width(40).height(20);
  let window = scene.window(scene.panel().child(image), 40.0, 20.0);
  let buffer = scene.full(&window, 2.0);
  assert!(!scene.renderer.decoding());
  assert!(buffer.pixels == Scene::new().full(&window, 2.0).pixels);
}

#[test]
fn a_blurred_picture_is_decoded_off_the_thread_and_a_failure_logged_once() {
  let mut scene = scene();
  let missing = Image::new(PathBuf::from("/nonexistent/picture.png"))
    .width(20)
    .height(20)
    .blur(4.0);
  let window = scene.window(scene.panel().child(missing), 20.0, 20.0);
  let logs = capture_logs(|| {
    scene.full(&window, 1.0);
    assert!(scene.renderer.decoding(), "blurred, so off the thread");
    assert_eq!(wait(&mut scene), [image_id(&window, &[0])]);
    scene.full(&window, 1.0);
    scene.full(&window, 1.0);
  });
  assert_eq!(logs.matches("couldn't draw").count(), 1, "{logs}");
  assert!(
    !scene.renderer.decoding(),
    "a failure isn't asked for again"
  );
}

#[test]
fn a_mask_takes_over_a_job_for_the_same_picture_and_it_is_decoded_once() {
  let mut scene = scene();
  let mask = fixture("disc.png").display().to_string().leak();
  // A large blur first keeps the worker busy, so the second job waits.
  let busy = Image::new(fixture("stripes.jpg"))
    .width(400)
    .height(400)
    .blur(30.0);
  let masked = Stack::new()
    .style(
      Style::new()
        .mask(Mask::image(mask))
        .width(300.0)
        .height(300.0),
    )
    .child(
      Image::new(fixture("disc.png"))
        .width(300)
        .height(300)
        .fit(Fit::Fill),
    );
  let row = Row::new().align(Align::Start).child(busy).child(masked);
  let window = scene.window(scene.panel().child(row), 700.0, 400.0);
  scene.full(&window, 1.0);
  let (blurred, disc) = (image_id(&window, &[0, 0]), image_id(&window, &[0, 1, 0]));
  let mut taken: BTreeSet<u64> = scene.renderer.take_decoded().into_iter().collect();
  assert!(
    taken.contains(&disc),
    "decoded by the mask, so ready at once"
  );
  while scene.renderer.decoding() {
    taken.extend(wait(&mut scene));
  }
  assert_eq!(taken, BTreeSet::from([blurred, disc]));
  idle(&scene);
  scene.renderer.take_decoded();
  assert_eq!(scene.renderer.decoded_off_thread(), 1, "only the blur");
  assert!(scene.full(&window, 1.0).pixels == Scene::new().full(&window, 1.0).pixels);
}

#[test]
fn the_worker_ends_once_nothing_is_left() {
  let mut scene = scene();
  let source = scene.cx.signal("stripes.png");
  let window = large(&mut scene, source);
  scene.full(&window, 1.0);
  wait(&mut scene);
  idle(&scene);
  // And another starts for the next job.
  source.set("bands.png");
  scene.frame(&window);
  scene.full(&window, 1.0);
  assert_eq!(wait(&mut scene).len(), 1);
}
