//! What the renderer tells an image's `status`: Ready once its picture is
//! loaded, drawn or not, Failed for a file that can't be read or is past a
//! limit, and nothing while a picture is decoded off the thread.

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
    scene.draw(window, &mut history, &mut buffer);
    scene.frame(window);
  }
  buffer
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
