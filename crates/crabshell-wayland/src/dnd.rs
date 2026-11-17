//! Drag and drop over `wl_data_device`. A button's drag that leaves its
//! window goes on as a Wayland drag the shell starts, offering a private
//! type and, for a `DragContent`, text or a URI list. Dropped on one of
//! the shell's windows, its value is handed over in-process; drags from
//! other programs are read through a capped pipe and dropped as a
//! `DragContent`.

use std::any::{Any, TypeId};
use std::os::fd::OwnedFd;
use std::rc::Rc;

use crabshell_ui::{DragContent, DropSpot, Point};
use smithay_client_toolkit::data_device_manager::data_offer::DragOffer;
use smithay_client_toolkit::data_device_manager::data_source::DragSource;
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::{
  wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_data_source::WlDataSource,
  wl_surface::WlSurface,
};

use crate::clipboard::{Received, TEXT_TYPES};
use crate::platform::Platform;
use crate::windows::WindowId;

/// The type every drag of the shell's offers, which marks it as the
/// shell's own; another program asking for it gets nothing.
pub(crate) const PRIVATE: &str = "application/x-crabshell-drag";
const URI_LIST: &str = "text/uri-list";
/// What a text `DragContent` is offered as.
const CONTENT_TEXT: [&str; 2] = ["text/plain;charset=utf-8", "UTF8_STRING"];
/// The most a dropped URI list and dropped text read.
pub(crate) const MAX_URIS: usize = 1024 * 1024;
pub(crate) const MAX_TEXT: usize = 4 * 1024 * 1024;

/// The drag the shell started and the one over its windows, if any.
#[derive(Default)]
pub(crate) struct Drags {
  out: Option<Outgoing>,
  over: Option<Over>,
}

/// A button's drag gone out of its window. Dropping it cancels the drag.
struct Outgoing {
  source: DragSource,
  window: WindowId,
  carried: TypeId,
  /// What other programs are sent, for a `DragContent`.
  bytes: Option<Rc<[u8]>>,
  /// Dropped somewhere; the source stays until the target finishes.
  dropped: bool,
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
  /// The shell's own, carrying a value of this type.
  Ours(TypeId),
  /// Another program's, read as this type into a `DragContent`.
  Content(&'static str),
  /// Nothing a button can take.
  Nothing,
}

impl Platform {
  /// Starts a Wayland drag for a window whose held drag went past its
  /// edge, and cancels the one started when its button is gone. Run each
  /// loop turn.
  pub(crate) fn update_drags(&mut self) {
    if let Some(out) = &self.drags.out
      && !out.dropped
      && !self
        .windows
        .get(out.window)
        .is_some_and(|entry| entry.window.dragging_out())
    {
      self.drags.out = None;
    }
    // A drag needs the held button's serial and version 3's drop events.
    let (Some(manager), Some((seat, serial)), true) = (
      &self.data_devices_manager,
      &self.last_button,
      self.button_held,
    ) else {
      return;
    };
    if manager.data_device_manager().version() < 3 {
      return;
    }
    let Some((_, device)) = self.data_devices.iter().find(|(ours, _)| ours == seat) else {
      return;
    };
    for entry in &self.windows.entries {
      let Some(surface) = &entry.surface else {
        continue;
      };
      let Some(out) = entry.window.take_drag_out() else {
        continue;
      };
      let (types, bytes): (&[&str], _) = match out.content {
        Some(DragContent::Text(text)) => (&CONTENT_TEXT, Some(text.into_bytes())),
        Some(DragContent::Uris(uris)) => (&[URI_LIST], Some(uri_list(&uris))),
        None => (&[], None),
      };
      let types = [PRIVATE].into_iter().chain(types.iter().copied());
      let source = manager.create_drag_and_drop_source(&self.qh, types, DndAction::Copy);
      source.start_drag(device, surface.wl_surface(), None, *serial);
      self.drags.out = Some(Outgoing {
        source,
        window: entry.id,
        carried: out.carried,
        bytes: bytes.map(Rc::from),
        dropped: false,
      });
      return;
    }
  }

  /// Answers another program's read of the drag's content, or gives
  /// `pipe` back when `source` isn't the drag's. The private type sends
  /// nothing.
  pub(crate) fn drag_send(
    &mut self,
    source: &WlDataSource,
    mime: &str,
    pipe: OwnedFd,
  ) -> Option<OwnedFd> {
    let Some(out) = self
      .drags
      .out
      .as_ref()
      .filter(|out| out.source.inner() == source)
    else {
      return Some(pipe);
    };
    if let Some(bytes) = out.bytes.clone().filter(|_| mime != PRIVATE) {
      self.send(pipe, bytes);
    }
    None
  }

  /// The drag's source was dropped on a target that took it (`true`) or
  /// was cancelled. Returns `false` when `source` isn't the drag's.
  pub(crate) fn drag_dropped(&mut self, source: &WlDataSource, taken: bool) -> bool {
    let Some(out) = self
      .drags
      .out
      .as_mut()
      .filter(|out| out.source.inner() == source)
    else {
      return false;
    };
    let window = out.window;
    if taken {
      out.dropped = true;
    } else {
      self.drags.out = None;
    }
    self.end_out(window, |_| taken);
    true
  }

  /// The target finished taking the drag. Returns `false` when `source`
  /// isn't the drag's.
  pub(crate) fn drag_finished(&mut self, source: &WlDataSource) -> bool {
    let ours = self
      .drags
      .out
      .as_ref()
      .is_some_and(|out| out.source.inner() == source);
    if ours {
      self.drags.out = None;
    }
    ours
  }

  /// Ends the drag of window `window` with what `drop` says, as a
  /// release would.
  fn end_out(&mut self, window: WindowId, drop: impl FnOnce(Rc<dyn Any>) -> bool) {
    self.released_unseen();
    if let Some(entry) = self.windows.get(window) {
      entry.window.end_drag_out(drop);
    }
  }

  /// The button or finger came up under the compositor's drag, unseen.
  fn released_unseen(&mut self) {
    self.button_held = false;
    for touch in &mut self.touches {
      touch.state.forget();
    }
  }

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
    let kind = match &self.drags.out {
      Some(out) if has(PRIVATE) => Kind::Ours(out.carried),
      _ if has(URI_LIST) => Kind::Content(URI_LIST),
      _ => match TEXT_TYPES.into_iter().find(|kind| has(kind)) {
        Some(kind) => Kind::Content(kind),
        None => Kind::Nothing,
      },
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
    let carried = match over.kind {
      Kind::Ours(carried) => carried,
      Kind::Content(_) => TypeId::of::<DragContent>(),
      Kind::Nothing => return,
    };
    let Some(entry) = self.windows.get(over.window) else {
      return;
    };
    let at = Point::new(x as f32, y as f32);
    let takes = (x.is_finite() && y.is_finite()) && entry.window.drag_over(at, carried);
    if takes == over.accepted {
      return;
    }
    over.accepted = takes;
    let mime = match over.kind {
      Kind::Ours(_) => PRIVATE,
      Kind::Content(mime) => mime,
      Kind::Nothing => return,
    };
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

  /// The drag was dropped on the window it is over: the shell's own is
  /// handed over now, another program's read first.
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
      (Kind::Ours(_), _) => {
        if let Some(out) = &mut self.drags.out {
          out.dropped = true;
          let source = out.window;
          let windows = &self.windows;
          let target = windows.get(over.window).map(|entry| &entry.window);
          if let Some(entry) = windows.get(source) {
            entry.window.end_drag_out(|value| {
              spot
                .zip(target)
                .is_some_and(|(spot, target)| target.drop_into(spot, value))
            });
          }
          self.released_unseen();
        }
        done(&offer, over.accepted);
      }
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

/// `uris` as a `text/uri-list`, one per line ended by CRLF.
fn uri_list(uris: &[String]) -> Vec<u8> {
  uris
    .iter()
    .flat_map(|uri| [uri.as_str(), "\r\n"])
    .collect::<String>()
    .into_bytes()
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
