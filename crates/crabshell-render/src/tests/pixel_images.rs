//! `Image::from_source`: raw pixels and icon names drawn with an image's
//! fit, status and cache, under an icon's limits on pixels.

use std::rc::Rc;

use crabshell_reactive::Signal;
use crabshell_ui::{
  Align, Fit, IconPixels, IconSource, Image, ImageStatus, IntoProp, Row, Style, Window,
};

use super::{Removed, Scene, capture_logs};
use crate::Renderer;

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// 16 by 8 ARGB pixels in columns 4 wide: green, red, blue, green.
fn columns() -> IconSource {
  let [green, red, blue] = [[255, 0, 255, 0], [255, 255, 0, 0], [255, 0, 0, 255]];
  let row = [green, red, blue, green]
    .map(|color| color.repeat(4))
    .concat();
  IconSource::Pixels(IconPixels {
    width: 16,
    height: 8,
    data: Rc::from(row.repeat(8)),
  })
}

/// A 20 by 20 image of `source`, cover-cropped, in a box with round
/// corners of 10 that clips it.
fn tile(
  scene: &mut Scene,
  source: impl IntoProp<Option<IconSource>>,
  status: Signal<ImageStatus>,
) -> Window {
  let image = Image::from_source(source)
    .width(20)
    .height(20)
    .fit(Fit::Cover)
    .status(status);
  let clip = Style::new().radius(10.0).clip(true);
  let row = Row::new()
    .align(Align::Start)
    .child(Row::new().style(clip).child(image));
  scene.window(scene.panel().child(row), 20.0, 20.0)
}

fn assert_near(got: [u8; 4], expected: [u8; 4], what: &str) {
  assert!(
    got.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 8),
    "{what}: {got:?}, not {expected:?}"
  );
}

#[test]
fn raw_pixels_cover_crop_in_a_round_clipped_box_at_scales_1_and_2() {
  let mut scene = Scene::new();
  let status = scene.cx.signal(ImageStatus::Empty);
  let window = tile(&mut scene, columns(), status);
  assert_eq!(status.get(), ImageStatus::Loading);
  for scale in [1.0, 2.0] {
    let buffer = scene.full(&window, scale);
    let at = |x: f32, y: f32| buffer.pixel((x * scale) as u32, (y * scale) as u32);
    // Scaled to 40 by 20 to cover: the green columns are cut off and the
    // middle two fill half the box each.
    assert_near(at(5.0, 10.0), RED, "the left half");
    assert_near(at(15.0, 10.0), BLUE, "the right half");
    assert_eq!(at(0.0, 0.0)[3], 0, "cut by the round corner at {scale}");
    assert_eq!(at(19.5, 19.5)[3], 0, "cut by the round corner at {scale}");
  }
  assert_eq!(status.get(), ImageStatus::Ready);
}

#[test]
fn no_source_is_empty() {
  let mut scene = Scene::new();
  let status = scene.cx.signal(ImageStatus::Loading);
  let window = tile(&mut scene, None, status);
  let mut buffer = None;
  let logs = capture_logs(|| buffer = Some(scene.full(&window, 1.0)));
  let buffer = buffer.expect("drawn");
  assert_eq!(logs, "");
  assert_eq!(status.get(), ImageStatus::Empty);
  assert!(buffer.pixels.iter().all(|&byte| byte == 0), "nothing drawn");
}

#[test]
fn bad_pixels_fail_and_are_logged_once() {
  let bad = [
    (4, 4, vec![0; 5], "4x4 pixels need 64 bytes, got 5"),
    (
      513,
      1,
      vec![0; 513 * 4],
      "513x1 pixels are larger than 512 a side",
    ),
  ];
  for (width, height, data, error) in bad {
    let mut scene = Scene::new();
    let status = scene.cx.signal(ImageStatus::Empty);
    let source = IconSource::Pixels(IconPixels {
      width,
      height,
      data: Rc::from(data),
    });
    let window = tile(&mut scene, source, status);
    let logs = capture_logs(|| {
      let first = scene.full(&window, 1.0);
      scene.full(&window, 1.0);
      assert!(first.pixels.iter().all(|&byte| byte == 0), "nothing drawn");
    });
    assert_eq!(status.get(), ImageStatus::Failed, "{error}");
    assert_eq!(logs.matches("couldn't draw").count(), 1, "{logs}");
    assert!(logs.contains(error), "{logs}");
  }
}

#[test]
fn the_same_pixels_again_reuse_the_cached_picture() {
  let mut scene = Scene::new();
  let pixels = columns();
  let source = scene.cx.signal(Some(pixels.clone()));
  let image = |source: Prop| {
    Image::from_source(source)
      .width(20)
      .height(20)
      .fit(Fit::Cover)
  };
  let row = Row::new()
    .align(Align::Start)
    .child(image(Box::new(move || source.get())))
    .child(image(Box::new(move || Some(pixels.clone()))));
  let window = scene.window(scene.panel().child(row), 40.0, 20.0);
  scene.full(&window, 1.0);
  assert_eq!(
    scene.renderer.rasterized_icons(),
    1,
    "two images, one picture"
  );
  let shown = source.get_untracked();
  source.set(None);
  scene.frame(&window);
  scene.full(&window, 1.0);
  let before = scene.renderer.rasterized_icons();
  source.set(shown);
  scene.frame(&window);
  scene.full(&window, 1.0);
  assert_eq!(
    scene.renderer.rasterized_icons(),
    before,
    "the same pixels came back"
  );
  // Equal bytes in another allocation are the same source, so the image
  // keeps the pixels it shows.
  source.set(Some(columns()));
  scene.frame(&window);
  scene.full(&window, 1.0);
  assert_eq!(scene.renderer.rasterized_icons(), before);
  let IconSource::Pixels(mut other) = columns() else {
    unreachable!("columns are pixels");
  };
  other.data = Rc::from(other.data.iter().map(|byte| !byte).collect::<Vec<u8>>());
  source.set(Some(IconSource::Pixels(other)));
  scene.frame(&window);
  scene.full(&window, 1.0);
  assert_eq!(scene.renderer.rasterized_icons(), before + 1);
}

type Prop = Box<dyn FnMut() -> Option<IconSource>>;

#[test]
fn an_icon_name_is_looked_up_and_drawn_as_its_file() {
  let dir =
    Removed(std::env::temp_dir().join(format!("crabshell-pixel-images-{}", std::process::id())));
  std::fs::create_dir_all(&*dir).expect("a directory for the icon");
  let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>"#;
  std::fs::write(dir.join("picture.svg"), svg).expect("write the icon");
  let mut scene = Scene::new();
  scene.renderer = Renderer::with_icon_dirs(vec![dir.to_path_buf()]);
  let name = scene.cx.signal(String::from("picture"));
  let status = scene.cx.signal(ImageStatus::Empty);
  let window = tile(
    &mut scene,
    move || Some(IconSource::Name(name.get())),
    status,
  );
  let buffer = scene.full(&window, 2.0);
  assert_near(buffer.pixel(20, 20), RED, "the middle");
  assert_eq!(status.get(), ImageStatus::Ready);
  name.set(String::from("nowhere"));
  scene.frame(&window);
  let logs = capture_logs(|| {
    scene.full(&window, 2.0);
  });
  assert_eq!(status.get(), ImageStatus::Failed);
  assert!(
    logs.contains(r#"no icon name "nowhere" in the theme"#),
    "{logs}"
  );
}

#[test]
fn a_transition_leaves_one_set_of_pixels_for_another() {
  let mut icons = crate::icons::Icons::with_themes(crate::theme::Themes::new(Vec::new()));
  let (from, to) = (
    columns(),
    IconSource::Pixels(IconPixels {
      width: 1,
      height: 1,
      data: Rc::from(&[255, 0, 0, 255][..]),
    }),
  );
  let view = |leaving| crabshell_ui::ImageView {
    id: 1,
    source: &to,
    rect: crabshell_ui::Rect::new(0.0, 0.0, 4.0, 4.0),
    fit: Fit::Cover,
    filters: crabshell_ui::ImageFilters::default(),
    playing: true,
    transition: crabshell_ui::ImageTransition::crossfade(crabshell_ui::Transition::new(100)),
    leaving,
  };
  icons.image(&view(None), (4, 4), 1.0);
  icons.end_frame();
  let drawn = icons.image(&view(Some((&from, 0.5))), (4, 4), 1.0);
  assert!(
    matches!(
      drawn,
      Some(crate::icons::Drawn::Two {
        from: Some(_),
        t: 0.5,
        ..
      })
    ),
    "both pictures drawn"
  );
}
