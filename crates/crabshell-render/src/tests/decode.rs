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

/// A solid red `width` by `height` PNG in the temp dir, removed when the
/// test ends.
fn red_picture(name: &str) -> super::Removed {
  let path = std::env::temp_dir().join(format!(
    "crabshell-decode-{}-{name}.png",
    std::process::id()
  ));
  super::write_png(&path, &[255, 0, 0, 255].repeat(16), 4, 4);
  super::Removed(path)
}

/// A window showing `path` 300 high and `width` wide, filled.
fn resizing(
  scene: &mut Scene,
  path: &std::path::Path,
  width: crabshell_reactive::Signal<u32>,
) -> Window {
  let path = path.to_owned();
  let image = Image::new(path)
    .width(move || width.get())
    .height(300)
    .fit(Fit::Fill);
  let panel = scene
    .panel()
    .child(Row::new().align(Align::Start).child(image));
  scene.window(panel, 500.0, 300.0)
}

#[test]
fn while_a_new_size_decodes_the_last_picture_shown_is_stretched_to_the_box() {
  let red = red_picture("stretched");
  let mut scene = scene();
  let width = scene.cx.signal(300_u32);
  let window = resizing(&mut scene, &red, width);
  scene.full(&window, 1.0);
  wait(&mut scene);
  assert_eq!(scene.full(&window, 1.0).pixel(150, 150), [255, 0, 0, 255]);
  width.set(400);
  scene.frame(&window);
  let buffer = scene.full(&window, 1.0);
  assert!(scene.renderer.decoding(), "the new size is decoded");
  for x in [10, 350, 395] {
    assert_eq!(buffer.pixel(x, 150), [255, 0, 0, 255], "stretched, at {x}");
  }
  assert_eq!(buffer.pixel(450, 150)[3], 0, "and only to the box");
}

#[test]
fn a_box_changing_size_while_it_waits_leaves_one_job() {
  let red = red_picture("sizes");
  let mut scene = scene();
  // A large blur first keeps the worker busy, so the sizes wait.
  let busy = Image::new(fixture("stripes.jpg"))
    .width(400)
    .height(400)
    .blur(30.0);
  let busy = scene.window(scene.panel().child(busy), 400.0, 400.0);
  scene.full(&busy, 1.0);
  let width = scene.cx.signal(300_u32);
  let window = resizing(&mut scene, &red, width);
  for step in 0..5 {
    width.set(300 + 10 * step);
    scene.frame(&window);
    scene.full(&window, 1.0);
  }
  while scene.renderer.decoding() {
    wait(&mut scene);
  }
  idle(&scene);
  scene.renderer.take_decoded();
  assert_eq!(
    scene.renderer.decoded_off_thread(),
    2,
    "the blur and the last size"
  );
  assert_eq!(scene.full(&window, 1.0).pixel(335, 150), [255, 0, 0, 255]);
}

#[test]
fn a_transition_waits_at_its_start_until_its_picture_comes_in() {
  use crabshell_ui::{Easing, ImageTransition, Transition, advance_animations};
  let red = red_picture("transition-from");
  let mut scene = scene();
  let source = scene.cx.signal(red.to_path_buf());
  let image = Image::new(move || source.get())
    .width(300)
    .height(300)
    .fit(Fit::Fill)
    .transition(ImageTransition::crossfade(
      Transition::new(100).easing(Easing::Linear),
    ));
  let panel = scene
    .panel()
    .child(Row::new().align(Align::Start).child(image));
  let window = scene.window(panel, 300.0, 300.0);
  scene.full(&window, 1.0);
  wait(&mut scene);
  let id = image_id(&window, &[0, 0]);
  assert!(window.repaint_image(id));
  scene.full(&window, 1.0);

  source.set(fixture("bands.png"));
  scene.frame(&window);
  let start = std::time::Instant::now();
  let ms = Duration::from_millis;
  advance_animations(start);
  let drawn = scene.full(&window, 1.0);
  assert_eq!(drawn.pixel(150, 150), [255, 0, 0, 255], "the old picture");
  for at in [50, 500] {
    assert!(advance_animations(start + ms(at)), "it waits at {at} ms");
  }
  let leaving = |window: &Window| match super::node_at(window.view().root(), &[0, 0]).content() {
    Content::Image(image) => image.leaving.map(|(_, t)| t),
    _ => None,
  };
  assert_eq!(leaving(&window), Some(0.0), "at its start");
  assert_eq!(wait(&mut scene), [id]);
  assert!(window.repaint_image(id));
  assert!(advance_animations(start + ms(600)));
  assert!(advance_animations(start + ms(650)));
  assert_eq!(leaving(&window), Some(0.5), "halfway once it came in");
  assert!(!advance_animations(start + ms(700)));
}

/// A solid `color` PNG in the temp dir, removed when the test ends.
fn solid_picture(name: &str, color: [u8; 4], side: u32) -> super::Removed {
  let path = std::env::temp_dir().join(format!(
    "crabshell-decode-{}-{name}.png",
    std::process::id()
  ));
  let pixels = color.repeat((side * side) as usize);
  super::write_png(&path, &pixels, side, side);
  super::Removed(path)
}

#[test]
fn waiting_draws_a_cached_copy_of_the_new_source_before_the_old_image() {
  let red = solid_picture("cached-red", [255, 0, 0, 255], 4);
  let green = solid_picture("cached-green", [0, 255, 0, 255], 4);
  let mut scene = scene();
  let hero_source = scene.cx.signal(red.to_path_buf());
  let side_path = green.to_path_buf();
  // Hero (300x300) shows red while side (200x200, decoded at once) caches green.
  let row = Row::new()
    .align(Align::Start)
    .child(
      Image::new(move || hero_source.get())
        .width(300)
        .height(300)
        .fit(Fit::Fill),
    )
    .child(Image::new(side_path).width(200).height(200).fit(Fit::Fill));
  let window = scene.window(scene.panel().child(row), 500.0, 300.0);
  scene.full(&window, 1.0);
  wait(&mut scene);
  let hero_id = image_id(&window, &[0, 0]);
  assert!(window.repaint_image(hero_id));
  let initial = scene.full(&window, 1.0);
  assert_eq!(initial.pixel(150, 150), [255, 0, 0, 255], "hero starts red");
  assert_eq!(initial.pixel(400, 100), [0, 255, 0, 255], "side is green");

  // Moving green into the hero slot queues 300x300 on the worker; while it
  // decodes, the hero stretches green's cached 200x200 copy instead of red.
  hero_source.set(green.to_path_buf());
  scene.frame(&window);
  let while_decoding = scene.full(&window, 1.0);
  assert!(scene.renderer.decoding(), "hero's 300x300 green is queued");
  assert_eq!(
    while_decoding.pixel(150, 150),
    [0, 255, 0, 255],
    "hero shows green stretched from cache instead of stale red"
  );
  assert_eq!(wait(&mut scene), [hero_id]);
}

#[test]
fn smaller_files_waiting_on_the_worker_decode_before_larger_ones() {
  // Make a noisy 512x512 PNG so its file size is much larger than the tiny 4x4 PNG.
  let big_path =
    std::env::temp_dir().join(format!("crabshell-decode-{}-big.png", std::process::id()));
  let mut noisy = vec![0_u8; 512 * 512 * 4];
  let mut state: u32 = 0x1234_5678;
  for chunk in noisy.as_chunks_mut::<4>().0 {
    state = state.wrapping_mul(1664525).wrapping_add(1013904223);
    chunk[0] = (state >> 24) as u8;
    chunk[1] = (state >> 16) as u8;
    chunk[2] = (state >> 8) as u8;
    chunk[3] = 255;
  }
  super::write_png(&big_path, &noisy, 512, 512);
  let big = super::Removed(big_path);
  let small = solid_picture("tiny-priority", [0, 0, 255, 255], 4);

  let mut scene = scene();
  // First keep the worker busy on a large blur so both queued files wait together.
  let busy = Image::new(fixture("stripes.jpg"))
    .width(400)
    .height(400)
    .blur(30.0);
  let busy_win = scene.window(scene.panel().child(busy), 400.0, 400.0);
  scene.full(&busy_win, 1.0);
  let busy_id = image_id(&busy_win, &[0]);

  let row = Row::new()
    .align(Align::Start)
    .child(
      Image::new(big.to_path_buf())
        .width(300)
        .height(300)
        .fit(Fit::Fill),
    )
    .child(
      Image::new(small.to_path_buf())
        .width(300)
        .height(300)
        .fit(Fit::Fill),
    );
  let window = scene.window(scene.panel().child(row), 600.0, 300.0);
  scene.full(&window, 1.0);
  let (big_id, small_id) = (image_id(&window, &[0, 0]), image_id(&window, &[0, 1]));

  let mut order = Vec::new();
  while scene.renderer.decoding() {
    order.extend(wait(&mut scene));
  }
  let pos_small = order
    .iter()
    .position(|&id| id == small_id)
    .expect("small decoded");
  let pos_big = order
    .iter()
    .position(|&id| id == big_id)
    .expect("big decoded");
  assert!(order.contains(&busy_id));
  assert!(
    pos_small < pos_big,
    "small file ({small_id}) should finish before large file ({big_id}); order={order:?}"
  );
}
