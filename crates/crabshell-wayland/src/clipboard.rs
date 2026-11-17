//! The clipboard through `wl_data_device`, and the primary selection
//! through `zwp_primary_selection_device_v1`: offering a field's copied or
//! selected text, and reading what another program offers into a field.
//! Both go through pipes that the event loop watches only while bytes are
//! moving, written and read a pipe's worth at a time, so a slow program
//! never blocks the shell.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::OwnedFd;
use std::rc::Rc;

use calloop::generic::Generic;
use calloop::{Interest, Mode, PostAction, RegistrationToken};
use smithay_client_toolkit::data_device_manager::WritePipe;
use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::{
  CopyPasteSource, DataSourceHandler,
};
use smithay_client_toolkit::primary_selection::PrimarySelectionManagerState;
use smithay_client_toolkit::primary_selection::device::{
  PrimarySelectionDevice, PrimarySelectionDeviceHandler,
};
use smithay_client_toolkit::primary_selection::selection::{
  PrimarySelectionSource, PrimarySelectionSourceHandler,
};
use smithay_client_toolkit::reexports::client::protocol::{
  wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_data_source::WlDataSource,
  wl_seat::WlSeat, wl_surface::WlSurface,
};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::primary_selection::zv1::client::{
  zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
  zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};

use crabshell_ui::Clipboard;

use crate::platform::Platform;
use crate::windows::WindowId;

/// The text types offered, best first.
pub(crate) const TEXT_TYPES: [&str; 5] = [
  "text/plain;charset=utf-8",
  "text/plain",
  "UTF8_STRING",
  "TEXT",
  "STRING",
];

/// The most text a paste reads; past that it is dropped.
const MAX_PASTE: usize = 1024 * 1024;

/// What one pipe write or read moves: a pipe takes at least this much
/// without blocking once it says it is ready.
const CHUNK: usize = 4096;

/// The bytes this shell offers as the clipboard's, with the source
/// offering them. Dropping it withdraws the offer.
pub(crate) struct Offered {
  source: CopyPasteSource,
  bytes: Rc<[u8]>,
}

/// The primary selection, which a middle click pastes.
pub(crate) struct Primary {
  /// Present when the compositor offers it.
  pub(crate) manager: Option<PrimarySelectionManagerState>,
  devices: Vec<(WlSeat, PrimarySelectionDevice)>,
  /// The text this shell offers as the primary selection.
  offered: Option<(PrimarySelectionSource, Rc<[u8]>)>,
  /// Whether its absence was logged.
  warned: bool,
}

impl Primary {
  pub(crate) fn new(manager: Option<PrimarySelectionManagerState>) -> Primary {
    Primary {
      manager,
      devices: Vec::new(),
      offered: None,
      warned: false,
    }
  }

  fn device(&self, seat: &WlSeat) -> Option<&PrimarySelectionDevice> {
    let found = self.devices.iter().find(|(ours, _)| ours == seat);
    found.map(|(_, device)| device)
  }
}

/// How a pipe's read ended.
pub(crate) enum Received {
  /// All of it, up to the cap.
  Done(Vec<u8>),
  /// More than the cap, so none of it.
  TooLarge,
  /// The read failed, as logged.
  Failed,
}

impl Platform {
  /// Gives `seat` a data device and a primary selection device, once the
  /// compositor offers them, and the data-control device once a config
  /// follows the clipboard.
  pub(crate) fn add_data_device(&mut self, qh: &QueueHandle<Platform>, seat: &WlSeat) {
    self.add_control_device(seat);
    if let Some(manager) = &self.primary.manager
      && self.primary.device(seat).is_none()
    {
      let device = manager.get_selection_device(qh, seat);
      self.primary.devices.push((seat.clone(), device));
    }
    let Some(manager) = &self.data_devices_manager else {
      return;
    };
    if self.data_devices.iter().any(|(ours, _)| ours == seat) {
      return;
    }
    let device = manager.get_data_device(qh, seat);
    self.data_devices.push((seat.clone(), device));
  }

  /// Drops `seat`'s data devices, as the seat goes.
  pub(crate) fn drop_data_device(&mut self, seat: &WlSeat) {
    self.data_devices.retain(|(ours, _)| ours != seat);
    self.primary.devices.retain(|(ours, _)| ours != seat);
  }

  fn data_device(&self, seat: &WlSeat) -> Option<&DataDevice> {
    self
      .data_devices
      .iter()
      .find(|(ours, _)| ours == seat)
      .map(|(_, device)| device)
  }

  /// Offers `text` as the clipboard's, for the key press `serial` on
  /// `seat`.
  pub(crate) fn copy(&mut self, seat: &WlSeat, serial: u32, text: String) {
    self.offer_selection(seat, serial, &TEXT_TYPES, text.into_bytes().into());
  }

  /// Offers `bytes` as the clipboard's, as each of `types`, for the input
  /// `serial` on `seat`.
  pub(crate) fn offer_selection(
    &mut self,
    seat: &WlSeat,
    serial: u32,
    types: &[&str],
    bytes: Rc<[u8]>,
  ) {
    let (Some(manager), Some(device)) = (&self.data_devices_manager, self.data_device(seat)) else {
      tracing::warn!("the compositor offers no clipboard; nothing was copied");
      return;
    };
    let source = manager.create_copy_paste_source(&self.qh, types.iter().copied());
    source.set_selection(device, serial);
    self.offered = Some(Offered { source, bytes });
  }

  /// Offers `text` as the primary selection, for the input `serial` on
  /// `seat`. Without the protocol, nothing happens, logged once.
  pub(crate) fn set_primary(&mut self, seat: &WlSeat, serial: u32, text: String) {
    let (Some(manager), Some(device)) = (&self.primary.manager, self.primary.device(seat)) else {
      self.warn_no_primary();
      return;
    };
    let source = manager.create_selection_source(&self.qh, TEXT_TYPES);
    source.set_selection(device, serial);
    self.primary.offered = Some((source, text.into_bytes().into()));
  }

  /// Does what a window asked of the primary selection after the input
  /// `serial` on `seat`.
  pub(crate) fn primary_asked(
    &mut self,
    asked: Option<Clipboard>,
    seat: &WlSeat,
    serial: u32,
    id: WindowId,
  ) {
    match asked {
      Some(Clipboard::Copy(text)) => self.set_primary(seat, serial, text),
      Some(Clipboard::Paste) => self.paste_primary(seat, id),
      None => {}
    }
  }

  fn warn_no_primary(&mut self) {
    if !std::mem::replace(&mut self.primary.warned, true) {
      tracing::warn!(
        "the compositor lacks the primary selection, so a middle click pastes nothing"
      );
    }
  }

  /// Reads the clipboard of `seat` and pastes it into the focused field
  /// of window `id`, once it has all come.
  pub(crate) fn paste(&mut self, seat: &WlSeat, id: WindowId) {
    let Some(offer) = self
      .data_device(seat)
      .and_then(|device| device.data().selection_offer())
    else {
      return;
    };
    let types = offer.with_mime_types(<[String]>::to_vec);
    self.paste_from(&types, id, |kind| {
      let pipe = offer.receive(kind);
      Ok(
        pipe
          .map_err(|error| std::io::Error::other(error.to_string()))?
          .into(),
      )
    });
  }

  /// Reads the primary selection of `seat` and pastes it into the focused
  /// field of window `id`, where a middle click put the caret.
  pub(crate) fn paste_primary(&mut self, seat: &WlSeat, id: WindowId) {
    if self.primary.manager.is_none() {
      self.warn_no_primary();
      return;
    }
    let Some(offer) = self
      .primary
      .device(seat)
      .and_then(|device| device.data().selection_offer())
    else {
      return;
    };
    let types = offer.with_mime_types(<[String]>::to_vec);
    self.paste_from(&types, id, |kind| Ok(offer.receive(kind)?.into()));
  }

  /// Reads the best text type of `types` through `receive` and pastes it
  /// into the focused field of window `id`.
  fn paste_from(
    &mut self,
    types: &[String],
    id: WindowId,
    receive: impl FnOnce(String) -> std::io::Result<OwnedFd>,
  ) {
    let Some(kind) = TEXT_TYPES
      .into_iter()
      .find(|kind| types.iter().any(|offered| offered == kind))
    else {
      tracing::debug!("the selection holds no text");
      return;
    };
    let pipe = match receive(kind.to_owned()) {
      Ok(pipe) => pipe,
      Err(error) => {
        tracing::warn!(%error, "couldn't read the selection");
        return;
      }
    };
    self.read_pipe(pipe, MAX_PASTE, move |platform, read| match read {
      Received::Done(text) => {
        if let Some(entry) = platform.windows.get(id) {
          entry.window.paste(&String::from_utf8_lossy(&text));
        }
      }
      Received::TooLarge => tracing::warn!(
        bytes = MAX_PASTE,
        "the selection holds too much text to paste"
      ),
      Received::Failed => {}
    });
  }

  /// Reads `pipe` to its end, a chunk at a time while the loop says it is
  /// ready, then calls `done`. More than `cap` bytes stops the read.
  pub(crate) fn read_pipe(
    &mut self,
    pipe: OwnedFd,
    cap: usize,
    done: impl FnOnce(&mut Platform, Received) + 'static,
  ) -> Option<RegistrationToken> {
    let mut bytes = Vec::new();
    let mut done = Some(done);
    let source = Generic::new(File::from(pipe), Interest::READ, Mode::Level);
    let inserted = self
      .loop_handle()
      .insert_source(source, move |_, file, platform| {
        let mut chunk = [0; CHUNK];
        // `&File` reads too, which the loop's shared handle allows.
        let read = match (&**file).read(&mut chunk) {
          Ok(0) => Received::Done(std::mem::take(&mut bytes)),
          Ok(read) if bytes.len() + read > cap => Received::TooLarge,
          Ok(read) => {
            bytes.extend_from_slice(&chunk[..read]);
            return Ok(PostAction::Continue);
          }
          Err(error) if error.kind() == ErrorKind::Interrupted => return Ok(PostAction::Continue),
          Err(error) => {
            tracing::warn!(%error, "couldn't read the selection");
            Received::Failed
          }
        };
        if let Some(done) = done.take() {
          done(platform, read);
        }
        Ok(PostAction::Remove)
      });
    match inserted {
      Ok(token) => Some(token),
      Err(error) => {
        tracing::warn!(%error, "couldn't watch the selection");
        None
      }
    }
  }

  /// Writes `bytes` into `pipe` for another program, a chunk at a time.
  pub(crate) fn send(&mut self, pipe: OwnedFd, bytes: Rc<[u8]>) {
    let mut written = 0;
    let source = Generic::new(File::from(pipe), Interest::WRITE, Mode::Level);
    let inserted = self.loop_handle().insert_source(source, move |_, file, _| {
      let end = (written + CHUNK).min(bytes.len());
      match (&**file).write(&bytes[written..end]) {
        Ok(count) => {
          written += count;
          Ok(if written >= bytes.len() {
            PostAction::Remove
          } else {
            PostAction::Continue
          })
        }
        Err(error) if error.kind() == ErrorKind::Interrupted => Ok(PostAction::Continue),
        // The reader went away, as `wl-paste | head` does.
        Err(_) => Ok(PostAction::Remove),
      }
    });
    if let Err(error) = inserted {
      tracing::warn!(%error, "couldn't watch a clipboard reader");
    }
  }
}

impl DataSourceHandler for Platform {
  fn accept_mime(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    _: &WlDataSource,
    _: Option<String>,
  ) {
  }

  fn send_request(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    source: &WlDataSource,
    _: String,
    pipe: WritePipe,
  ) {
    let Some(offered) = &self.offered else {
      return;
    };
    if offered.source.inner() == source {
      let bytes = Rc::clone(&offered.bytes);
      self.send(pipe.into(), bytes);
    }
  }

  /// Another program took the clipboard.
  fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
    if self
      .offered
      .as_ref()
      .is_some_and(|offered| offered.source.inner() == source)
    {
      self.offered = None;
    }
  }

  fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

  fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

  fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

/// Drags aren't taken, so their events are ignored.
impl DataDeviceHandler for Platform {
  fn enter(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    _: &WlDataDevice,
    _: f64,
    _: f64,
    _: &WlSurface,
  ) {
  }

  fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

  fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

  /// Without data control, a config's clipboard follows this, which the
  /// compositor tells only while a shell window has the keyboard.
  fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
    if !self.clipboard_without_control() {
      return;
    }
    let found = self
      .data_devices
      .iter()
      .find(|(_, ours)| ours.inner() == device);
    let offer = found.and_then(|(_, ours)| ours.data().selection_offer());
    let types = offer
      .as_ref()
      .map(|offer| offer.with_mime_types(<[String]>::to_vec));
    self.read_selection(&types.unwrap_or_default(), |kind| {
      let offer = offer.ok_or_else(|| std::io::Error::other("no offer"))?;
      let pipe = offer.receive(kind);
      Ok(
        pipe
          .map_err(|error| std::io::Error::other(error.to_string()))?
          .into(),
      )
    });
  }

  fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for Platform {
  fn source_actions(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    _: &mut DragOffer,
    _: DndAction,
  ) {
  }

  fn selected_action(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    _: &mut DragOffer,
    _: DndAction,
  ) {
  }
}

/// The offer is kept on the device, which a middle click reads.
impl PrimarySelectionDeviceHandler for Platform {
  fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ZwpPrimarySelectionDeviceV1) {}
}

impl PrimarySelectionSourceHandler for Platform {
  fn send_request(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    source: &ZwpPrimarySelectionSourceV1,
    _: String,
    pipe: WritePipe,
  ) {
    let Some((ours, bytes)) = &self.primary.offered else {
      return;
    };
    if ours.inner() == source {
      let bytes = Rc::clone(bytes);
      self.send(pipe.into(), bytes);
    }
  }

  /// Another selection took its place.
  fn cancelled(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    source: &ZwpPrimarySelectionSourceV1,
  ) {
    if let Some((ours, _)) = &self.primary.offered
      && ours.inner() == source
    {
      self.primary.offered = None;
    }
  }
}
