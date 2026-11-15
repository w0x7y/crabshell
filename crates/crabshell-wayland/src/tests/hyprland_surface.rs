//! What a window's compositor opacity and visible mask send Hyprland.

use crabshell_ui::Region;

use crate::hyprland_surface::{Request, Shown, buffer_rects, requests};
use crate::scale::Scale;

#[test]
fn a_visible_mask_is_sent_in_buffer_pixels_grown_out_at_every_scale() {
  let mask = Region::rect(10, 0, 100, 30);
  for (scale, rect) in [
    (Scale::Integer(1), [10, 0, 100, 30]),
    // 12.5 to 137.5 across and 37.5 down, grown out to whole pixels.
    (Scale::Fractional(150), [12, 0, 126, 38]),
    (Scale::Fractional(180), [15, 0, 150, 45]),
    // 17.5 to 192.5, and 52.5 down in a buffer 53 tall.
    (Scale::Fractional(210), [17, 0, 176, 53]),
    (Scale::Fractional(240), [20, 0, 200, 60]),
    (Scale::Integer(2), [20, 0, 200, 60]),
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
