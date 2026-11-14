//! Quantizing: an image's main colors by median cut.

use std::path::{Path, PathBuf};

use crabshell_ui::Color;

use super::{fixture, write_png};
use crate::quantize_colors;

const DARK_RED: [u8; 4] = [200, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// A `width` pixels wide PNG of `pixels`, straight RGBA, in the temp dir.
fn png(name: &str, width: u32, pixels: &[[u8; 4]]) -> PathBuf {
  let path = std::env::temp_dir().join(format!(
    "crabshell-colors-{}-{name}.png",
    std::process::id()
  ));
  let height = pixels.len() as u32 / width;
  write_png(&path, pixels.as_flattened(), width, height);
  path
}

fn colors(path: &Path, depth: u8, size: u32) -> Vec<Color> {
  quantize_colors(path, depth, size).unwrap_or_else(|error| panic!("{error}"))
}

fn rgb([r, g, b, _]: [u8; 4]) -> Color {
  Color::rgb(r, g, b)
}

#[test]
fn two_colors_give_those_two_at_depth_1_most_first() {
  // The cut puts dark red, low on blue, first; the counts decide.
  let more_blue = png("more-blue", 4, &[DARK_RED, BLUE, BLUE, BLUE]);
  assert_eq!(colors(&more_blue, 1, 0), [rgb(BLUE), rgb(DARK_RED)]);
  // Blue's bytes come first; the counts still decide.
  let more_red = png("more-red", 4, &[DARK_RED, BLUE, DARK_RED, DARK_RED]);
  assert_eq!(colors(&more_red, 1, 0), [rgb(DARK_RED), rgb(BLUE)]);
}

#[test]
fn colors_with_as_many_pixels_go_by_their_bytes() {
  let even = png("even", 2, &[DARK_RED, BLUE, BLUE, DARK_RED]);
  assert_eq!(colors(&even, 1, 0), [rgb(BLUE), rgb(DARK_RED)]);
}

#[test]
fn depth_0_gives_the_mean() {
  let two = png("mean", 2, &[[255, 0, 0, 255], BLUE]);
  assert_eq!(colors(&two, 0, 0), [Color::rgb(128, 0, 128)]);
}

#[test]
fn depth_is_clamped_to_8() {
  // 512 grays and greens: more distinct colors than 256 boxes.
  let pixels: Vec<[u8; 4]> = (0..512_u32)
    .map(|n| [n as u8, (n / 2) as u8, 0, 255])
    .collect();
  let many = png("many", 32, &pixels);
  let deepest = colors(&many, 8, 0);
  assert_eq!(deepest.len(), 256);
  assert_eq!(colors(&many, 9, 0), deepest);
  assert_eq!(colors(&many, u8::MAX, 0), deepest);
}

#[test]
fn pixels_under_half_alpha_are_skipped() {
  let green = [0, 255, 0, 128];
  let faint_blue = [0, 0, 255, 127];
  let clear = [0, 0, 255, 0];
  let pixels = [
    DARK_RED, DARK_RED, green, faint_blue, faint_blue, faint_blue, clear, clear,
  ];
  let faint = png("faint", 4, &pixels);
  assert_eq!(colors(&faint, 1, 0), [rgb(DARK_RED), Color::rgb(0, 255, 0)]);
  let invisible = png("invisible", 2, &[clear, clear]);
  assert!(colors(&invisible, 3, 0).is_empty());
}

#[test]
fn fewer_distinct_colors_than_boxes_gives_each_once() {
  let white = [255, 255, 255, 255];
  let pixels = [BLUE, BLUE, white, DARK_RED, DARK_RED, DARK_RED];
  let three = png("three", 3, &pixels);
  assert_eq!(colors(&three, 3, 0), [rgb(DARK_RED), rgb(BLUE), rgb(white)]);
}

#[test]
fn shrinking_keeps_big_areas_and_averages_fine_detail() {
  // 64 by 32: red on the left, blue on the right, and one white pixel in
  // the top-left corner.
  let red = [255, 0, 0, 255];
  let mut pixels = Vec::new();
  for _ in 0..32 {
    pixels.extend([red; 32]);
    pixels.extend([BLUE; 32]);
  }
  pixels[0] = [255, 255, 255, 255];
  let halves = png("halves", 64, &pixels);
  let whole = [rgb(BLUE), rgb(red), Color::rgb(255, 255, 255)];
  // No scaling, and none for a size the image already fits.
  assert_eq!(colors(&halves, 2, 0), whole);
  assert_eq!(colors(&halves, 2, 64), whole);
  // Halved twice, to 16 by 8: the white pixel blends into its 4 by 4
  // block, and the halves stay as they were.
  assert_eq!(
    colors(&halves, 2, 16),
    [rgb(BLUE), rgb(red), Color::rgb(255, 16, 16)]
  );
}

/// An SVG file in the temp dir.
fn svg(name: &str, text: &str) -> PathBuf {
  let path = std::env::temp_dir().join(format!(
    "crabshell-colors-{}-{name}.svg",
    std::process::id()
  ));
  std::fs::write(&path, text).expect("write the SVG");
  path
}

#[test]
fn an_svg_is_drawn_at_its_own_size_or_smaller() {
  // Dark red on the left, blue on the right, and a white pixel at the
  // top-left corner.
  let path = svg(
    "halves",
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
      <rect width="10" height="20" fill="#c80000"/>
      <rect x="10" width="30" height="20" fill="#0000ff"/>
      <rect width="1" height="1" fill="#ffffff"/>
    </svg>"##,
  );
  let white = Color::rgb(255, 255, 255);
  assert_eq!(colors(&path, 2, 0), [rgb(BLUE), rgb(DARK_RED), white]);
  // Drawn at 20 by 10, where the white pixel covers a quarter of one.
  let small = colors(&path, 2, 20);
  assert_eq!(small[..2], [rgb(BLUE), rgb(DARK_RED)]);
  assert_eq!(small.len(), 3);
  assert_ne!(small[2], white);
}

#[test]
fn a_missing_file_or_an_image_past_the_caps_is_an_error() {
  let error = quantize_colors(&fixture("missing.png"), 1, 0).expect_err("missing");
  assert!(error.contains("No such file"), "{error}");
  for name in ["too-wide.png", "too-wide.jpg"] {
    let error = quantize_colors(&fixture(name), 1, 64).expect_err(name);
    assert!(error.contains("9000"), "{name}: {error}");
  }
  let error = quantize_colors(&fixture("too-many.webp"), 1, 64).expect_err("too many");
  assert!(error.contains("5000x5000"), "{error}");
  // An SVG drawn at its own size is held to the same caps.
  let wide = svg(
    "wide",
    r#"<svg xmlns="http://www.w3.org/2000/svg" width="9000" height="10"/>"#,
  );
  let error = quantize_colors(&wide, 1, 0).expect_err("a wide SVG");
  assert!(error.contains("9000"), "{error}");
  let error = quantize_colors(&fixture("shapes.bmp"), 1, 0).expect_err("bmp");
  assert!(error.contains(".png, .jpg"), "{error}");
}
