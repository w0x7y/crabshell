//! Image transitions in the picture cache: the old picture kept for a
//! source that can't be loaded, both pictures through the same fit and
//! filters, and the picture left dropped as soon as a transition ends.

use std::path::{Path, PathBuf};

use crabshell_ui::{Easing, Fit, ImageFilters, ImageTransition, ImageView, Rect, Transition};

use super::{Removed, capture_logs, write_png};
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
