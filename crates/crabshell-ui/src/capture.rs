//! [`Capture`], one frame of an output or a window that any number of
//! screencopy views show, and what the platform reads and writes of it.

use std::cell::RefCell;
use std::panic::Location;

use crabshell_reactive::{ReadSignal, Signal, untrack};

use crate::cx::Cx;
use crate::prop::{IntoProp, PropKind};
use crate::screencopy::{CaptureFrame, CaptureSource, next_id};

thread_local! {
  /// Every capture made, for the platform to find those waiting for a
  /// frame. Per thread, like the reactive runtime.
  static CAPTURES: RefCell<Vec<Capture>> = const { RefCell::new(Vec::new()) };
}

/// One captured frame of an output or a window, made by
/// [`Cx::capture`], for [`ScreencopyView::of`](crate::ScreencopyView::of)
/// views to show. It is a `Copy` handle, like a signal, and goes with the
/// context that made it. Calls on a capture whose context is gone do
/// nothing.
///
/// Every view of it shows the same frame, and none of them captures
/// anything itself, so a frame shown whole and cut into pieces is taken
/// once. A capture takes its frame at once, whether or not a view shows
/// it, and then wakes nothing until [`refresh`](Self::refresh). It holds
/// the source's whole frame, at the source's own resolution.
///
/// A capture of an output holds the shell's own windows on it too, so
/// hide a view of the output that covers it before you refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Capture {
  /// `None` until the source's binding first runs.
  source: Signal<Option<CaptureSource>>,
  frame: Signal<Option<CaptureFrame>>,
  ready: Signal<bool>,
  /// The number the frame waited for is asked under, never reused, so a
  /// frame asked for before a refresh or another source is turned away.
  /// `None` while no frame is waited for.
  asked: Signal<Option<u64>>,
}

impl Cx {
  /// A capture of `source`: a [`CaptureSource`], or a closure that
  /// returns one. It takes one frame as soon as the source is there,
  /// waiting while it isn't, as for an output not plugged in yet. A new
  /// source drops the frame and takes one of the new source. The capture
  /// belongs to this context and goes with it.
  ///
  /// ```
  /// use crabshell_reactive::Runtime;
  /// use crabshell_ui::{CaptureSource, Cx, Rect, ScreencopyView, Style};
  ///
  /// let cx = Cx::new(Runtime::current().new_owner());
  /// let screen = cx.capture(CaptureSource::Output(String::from("DP-1")));
  /// let whole = ScreencopyView::of(screen).style(Style::new().width(1920.0).height(1080.0));
  /// let corner = ScreencopyView::of(screen)
  ///     .crop(Some(Rect::new(0.0, 0.0, 480.0, 270.0)))
  ///     .style(Style::new().width(480.0).height(270.0));
  /// # let _ = (whole, corner);
  /// ```
  #[track_caller]
  pub fn capture(&self, source: impl IntoProp<CaptureSource>) -> Capture {
    let capture = Capture {
      source: self.signal(None),
      frame: self.signal(None),
      ready: self.signal(false),
      asked: self.signal(None),
    };
    match source.into_prop().0 {
      PropKind::Value(source) => capture.retarget(source),
      PropKind::Binding(mut compute) => {
        let place = self
          .owner()
          .name()
          .map(|name| format!(" in {name}"))
          .unwrap_or_default();
        let label = format!("cx.capture source at {}{place}", Location::caller());
        self
          .effect(move || {
            let source = compute();
            untrack(|| capture.retarget(source));
          })
          .named(label);
      }
    }
    CAPTURES.with(|captures| captures.borrow_mut().push(capture));
    capture
  }
}

impl Capture {
  /// Whether it holds a frame of its source taken since the last
  /// [`refresh`](Self::refresh): `false` from a refresh until the new
  /// frame lands, while its views keep showing the old one.
  pub fn ready(&self) -> ReadSignal<bool> {
    self.ready.read_only()
  }

  /// Takes a new frame. The views keep showing the old one until the new
  /// one lands, and [`ready`](Self::ready) reads `false` meanwhile. A
  /// refresh while a frame is on its way asks again, and the earlier one
  /// is turned away.
  pub fn refresh(&self) {
    if untrack(|| self.source.try_with(Option::is_some)) == Some(true) {
      self.ready.try_set(false);
      self.asked.try_set(Some(next_id()));
    }
  }

  /// Takes `source`, dropping the frame of another.
  fn retarget(&self, source: CaptureSource) {
    if self.source.try_with(|now| now.as_ref() == Some(&source)) != Some(false) {
      return;
    }
    self.source.try_set(Some(source));
    if self.frame.try_with(Option::is_some) == Some(true) {
      self.frame.try_set(None);
    }
    if self.ready.try_get() == Some(true) {
      self.ready.try_set(false);
    }
    self.asked.try_set(Some(next_id()));
  }

  /// The frame its views show, for their bindings.
  pub(crate) fn frame(&self) -> Option<CaptureFrame> {
    self.frame.try_get().flatten()
  }
}

/// The captures waiting for a frame, for the platform: the number each
/// frame is asked under, for [`deliver_capture`], and the source. Captures
/// whose context is gone are freed first.
pub fn wanted_captures() -> Vec<(u64, CaptureSource)> {
  untrack(|| {
    CAPTURES.with(|captures| {
      let mut captures = captures.borrow_mut();
      captures.retain(|capture| capture.asked.try_get().is_some());
      captures
        .iter()
        .filter_map(|capture| {
          let asked = capture.asked.try_get().flatten()?;
          Some((asked, capture.source.try_get().flatten()?))
        })
        .collect()
    })
  })
}

/// Gives the capture that asked for a frame under `asked` its frame, and
/// returns whether one did: a frame asked for before a refresh or a new
/// source is turned away.
pub fn deliver_capture(asked: u64, frame: CaptureFrame) -> bool {
  let found = CAPTURES.with(|captures| {
    let captures = captures.borrow();
    let mut all = captures.iter();
    all
      .find(|capture| untrack(|| capture.asked.try_get()) == Some(Some(asked)))
      .copied()
  });
  let Some(capture) = found else {
    return false;
  };
  // Written with the table released: a write drops the old frame.
  capture.asked.try_set(None);
  capture.frame.try_set(Some(frame));
  capture.ready.try_set(true);
  true
}
