//! The fake's `wl_data_device_manager`, version 3: data sources, devices,
//! the drag a client starts, and the offers of a drag over a surface,
//! made from the client's drag or from another program's bytes. It logs
//! `start_drag <serial> <types>`, `accept <type|none>`, `receive <type>`,
//! `finish`, `drop`, `cancel` and `source destroyed`. A program's offer
//! can hold its pipe open, as a stuck program would.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use wayland_server::protocol::{
  wl_data_device::{self, WlDataDevice},
  wl_data_device_manager::{self, DndAction, WlDataDeviceManager},
  wl_data_offer::{self, WlDataOffer},
  wl_data_source::{self, WlDataSource},
  wl_pointer,
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
  /// The pipes a stuck program never closes.
  held: Vec<std::fs::File>,
}

/// Where an offer's bytes come from.
enum From {
  Client(WlDataSource),
  /// Another program's bytes, and whether it keeps its pipe open after.
  Program(Vec<u8>, bool),
}

struct Offer {
  from: From,
  accepted: Mutex<Option<String>>,
}

impl State {
  /// A left press at `from` on layer surface `index`, its serial logged
  /// as `press <serial>`, then a move to `to`, outside it as a held
  /// button's motion goes.
  pub fn press_out(&mut self, index: usize, from: (f64, f64), to: (f64, f64)) {
    const LEFT: u32 = 0x110;
    let surface = self.layer(index);
    let (enter, press) = (self.serial(), self.serial());
    self.log(format!("press {press}"));
    for pointer in &self.pointers {
      pointer.enter(enter, &surface, from.0, from.1);
      pointer.frame();
      pointer.button(press, 1, LEFT, wl_pointer::ButtonState::Pressed);
      pointer.frame();
      pointer.motion(2, to.0, to.1);
      pointer.frame();
    }
  }

  /// One finger down at `from` on layer surface `index`, its serial
  /// logged as `press <serial>`, then moved to `to`, outside it.
  pub fn touch_out(&mut self, index: usize, from: (f64, f64), to: (f64, f64)) {
    let surface = self.layer(index);
    let down = self.serial();
    self.log(format!("press {down}"));
    for touch in &self.touches {
      touch.down(down, 1, &surface, 0, from.0, from.1);
      touch.frame();
      touch.motion(2, 0, to.0, to.1);
      touch.frame();
    }
  }

  /// The pointer leaves layer surface `index`, as a drag taking it over
  /// makes it.
  pub fn pointer_leave(&mut self, index: usize) {
    let surface = self.layer(index);
    let serial = self.serial();
    for pointer in &self.pointers {
      pointer.leave(serial, &surface);
      pointer.frame();
    }
  }

  /// A drag enters layer surface `index` at `at`: the client's own, or
  /// another program's offering `types`, each sending `bytes`.
  pub fn drag_enter(&mut self, index: usize, at: (f64, f64), program: Option<(&[&str], Vec<u8>)>) {
    self.drag_enter_from(index, at, program, false);
  }

  /// As [`drag_enter`](Self::drag_enter), from a program that never
  /// closes its pipe after its bytes.
  pub fn stuck_drag_enter(&mut self, index: usize, at: (f64, f64), kind: &str, bytes: Vec<u8>) {
    self.drag_enter_from(index, at, Some((&[kind], bytes)), true);
  }

  fn drag_enter_from(
    &mut self,
    index: usize,
    at: (f64, f64),
    program: Option<(&[&str], Vec<u8>)>,
    stuck: bool,
  ) {
    let surface = self.layer(index);
    let serial = self.serial();
    let device = self.dnd.devices.first().expect("a data device").clone();
    let (from, types) = match program {
      Some((types, bytes)) => (
        From::Program(bytes, stuck),
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
    self.drag_drop_in_order(false);
  }

  /// As [`drag_drop`](Self::drag_drop), but the client's source hears
  /// `dnd_drop_performed` before its device hears the drop.
  pub fn drag_drop_source_first(&mut self) {
    self.drag_drop_in_order(true);
  }

  fn drag_drop_in_order(&mut self, source_first: bool) {
    let offer = self.dnd.over.take().expect("a drag over a surface");
    let data = offer.data::<Offer>().expect("an offer's data");
    let accepted = data.accepted.lock().expect("accepted").is_some();
    let source = match &data.from {
      From::Client(source) => Some(source),
      From::Program(..) => None,
    };
    self.log(String::from(if accepted { "drop" } else { "cancel" }));
    let performed = || {
      if let Some(source) = source.filter(|_| accepted) {
        source.dnd_drop_performed();
      }
    };
    if source_first {
      performed();
    }
    for device in &self.dnd.devices {
      if accepted {
        device.drop();
      }
      device.leave();
    }
    if !source_first {
      performed();
    }
    if let Some(source) = source.filter(|_| !accepted) {
      source.cancelled();
    }
  }

  /// Whether the client let go of the stuck program's pipe: a write into
  /// it then finds no reader.
  pub fn stuck_pipe_closed(&mut self) -> bool {
    let pipe = self.dnd.held.first_mut().expect("a stuck drop");
    pipe.write_all(b"x").is_err()
  }

  /// Another program over no shell window asks the client's drag for
  /// `kind` and gets what it sends.
  pub fn read_started(&mut self, kind: &str) -> UnixStream {
    let source = self
      .dnd
      .started
      .as_ref()
      .expect("a drag the client started");
    let (ours, theirs) = UnixStream::pair().expect("a socket pair");
    source.send(kind.to_owned(), theirs.as_fd());
    ours
  }

  /// That program took the drop and never finished.
  pub fn drop_started(&mut self) {
    let source = self
      .dnd
      .started
      .as_ref()
      .expect("a drag the client started");
    source.dnd_drop_performed();
  }

  /// That program took the drop and finished.
  pub fn take_started(&mut self) {
    let source = self
      .dnd
      .started
      .as_ref()
      .expect("a drag the client started");
    source.dnd_drop_performed();
    source.dnd_finished();
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
          From::Program(bytes, true) => {
            let mut pipe = std::fs::File::from(fd);
            pipe.write_all(bytes).expect("a small write fits the pipe");
            state.dnd.held.push(pipe);
          }
          From::Program(bytes, false) => {
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
