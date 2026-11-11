//! Frame pacing: draw after the first configure, once the last frame was
//! shown, and only when something changed.

use crate::pacing::Pacing;

#[test]
fn nothing_is_drawn_before_the_first_configure() {
  let pacing = Pacing::default();
  assert!(!pacing.configured());
  assert!(!pacing.should_draw(true));
}

#[test]
fn the_first_configure_draws_even_without_damage() {
  let mut pacing = Pacing::default();
  pacing.configure();
  assert!(pacing.should_draw(false));
}

#[test]
fn a_commit_waits_for_its_frame_callback() {
  let mut pacing = Pacing::default();
  pacing.configure();
  pacing.committed();
  assert!(!pacing.should_draw(true), "damage waits for the callback");
  pacing.redraw();
  pacing.configure();
  assert!(!pacing.should_draw(true), "so does a configure");
  pacing.frame_done();
  assert!(pacing.should_draw(false));
}

#[test]
fn an_idle_window_draws_nothing_after_its_frame() {
  let mut pacing = Pacing::default();
  pacing.configure();
  pacing.committed();
  pacing.frame_done();
  // Nothing changed: no draw, so no commit and no further callback.
  assert!(!pacing.should_draw(false));
  assert!(pacing.should_draw(true), "until something is damaged");
}

#[test]
fn a_redraw_draws_without_damage() {
  let mut pacing = Pacing::default();
  pacing.configure();
  pacing.committed();
  pacing.frame_done();
  pacing.redraw();
  assert!(pacing.should_draw(false));
  pacing.committed();
  pacing.frame_done();
  assert!(!pacing.should_draw(false), "once drawn, the redraw is done");
}
