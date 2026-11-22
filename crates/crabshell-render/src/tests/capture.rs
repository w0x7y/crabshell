//! Captured frames: keeping them small, and drawing them into their view.

use std::rc::Rc;

use crabshell_ui::{CaptureFrame, CaptureSource, Fit, Rect, Row, ScreencopyView, Style};

use super::Scene;
use crate::{PixelFormat, Turn, capture_frame, frame_size};

#[test]
fn a_frame_is_kept_just_large_enough_to_cover_its_view() {
  // A thumbnail of a 4K output keeps a thumbnail's pixels.
  assert_eq!(frame_size((3840, 2160), (192, 108)), (192, 108));
  // Another shape: it covers the box and keeps the output's.
  assert_eq!(frame_size((1920, 1080), (100, 100)), (178, 100));
  assert_eq!(frame_size((1920, 1080), (400, 100)), (400, 225));
  // Never scaled up.
  assert_eq!(frame_size((640, 360), (1280, 720)), (640, 360));
  assert_eq!(frame_size((640, 360), (640, 720)), (640, 360));
  // At least a pixel a side.
  assert_eq!(frame_size((1920, 1), (10, 0)), (10, 1));
}

#[test]
fn a_buffer_becomes_premultiplied_rgba_scaled_down() {
  // 4 by 2, B, G, R, A: a translucent red, premultiplied.
  let bgra = [0, 0, 128, 128].repeat(8);
  let frame = capture_frame(
    &bgra,
    4,
    2,
    PixelFormat::Argb8888,
    false,
    Turn::default(),
    (2, 1),
  );
  let frame = frame.expect("the bytes fit");
  assert_eq!((frame.width, frame.height), (2, 1));
  assert_eq!(&frame.rgba[..], [128, 0, 0, 128].repeat(2));

  // XRGB's fourth byte is anything; the frame is opaque.
  let bgrx = [255, 0, 0, 7].repeat(8);
  let frame = capture_frame(
    &bgrx,
    4,
    2,
    PixelFormat::Argb8888,
    true,
    Turn::default(),
    (4, 2),
  );
  let frame = frame.expect("the bytes fit");
  assert_eq!((frame.width, frame.height), (4, 2));
  assert_eq!(&frame.rgba[..], [0, 0, 255, 255].repeat(8));

  // XBGR is R, G, B, X already. Halved, it is still opaque.
  let frame = capture_frame(
    &bgrx,
    4,
    2,
    PixelFormat::Abgr8888,
    true,
    Turn::default(),
    (2, 1),
  );
  assert_eq!(
    &frame.expect("the bytes fit").rgba[..],
    [255, 0, 0, 255].repeat(2)
  );

  let short = capture_frame(
    &bgrx[4..],
    4,
    2,
    PixelFormat::Argb8888,
    true,
    Turn::default(),
    (4, 2),
  );
  assert_eq!(short, None);
}

/// A frame of `width` by `height` pixels of one color.
fn solid(width: u32, height: u32, rgba: [u8; 4]) -> CaptureFrame {
  CaptureFrame {
    width,
    height,
    logical_width: width,
    logical_height: height,
    rgba: Rc::from(rgba.repeat((width * height) as usize)),
  }
}

#[test]
fn a_view_draws_its_frame_scaled_into_its_box_from_one_kept_copy() {
  let mut scene = Scene::new();
  let fit = scene.cx.signal(Fit::Fill);
  let view = ScreencopyView::new(CaptureSource::Output(String::from("DP-2")))
    .fit(move || fit.get())
    .style(Style::new().width(40.0).height(20.0));
  let row = Row::new()
    .child(Row::new().style(Style::new().width(10.0)))
    .child(view);
  let window = scene.window(scene.panel().child(row), 100.0, 20.0);
  let id = window.captures()[0].id;
  // Nothing before the first frame.
  let blank = scene.full(&window, 2.0);
  assert_eq!(blank.pixel(40, 20), [0, 0, 0, 0]);

  window.show_capture(id, solid(16, 9, [0, 0, 255, 255]));
  let drawn = scene.full(&window, 2.0);
  // At scale 2 the box is pixels 20 to 100 across and 0 to 40 down.
  for (x, y) in [(20, 0), (60, 20), (99, 39)] {
    assert_eq!(drawn.pixel(x, y), [0, 0, 255, 255], "at {x},{y}");
  }
  assert_eq!(drawn.pixel(19, 20), [0, 0, 0, 0]);
  assert_eq!(drawn.pixel(100, 20), [0, 0, 0, 0]);
  assert_eq!(scene.renderer.cached_captures(), 1);

  // A new frame replaces the copy kept for the view.
  window.show_capture(id, solid(16, 9, [0, 255, 0, 255]));
  let drawn = scene.full(&window, 2.0);
  assert_eq!(drawn.pixel(60, 20), [0, 255, 0, 255]);
  assert_eq!(scene.renderer.cached_captures(), 1);

  // Contained, a 16:9 frame leaves the 2:1 box's sides clear.
  fit.set(Fit::Contain);
  scene.frame(&window);
  let drawn = scene.full(&window, 2.0);
  assert_eq!(drawn.pixel(60, 20), [0, 255, 0, 255]);
  assert_eq!(drawn.pixel(22, 20), [0, 0, 0, 0]);

  // The platform drops a gone view's copy at once.
  scene.renderer.forget_captures(&[id + 1]);
  assert_eq!(scene.renderer.cached_captures(), 1, "another view's");
  scene.renderer.forget_captures(&[id]);
  assert_eq!(scene.renderer.cached_captures(), 0);
  scene.full(&window, 2.0);
  assert_eq!(scene.renderer.cached_captures(), 1);

  // Gone from the window, its copy goes after a few draws.
  drop(window);
  let other = scene.window(scene.panel(), 10.0, 10.0);
  for _ in 0..20 {
    scene.full(&other, 1.0);
  }
  assert_eq!(scene.renderer.cached_captures(), 0);
}

#[test]
fn a_turned_frame_is_turned_back_upright() {
  // Two pixels, red then blue, in BGRA.
  let pixels = [0, 0, 255, 255, 255, 0, 0, 255];
  let (red, blue) = ([255, 0, 0, 255], [0, 0, 255, 255]);
  let turned = |quarters, flipped| {
    let turn = Turn { quarters, flipped };
    let frame =
      capture_frame(&pixels, 2, 1, PixelFormat::Argb8888, false, turn, (2, 2)).expect("a frame");
    let rgba = frame.rgba.to_vec();
    (
      (frame.width, frame.height),
      [rgba[..4].to_vec(), rgba[4..].to_vec()],
    )
  };
  assert_eq!(turned(0, false), ((2, 1), [red.to_vec(), blue.to_vec()]));
  assert_eq!(turned(2, false), ((2, 1), [blue.to_vec(), red.to_vec()]));
  assert_eq!(turned(0, true), ((2, 1), [blue.to_vec(), red.to_vec()]));
  assert_eq!(turned(2, true), ((2, 1), [red.to_vec(), blue.to_vec()]));
  let (size, sideways) = turned(1, false);
  assert_eq!(size, (1, 2));
  let turn = Turn {
    quarters: 1,
    flipped: false,
  };
  let frame = capture_frame(&pixels, 2, 1, PixelFormat::Argb8888, false, turn, (1, 1));
  let frame = frame.expect("a frame");
  assert_eq!(
    (frame.logical_width, frame.logical_height),
    (1, 2),
    "the source's whole size, upright"
  );
  let (_, other) = turned(3, false);
  assert_eq!(
    sideways,
    [other[1].clone(), other[0].clone()],
    "opposite turns"
  );
}

#[test]
fn a_crop_draws_that_part_of_the_frame_into_the_box() {
  let mut scene = Scene::new();
  let crop = scene.cx.signal(Some(Rect::new(1.0, 0.0, 1.0, 1.0)));
  let view = ScreencopyView::new(CaptureSource::Output(String::from("DP-2")))
    .crop(move || crop.get())
    .style(Style::new().width(20.0).height(10.0));
  let window = scene.window(scene.panel().child(view), 20.0, 10.0);
  let id = window.captures()[0].id;
  let (red, blue) = ([255, 0, 0, 255], [0, 0, 255, 255]);
  // 4 by 2 pixels of a 2 by 1 source at scale 2: red left, blue right.
  let halves = |width: u32, height: u32| CaptureFrame {
    width,
    height,
    logical_width: 2,
    logical_height: 1,
    rgba: Rc::from(
      (0..width * height)
        .flat_map(|at| if at % width < width / 2 { red } else { blue })
        .collect::<Vec<u8>>(),
    ),
  };
  window.show_capture(id, halves(4, 2));
  for scale in [1.0, 2.0] {
    let drawn = scene.full(&window, scale);
    let last = (20.0 * scale) as u32 - 1;
    for (x, y) in [(0, 0), (last, last / 2)] {
      assert_eq!(drawn.pixel(x, y), blue, "at {x},{y}, scale {scale}");
    }
  }
  // A frame kept smaller maps the crop the same.
  window.show_capture(id, halves(2, 1));
  assert_eq!(scene.full(&window, 2.0).pixel(0, 0), blue);
  crop.set(Some(Rect::new(0.0, 0.0, 1.0, 1.0)));
  scene.frame(&window);
  assert_eq!(scene.full(&window, 2.0).pixel(39, 19), red);

  // Cut to the frame, and nothing when none of it is left.
  crop.set(Some(Rect::new(1.0, -5.0, 9.0, 9.0)));
  scene.frame(&window);
  assert_eq!(scene.full(&window, 1.0).pixel(0, 0), blue);
  for outside in [
    Rect::new(2.0, 0.0, 1.0, 1.0),
    Rect::new(f32::NAN, 0.0, 1.0, 1.0),
  ] {
    crop.set(Some(outside));
    scene.frame(&window);
    assert_eq!(
      scene.full(&window, 1.0).pixel(0, 0),
      [0, 0, 0, 0],
      "{outside:?}"
    );
  }
}

#[test]
fn a_crop_at_its_boxs_size_draws_the_frames_pixels_as_they_are() {
  let mut scene = Scene::new();
  let view = ScreencopyView::new(CaptureSource::Output(String::from("DP-2")))
    .crop(Some(Rect::new(3.0, 2.0, 5.0, 4.0)))
    .style(Style::new().width(5.0).height(4.0));
  let window = scene.window(scene.panel().child(view), 5.0, 4.0);
  let pixel = |x: u32, y: u32| [(x * 20) as u8, (y * 40) as u8, 90, 255];
  let frame = CaptureFrame {
    width: 10,
    height: 8,
    logical_width: 10,
    logical_height: 8,
    rgba: Rc::from(
      (0..80u32)
        .flat_map(|at| pixel(at % 10, at / 10))
        .collect::<Vec<u8>>(),
    ),
  };
  window.show_capture(window.captures()[0].id, frame);
  scene.frame(&window);
  let drawn = scene.full(&window, 1.0);
  for (x, y) in (0..5).flat_map(|x| (0..4).map(move |y| (x, y))) {
    assert_eq!(drawn.pixel(x, y), pixel(x + 3, y + 2), "at {x},{y}");
  }
}
