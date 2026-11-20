//! Animated GIFs: frames composed as their disposal says, the caps, and
//! playback that wakes only for a GIF drawn.

use std::borrow::Cow;
use std::path::Path;
use std::time::{Duration, Instant};

use crabshell_ui::{Fit, ImageFilters, ImageView, Rect};
use gif::{DisposalMethod, Repeat};

use super::{Removed, capture_logs};
use crate::animation::{Frames, Limits, frames_within};

/// Index 0 is red, 1 green, 2 blue, 3 clear.
const PALETTE: [u8; 12] = [255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0];
const CLEAR: u8 = 3;

/// One frame to write: a `width` by `height` patch at `left`, `top` of one
/// palette index, shown for `delay` hundredths of a second.
struct Patch {
  rect: (u16, u16, u16, u16),
  index: u8,
  delay: u16,
  dispose: DisposalMethod,
}

fn patch(rect: (u16, u16, u16, u16), index: u8, delay: u16) -> Patch {
  Patch {
    rect,
    index,
    delay,
    dispose: DisposalMethod::Keep,
  }
}

/// A 4 by 4 GIF of `patches`, as bytes.
fn encode(patches: &[Patch], repeat: Option<Repeat>) -> Vec<u8> {
  let mut bytes = Vec::new();
  {
    let mut encoder = gif::Encoder::new(&mut bytes, 4, 4, &PALETTE).expect("an encoder");
    if let Some(repeat) = repeat {
      encoder.set_repeat(repeat).expect("the loop count");
    }
    for patch in patches {
      let (left, top, width, height) = patch.rect;
      let pixels = vec![patch.index; usize::from(width) * usize::from(height)];
      let frame = gif::Frame {
        left,
        top,
        width,
        height,
        delay: patch.delay,
        dispose: patch.dispose,
        transparent: Some(CLEAR),
        buffer: Cow::Owned(pixels),
        ..gif::Frame::default()
      };
      encoder.write_frame(&frame).expect("a frame");
    }
  }
  bytes
}

/// `bytes` written to a GIF file of its own, removed when the test ends.
fn file(name: &str, bytes: &[u8]) -> Removed {
  let path = std::env::temp_dir().join(format!(
    "crabshell-animation-{}-{name}.gif",
    std::process::id()
  ));
  std::fs::write(&path, bytes).expect("write the GIF");
  Removed(path)
}

/// No limit on decoding or keeping frames.
const ANY: Limits = Limits {
  work: u64::MAX,
  kept: u64::MAX,
};

fn decode(path: &Path, limits: Limits) -> Frames {
  frames_within(
    path,
    (4, 4),
    Fit::Fill,
    &ImageFilters::default(),
    1.0,
    limits,
  )
  .unwrap_or_else(|error| panic!("{error}"))
}

/// Each frame's pixel at `x`, `y`, as RGBA.
fn pixels_at(frames: &Frames, x: u32, y: u32) -> Vec<[u8; 4]> {
  frames
    .frames
    .iter()
    .map(|frame| {
      let pixel = frame.pixmap.pixel(x, y).expect("inside");
      [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
    })
    .collect()
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const NOTHING: [u8; 4] = [0, 0, 0, 0];

#[test]
fn every_frame_is_decoded_with_its_delay_and_the_loop_count() {
  let gif = file(
    "delays",
    &encode(
      &[
        patch((0, 0, 4, 4), 0, 5),
        patch((0, 0, 4, 4), 1, 0),
        patch((0, 0, 4, 4), 2, 1),
      ],
      Some(Repeat::Infinite),
    ),
  );
  let frames = decode(&gif, ANY);
  assert_eq!(pixels_at(&frames, 1, 1), [RED, GREEN, BLUE]);
  let delays: Vec<_> = frames.frames.iter().map(|frame| frame.delay).collect();
  let ms = Duration::from_millis;
  assert_eq!(delays, [ms(50), ms(100), ms(100)], "under 20 ms is 100");
  assert_eq!(frames.repeats, None, "for ever");

  let twice = file(
    "twice",
    &encode(
      &[patch((0, 0, 4, 4), 0, 5), patch((0, 0, 4, 4), 1, 5)],
      Some(Repeat::Finite(2)),
    ),
  );
  assert_eq!(decode(&twice, ANY).repeats, Some(2));
  let once = file(
    "once",
    &encode(
      &[patch((0, 0, 4, 4), 0, 5), patch((0, 0, 4, 4), 1, 5)],
      None,
    ),
  );
  assert_eq!(decode(&once, ANY).repeats, Some(0), "no loop count");
}

#[test]
fn frames_are_composed_as_their_disposal_says() {
  let gif = file(
    "dispose",
    &encode(
      &[
        // Red all over.
        patch((0, 0, 4, 4), 0, 5),
        // Green in the top-left 2 by 2, cleared after it shows.
        Patch {
          dispose: DisposalMethod::Background,
          ..patch((0, 0, 2, 2), 1, 5)
        },
        // Blue in the bottom-right 2 by 2, put back after it shows.
        Patch {
          dispose: DisposalMethod::Previous,
          ..patch((2, 2, 2, 2), 2, 5)
        },
        // A clear frame changes nothing it covers.
        patch((0, 0, 4, 4), CLEAR, 5),
      ],
      None,
    ),
  );
  let frames = decode(&gif, ANY);
  assert_eq!(frames.frames.len(), 4);
  assert_eq!(pixels_at(&frames, 0, 0), [RED, GREEN, NOTHING, NOTHING]);
  assert_eq!(pixels_at(&frames, 3, 3), [RED, RED, BLUE, RED]);
  assert_eq!(pixels_at(&frames, 3, 0), [RED, RED, RED, RED]);
}

#[test]
fn a_broken_frame_stops_the_animation_on_the_last_good_one() {
  let mut bytes = encode(
    &[
      patch((0, 0, 4, 4), 0, 5),
      patch((0, 0, 4, 4), 1, 5),
      patch((0, 0, 4, 4), 2, 5),
    ],
    Some(Repeat::Infinite),
  );
  // Cut inside the last frame's pixels.
  bytes.truncate(bytes.len() - 6);
  let gif = file("broken", &bytes);
  let mut frames = None;
  let logs = capture_logs(|| frames = Some(decode(&gif, ANY)));
  let frames = frames.expect("decoded");
  assert_eq!(pixels_at(&frames, 0, 0), [RED, GREEN]);
  assert_eq!(frames.repeats, Some(0), "plays once and stops");
  assert!(logs.contains("a GIF stops at a broken frame"), "{logs}");
}

fn three_frames(name: &str) -> Removed {
  file(
    name,
    &encode(
      &[
        patch((0, 0, 4, 4), 0, 5),
        patch((0, 0, 4, 4), 1, 5),
        patch((0, 0, 4, 4), 2, 5),
      ],
      Some(Repeat::Infinite),
    ),
  )
}

/// `gif` decoded within `limits`, and what that logged.
fn decode_logged(
  gif: &Path,
  size: (u32, u32),
  filters: ImageFilters,
  limits: Limits,
) -> (Frames, String) {
  let mut frames = None;
  let logs = capture_logs(|| {
    frames = Some(
      frames_within(gif, size, Fit::Fill, &filters, 1.0, limits)
        .unwrap_or_else(|error| panic!("{error}")),
    );
  });
  (frames.expect("decoded"), logs)
}

#[test]
fn a_gif_past_the_decoding_limit_shows_its_first_frame_and_says_so() {
  let gif = three_frames("work");
  // Three frames on a 4 by 4 canvas are 48 pixels of decoding, whatever
  // the size drawn.
  let at = |work| Limits { work, ..ANY };
  assert_eq!(decode(&gif, at(48)).frames.len(), 3);
  let (frames, logs) = decode_logged(&gif, (1, 1), ImageFilters::default(), at(47));
  assert_eq!(frames.frames.len(), 1);
  assert_eq!(frames.repeats, Some(0));
  assert!(logs.contains("shows its first"), "{logs}");
}

#[test]
fn a_gif_whose_frames_would_keep_too_much_shows_its_first_frame_and_says_so() {
  let gif = three_frames("kept");
  // Kept at 8 by 8, three frames are 192 pixels, however small the canvas.
  let at = |kept| Limits { kept, ..ANY };
  let (frames, _) = decode_logged(&gif, (8, 8), ImageFilters::default(), at(192));
  assert_eq!(frames.frames.len(), 3);
  let (frames, logs) = decode_logged(&gif, (8, 8), ImageFilters::default(), at(191));
  assert_eq!(frames.frames.len(), 1);
  assert_eq!(frames.repeats, Some(0));
  assert!(logs.contains("shows its first"), "{logs}");
}

#[test]
fn a_blurred_gif_shows_its_first_frame_still() {
  let gif = three_frames("blurred");
  let blurred = ImageFilters {
    blur: 1.0,
    ..ImageFilters::default()
  };
  let (frames, _) = decode_logged(&gif, (4, 4), blurred, ANY);
  assert_eq!(frames.frames.len(), 1);
  assert_eq!(frames.repeats, Some(0));
  assert_eq!(pixels_at(&frames, 0, 0), [RED]);
}

/// A GIF of a good red frame and then one at `rect`.
fn then(name: &str, rect: (u16, u16, u16, u16)) -> Removed {
  file(
    name,
    &encode(
      &[patch((0, 0, 4, 4), 0, 5), patch(rect, 1, 5)],
      Some(Repeat::Infinite),
    ),
  )
}

#[test]
fn a_frame_off_or_past_its_canvas_stops_the_animation_on_the_last_good_one() {
  for (name, rect) in [
    ("right", (200, 0, 2, 2)),
    ("below", (0, 200, 2, 2)),
    ("far", (60000, 60000, 2, 2)),
    ("part-right", (3, 0, 2, 2)),
    ("part-below", (0, 3, 2, 2)),
    ("larger", (0, 0, 5, 5)),
  ] {
    let gif = then(name, rect);
    let (frames, logs) = decode_logged(&gif, (4, 4), ImageFilters::default(), ANY);
    assert_eq!(pixels_at(&frames, 0, 0), [RED], "{name}");
    assert_eq!(frames.repeats, Some(0), "{name}");
    assert!(
      logs.contains("a GIF stops at a broken frame"),
      "{name}: {logs}"
    );
  }
}

#[test]
fn a_first_frame_off_or_larger_than_its_canvas_is_refused() {
  for (name, rect) in [
    ("first-right", (200, 0, 2, 2)),
    ("first-part", (3, 3, 2, 2)),
    ("first-larger", (0, 0, 8, 8)),
  ] {
    let gif = file(name, &encode(&[patch(rect, 0, 5)], None));
    let result = frames_within(&gif, (4, 4), Fit::Fill, &ImageFilters::default(), 1.0, ANY);
    assert!(result.is_err(), "{name}");
    // A still GIF's first frame, as colors and the cache read it.
    let data = std::fs::read(&*gif).expect("read");
    assert!(crate::images::gif(&data).is_err(), "{name}");
  }
}

/// What an `Image` `id` of the GIF at `path` hands the renderer.
fn view(id: u64, path: &Path, playing: bool) -> ImageView<'_> {
  ImageView {
    id,
    source: super::file_source(path),
    rect: Rect::new(0.0, 0.0, 4.0, 4.0),
    fit: Fit::Fill,
    filters: ImageFilters::default(),
    playing,
    transition: crabshell_ui::ImageTransition::None,
    leaving: None,
  }
}

fn icons() -> crate::icons::Icons {
  crate::icons::Icons::with_themes(crate::theme::Themes::new(Vec::new()))
}

/// The red of the frame `icons` draws for `image`: 255 for the red
/// frame, 0 for the others.
fn red(icons: &mut crate::icons::Icons, image: &ImageView<'_>) -> u8 {
  icons
    .picture(image, (4, 4), 1.0)
    .and_then(|pixmap| pixmap.pixel(0, 0))
    .expect("drawn")
    .red()
}

/// Red for 50 ms, then green for 100 ms, for ever.
fn red_then_green(name: &str) -> Removed {
  file(
    name,
    &encode(
      &[patch((0, 0, 4, 4), 0, 5), patch((0, 0, 4, 4), 1, 10)],
      Some(Repeat::Infinite),
    ),
  )
}

#[test]
fn a_drawn_gif_is_due_at_its_delays_and_keeps_its_pace() {
  let gif = red_then_green("pace");
  let mut icons = icons();
  let image = view(7, &gif, true);
  let start = Instant::now();
  let ms = Duration::from_millis;
  assert_eq!(icons.next_frame(start), None, "nothing drawn yet");
  assert_eq!(red(&mut icons, &image), 255);
  assert_eq!(icons.next_frame(start), Some(start + ms(50)));
  assert_eq!(icons.advance(start + ms(49)), Vec::<u64>::new());
  assert_eq!(icons.advance(start + ms(50)), [7]);
  assert_eq!(icons.next_frame(start + ms(51)), None, "waits to be drawn");
  // Drawn 16 ms late, the next frame is still 100 ms after the last.
  assert_eq!(red(&mut icons, &image), 0);
  assert_eq!(icons.next_frame(start + ms(66)), Some(start + ms(150)));
  assert_eq!(icons.advance(start + ms(150)), [7]);
  assert_eq!(red(&mut icons, &image), 255, "loops");
  // Drawn after a long while, it counts from the draw.
  let late = start + ms(5000);
  assert_eq!(icons.next_frame(late), Some(late + ms(50)));
}

#[test]
fn a_gif_not_drawn_again_or_paused_waits_for_nothing() {
  let gif = red_then_green("waits");
  let mut icons = icons();
  let start = Instant::now();
  let ms = Duration::from_millis;
  red(&mut icons, &view(1, &gif, true));
  assert!(icons.next_frame(start).is_some());
  assert_eq!(icons.advance(start + ms(50)), [1]);
  // Hidden or scrolled away: never drawn again, so never due.
  assert_eq!(icons.next_frame(start + ms(60)), None);
  icons.end_frame();
  assert_eq!(icons.next_frame(start + ms(70)), None);

  // Paused, it stays on its frame and waits for nothing.
  assert_eq!(red(&mut icons, &view(1, &gif, false)), 0);
  assert_eq!(icons.next_frame(start + ms(80)), None);
  assert_eq!(icons.advance(start + ms(10_000)), Vec::<u64>::new());
  assert_eq!(red(&mut icons, &view(1, &gif, false)), 0);
  // Playing again, it goes on from there.
  assert_eq!(red(&mut icons, &view(1, &gif, true)), 0);
  let resumed = start + ms(20_000);
  assert_eq!(icons.next_frame(resumed), Some(resumed + ms(100)));
  // Paused while it waits, it waits no more.
  assert_eq!(red(&mut icons, &view(1, &gif, false)), 0);
  assert_eq!(icons.next_frame(resumed), None);
  assert_eq!(icons.advance(resumed + ms(100)), Vec::<u64>::new());
}

#[test]
fn a_gif_that_plays_once_stops_on_its_last_frame() {
  let gif = file(
    "once-playing",
    &encode(
      &[patch((0, 0, 4, 4), 0, 5), patch((0, 0, 4, 4), 1, 5)],
      None,
    ),
  );
  let mut icons = icons();
  let image = view(3, &gif, true);
  let start = Instant::now();
  let ms = Duration::from_millis;
  red(&mut icons, &image);
  icons.next_frame(start);
  assert_eq!(icons.advance(start + ms(50)), [3]);
  assert_eq!(red(&mut icons, &image), 0);
  icons.next_frame(start + ms(50));
  assert_eq!(icons.advance(start + ms(100)), Vec::<u64>::new());
  assert_eq!(red(&mut icons, &image), 0);
  assert_eq!(icons.next_frame(start + ms(100)), None, "done");
}

#[test]
fn two_images_of_one_gif_play_apart_and_a_new_file_starts_over() {
  let gif = red_then_green("apart");
  let other = file(
    "other",
    &encode(
      &[patch((0, 0, 4, 4), 2, 5), patch((0, 0, 4, 4), 0, 5)],
      Some(Repeat::Infinite),
    ),
  );
  let mut icons = icons();
  let start = Instant::now();
  let ms = Duration::from_millis;
  red(&mut icons, &view(1, &gif, true));
  icons.next_frame(start);
  assert_eq!(red(&mut icons, &view(2, &gif, true)), 255);
  assert_eq!(icons.advance(start + ms(50)), [1]);
  assert_eq!(red(&mut icons, &view(1, &gif, true)), 0);
  assert_eq!(red(&mut icons, &view(2, &gif, true)), 255, "its own frame");
  assert_eq!(icons.rasterized(), 1, "the frames are shared");
  // Image 1 now shows another file: its first frame, blue.
  let blue = icons
    .picture(&view(1, &other, true), (4, 4), 1.0)
    .and_then(|pixmap| pixmap.pixel(0, 0))
    .map(|pixel| pixel.blue());
  assert_eq!(blue, Some(255));
}

#[test]
fn a_playing_gif_keeps_its_frames_past_the_byte_budget() {
  let gif = red_then_green("budget");
  let mut icons = icons();
  let start = Instant::now();
  red(&mut icons, &view(1, &gif, true));
  icons.next_frame(start);
  let stripes = super::fixture("stripes.png");
  for side in [2000, 2001, 2002] {
    let image = view(0, &stripes, true);
    icons.picture(&image, (side, 2000), 1.0);
  }
  for _ in 0..20 {
    icons.end_frame();
  }
  assert_eq!(icons.cached(), 1, "only the waiting GIF's frames stay");
  assert_eq!(icons.rasterized(), 4);
  red(&mut icons, &view(1, &gif, true));
  assert_eq!(icons.rasterized(), 4, "not decoded again");
}

/// A window with the GIF at `path` in a 20 by 20 clip, pushed `down`
/// logical pixels, shown while `shown` says.
fn clipped(scene: &mut super::Scene, path: &Path, down: f32, shown: bool) -> crabshell_ui::Window {
  use crabshell_ui::{Column, Image, Spacer, Stack, Style};
  let gif = Image::new(path.to_owned())
    .width(10)
    .height(10)
    .visible(shown);
  let panel = scene.panel().child(
    Stack::new()
      .style(Style::new().width(20.0).height(20.0).clip(true))
      .child(
        Column::new()
          .child(Spacer::new().style(Style::new().height(down)))
          .child(gif),
      ),
  );
  scene.window(panel, 40.0, 40.0)
}

#[test]
fn only_a_gif_drawn_on_screen_waits_for_its_next_frame() {
  let gif = red_then_green("shown");
  let now = Instant::now();
  for (down, shown, waits) in [(0.0, true, true), (0.0, false, false), (25.0, true, false)] {
    let mut scene = super::Scene::new();
    let window = clipped(&mut scene, &gif, down, shown);
    scene.full(&window, 1.5);
    assert_eq!(
      scene.renderer.next_image_frame(now).is_some(),
      waits,
      "down {down}, shown {shown}"
    );
  }
}

#[test]
fn a_gif_frame_repaints_its_box_and_matches_a_full_draw() {
  let gif = red_then_green("repaint");
  let mut scene = super::Scene::new();
  let window = clipped(&mut scene, &gif, 4.0, true);
  let mut history = crate::DamageHistory::new();
  let mut buffer = super::Buffer::new(&window, 2.0);
  scene.draw(&window, &mut history, &mut buffer);
  let start = Instant::now();
  let due = scene.renderer.next_image_frame(start).expect("waits");
  let moved = scene.renderer.advance_images(due);
  assert_eq!(moved.len(), 1);
  assert!(window.repaint_image(moved[0]));
  let report = scene.draw(&window, &mut history, &mut buffer);
  // Only the image's box, at scale 2 with a pixel's margin.
  let image = super::node_at(window.view().root(), &[0, 0, 1]).rect();
  let x = (image.x * 2.0) as u32;
  let y = (image.y * 2.0) as u32;
  let box_ = crate::PixelRect::new(x, y.saturating_sub(1), 41, 22);
  assert_eq!(report, [box_], "the image is at {image:?}");
  let (middle_x, middle_y) = ((image.x + image.width / 2.0) as u32, (image.y + 5.0) as u32);
  assert_eq!(
    buffer.pixel(middle_x * 2, middle_y * 2),
    [0, 255, 0, 255],
    "the green frame"
  );
  assert!(buffer.pixels == scene.full(&window, 2.0).pixels);
  assert!(scene.renderer.next_image_frame(due).is_some());
}

#[test]
fn a_gif_keeps_its_frames_only_within_what_the_cache_has_left() {
  let gif = red_then_green("cache-budget");
  let stripes = super::fixture("stripes.png");
  // Each GIF frame at 500 by 500 is 1,000,000 bytes.
  let draw_gif = |icons: &mut crate::icons::Icons, id| {
    icons.picture(&view(id, &gif, true), (500, 500), 1.0);
  };
  let mut icons = icons();
  draw_gif(&mut icons, 1);
  assert_eq!(icons.frames_cached(), 2, "room for both frames");

  // Two pictures in use hold 32,000,000 of the 33,554,432 bytes, so the
  // GIF has room for one frame only.
  let mut icons = icons_with_pictures(&stripes);
  let logs = capture_logs(|| draw_gif(&mut icons, 1));
  assert_eq!(icons.frames_cached(), 3, "two pictures and one frame");
  assert!(logs.contains("shows its first"), "{logs}");
  let start = Instant::now();
  assert_eq!(
    icons.next_frame(start),
    None,
    "a still frame waits for nothing"
  );
}

/// A cache holding two 2000 by 2000 pictures, both used by this draw.
fn icons_with_pictures(path: &Path) -> crate::icons::Icons {
  let mut icons = icons();
  for side in [2000, 2001] {
    let image = view(0, path, true);
    icons.picture(&image, (side, 2000), 1.0);
  }
  icons
}

/// A 200 by 40 window with the GIF at `path` at its left, shown while
/// `shown` says, and `label` far to its right, in tiles of its own.
fn gif_and_label(
  scene: &mut super::Scene,
  path: &Path,
  shown: crabshell_reactive::Signal<bool>,
  label: crabshell_reactive::Signal<&'static str>,
) -> crabshell_ui::Window {
  use crabshell_ui::{Image, Row, Spacer, Style, Text};
  let gif = Image::new(path.to_owned())
    .width(10)
    .height(10)
    .visible(move || shown.get());
  let panel = scene.panel().child(
    Row::new()
      .child(gif)
      .child(Spacer::new().style(Style::new().width(150.0)))
      .child(Text::new(move || label.get().to_owned())),
  );
  scene.window(panel, 200.0, 40.0)
}

#[test]
fn a_gif_hidden_while_it_waits_wakes_nothing_but_one_still_shown_goes_on() {
  let gif = red_then_green("hidden-waiting");
  let now = Instant::now();
  let mut scene = super::Scene::new();
  let shown = scene.cx.signal(true);
  let label = scene.cx.signal("a");
  let window = gif_and_label(&mut scene, &gif, shown, label);
  let mut history = crate::DamageHistory::new();
  let mut buffer = super::Buffer::new(&window, 1.0);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(scene.renderer.next_image_frame(now).is_some(), "it waits");

  // Another window drawing, or this one repainting only the label, leaves
  // the GIF waiting: it is still on screen.
  let other_shown = scene.cx.signal(true);
  let other_label = scene.cx.signal("b");
  let other = gif_and_label(&mut scene, &gif, other_shown, other_label);
  let mut other_history = crate::DamageHistory::new();
  let mut other_buffer = super::Buffer::new(&other, 1.0);
  scene.draw(&other, &mut other_history, &mut other_buffer);
  label.set("ab");
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);
  assert!(
    scene.renderer.next_image_frame(now).is_some(),
    "still shown, it still waits"
  );

  // Hidden, it is no longer drawn where it was, so it waits for nothing,
  // while the other window's goes on.
  shown.set(false);
  scene.frame(&window);
  scene.draw(&window, &mut history, &mut buffer);
  let due = scene
    .renderer
    .next_image_frame(now)
    .expect("the other waits");
  assert_eq!(
    scene.renderer.advance_images(due).len(),
    1,
    "only one moves"
  );
  other_shown.set(false);
  scene.frame(&other);
  scene.draw(&other, &mut other_history, &mut other_buffer);
  assert_eq!(scene.renderer.next_image_frame(due), None, "both hidden");
  assert_eq!(
    scene.renderer.advance_images(due + Duration::from_secs(1)),
    Vec::<u64>::new()
  );
}

#[test]
fn gifs_decoded_off_the_thread_together_keep_their_frames_within_the_budget() {
  let first = red_then_green("together-a");
  let second = file(
    "together-b",
    &encode(
      &[patch((0, 0, 4, 4), 2, 5), patch((0, 0, 4, 4), 0, 5)],
      Some(Repeat::Infinite),
    ),
  );
  let mut icons = icons();
  icons.decode_off_thread();
  // Each frame at 1250 by 2000 is 10,000,000 bytes, so both GIFs' four
  // frames are past the 33,554,432 the cache holds.
  for (id, gif) in [(1, &first), (2, &second)] {
    assert!(
      icons
        .picture(&view(id, gif, true), (1250, 2000), 1.0)
        .is_none()
    );
  }
  let logs = capture_logs(|| {
    while icons.decoding() {
      let (sender, woken) = std::sync::mpsc::channel();
      icons.wake_when_decoded(Box::new(move || {
        let _ = sender.send(());
      }));
      // A debug build takes 20 s over both idle, and minutes when busy.
      woken
        .recv_timeout(Duration::from_secs(300))
        .expect("the worker wakes the loop");
      icons.take_decoded();
    }
  });
  assert_eq!(
    icons.frames_cached(),
    3,
    "both frames of one, the first of the other"
  );
  assert!(logs.contains("shows its first"), "{logs}");
}
