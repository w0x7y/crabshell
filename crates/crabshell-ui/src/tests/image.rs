//! An image's filters and playback, and an icon's tint and fallback: what the view
//! hands the renderer, what a change repaints, and the platform's repaint
//! of a GIF's next frame.

use super::{Harness, capture_logs, node_at};
use crate::{Color, Content, Icon, IconSource, Image, ImageFilters, Row, Stack, Style, Window};

/// The filters, id and playing flag of the image at `path`.
fn image_at(window: &Window, path: &[usize]) -> (ImageFilters, u64, bool) {
  let view = window.view();
  match node_at(view.root(), path).content() {
    Content::Image(image) => (image.filters, image.id, image.playing),
    _ => panic!("no image at {path:?}"),
  }
}

fn tint_at(window: &Window, path: &[usize]) -> Option<Color> {
  let view = window.view();
  match node_at(view.root(), path).content() {
    Content::Icon(icon) => icon.tint,
    _ => panic!("no icon at {path:?}"),
  }
}

#[test]
fn an_image_draws_unfiltered_and_playing_by_default() {
  let mut h = Harness::new();
  let window = h.window(h.panel().child(Image::new("a.png")), 100.0, 64.0);
  let (filters, _, playing) = image_at(&window, &[0]);
  assert_eq!(filters, ImageFilters::default());
  assert_eq!(
    (
      filters.tint,
      filters.brightness,
      filters.contrast,
      filters.saturation,
      filters.blur
    ),
    (None, 1.0, 1.0, 1.0, 0.0)
  );
  assert!(playing);
}

#[test]
fn filter_values_reach_the_view_and_ones_that_arent_numbers_count_as_unchanged() {
  let mut h = Harness::new();
  let red = Color::rgb(255, 0, 0);
  let blur = h.cx.signal(3.0_f32);
  let image = |brightness, contrast, saturation, blur| {
    Image::new("a.png")
      .tint(red)
      .brightness(brightness)
      .contrast(contrast)
      .saturation(saturation)
      .blur(blur)
      .playing(false)
  };
  let row = Row::new()
    .child(image(0.5, 2.0, 0.0, 4.0))
    .child(image(f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::NAN))
    .child(image(-1.0, -2.0, -3.0, -4.0))
    .child(Image::new("a.png").blur(move || blur.get()));
  let window = h.window(h.panel().child(row), 400.0, 64.0);
  let (filters, _, playing) = image_at(&window, &[0, 0]);
  assert_eq!(
    filters,
    ImageFilters {
      tint: Some(red),
      brightness: 0.5,
      contrast: 2.0,
      saturation: 0.0,
      blur: 4.0,
    }
  );
  assert!(!playing);
  let (filters, _, _) = image_at(&window, &[0, 1]);
  assert_eq!(
    filters,
    ImageFilters {
      tint: Some(red),
      ..ImageFilters::default()
    },
    "not finite counts as unchanged"
  );
  let (filters, _, _) = image_at(&window, &[0, 2]);
  assert_eq!(
    (
      filters.brightness,
      filters.contrast,
      filters.saturation,
      filters.blur
    ),
    (0.0, 0.0, 0.0, 0.0),
    "below 0 counts as 0"
  );
  // -0 equals 0, so it is only stored over another value.
  blur.set(-0.0);
  h.frame(&window);
  let (filters, _, _) = image_at(&window, &[0, 3]);
  assert!(
    filters.blur.is_sign_positive(),
    "never -0, for the cache key"
  );
}

#[test]
fn each_image_has_an_id_of_its_own() {
  let mut h = Harness::new();
  let row = Row::new()
    .child(Image::new("a.png"))
    .child(Image::new("a.png"));
  let window = h.window(h.panel().child(row), 200.0, 64.0);
  assert_ne!(image_at(&window, &[0, 0]).1, image_at(&window, &[0, 1]).1);
}

#[test]
fn an_icon_takes_a_tint_and_none_by_default() {
  let mut h = Harness::new();
  let blue = Color::rgb(0, 0, 255);
  let row = Row::new()
    .child(Icon::new("a"))
    .child(Icon::new("b").tint(blue))
    .child(Icon::new("c").tint(None));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(tint_at(&window, &[0, 0]), None);
  assert_eq!(tint_at(&window, &[0, 1]), Some(blue));
  assert_eq!(tint_at(&window, &[0, 2]), None);
}

#[test]
fn a_filter_or_tint_change_repaints_only_that_widget() {
  let mut h = Harness::new();
  let blur = h.cx.signal(0.0_f32);
  let tint = h.cx.signal(None::<Color>);
  let playing = h.cx.signal(true);
  let row = Row::new()
    .child(
      Image::new("a.png")
        .width(20)
        .height(20)
        .blur(move || blur.get()),
    )
    .child(
      Image::new("a.png")
        .width(20)
        .height(20)
        .playing(move || playing.get()),
    )
    .child(Icon::new("a").size(10).tint(move || tint.get()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let image = super::rect_at(&window, &[0, 0]);
  let paused = super::rect_at(&window, &[0, 1]);
  let icon = super::rect_at(&window, &[0, 2]);
  window.take_damage();

  blur.set(3.0);
  h.runtime.flush();
  assert!(!window.needs_layout());
  assert_eq!(window.take_damage(), vec![image]);

  playing.set(false);
  h.runtime.flush();
  assert!(!window.needs_layout());
  assert_eq!(window.take_damage(), vec![paused]);

  tint.set(Some(Color::rgb(1, 2, 3)));
  h.runtime.flush();
  assert!(!window.needs_layout());
  assert_eq!(window.take_damage(), vec![icon]);

  // The same value again changes nothing.
  blur.set(3.0);
  tint.set(Some(Color::rgb(1, 2, 3)));
  h.frame(&window);
  assert!(!window.has_damage());
}

#[test]
fn a_panicking_filter_is_logged_with_its_path_and_the_rest_keeps_working() {
  let mut h = Harness::new();
  let fail = h.cx.signal(false);
  let tint = h.cx.signal(None::<Color>);
  let row = Row::new()
    .child(Image::new("a.png").blur(move || {
      assert!(!fail.get(), "blur failure");
      1.0
    }))
    .child(Icon::new("a").tint(move || tint.get()));
  let window = h.window(h.panel().child(row), 200.0, 64.0);
  let logs = capture_logs(|| {
    fail.set(true);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect PanelWindow[DP-1]/Row/Image[0].blur panicked and is disabled"),
    "{logs}"
  );
  assert_eq!(
    image_at(&window, &[0, 0]).0.blur,
    1.0,
    "keeps its last value"
  );
  tint.set(Some(Color::rgb(9, 9, 9)));
  h.frame(&window);
  assert_eq!(tint_at(&window, &[0, 1]), Some(Color::rgb(9, 9, 9)));
}

#[test]
fn a_panicking_tint_is_logged_with_its_path() {
  let mut h = Harness::new();
  let fail = h.cx.signal(false);
  let icon = Icon::new("a").tint(move || {
    assert!(!fail.get(), "tint failure");
    None
  });
  let window = h.window(h.panel().child(Row::new().child(icon)), 200.0, 30.0);
  let logs = capture_logs(|| {
    fail.set(true);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect PanelWindow[DP-1]/Row/Icon[0].tint panicked and is disabled"),
    "{logs}"
  );
}

#[test]
fn the_platform_repaints_one_image_by_its_id() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let row = Row::new()
    .child(Image::new("a.png").width(20).height(20))
    .child(
      Image::new("b.png")
        .width(20)
        .height(20)
        .visible(move || shown.get()),
    )
    .child(Stack::new().style(Style::new().background(Color::rgb(1, 1, 1))));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let (_, first, _) = image_at(&window, &[0, 0]);
  let (_, second, _) = image_at(&window, &[0, 1]);
  let rect = super::rect_at(&window, &[0, 0]);
  window.take_damage();

  assert!(window.repaint_image(first));
  assert_eq!(window.take_damage(), vec![rect]);
  assert!(!window.needs_layout());

  // Another window's image, a hidden one or one removed repaints nothing.
  assert!(!window.repaint_image(u64::MAX));
  shown.set(false);
  h.frame(&window);
  window.take_damage();
  assert!(!window.repaint_image(second));
  assert!(!window.has_damage());
}

fn fallback_at(window: &Window, path: &[usize]) -> Option<IconSource> {
  let view = window.view();
  match node_at(view.root(), path).content() {
    Content::Icon(icon) => icon.fallback.cloned(),
    _ => panic!("no icon at {path:?}"),
  }
}

#[test]
fn an_icons_fallback_reaches_the_view_and_a_change_repaints_only_it() {
  let mut h = Harness::new();
  let fallback = h.cx.signal(String::from("b"));
  let row = Row::new().child(Icon::new("a")).child(
    Icon::new("a")
      .size(10)
      .fallback(move || IconSource::Name(fallback.get())),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(fallback_at(&window, &[0, 0]), None);
  assert_eq!(
    fallback_at(&window, &[0, 1]),
    Some(IconSource::Name("b".into()))
  );
  let icon = super::rect_at(&window, &[0, 1]);
  window.take_damage();
  fallback.set("c".into());
  h.runtime.flush();
  assert!(!window.needs_layout());
  assert_eq!(window.take_damage(), vec![icon]);
  assert_eq!(
    fallback_at(&window, &[0, 1]),
    Some(IconSource::Name("c".into()))
  );
}

#[test]
fn a_panicking_fallback_is_logged_with_its_path() {
  let mut h = Harness::new();
  let fail = h.cx.signal(false);
  let icon = Icon::new("a").fallback(move || {
    assert!(!fail.get(), "fallback failure");
    IconSource::Name("b".into())
  });
  let window = h.window(h.panel().child(Row::new().child(icon)), 200.0, 30.0);
  let logs = capture_logs(|| {
    fail.set(true);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect PanelWindow[DP-1]/Row/Icon[0].fallback panicked and is disabled"),
    "{logs}"
  );
  assert_eq!(
    fallback_at(&window, &[0, 0]),
    Some(IconSource::Name("b".into()))
  );
}
