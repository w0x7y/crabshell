//! What the renderer tells an image's `status`: Ready once its picture is
//! loaded, drawn or not, Failed for a file that can't be read or is past a
//! limit, and nothing while a picture is decoded off the thread. Images a
//! draw skips load when the platform asks, after a source changed.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crabshell_reactive::Signal;
use crabshell_ui::{
  Align, Content, Easing, Fit, Image, ImageStatus, ImageTransition, Row, Transition, Window,
  image_sources_changed, image_status_changed,
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
fn a_picture_decoded_off_the_thread_is_ready_once_it_comes_in() {
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
  assert_eq!(status.get(), ImageStatus::Ready, "as it comes in");
  assert!(image_status_changed());
  assert!(window.repaint_image(id(&window)));
  scene.draw(&window, &mut history, &mut buffer);
  assert_eq!(status.get(), ImageStatus::Ready);
  assert!(!image_status_changed(), "drawing it tells nothing new");
}

/// What a test's flush and draws saw of an image whose look waits for
/// its own status.
fn settled(scene: &mut Scene, window: &Window) -> Buffer {
  let mut history = crate::DamageHistory::new();
  let mut buffer = Buffer::new(window, 1.0);
  for _ in 0..3 {
    turn(scene, window, &mut history, &mut buffer);
  }
  buffer
}

/// One loop turn as the platform runs it: flush and lay out, draw the
/// damage, load the waiting images if a source changed, flush again.
fn turn(
  scene: &mut Scene,
  window: &Window,
  history: &mut crate::DamageHistory,
  buffer: &mut Buffer,
) {
  scene.frame(window);
  scene.draw(window, history, buffer);
  if image_sources_changed() {
    scene.renderer.load_images(&window.view(), 1.0);
  }
  scene.frame(window);
}

/// Takes what the worker decoded until `status` leaves Loading.
fn decoded(scene: &mut Scene, status: Signal<ImageStatus>) {
  while status.get() == ImageStatus::Loading {
    let (sender, woken) = mpsc::channel();
    scene.renderer.wake_when_decoded(move || {
      let _ = sender.send(());
    });
    woken
      .recv_timeout(Duration::from_secs(20))
      .expect("the worker wakes the loop while the status waits");
    scene.renderer.take_decoded();
  }
}

/// A 40 by 20 image of `source`, shown only once its status is Ready.
fn hidden_until_ready(
  scene: &mut Scene,
  source: Signal<PathBuf>,
  status: Signal<ImageStatus>,
  blur: f32,
) -> Window {
  let image = Image::new(move || source.get())
    .width(40)
    .height(20)
    .blur(blur)
    .visible(move || status.get() == ImageStatus::Ready)
    .status(status);
  let row = Row::new().align(Align::Start).child(image);
  scene.window(scene.panel().child(row), 40.0, 20.0)
}

fn shows_pixels(buffer: &Buffer) -> bool {
  buffer
    .pixels
    .iter()
    .skip(3)
    .step_by(4)
    .any(|&alpha| alpha > 0)
}

#[test]
fn a_hidden_image_that_failed_loads_its_next_source_without_a_draw() {
  let mut scene = Scene::new();
  let source = scene.cx.signal(fixture("missing.png"));
  let status = scene.cx.signal(ImageStatus::Empty);
  let window = hidden_until_ready(&mut scene, source, status, 0.0);
  let mut history = crate::DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  capture_logs(|| turn(&mut scene, &window, &mut history, &mut buffer));
  assert_eq!(status.get(), ImageStatus::Failed);
  source.set(fixture("stripes.png"));
  scene.frame(&window);
  assert!(!window.has_damage(), "nothing to draw while it is hidden");
  turn(&mut scene, &window, &mut history, &mut buffer);
  assert_eq!(status.get(), ImageStatus::Ready);
  turn(&mut scene, &window, &mut history, &mut buffer);
  assert!(shows_pixels(&buffer), "shown once Ready");
}

#[test]
fn a_hidden_image_given_a_new_source_mid_decode_ends_with_the_new_ones_status() {
  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  let source = scene.cx.signal(fixture("stripes.png"));
  let status = scene.cx.signal(ImageStatus::Empty);
  // Blurred, so each is decoded on the worker.
  let window = hidden_until_ready(&mut scene, source, status, 2.0);
  let mut history = crate::DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  turn(&mut scene, &window, &mut history, &mut buffer);
  assert!(scene.renderer.decoding());
  source.set(fixture("missing.png"));
  capture_logs(|| {
    turn(&mut scene, &window, &mut history, &mut buffer);
    decoded(&mut scene, status);
  });
  assert_eq!(
    status.get(),
    ImageStatus::Failed,
    "the new source's, not the old"
  );
  source.set(fixture("bands.png"));
  turn(&mut scene, &window, &mut history, &mut buffer);
  decoded(&mut scene, status);
  assert_eq!(status.get(), ImageStatus::Ready);
}

#[test]
fn an_image_shown_only_once_it_is_ready_still_loads() {
  use crabshell_ui::Style;
  type Shown = fn(Image, Signal<ImageStatus>) -> Image;
  let ways: [(&str, Shown); 3] = [
    ("opacity", |image, status| {
      image.style(move || {
        Style::new().opacity(if status.get() == ImageStatus::Ready {
          1.0
        } else {
          0.0
        })
      })
    }),
    ("visible", |image, status| {
      image.visible(move || status.get() == ImageStatus::Ready)
    }),
    ("width", |image, status| {
      image.width(move || {
        if status.get() == ImageStatus::Ready {
          40
        } else {
          0
        }
      })
    }),
  ];
  for (name, shown) in ways {
    for (file, expected) in [
      ("stripes.png", ImageStatus::Ready),
      ("too-wide.png", ImageStatus::Failed),
    ] {
      let mut scene = Scene::new();
      let status = scene.cx.signal(ImageStatus::Empty);
      let image = shown(Image::new(fixture(file)).width(40).height(20), status).status(status);
      let row = Row::new().align(Align::Start).child(image);
      let window = scene.window(scene.panel().child(row), 40.0, 20.0);
      assert_eq!(status.get(), ImageStatus::Loading);
      let mut buffer = None;
      capture_logs(|| buffer = Some(settled(&mut scene, &window)));
      let buffer = buffer.expect("drawn");
      assert_eq!(status.get(), expected, "{name} {file}");
      if expected == ImageStatus::Ready {
        let drawn = buffer
          .pixels
          .iter()
          .skip(3)
          .step_by(4)
          .any(|&alpha| alpha > 0);
        assert!(drawn, "{name}: shown once Ready");
      }
    }
  }
}

#[test]
fn a_transparent_picture_decoded_off_the_thread_is_ready_when_it_comes_in() {
  use crabshell_ui::Style;
  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  let status = scene.cx.signal(ImageStatus::Empty);
  let image = Image::new(fixture("stripes.png"))
    .width(300)
    .height(300)
    .style(move || {
      Style::new().opacity(if status.get() == ImageStatus::Ready {
        1.0
      } else {
        0.0
      })
    })
    .status(status);
  let row = Row::new().align(Align::Start).child(image);
  let window = scene.window(scene.panel().child(row), 300.0, 300.0);
  scene.full(&window, 1.0);
  assert!(image_sources_changed(), "the platform loads it");
  scene.renderer.load_images(&window.view(), 1.0);
  assert!(scene.renderer.decoding(), "sent to the worker undrawn");
  let (sender, woken) = mpsc::channel();
  scene.renderer.wake_when_decoded(move || {
    let _ = sender.send(());
  });
  woken
    .recv_timeout(Duration::from_secs(20))
    .expect("the worker wakes the loop");
  scene.renderer.take_decoded();
  assert_eq!(status.get(), ImageStatus::Ready, "told as it came in");
  assert!(image_status_changed());
  // Shown now, it draws the picture decoded for it, without a new job.
  scene.frame(&window);
  scene.full(&window, 1.0);
  assert!(!scene.renderer.decoding());
}

#[test]
fn a_scaled_image_waiting_on_the_worker_is_decoded_once() {
  let mut scene = Scene::new();
  scene.renderer.decode_off_thread();
  let source = scene.cx.signal(fixture("stripes.png"));
  let status = scene.cx.signal(ImageStatus::Empty);
  // Drawn at twice its box inside the scale, so past 256 pixels a side
  // and on the worker, at another size than the window's scale gives.
  let image = Image::new(move || source.get())
    .width(150)
    .height(150)
    .status(status);
  let row = Row::new()
    .align(Align::Start)
    .style(crabshell_ui::Style::new().scale(2.0).origin(0.0, 0.0))
    .child(image);
  let window = scene.window(scene.panel().child(row), 300.0, 300.0);
  let mut history = crate::DamageHistory::new();
  let mut buffer = Buffer::new(&window, 1.0);
  turn(&mut scene, &window, &mut history, &mut buffer);
  decoded(&mut scene, status);
  assert_eq!(status.get(), ImageStatus::Ready);
  turn(&mut scene, &window, &mut history, &mut buffer);
  assert!(
    !scene.renderer.decoding(),
    "the draw's own job was kept, so Ready means it can draw"
  );
}
