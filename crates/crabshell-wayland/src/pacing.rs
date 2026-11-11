//! When a window draws: after its first configure, once the compositor has
//! shown the last frame, and only if something changed.
//!
//! Each commit asks for a frame callback. Until it arrives, the window
//! doesn't draw, so it never draws faster than the screen shows. A window
//! with nothing to draw doesn't commit, asks for no callback and so gets
//! no more events: an idle bar never wakes the loop.

/// One window's drawing state.
#[derive(Debug, Default)]
pub(crate) struct Pacing {
  /// The first configure arrived; until then the window may not draw.
  configured: bool,
  /// The last commit's frame callback hasn't arrived.
  waiting: bool,
  /// Something besides the window's own damage needs a frame: a
  /// configure, which a commit acknowledges, or a new scale.
  redraw: bool,
}

impl Pacing {
  /// A configure arrived.
  pub(crate) fn configure(&mut self) {
    self.configured = true;
    self.redraw = true;
  }

  /// The window needs a frame even without damage.
  pub(crate) fn redraw(&mut self) {
    self.redraw = true;
  }

  /// A frame was committed, with a frame callback requested.
  pub(crate) fn committed(&mut self) {
    self.waiting = true;
    self.redraw = false;
  }

  /// The frame callback arrived.
  pub(crate) fn frame_done(&mut self) {
    self.waiting = false;
  }

  /// Whether the first configure arrived.
  pub(crate) fn configured(&self) -> bool {
    self.configured
  }

  /// Whether to draw now. `damaged` is whether the window has damage.
  pub(crate) fn should_draw(&self, damaged: bool) -> bool {
    self.configured && !self.waiting && (damaged || self.redraw)
  }
}
