//! Drag and drop over `wl_data_device`: drags from other programs are
//! read through a capped pipe and dropped as a `DragContent`.

use std::any::TypeId;
use std::rc::Rc;

use crabshell_ui::{DragContent, DropSpot, Point};
use smithay_client_toolkit::data_device_manager::data_offer::DragOffer;
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::{
  wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_surface::WlSurface,
};

use crate::clipboard::{Received, TEXT_TYPES};
use crate::platform::Platform;
use crate::windows::WindowId;

const URI_LIST: &str = "text/uri-list";
/// The most a dropped URI list and dropped text read.
pub(crate) const MAX_URIS: usize = 1024 * 1024;
pub(crate) const MAX_TEXT: usize = 4 * 1024 * 1024;

/// The drag over the shell's windows, if any.
#[derive(Default)]
pub(crate) struct Drags {
  over: Option<Over>,
}

/// A drag over one of the shell's windows.
struct Over {
  window: WindowId,
  device: WlDataDevice,
  kind: Kind,
  /// Whether a button under it takes it, as last told.
  accepted: bool,
}

#[derive(Clone, Copy)]
enum Kind {
  /// Another program's, read as this type into a `DragContent`.
  Content(&'static str),
  /// Nothing a button can take.
  Nothing,
}

impl Platform {
  /// A drag entered `surface` at `at`.
  pub(crate) fn drag_enter(&mut self, device: &WlDataDevice, surface: &WlSurface, at: (f64, f64)) {
    self.drag_leave();
    let Some(window) = self.window_of(surface) else {
      return;
    };
    let Some(offer) = drag_offer(device) else {
      return;
    };
    let types = offer.with_mime_types(<[String]>::to_vec);
    let has = |kind: &str| types.iter().any(|offered| offered == kind);
    let kind = match TEXT_TYPES.into_iter().find(|kind| has(kind)) {
      _ if has(URI_LIST) => Kind::Content(URI_LIST),
      Some(kind) => Kind::Content(kind),
      None => Kind::Nothing,
    };
    if !matches!(kind, Kind::Nothing) {
      offer.set_actions(DndAction::Copy, DndAction::Copy);
    }
    self.drags.over = Some(Over {
      window,
      device: device.clone(),
      kind,
      accepted: false,
    });
    self.drag_motion(at);
  }

  /// The drag over a window moved to `at`, in its surface's coordinates.
  pub(crate) fn drag_motion(&mut self, (x, y): (f64, f64)) {
    let Some(over) = &mut self.drags.over else {
      return;
    };
    let Kind::Content(mime) = over.kind else {
      return;
    };
    let Some(entry) = self.windows.get(over.window) else {
      return;
    };
    let at = Point::new(x as f32, y as f32);
    let takes =
      (x.is_finite() && y.is_finite()) && entry.window.drag_over(at, TypeId::of::<DragContent>());
    if takes == over.accepted {
      return;
    }
    over.accepted = takes;
    if let Some(offer) = drag_offer(&over.device) {
      offer.accept_mime_type(offer.serial, takes.then(|| mime.to_owned()));
    }
  }

  /// The drag left the window it was over.
  pub(crate) fn drag_leave(&mut self) {
    if let Some(over) = self.drags.over.take()
      && let Some(entry) = self.windows.get(over.window)
    {
      entry.window.drag_leave();
    }
  }

  /// The drag was dropped on the window it is over: what the program
  /// sends is read, then dropped.
  pub(crate) fn drag_drop(&mut self) {
    let Some(over) = self.drags.over.take() else {
      return;
    };
    let Some(offer) = drag_offer(&over.device) else {
      return;
    };
    let spot = self
      .windows
      .get(over.window)
      .and_then(|entry| entry.window.drag_drop());
    match (over.kind, spot) {
      (Kind::Content(mime), Some(spot)) if over.accepted => {
        self.read_drop(offer, mime, over.window, spot);
      }
      _ => done(&offer, over.accepted),
    }
  }

  /// Reads another program's drop as `mime`, then drops it on `spot` in
  /// window `window`.
  fn read_drop(&mut self, offer: DragOffer, mime: &'static str, window: WindowId, spot: DropSpot) {
    let pipe = match offer.receive(mime.to_owned()) {
      Ok(pipe) => pipe,
      Err(error) => {
        tracing::warn!(%error, "couldn't read a drop");
        done(&offer, true);
        return;
      }
    };
    let cap = if mime == URI_LIST { MAX_URIS } else { MAX_TEXT };
    self.read_pipe(pipe.into(), cap, move |platform, read| {
      done(&offer, true);
      let content = match read {
        Received::Done(bytes) => content(mime, &bytes),
        Received::TooLarge => {
          tracing::warn!(bytes = cap, mime, "a drop held too much; dropped nothing");
          None
        }
        Received::Failed => None,
      };
      if let Some(content) = content
        && let Some(entry) = platform.windows.get(window)
      {
        entry.window.drop_into(spot, Rc::new(content));
      }
    });
  }
}

fn drag_offer(device: &WlDataDevice) -> Option<DragOffer> {
  use smithay_client_toolkit::data_device_manager::data_device::DataDeviceData;
  device.data::<DataDeviceData>()?.drag_offer()
}

/// Tells the program the drop was taken, which only an accepted offer
/// may, and lets the offer go.
fn done(offer: &DragOffer, accepted: bool) {
  if accepted {
    offer.finish();
  }
  offer.destroy();
}

/// What `bytes` read as `mime` hold: a URI list's lines, skipping
/// comments and blank ones, or text. Nothing, for none.
pub(crate) fn content(mime: &str, bytes: &[u8]) -> Option<DragContent> {
  let text = String::from_utf8_lossy(bytes);
  if mime == URI_LIST {
    let uris: Vec<String> = text
      .lines()
      .map(str::trim)
      .filter(|line| !line.is_empty() && !line.starts_with('#'))
      .map(String::from)
      .collect();
    return (!uris.is_empty()).then_some(DragContent::Uris(uris));
  }
  (!text.is_empty()).then(|| DragContent::Text(text.into_owned()))
}
