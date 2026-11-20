//! Image filters: the tint, the color filters CSS defines, and the blur,
//! on pixels and through the picture cache.

use crabshell_ui::{Color, Fit, ImageFilters, ImageView, Rect};
use tiny_skia::Pixmap;

use super::fixture;
use crate::filters::{apply, blur};

const NONE: ImageFilters = ImageFilters {
  tint: None,
  brightness: 1.0,
  contrast: 1.0,
  saturation: 1.0,
  blur: 0.0,
};

/// One premultiplied pixel run through `filters`.
fn filtered(pixel: [u8; 4], filters: ImageFilters) -> [u8; 4] {
  let mut pixmap = Pixmap::new(1, 1).expect("a pixmap");
  pixmap.data_mut().copy_from_slice(&pixel);
  apply(&mut pixmap, &filters, 1.0);
  pixmap.data().try_into().expect("one pixel")
}

#[test]
fn a_tint_paints_every_pixel_and_keeps_its_alpha() {
  let red = ImageFilters {
    tint: Some(Color::rgb(255, 0, 0)),
    ..NONE
  };
  assert_eq!(filtered([20, 60, 100, 128], red), [128, 0, 0, 128]);
  assert_eq!(filtered([0, 0, 0, 0], red), [0, 0, 0, 0]);
  // A translucent tint scales the alpha too.
  let faint = ImageFilters {
    tint: Some(Color::rgba(0, 0, 255, 128)),
    ..NONE
  };
  assert_eq!(filtered([255, 255, 255, 255], faint), [0, 0, 128, 128]);
}

#[test]
fn brightness_contrast_and_saturation_follow_css() {
  let pixel = [200, 100, 50, 255];
  assert_eq!(filtered(pixel, NONE), pixel, "the defaults change nothing");
  let brightness = |value| ImageFilters {
    brightness: value,
    ..NONE
  };
  assert_eq!(filtered(pixel, brightness(0.0)), [0, 0, 0, 255]);
  assert_eq!(filtered(pixel, brightness(0.5)), [100, 50, 25, 255]);
  assert_eq!(filtered(pixel, brightness(2.0)), [255, 200, 100, 255]);
  let contrast = |value| ImageFilters {
    contrast: value,
    ..NONE
  };
  assert_eq!(filtered(pixel, contrast(0.0)), [128, 128, 128, 255]);
  assert_eq!(filtered(pixel, contrast(2.0)), [255, 73, 0, 255]);
  let saturation = |value| ImageFilters {
    saturation: value,
    ..NONE
  };
  // Gray is 0.213 R + 0.715 G + 0.072 B.
  assert_eq!(filtered(pixel, saturation(0.0)), [118, 118, 118, 255]);
  let vivid = filtered(pixel, saturation(2.0));
  assert!(
    vivid[0] == 255 && vivid[1] < 100 && vivid[2] < 50,
    "{vivid:?}"
  );
  // Premultiplied: a half-transparent pixel goes half as far.
  assert_eq!(
    filtered([100, 50, 25, 128], brightness(2.0)),
    [128, 100, 50, 128]
  );
  assert_eq!(
    filtered([100, 50, 26, 128], contrast(0.0)),
    [64, 64, 64, 128]
  );
}

#[test]
fn each_color_filter_clamps_before_the_next() {
  // Brightened past white, then gray: the clamp keeps it at white's gray.
  let both = ImageFilters {
    brightness: 4.0,
    saturation: 0.0,
    ..NONE
  };
  assert_eq!(filtered([200, 200, 0, 255], both), [237, 237, 237, 255]);
}

/// A `width` by `height` picture of one premultiplied color.
fn solid(width: usize, height: usize, pixel: [u8; 4]) -> Vec<u8> {
  pixel.repeat(width * height)
}

#[test]
fn a_blur_keeps_a_solid_picture_solid_to_its_edges() {
  let (width, height) = (30, 20);
  let mut data = solid(width, height, [40, 80, 120, 255]);
  blur(&mut data, width, height, 6.0);
  assert_eq!(
    data,
    solid(width, height, [40, 80, 120, 255]),
    "no dark rim"
  );
}

#[test]
fn a_blur_spreads_a_square_as_a_gaussian_of_its_deviation() {
  let (width, height) = (61, 61);
  // A 5 by 5 white square in the middle, bright enough that rounding
  // keeps the tails; its own spread adds 2 to the variance.
  let mut data = solid(width, height, [0, 0, 0, 0]);
  for y in 28..33 {
    for x in 28..33 {
      let at = (y * width + x) * 4;
      data[at..at + 4].copy_from_slice(&[255; 4]);
    }
  }
  let mut wide: Vec<u32> = Vec::new();
  blur(&mut data, width, height, 4.0);
  wide.extend(
    data
      .as_chunks::<4>()
      .0
      .iter()
      .map(|pixel| u32::from(pixel[3])),
  );
  let total: u32 = wide.iter().sum();
  assert!(total > 0, "something is left");
  // The spread along x: the variance of the alpha around the middle.
  let variance = wide
    .iter()
    .enumerate()
    .map(|(index, &alpha)| {
      let x = (index % width) as f64 - 30.0;
      x * x * f64::from(alpha)
    })
    .sum::<f64>()
    / f64::from(total);
  assert!(
    (14.0..=22.0).contains(&variance),
    "variance {variance}, not near 18"
  );
  // Symmetric about the middle.
  let at = |x: usize, y: usize| wide[y * width + x];
  assert_eq!(at(26, 30), at(34, 30));
  assert_eq!(at(30, 26), at(30, 34));
  assert!(at(30, 30) > at(33, 30) && at(33, 30) > at(37, 30));
}

#[test]
fn a_blur_of_nothing_or_of_an_empty_picture_does_nothing() {
  let mut data = vec![9, 8, 7, 255];
  blur(&mut data, 1, 1, 0.1);
  assert_eq!(data, [9, 8, 7, 255]);
  blur(&mut [], 0, 0, 5.0);
  let mut pixmap = Pixmap::new(2, 1).expect("a pixmap");
  pixmap
    .data_mut()
    .copy_from_slice(&[255, 0, 0, 255, 0, 0, 255, 255]);
  let before = pixmap.data().to_vec();
  apply(&mut pixmap, &NONE, 2.0);
  assert_eq!(pixmap.data(), before);
}

fn view(fit: Fit, filters: ImageFilters) -> ImageView<'static> {
  ImageView {
    id: 0,
    source: super::file_source(&fixture("stripes.png")),
    rect: Rect::new(0.0, 0.0, 1.0, 1.0),
    fit,
    filters,
    playing: true,
    transition: crabshell_ui::ImageTransition::None,
    leaving: None,
  }
}

#[test]
fn the_cache_keeps_each_filtered_copy_and_filters_once() {
  let mut icons = crate::icons::Icons::with_themes(crate::theme::Themes::new(Vec::new()));
  let dim = ImageFilters {
    brightness: 0.5,
    ..NONE
  };
  let plain = icons
    .picture(&view(Fit::Fill, NONE), (40, 20), 1.0)
    .and_then(|pixmap| pixmap.pixel(2, 10))
    .map(|pixel| pixel.red());
  let dimmed = icons
    .picture(&view(Fit::Fill, dim), (40, 20), 1.0)
    .and_then(|pixmap| pixmap.pixel(2, 10))
    .map(|pixel| pixel.red());
  assert_eq!((plain, dimmed), (Some(255), Some(128)));
  assert_eq!(icons.rasterized(), 2);
  for _ in 0..3 {
    icons.picture(&view(Fit::Fill, dim), (40, 20), 1.0);
    icons.end_frame();
  }
  assert_eq!(icons.rasterized(), 2, "a filtered copy is made once");
  // The same blur at another scale is another copy.
  let blurred = ImageFilters { blur: 2.0, ..NONE };
  icons.picture(&view(Fit::Fill, blurred), (40, 20), 1.0);
  icons.picture(&view(Fit::Fill, blurred), (40, 20), 2.0);
  assert_eq!(icons.rasterized(), 4);
}

#[test]
fn a_contained_blur_blurs_the_picture_alone_and_keeps_the_box_clear() {
  let mut icons = crate::icons::Icons::with_themes(crate::theme::Themes::new(Vec::new()));
  // The 40 by 20 stripes in a 40 by 40 box: rows 10 to 30.
  let blurred = ImageFilters { blur: 3.0, ..NONE };
  let pixmap = icons
    .picture(&view(Fit::Contain, blurred), (40, 40), 1.0)
    .expect("drawn")
    .clone();
  let alpha = |x, y| pixmap.pixel(x, y).expect("inside").alpha();
  assert_eq!(alpha(20, 5), 0, "above the picture stays clear");
  assert_eq!(alpha(20, 35), 0, "below too");
  assert_eq!(alpha(0, 10), 255, "the picture's corner stays opaque");
  assert_eq!(alpha(39, 29), 255);
  let left = pixmap.pixel(0, 20).expect("inside");
  assert!(left.red() > 200, "the red edge stays red: {left:?}");
}

#[test]
fn a_tinted_icon_is_cached_apart_from_the_plain_one() {
  let mut icons = crate::icons::Icons::with_themes(crate::theme::Themes::new(Vec::new()));
  let source = crabshell_ui::IconSource::File(fixture("shapes.svg"));
  let white = Some(Color::rgb(255, 255, 255));
  // Inside the dark circle, off the line across it.
  let middle = |pixmap: &Pixmap| {
    let pixel = pixmap.pixel(6, 6).expect("inside");
    [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
  };
  let plain = icons.get(&source, (16, 16, 1), None).map(middle);
  let tinted = icons.get(&source, (16, 16, 1), white).map(middle);
  assert_eq!(plain, Some([0x1e, 0x1e, 0x2e, 255]));
  assert_eq!(tinted, Some([255, 255, 255, 255]));
  let corner = icons
    .get(&source, (16, 16, 1), white)
    .and_then(|pixmap| pixmap.pixel(0, 0))
    .map(|pixel| pixel.alpha());
  assert_eq!(corner, Some(0), "clear stays clear");
  assert_eq!(icons.cached(), 2);
  assert_eq!(icons.rasterized(), 2);
}

/// How long blurring a 1920 by 1080 picture by 20 pixels takes, the cost
/// of a blurred wallpaper's first draw at scale 1. Run it in a release
/// build:
/// `cargo test --release -p crabshell-render --lib time_a_1080p_blur -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn time_a_1080p_blur() {
  let (width, height) = (1920, 1080);
  let picture: Vec<u8> = (0..width * height)
    .flat_map(|index| {
      let (x, y) = (index % width, index / width);
      [(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255]
    })
    .collect();
  let runs = 10;
  let mut total = std::time::Duration::ZERO;
  for sigma in [5.0, 20.0, 60.0] {
    for _ in 0..runs {
      let mut data = picture.clone();
      let start = std::time::Instant::now();
      blur(&mut data, width, height, sigma);
      total += start.elapsed();
    }
    println!(
      "blurring 1920x1080 by {sigma} pixels takes {:.1} ms on average over {runs} runs",
      total.as_secs_f64() * 1000.0 / f64::from(runs)
    );
    total = std::time::Duration::ZERO;
  }
}

#[test]
fn repainting_part_of_a_filtered_image_matches_drawing_everything() {
  let mut scene = super::Scene::new();
  let panel = scene.panel().child(
    crabshell_ui::Row::new()
      .padding(3, 3)
      .align(crabshell_ui::Align::Start)
      .child(
        crabshell_ui::Image::new(fixture("swatches.png"))
          .width(90)
          .height(45)
          .fit(Fit::Contain)
          .blur(2.5)
          .saturation(1.5),
      ),
  );
  let window = scene.window(panel, 100.0, 60.0);
  for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
    let full = scene.full(&window, scale);
    // Damage strips that cut through the picture at odd places.
    for (x, y) in [(7.0, 4.0), (33.0, 21.0), (61.5, 40.0)] {
      let mut history = crate::DamageHistory::new();
      let mut buffer = super::Buffer::new(&window, scale);
      scene.draw(&window, &mut history, &mut buffer);
      let damage = Rect::new(x, y, 11.0, 7.0);
      super::repaint::repaint(&mut scene, &window, &[damage], &mut history, &mut buffer);
      assert!(
        buffer.pixels == full.pixels,
        "a repaint at {x}, {y} at scale {scale} differs from a full draw"
      );
    }
  }
}

#[test]
fn a_huge_blur_is_the_blur_of_the_picture_s_longer_side_and_ends_quickly() {
  let (width, height) = (24, 12);
  let picture: Vec<u8> = (0..width * height)
    .flat_map(|index| [(index * 7 % 256) as u8, 0, 0, 255])
    .collect();
  let blurred = |sigma| {
    let mut data = picture.clone();
    blur(&mut data, width, height, sigma);
    data
  };
  let longest = blurred(24.0);
  let started = std::time::Instant::now();
  assert_eq!(blurred(1e9), longest);
  assert_eq!(blurred(f32::MAX), longest);
  assert_eq!(blurred(f32::INFINITY), longest);
  assert!(started.elapsed() < std::time::Duration::from_secs(1));
  // Through the filters, where blur times scale overflows to infinity.
  let mut pixmap = Pixmap::new(width as u32, height as u32).expect("a pixmap");
  pixmap.data_mut().copy_from_slice(&picture);
  let huge = ImageFilters {
    blur: f32::MAX,
    ..NONE
  };
  apply(&mut pixmap, &huge, 2.0);
  assert_eq!(pixmap.data(), longest);
}
