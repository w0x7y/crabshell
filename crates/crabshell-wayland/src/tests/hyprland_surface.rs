//! What a window's compositor opacity and visible mask send Hyprland.

use crabshell_ui::Region;

use crate::hyprland_surface::{Request, Shown, Told, buffer_rects, requests};
use crate::scale::Scale;

#[test]
fn a_visible_mask_is_sent_in_buffer_pixels_grown_out_at_every_scale() {
  // Grown by a logical pixel to 9 to 111 across, and cut to the buffer.
  let mask = Region::rect(10, 0, 100, 30);
  for (scale, rect) in [
    (Scale::Integer(1), [9, 0, 102, 30]),
    // 11.25 to 138.75 across, grown out to whole pixels, in a buffer 38
    // tall.
    (Scale::Fractional(150), [11, 0, 128, 38]),
    (Scale::Fractional(180), [13, 0, 154, 45]),
    // 15.75 to 194.25, in a buffer 53 tall.
    (Scale::Fractional(210), [15, 0, 180, 53]),
    (Scale::Fractional(240), [18, 0, 204, 60]),
    (Scale::Integer(2), [18, 0, 204, 60]),
  ] {
    assert_eq!(
      buffer_rects(&mask, (200, 30), scale),
      Some(vec![rect]),
      "at {scale:?}"
    );
  }
}

#[test]
fn a_visible_mask_is_cut_to_the_buffer_and_nothing_left_is_none() {
  // 201 by 30 at 1.25 is a 251 by 38 buffer: 251.25 rounds down.
  let scale = Scale::Fractional(150);
  let all = Region::rect(-5, -5, 1000, 1000);
  assert_eq!(
    buffer_rects(&all, (201, 30), scale),
    Some(vec![[0, 0, 251, 38]])
  );
  // Hyprland reads an empty region as no hint at all.
  assert_eq!(buffer_rects(&Region::empty(), (200, 30), scale), None);
  let outside = Region::rect(300, 0, 10, 10);
  assert_eq!(buffer_rects(&outside, (200, 30), scale), None);
}

#[test]
fn a_visible_mask_covers_every_pixel_a_rounded_shape_touches() {
  // A bar's rounded background, whose soft edge the renderer draws past
  // the whole pixels the region's curve is cut at.
  let (width, height, radius) = (100_u32, 30_u32, 12.0_f32);
  let mask = Region::rect(0, 0, width, height).radius(radius);
  let inside = |x: f32, y: f32| {
    let nearest_x = x.clamp(radius, width as f32 - radius);
    let nearest_y = y.clamp(radius, height as f32 - radius);
    (x - nearest_x).powi(2) + (y - nearest_y).powi(2) <= radius * radius
  };
  for scale in [Scale::Integer(1), Scale::Fractional(150), Scale::Integer(2)] {
    let rects = buffer_rects(&mask, (width, height), scale).expect("a region");
    let (buffer_width, buffer_height) = scale.buffer_size(width, height).expect("a buffer");
    for (px, py) in (0..buffer_height).flat_map(|py| (0..buffer_width).map(move |px| (px, py))) {
      // Any part of the pixel in the shape, on an 8 by 8 grid within it.
      let samples = (0..8).flat_map(|i| (0..8).map(move |j| (i, j)));
      let touched = samples.into_iter().any(|(i, j)| {
        let at =
          |pixel: u32, step: i32| (pixel as f32 + (step as f32 + 0.5) / 8.0) / scale.factor();
        inside(at(px, i), at(py, j))
      });
      let (px, py) = (px as i32, py as i32);
      let covered = rects
        .iter()
        .any(|&[x, y, w, h]| (x..x + w).contains(&px) && (y..y + h).contains(&py));
      assert!(!touched || covered, "pixel {px},{py} at {scale:?}");
    }
  }
}

#[test]
fn only_what_changed_is_sent_and_the_defaults_need_nothing() {
  let defaults = Shown::default();
  assert_eq!(
    defaults,
    Shown {
      opacity: 1.0,
      visible: None
    }
  );
  assert_eq!(
    requests(&defaults, &defaults, 2),
    [],
    "so no object is made"
  );
  let faded = Shown {
    opacity: 0.5,
    visible: None,
  };
  assert_eq!(requests(&defaults, &faded, 2), [Request::Opacity(0.5)]);
  let masked = Shown {
    opacity: 0.5,
    visible: Some(vec![[0, 0, 10, 10]]),
  };
  assert_eq!(
    requests(&faded, &masked, 2),
    [Request::Visible(Some(vec![[0, 0, 10, 10]]))]
  );
  assert_eq!(
    requests(&masked, &defaults, 2),
    [Request::Opacity(1.0), Request::Visible(None)]
  );
}

#[test]
fn version_1_gets_no_visible_regions() {
  let masked = Shown {
    opacity: 1.0,
    visible: Some(vec![[0, 0, 10, 10]]),
  };
  assert_eq!(requests(&Shown::default(), &masked, 1), []);
  let both = Shown {
    opacity: 0.25,
    ..masked
  };
  assert_eq!(
    requests(&Shown::default(), &both, 1),
    [Request::Opacity(0.25)]
  );
}

#[test]
fn a_surface_asking_only_for_the_defaults_tells_hyprland_nothing() {
  let mut told = Told::default();
  assert!(
    told.outdated((200, 30), Scale::Integer(1)),
    "nothing told yet"
  );
  // Nothing to send, so no object is made.
  assert_eq!(told.take(1.0, None, (200, 30), Scale::Integer(1)), None);
  assert!(!told.outdated((200, 30), Scale::Integer(1)));
  // A new size or scale with no mask to cut again tells nothing either.
  assert!(told.outdated((300, 30), Scale::Integer(1)));
  assert!(told.outdated((200, 30), Scale::Fractional(180)));
  assert_eq!(
    told.take(1.0, None, (300, 30), Scale::Fractional(180)),
    None
  );
}

#[test]
fn a_visible_mask_is_told_again_at_a_new_size_or_scale() {
  let mut told = Told::default();
  let mask = Some(Region::rect(0, 0, 1000, 30));
  let at = |size, scale, told: &mut Told| {
    let before = told.take(1.0, mask.clone(), size, scale);
    (before.is_some(), told.shown.visible.clone())
  };
  assert_eq!(
    at((200, 30), Scale::Integer(1), &mut told),
    (true, Some(vec![[0, 0, 200, 30]]))
  );
  assert_eq!(
    told.take(1.0, mask.clone(), (200, 30), Scale::Integer(1)),
    None,
    "told once"
  );
  assert_eq!(
    at((300, 30), Scale::Integer(1), &mut told),
    (true, Some(vec![[0, 0, 300, 30]]))
  );
  assert_eq!(
    at((300, 30), Scale::Fractional(180), &mut told),
    (true, Some(vec![[0, 0, 450, 45]]))
  );
}

#[test]
fn a_visible_region_that_couldnt_be_made_is_told_again() {
  let mut told = Told::default();
  let mask = Some(Region::rect(0, 0, 10, 10));
  let before = told
    .take(0.5, mask.clone(), (200, 30), Scale::Integer(1))
    .expect("a change");
  told.unsent_visible(before.visible);
  assert_eq!(told.shown.visible, None, "Hyprland still has none");
  assert!(
    told.outdated((200, 30), Scale::Integer(1)),
    "tried next turn"
  );
  let before = told
    .take(0.5, mask, (200, 30), Scale::Integer(1))
    .expect("the region again");
  assert_eq!(
    requests(&before, &told.shown, 2),
    [Request::Visible(Some(vec![[0, 0, 11, 11]]))]
  );
}
