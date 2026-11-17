//! The clipboard for configs, through `ext-data-control-v1`, which sees
//! every selection whether or not a shell window has the keyboard, and
//! sets one without an input serial. Without it, `wl_data_device` stands
//! in, which sees the selection only while a shell window has the
//! keyboard.

use std::os::fd::OwnedFd;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::Mutex;

use calloop::RegistrationToken;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::{
  Connection, Dispatch, Proxy, QueueHandle, event_created_child,
};
use wayland_protocols::ext::data_control::v1::client::{
  ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
  ext_data_control_manager_v1::ExtDataControlManagerV1,
  ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
  ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};

use crate::clipboard::{Received, TEXT_TYPES};
use crate::platform::Platform;

/// The text types read, best first.
const READ_TEXT: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

/// The image types read, best first.
const READ_IMAGE: [&str; 2] = ["image/png", "image/jpeg"];

/// The type password managers mark a secret with.
const SECRET: &str = "x-kde-passwordManagerHint";

/// The most text read from a selection.
const MAX_TEXT: usize = 4 * 1024 * 1024;

/// The largest image read from a selection.
const MAX_IMAGE: usize = 8 * 1024 * 1024;

/// The most types an offer keeps; the rest are ignored.
const MAX_TYPES: usize = 64;

/// What a selection on the clipboard holds, as a watch hears it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
  /// Its text, when it offers `text/plain;charset=utf-8`, `UTF8_STRING`
  /// or `text/plain` and holds at most 4 MiB.
  pub text: Option<String>,
  /// Its MIME type and bytes, when it offers no text but a PNG or JPEG of
  /// at most 8 MiB, isn't a secret and images were asked for.
  pub image: Option<(String, Vec<u8>)>,
  /// Whether it offers `x-kde-passwordManagerHint`, which password
  /// managers mark secrets with.
  pub secret: bool,
}

/// What [`Platform::watch_clipboard`] calls.
type Watch = Box<dyn FnMut(&Selection)>;

/// The clipboard as configs follow it, once a watch asked.
#[derive(Default)]
pub(crate) struct Control {
  /// Whether a watch asked, so the manager is bound once at most.
  asked: bool,
  manager: Option<ExtDataControlManagerV1>,
  device: Option<ExtDataControlDeviceV1>,
  /// The offer of the selection now.
  offer: Option<ExtDataControlOfferV1>,
  /// What this shell set as the selection.
  source: Option<(ExtDataControlSourceV1, Rc<[u8]>)>,
  on_change: Option<Watch>,
  /// Whether an image is read when a selection has no text.
  images: bool,
  /// The read of the selection under way.
  reading: Option<RegistrationToken>,
  warned_text: bool,
  warned_image: bool,
}

impl Platform {
  /// Calls `on_change` with each new selection on the clipboard, once it
  /// has been read, starting with the one there now. It replaces any watch
  /// set before. If it panics, the panic is logged and the watch kept.
  ///
  /// The first watch binds `ext_data_control_manager_v1`, so a shell that
  /// never asks hears nothing. Without it, `wl_data_device` is read
  /// instead, which tells the selection only while a shell window has the
  /// keyboard, and that is logged.
  pub fn watch_clipboard(&mut self, on_change: impl FnMut(&Selection) + 'static) {
    self.control.on_change = Some(Box::new(on_change));
    if std::mem::replace(&mut self.control.asked, true) {
      return;
    }
    match self
      .registry
      .bind_one::<ExtDataControlManagerV1, _, _>(&self.qh, 1..=1, ())
    {
      Ok(manager) => self.control.manager = Some(manager),
      Err(error) => tracing::warn!(
        %error,
        "the compositor lacks ext-data-control, so the clipboard is read only while a shell window has the keyboard"
      ),
    }
    if let Some(seat) = self.seats.seats().next() {
      self.add_control_device(&seat);
    }
  }

  /// Makes the watch read an image from a selection with no text, or stop.
  pub fn read_clipboard_images(&mut self, images: bool) {
    self.control.images = images;
  }

  /// Sets the clipboard to `bytes`, offered as each of `types`: through
  /// data control when the compositor has it, and otherwise
  /// `wl_data_device` with the latest input on one of the shell's
  /// windows, logged when there was none.
  pub fn set_clipboard(&mut self, types: &[&str], bytes: Vec<u8>) {
    let bytes: Rc<[u8]> = bytes.into();
    if let (Some(manager), Some(device)) = (&self.control.manager, &self.control.device) {
      let source = manager.create_data_source(&self.qh, ());
      for kind in types {
        source.offer((*kind).to_owned());
      }
      device.set_selection(Some(&source));
      if let Some((old, _)) = self.control.source.replace((source, bytes)) {
        old.destroy();
      }
      return;
    }
    let Some(((seat, serial, _), _)) = self.last_input.clone() else {
      tracing::warn!("nothing was copied: no input on the shell's windows has come yet");
      return;
    };
    self.offer_selection(&seat, serial, types, bytes);
  }

  /// Sets text as the clipboard's, as [`set_clipboard`](Self::set_clipboard)
  /// does.
  pub fn set_clipboard_text(&mut self, text: String) {
    self.set_clipboard(&TEXT_TYPES, text.into_bytes());
  }

  /// Gives `seat` the data-control device, once a watch asked and if none
  /// has one.
  pub(crate) fn add_control_device(&mut self, seat: &WlSeat) {
    let Some(manager) = &self.control.manager else {
      return;
    };
    if self.control.device.is_none() {
      self.control.device = Some(manager.get_data_device(seat, &self.qh, ()));
    }
  }

  /// Whether `wl_data_device` stands in for data control.
  pub(crate) fn clipboard_without_control(&self) -> bool {
    self.control.on_change.is_some() && self.control.manager.is_none()
  }

  /// Reads the new selection, which offers `types`, through `receive`, and
  /// tells the watch. A read still going for an older one is dropped.
  pub(crate) fn read_selection(
    &mut self,
    types: &[String],
    receive: impl FnOnce(String) -> std::io::Result<OwnedFd>,
  ) {
    if let Some(token) = self.control.reading.take() {
      self.loop_handle().remove(token);
    }
    let offers = |wanted: &[&str]| {
      let found = wanted.iter().find(|kind| types.iter().any(|t| t == *kind));
      found.map(|kind| (*kind).to_owned())
    };
    let secret = types.iter().any(|kind| kind == SECRET);
    let (kind, cap) = match (offers(&READ_TEXT), offers(&READ_IMAGE)) {
      (Some(text), _) => (text, MAX_TEXT),
      (None, Some(image)) if self.control.images && !secret => (image, MAX_IMAGE),
      _ => {
        self.report_selection(Selection {
          secret,
          ..Selection::default()
        });
        return;
      }
    };
    let pipe = match receive(kind.clone()) {
      Ok(pipe) => pipe,
      Err(error) => {
        tracing::warn!(%error, "couldn't read the clipboard");
        return;
      }
    };
    let text = cap == MAX_TEXT;
    self.control.reading = self.read_pipe(pipe, cap, move |platform, read| {
      platform.control.reading = None;
      let mut selection = Selection {
        secret,
        ..Selection::default()
      };
      match read {
        Received::Done(bytes) if text => {
          selection.text = Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        Received::Done(bytes) => selection.image = Some((kind, bytes)),
        Received::TooLarge if text => {
          if !std::mem::replace(&mut platform.control.warned_text, true) {
            tracing::warn!("the clipboard holds over 4 MiB of text, so it isn't read");
          }
        }
        Received::TooLarge => {
          if !std::mem::replace(&mut platform.control.warned_image, true) {
            tracing::warn!("the clipboard holds an image over 8 MiB, so it isn't read");
          }
        }
        Received::Failed => {}
      }
      platform.report_selection(selection);
    });
  }

  fn report_selection(&mut self, selection: Selection) {
    if let Some(on_change) = &mut self.control.on_change
      && let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| on_change(&selection)))
    {
      let message = crate::events::panic_message(payload.as_ref());
      tracing::error!("a clipboard watch panicked: {message}");
    }
  }
}

impl Dispatch<ExtDataControlManagerV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &ExtDataControlManagerV1,
    _: <ExtDataControlManagerV1 as Proxy>::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    device: &ExtDataControlDeviceV1,
    event: ext_data_control_device_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    match event {
      ext_data_control_device_v1::Event::Selection { id } => {
        if let Some(old) = std::mem::replace(&mut platform.control.offer, id.clone()) {
          old.destroy();
        }
        let types = id
          .as_ref()
          .and_then(|offer| offer.data::<Mutex<Vec<String>>>())
          .and_then(|types| types.lock().ok().map(|types| types.clone()))
          .unwrap_or_default();
        platform.read_selection(&types, |kind| {
          let (read, write) = std::io::pipe()?;
          if let Some(offer) = &id {
            offer.receive(kind, std::os::fd::AsFd::as_fd(&write));
          }
          Ok(read.into())
        });
      }
      // Fields read the primary selection through its own protocol.
      ext_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
      ext_data_control_device_v1::Event::Finished => {
        tracing::warn!("the compositor ended the clipboard's data control");
        device.destroy();
        platform.control.device = None;
      }
      _ => {}
    }
  }

  event_created_child!(Platform, ExtDataControlDeviceV1, [
    ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, Mutex::default()),
  ]);
}

impl Dispatch<ExtDataControlOfferV1, Mutex<Vec<String>>> for Platform {
  fn event(
    _: &mut Platform,
    _: &ExtDataControlOfferV1,
    event: ext_data_control_offer_v1::Event,
    types: &Mutex<Vec<String>>,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let ext_data_control_offer_v1::Event::Offer { mime_type } = event else {
      return;
    };
    if let Ok(mut types) = types.lock()
      && types.len() < MAX_TYPES
    {
      types.push(mime_type);
    }
  }
}

impl Dispatch<ExtDataControlSourceV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    source: &ExtDataControlSourceV1,
    event: ext_data_control_source_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let ours = platform
      .control
      .source
      .as_ref()
      .filter(|(ours, _)| ours == source);
    match event {
      ext_data_control_source_v1::Event::Send { fd, .. } => {
        if let Some((_, bytes)) = ours {
          let bytes = Rc::clone(bytes);
          platform.send(fd, bytes);
        }
      }
      ext_data_control_source_v1::Event::Cancelled => {
        source.destroy();
        if ours.is_some() {
          platform.control.source = None;
        }
      }
      _ => {}
    }
  }
}
