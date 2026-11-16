//! Image files: each format, the fit modes and the caps.

use crabshell_ui::{Fit, ImageFilters, ImageView};
use tiny_skia::Pixmap;

use super::fixture;
use crate::images::{filtered, scale};

/// The file at `path` as a `width` by `height` pixmap, unfiltered.
fn rasterize(path: &std::path::Path, width: u32, height: u32, fit: Fit) -> Result<Pixmap, String> {
  filtered(path, (width, height), fit, &ImageFilters::default(), 1.0)
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// `stripes.*` is 40 by 20 pixels: red, green and blue stripes 10, 20 and
/// 10 pixels wide.
fn load(name: &str, width: u32, height: u32, fit: Fit) -> Pixmap {
  rasterize(&fixture(name), width, height, fit).unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// Checks the pixel at `x`, `y` within 8/255 per channel, since JPEG is
/// lossy and scaling blends near edges.
fn assert_pixel(pixmap: &Pixmap, (x, y): (u32, u32), expected: [u8; 4], what: &str) {
  let pixel = pixmap.pixel(x, y).expect("the pixel is inside");
  let got = [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()];
  assert!(
    got.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 8),
    "{what}: pixel {x},{y} is {got:?}, not {expected:?}"
  );
}

#[test]
fn every_format_decodes_to_the_same_picture() {
  for name in ["stripes.png", "stripes.jpg", "stripes.gif", "stripes.webp"] {
    let image = load(name, 40, 20, Fit::Fill);
    assert_pixel(&image, (4, 10), RED, name);
    assert_pixel(&image, (20, 10), GREEN, name);
    assert_pixel(&image, (35, 10), BLUE, name);
  }
}

#[test]
fn fill_stretches_contain_letterboxes_and_cover_crops() {
  let fill = load("stripes.png", 20, 20, Fit::Fill);
  assert_pixel(&fill, (1, 10), RED, "fill");
  assert_pixel(&fill, (10, 0), GREEN, "fill");
  assert_pixel(&fill, (18, 19), BLUE, "fill");

  // 20 by 10, in rows 5 to 15.
  let contain = load("stripes.png", 20, 20, Fit::Contain);
  assert_pixel(&contain, (10, 2), CLEAR, "contain");
  assert_pixel(&contain, (10, 17), CLEAR, "contain");
  assert_pixel(&contain, (1, 10), RED, "contain");
  assert_pixel(&contain, (10, 10), GREEN, "contain");

  // 40 by 20, so only the middle 20 columns show: all green.
  let cover = load("stripes.png", 20, 20, Fit::Cover);
  for x in [0, 10, 19] {
    assert_pixel(&cover, (x, 10), GREEN, "cover");
  }
}

#[test]
fn a_large_picture_shrinks_smoothly() {
  // Halved twice, then scaled: the stripes blend a little at their edges
  // but stay where they were.
  let small = load("stripes.png", 4, 2, Fit::Fill);
  let channels = |x, y| {
    let pixel = small.pixel(x, y).expect("the pixel is inside");
    [pixel.red(), pixel.green(), pixel.blue()]
  };
  let [red, _, blue] = channels(0, 1);
  assert!(red > 200 && blue < 50, "left {:?}", channels(0, 1));
  let [red, _, blue] = channels(3, 0);
  assert!(blue > 200 && red < 50, "right {:?}", channels(3, 0));
  let [_, green, _] = channels(1, 1);
  assert!(green > 200, "middle {:?}", channels(1, 1));
}

#[test]
fn a_gif_frame_covering_part_of_its_canvas_lands_where_it_says() {
  // A 10 by 10 red frame at 5, 5 on a 40 by 20 canvas.
  let image = load("partial.gif", 40, 20, Fit::Fill);
  assert_pixel(&image, (10, 10), RED, "partial");
  assert_pixel(&image, (2, 2), CLEAR, "partial");
  assert_pixel(&image, (30, 10), CLEAR, "partial");
}

#[test]
fn an_svg_is_drawn_at_the_size_asked() {
  let image = load("shapes.svg", 40, 20, Fit::Contain);
  assert_eq!((image.width(), image.height()), (40, 20));
  assert!(image.pixels().iter().any(|pixel| pixel.alpha() > 0));
}

#[test]
fn oversized_missing_and_unknown_files_are_refused() {
  for name in [
    "too-wide.png",
    "too-wide.jpg",
    "too-wide.gif",
    "too-wide.webp",
  ] {
    let error = rasterize(&fixture(name), 10, 10, Fit::Fill).expect_err(name);
    assert!(error.contains("9000"), "{name}: {error}");
  }
  // 5000 by 5000: each side fits, but 25M pixels is past the cap.
  for name in [
    "too-many.png",
    "too-many.jpg",
    "too-many.gif",
    "too-many.webp",
  ] {
    let error = rasterize(&fixture(name), 10, 10, Fit::Fill).expect_err(name);
    assert!(error.contains("5000x5000"), "{name}: {error}");
  }
  assert!(rasterize(&fixture("missing.png"), 10, 10, Fit::Fill).is_err());
  let error = rasterize(&fixture("shapes.bmp"), 10, 10, Fit::Fill).expect_err("bmp");
  assert!(error.contains(".png, .jpg"), "{error}");
  assert!(rasterize(&fixture("stripes.png"), 0, 10, Fit::Fill).is_err());
}

#[test]
fn fine_detail_averages_out_instead_of_aliasing() {
  // One-pixel black and white columns.
  let mut columns = Pixmap::new(64, 4).expect("a pixmap");
  for (index, pixel) in columns
    .data_mut()
    .as_chunks_mut::<4>()
    .0
    .iter_mut()
    .enumerate()
  {
    let value = if index % 2 == 0 { 0 } else { 255 };
    *pixel = [value, value, value, 255];
  }
  let small = scale(columns.as_ref(), 3, 1, Fit::Fill).expect("scaled");
  for x in 0..3 {
    let gray = small.pixel(x, 0).expect("inside").red();
    assert!((100..=156).contains(&gray), "pixel {x} is {gray}, not gray");
  }
}

#[test]
fn a_gif_frame_with_no_pixels_is_refused() {
  // A 4 by 4 GIF whose one frame is interlaced and 0 by 0.
  let mut gif = b"GIF89a\x04\x00\x04\x00\x80\x00\x00".to_vec();
  gif.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
  gif.extend_from_slice(&[0x2c, 0, 0, 0, 0, 0, 0, 0, 0, 0x40]);
  gif.extend_from_slice(&[2, 2, 0x44, 0x01, 0, 0x3b]);
  assert!(crate::images::gif(&gif).is_err());
}

#[test]
fn an_image_past_the_window_edge_is_cut_not_squashed() {
  // A 20 by 20 picture, red above blue, centered in a 10-pixel bar: rows
  // 5 to 15 show, so the top half of the bar is red.
  let mut scene = super::Scene::new();
  let panel = scene.panel().child(
    crabshell_ui::Row::new()
      .align(crabshell_ui::Align::Center)
      .child(
        crabshell_ui::Image::new(fixture("bands.png"))
          .width(20)
          .height(20),
      ),
  );
  let window = scene.window(panel, 40.0, 10.0);
  let buffer = scene.full(&window, 1.0);
  let pixel = |x: u32, y: u32| {
    let at = ((y * buffer.width + x) * 4) as usize;
    [buffer.pixels[at], buffer.pixels[at + 2]]
  };
  assert_eq!(pixel(5, 4), [255, 0], "row 4 is red");
  assert_eq!(pixel(5, 5), [0, 255], "row 5 is blue");
}

/// What an `Image` of `path` with `filters` hands the renderer.
fn view(path: &std::path::Path, fit: Fit, filters: ImageFilters) -> ImageView<'_> {
  ImageView {
    id: 0,
    path,
    rect: crabshell_ui::Rect::new(0.0, 0.0, 1.0, 1.0),
    fit,
    filters,
    playing: true,
    transition: crabshell_ui::ImageTransition::None,
    leaving: None,
  }
}

/// The image file at `path` from `icons`'s cache, unfiltered, at scale 1.
fn cached<'a>(
  icons: &'a mut crate::icons::Icons,
  path: &std::path::Path,
  size: (u32, u32),
  fit: Fit,
) -> Option<&'a Pixmap> {
  icons.image(&view(path, fit, ImageFilters::default()), size, 1.0)
}

fn icons() -> crate::icons::Icons {
  crate::icons::Icons::with_themes(crate::theme::Themes::new(Vec::new()))
}

/// A directory of its own for one test, removed first if a run left it.
fn scratch(name: &str) -> std::path::PathBuf {
  let dir = std::env::temp_dir().join(format!("crabshell-images-{}-{name}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).expect("a scratch directory");
  dir
}

#[test]
fn a_file_rewritten_or_appearing_later_is_read_again() {
  let dir = scratch("rewritten");
  let path = dir.join("cover.png");
  let mut icons = icons();
  let top_left = |icons: &mut crate::icons::Icons| {
    cached(icons, &path, (20, 20), Fit::Fill)
      .and_then(|pixmap| pixmap.pixel(0, 0))
      .map(|pixel| [pixel.red(), pixel.blue()])
  };
  // Missing at first: drawn as nothing.
  assert_eq!(top_left(&mut icons), None);
  std::fs::copy(fixture("stripes.png"), &path).expect("copy");
  assert_eq!(top_left(&mut icons), Some([255, 0]));
  // Rewritten in place with another picture, a moment later.
  std::fs::copy(fixture("bands.png"), &path).expect("copy");
  let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
  std::fs::File::options()
    .write(true)
    .open(&path)
    .and_then(|file| file.set_modified(later))
    .expect("set the time");
  let pixel = top_left(&mut icons).expect("drawn");
  assert_eq!(pixel, [255, 0]);
  let bottom = cached(&mut icons, &path, (20, 20), Fit::Fill)
    .and_then(|pixmap| pixmap.pixel(0, 19))
    .map(|pixel| pixel.blue());
  assert_eq!(bottom, Some(255), "the new picture, blue below");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cached_pictures_past_the_byte_budget_are_dropped_once_unused() {
  let mut icons = icons();
  let path = fixture("stripes.png");
  // 2000 by 2000 is 16 MB a picture; three are past the 32 MB budget.
  for side in [2000, 2001, 2002] {
    assert!(cached(&mut icons, &path, (side, 2000), Fit::Fill).is_some());
  }
  assert_eq!(icons.cached(), 3);
  for _ in 0..20 {
    cached(&mut icons, &path, (2002, 2000), Fit::Fill);
    icons.end_frame();
  }
  assert_eq!(icons.cached(), 1);
}
