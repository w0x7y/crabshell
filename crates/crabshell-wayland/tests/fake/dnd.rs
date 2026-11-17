//! The fake's `wl_data_device_manager`, version 3: data sources, devices,
//! the drag a client starts, and the offers of a drag over a surface,
//! made from the client's drag or from another program's bytes. It logs
//! `start_drag <serial> <types>`, `accept <type|none>`, `receive <type>`,
//! `finish`, `drop`, `cancel` and `source destroyed`.

use std::io::Write;
use std::os::fd::AsFd;
use std::sync::Mutex;

use wayland_server::protocol::{
  wl_data_device::{self, WlDataDevice},
  wl_data_device_manager::{self, DndAction, WlDataDeviceManager},
  wl_data_offer::{self, WlDataOffer},
  wl_data_source::{self, WlDataSource},
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource};

use super::State;

/// The fake's drag state.
#[derive(Default)]
pub struct Dnd {
  devices: Vec<WlDataDevice>,
  /// The source of the drag the client started.
  started: Option<WlDataSource>,
  /// The offer of the drag over a surface.
  over: Option<WlDataOffer>,
}

/// Where an offer's bytes come from.
enum From {
  Client(WlDataSource),
  Program(Vec<u8>),
}

struct Offer {
  from: From,
  accepted: Mutex<Option<String>>,
}

impl State {
  /// A drag enters layer surface `index` at `at`: the client's own, or
  /// another program's offering `types`, each sending `bytes`.
  pub fn drag_enter(&mut self, index: usize, at: (f64, f64), program: Option<(&[&str], Vec<u8>)>) {
    let surface = self.layer(index);
    let serial = self.serial();
    let device = self.dnd.devices.first().expect("a data device").clone();
    let (from, types) = match program {
      Some((types, bytes)) => (
        From::Program(bytes),
        types.iter().map(|t| t.to_string()).collect(),
      ),
      None => {
        let source = self.dnd.started.clone().expect("a drag the client started");
        let types = source
          .data::<Mutex<Vec<String>>>()
          .expect("a source's types");
        let types = types.lock().expect("types").clone();
        (From::Client(source), types)
      }
    };
    let client = device.client().expect("the client");
    let handle = DisplayHandle::from(device.handle().upgrade().expect("the display"));
    let offer = Offer {
      from,
      accepted: Mutex::new(None),
    };
    let offer = client
      .create_resource::<WlDataOffer, Offer, State>(&handle, device.version(), offer)
      .expect("an offer");
    device.data_offer(&offer);
    for kind in types {
      offer.offer(kind);
    }
    offer.source_actions(DndAction::Copy);
    device.enter(serial, &surface, at.0, at.1, Some(&offer));
    self.dnd.over = Some(offer);
  }

  /// The drag moves to `at`.
  pub fn drag_motion(&mut self, at: (f64, f64)) {
    for device in &self.dnd.devices {
      device.motion(3, at.0, at.1);
    }
  }

  /// The drag is let go: dropped where an offer was accepted, as
  /// wlroots does, and cancelled otherwise; then it leaves.
  pub fn drag_drop(&mut self) {
    let offer = self.dnd.over.take().expect("a drag over a surface");
    let data = offer.data::<Offer>().expect("an offer's data");
    let accepted = data.accepted.lock().expect("accepted").is_some();
    let source = match &data.from {
      From::Client(source) => Some(source),
      From::Program(_) => None,
    };
    self.log(String::from(if accepted { "drop" } else { "cancel" }));
    for device in &self.dnd.devices {
      if accepted {
        device.drop();
      }
      device.leave();
    }
    match (source, accepted) {
      (Some(source), true) => source.dnd_drop_performed(),
      (Some(source), false) => source.cancelled(),
      _ => {}
    }
  }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
  fn request(
    state: &mut State,
    _: &Client,
    _: &WlDataDeviceManager,
    request: wl_data_device_manager::Request,
    _: &(),
    _: &DisplayHandle,
    init: &mut DataInit<'_, State>,
  ) {
    match request {
      wl_data_device_manager::Request::CreateDataSource { id } => {
        init.init(id, Mutex::new(Vec::<String>::new()));
      }
      wl_data_device_manager::Request::GetDataDevice { id, .. } => {
        state.dnd.devices.push(init.init(id, ()));
      }
      _ => {}
    }
  }
}

impl Dispatch<WlDataSource, Mutex<Vec<String>>> for State {
  fn request(
    state: &mut State,
    _: &Client,
    source: &WlDataSource,
    request: wl_data_source::Request,
    types: &Mutex<Vec<String>>,
    _: &DisplayHandle,
    _: &mut DataInit<'_, State>,
  ) {
    match request {
      wl_data_source::Request::Offer { mime_type } => types.lock().expect("types").push(mime_type),
      wl_data_source::Request::Destroy if state.dnd.started.as_ref() == Some(source) => {
        state.dnd.started = None;
        state.log(String::from("source destroyed"));
      }
      _ => {}
    }
  }
}

impl Dispatch<WlDataDevice, ()> for State {
  fn request(
    state: &mut State,
    _: &Client,
    _: &WlDataDevice,
    request: wl_data_device::Request,
    _: &(),
    _: &DisplayHandle,
    _: &mut DataInit<'_, State>,
  ) {
    if let wl_data_device::Request::StartDrag { source, serial, .. } = request {
      let source = source.expect("a drag with a source");
      let types = source
        .data::<Mutex<Vec<String>>>()
        .expect("a source's types");
      let types = types.lock().expect("types").join(",");
      state.log(format!("start_drag {serial} {types}"));
      state.dnd.started = Some(source);
    }
  }
}

impl Dispatch<WlDataOffer, Offer> for State {
  fn request(
    state: &mut State,
    _: &Client,
    _: &WlDataOffer,
    request: wl_data_offer::Request,
    offer: &Offer,
    _: &DisplayHandle,
    _: &mut DataInit<'_, State>,
  ) {
    match request {
      wl_data_offer::Request::Accept { mime_type, .. } => {
        state.log(format!("accept {}", mime_type.as_deref().unwrap_or("none")));
        *offer.accepted.lock().expect("accepted") = mime_type;
      }
      wl_data_offer::Request::Receive { mime_type, fd } => {
        state.log(format!("receive {mime_type}"));
        match &offer.from {
          From::Client(source) => source.send(mime_type, fd.as_fd()),
          From::Program(bytes) => {
            let bytes = bytes.clone();
            // A large write would block the fake until the client reads.
            std::thread::spawn(move || {
              let _ = std::fs::File::from(fd).write_all(&bytes);
            });
          }
        }
      }
      wl_data_offer::Request::Finish => {
        state.log(String::from("finish"));
        if let From::Client(source) = &offer.from {
          source.dnd_finished();
        }
      }
      _ => {}
    }
  }
}
