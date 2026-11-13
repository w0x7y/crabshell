//! The clipboard through `wl_data_device`: offering a field's copied text,
//! and reading what another program offers into a field. Both go through
//! pipes that the event loop watches only while text is moving, written
//! and read a pipe's worth at a time, so a slow program never blocks the
//! shell.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::OwnedFd;
use std::rc::Rc;

use calloop::generic::Generic;
use calloop::{Interest, Mode, PostAction};
use smithay_client_toolkit::data_device_manager::WritePipe;
use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::{
  CopyPasteSource, DataSourceHandler,
};
use smithay_client_toolkit::reexports::client::protocol::{
  wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_data_source::WlDataSource,
  wl_seat::WlSeat, wl_surface::WlSurface,
};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};

use crate::platform::Platform;
use crate::windows::WindowId;

/// The text types offered and asked for, best first.
const TEXT_TYPES: [&str; 5] = [
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

/// The text this shell offers as the clipboard's, with the source offering
/// it. Dropping it withdraws the offer.
pub(crate) struct Offered {
  source: CopyPasteSource,
  text: Rc<[u8]>,
}

impl Platform {
  /// Gives `seat` a data device, once the compositor offers them.
  pub(crate) fn add_data_device(&mut self, qh: &QueueHandle<Platform>, seat: &WlSeat) {
    let Some(manager) = &self.data_devices_manager else {
      return;
    };
    if self.data_devices.iter().any(|(ours, _)| ours == seat) {
      return;
    }
    let device = manager.get_data_device(qh, seat);
    self.data_devices.push((seat.clone(), device));
  }

  /// Drops `seat`'s data device, as the seat goes.
  pub(crate) fn drop_data_device(&mut self, seat: &WlSeat) {
    self.data_devices.retain(|(ours, _)| ours != seat);
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
    let (Some(manager), Some(device)) = (&self.data_devices_manager, self.data_device(seat)) else {
      tracing::warn!("the compositor offers no clipboard; nothing was copied");
      return;
    };
    let source = manager.create_copy_paste_source(&self.qh, TEXT_TYPES);
    source.set_selection(device, serial);
    self.offered = Some(Offered {
      source,
      text: text.into_bytes().into(),
    });
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
    let Some(kind) = offer.with_mime_types(|types| {
      TEXT_TYPES
        .into_iter()
        .find(|kind| types.iter().any(|offered| offered == kind))
    }) else {
      tracing::debug!("the clipboard holds no text");
      return;
    };
    let pipe = match offer.receive(kind.to_owned()) {
      Ok(pipe) => pipe,
      Err(error) => {
        tracing::warn!(%error, "couldn't read the clipboard");
        return;
      }
    };
    let file = File::from(OwnedFd::from(pipe));
    let mut text = Vec::new();
    let source = Generic::new(file, Interest::READ, Mode::Level);
    let inserted = self
      .loop_handle()
      .insert_source(source, move |_, file, platform| {
        let mut chunk = [0; CHUNK];
        // `&File` reads too, which the loop's shared handle allows.
        match (&**file).read(&mut chunk) {
          Ok(0) => {
            if let Some(entry) = platform.windows.get(id) {
              entry.window.paste(&String::from_utf8_lossy(&text));
            }
            Ok(PostAction::Remove)
          }
          Ok(read) if text.len() + read > MAX_PASTE => {
            tracing::warn!(
              bytes = MAX_PASTE,
              "the clipboard holds too much text to paste"
            );
            Ok(PostAction::Remove)
          }
          Ok(read) => {
            text.extend_from_slice(&chunk[..read]);
            Ok(PostAction::Continue)
          }
          Err(error) if error.kind() == ErrorKind::Interrupted => Ok(PostAction::Continue),
          Err(error) => {
            tracing::warn!(%error, "couldn't read the clipboard");
            Ok(PostAction::Remove)
          }
        }
      });
    if let Err(error) = inserted {
      tracing::warn!(%error, "couldn't watch the clipboard");
    }
  }

  /// Writes the offered text into `pipe` for another program, a chunk at
  /// a time.
  fn send(&mut self, pipe: WritePipe) {
    let Some(offered) = &self.offered else {
      return;
    };
    let text = Rc::clone(&offered.text);
    let mut written = 0;
    let file = File::from(OwnedFd::from(pipe));
    let source = Generic::new(file, Interest::WRITE, Mode::Level);
    let inserted = self.loop_handle().insert_source(source, move |_, file, _| {
      let end = (written + CHUNK).min(text.len());
      match (&**file).write(&text[written..end]) {
        Ok(count) => {
          written += count;
          Ok(if written >= text.len() {
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
    if self
      .offered
      .as_ref()
      .is_some_and(|offered| offered.source.inner() == source)
    {
      self.send(pipe);
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

  fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

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
