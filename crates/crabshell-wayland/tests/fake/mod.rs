//! A fake compositor for tests: the smallest server the platform can run
//! a panel on, in a thread of the test, over a socket pair. It needs no
//! Sway, GPU, root or session.
//!
//! It offers `wl_compositor` 4, `wl_shm`, `xdg_wm_base`, layer-shell and
//! one output, `FAKE-1`, at the scale it is started with, and each extra
//! global a test asks for: a second output described a read later, a seat
//! with touch, the background effect manager, or workspaces. Each layer surface is configured at its first
//! commit, at the size it asked for or the output's, and each frame
//! callback is answered at the next commit, so windows draw freely. The
//! requests a test checks are written to a log as short lines, such as
//! `blur 0,0,200,30` or `deactivate two`.

use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use wayland_protocols::ext::background_effect::v1::server::{
  ext_background_effect_manager_v1::{self, Capability, ExtBackgroundEffectManagerV1},
  ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};
use wayland_protocols::ext::workspace::v1::server::{
  ext_workspace_group_handle_v1::{ExtWorkspaceGroupHandleV1, GroupCapabilities},
  ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1, WorkspaceCapabilities},
  ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};
use wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
use wayland_protocols_wlr::layer_shell::v1::server::{
  zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
  zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};
use wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use wayland_server::protocol::{
  wl_buffer::WlBuffer,
  wl_callback::WlCallback,
  wl_compositor::{self, WlCompositor},
  wl_output::{self, WlOutput},
  wl_region::{self, WlRegion},
  wl_seat::{self, WlSeat},
  wl_shm::{self, WlShm},
  wl_shm_pool::{self, WlShmPool},
  wl_surface::{self, WlSurface},
  wl_touch::WlTouch,
};
use wayland_server::{
  Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
};

/// The output's size in logical pixels.
pub const OUTPUT: (i32, i32) = (640, 480);

/// The globals a test adds to the base ones.
#[derive(Clone, Default)]
pub struct Globals {
  /// The output's whole scale; 0 counts as 1.
  pub scale: i32,
  /// A second output, `FAKE-2`, whose description is sent 100 ms after
  /// the first's, in a read of its own, as a slow compositor's may be.
  pub late_output: bool,
  /// A seat with a touchscreen.
  pub touch: bool,
  /// `ext_background_effect_manager_v1`, and whether it says it can blur
  /// as soon as it is bound.
  pub effects: Option<bool>,
  /// `ext_workspace_manager_v1`, with one group of these workspaces: a
  /// name and what it takes.
  pub workspaces: Option<Vec<(&'static str, WorkspaceCapabilities)>>,
}

/// Something the test runs on the fake's thread, with its state.
type Command = Box<dyn FnOnce(&mut State) + Send>;

/// A running fake. Dropping it stops the thread.
pub struct Fake {
  log: Arc<Mutex<Vec<String>>>,
  commands: Option<Sender<Command>>,
  thread: Option<JoinHandle<()>>,
}

impl Fake {
  /// Starts a fake with `globals`, and returns it with the client's end
  /// of its socket.
  pub fn start(globals: Globals) -> (Fake, UnixStream) {
    let (ours, theirs) = UnixStream::pair().expect("a socket pair");
    let log = Arc::new(Mutex::new(Vec::new()));
    let (commands, received) = mpsc::channel();
    let state = State {
      log: Arc::clone(&log),
      globals,
      outputs: Vec::new(),
      late: Vec::new(),
      surfaces: Vec::new(),
      touches: Vec::new(),
      effects: Vec::new(),
      serial: 0,
      copying: false,
    };
    let thread = std::thread::spawn(move || serve(ours, state, received));
    let fake = Fake {
      log,
      commands: Some(commands),
      thread: Some(thread),
    };
    (fake, theirs)
  }

  /// The lines logged so far.
  pub fn log(&self) -> Vec<String> {
    self.log.lock().expect("the log").clone()
  }

  /// Empties the log.
  pub fn clear(&self) {
    self.log.lock().expect("the log").clear();
  }

  /// Runs `command` on the fake's thread, soon.
  pub fn run(&self, command: impl FnOnce(&mut State) + Send + 'static) {
    let commands = self.commands.as_ref().expect("the fake runs");
    commands.send(Box::new(command)).expect("the fake runs");
  }
}

impl Drop for Fake {
  fn drop(&mut self) {
    // The thread stops once its channel closes.
    self.commands.take();
    if let Some(thread) = self.thread.take() {
      let _ = thread.join();
    }
  }
}

/// Dispatches the client and runs the test's commands until the test
/// drops its [`Fake`] or the client goes.
fn serve(stream: UnixStream, mut state: State, commands: Receiver<Command>) {
  let mut display: Display<State> = Display::new().expect("a display");
  let mut handle = display.handle();
  let scale = state.globals.scale.max(1);
  handle.create_global::<State, WlCompositor, ()>(4, ());
  handle.create_global::<State, WlShm, ()>(1, ());
  handle.create_global::<State, XdgWmBase, ()>(1, ());
  handle.create_global::<State, ZwlrLayerShellV1, ()>(4, ());
  handle.create_global::<State, WlOutput, (i32, bool)>(4, (scale, false));
  if state.globals.late_output {
    handle.create_global::<State, WlOutput, (i32, bool)>(4, (scale, true));
  }
  if state.globals.touch {
    handle.create_global::<State, WlSeat, ()>(7, ());
  }
  if let Some(now) = state.globals.effects {
    handle.create_global::<State, ExtBackgroundEffectManagerV1, bool>(1, now);
  }
  if state.globals.workspaces.is_some() {
    handle.create_global::<State, ExtWorkspaceManagerV1, ()>(1, ());
  }
  handle
    .insert_client(stream, Arc::new(Gone))
    .expect("the client");
  loop {
    loop {
      match commands.try_recv() {
        Ok(command) => command(&mut state),
        Err(mpsc::TryRecvError::Empty) => break,
        Err(mpsc::TryRecvError::Disconnected) => return,
      }
    }
    // Before the client's requests, so a sync it sent meanwhile is done
    // after this description.
    for output in std::mem::take(&mut state.late) {
      describe(&output, scale, "FAKE-2");
      state.outputs.push(output);
    }
    if display.dispatch_clients(&mut state).is_err() {
      return;
    }
    let _ = display.flush_clients();
    let pause = if state.late.is_empty() { 1 } else { 100 };
    std::thread::sleep(Duration::from_millis(pause));
  }
}

/// The client's data, which nothing reads.
struct Gone;

impl ClientData for Gone {
  fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

/// What the fake keeps of the client's objects.
pub struct State {
  log: Arc<Mutex<Vec<String>>>,
  globals: Globals,
  outputs: Vec<WlOutput>,
  /// `FAKE-2`, bound and not described yet.
  late: Vec<WlOutput>,
  /// Every surface, in the order made.
  surfaces: Vec<WlSurface>,
  touches: Vec<WlTouch>,
  effects: Vec<ExtBackgroundEffectManagerV1>,
  serial: u32,
  /// Whether a buffer is released as soon as it is committed, as a
  /// compositor that copies it does; otherwise at the next commit.
  copying: bool,
}

impl State {
  fn log(&self, line: String) {
    self.log.lock().expect("the log").push(line);
  }

  fn serial(&mut self) -> u32 {
    self.serial += 1;
    self.serial
  }

  /// The first surface with a layer surface, the panel's.
  fn panel(&self) -> WlSurface {
    let panel = self.surfaces.iter().find(|surface| {
      let data = surface.data::<Mutex<Surface>>().expect("a surface's data");
      data.lock().expect("a surface").layer.is_some()
    });
    panel.expect("a panel").clone()
  }

  /// Releases every buffer shown, and from now on each as it is
  /// committed, as a compositor that copies them does.
  pub fn copy_buffers(&mut self) {
    self.copying = true;
    for surface in &self.surfaces {
      let data = surface.data::<Mutex<Surface>>().expect("a surface's data");
      if let Some(shown) = data.lock().expect("a surface").shown.take() {
        shown.release();
      }
    }
  }

  /// Says the compositor can blur now.
  pub fn blur_on(&mut self) {
    for manager in &self.effects {
      manager.capabilities(Capability::Blur);
    }
  }

  /// One finger down at `x`, `y` on the panel, moved by `dx`, then lifted,
  /// or cancelled instead if `cancel`.
  pub fn tap(&mut self, (x, y): (f64, f64), dx: f64, cancel: bool) {
    let surface = self.panel();
    let (down, up) = (self.serial(), self.serial());
    for touch in &self.touches {
      touch.down(down, 1, &surface, 0, x, y);
      touch.frame();
      touch.motion(2, 0, x + dx, y);
      touch.frame();
      if cancel {
        touch.cancel();
      } else {
        touch.up(up, 3, 0);
        touch.frame();
      }
    }
  }
}

/// What a surface holds between commits.
#[derive(Default)]
struct Surface {
  buffer: Option<WlBuffer>,
  shown: Option<WlBuffer>,
  callbacks: Vec<WlCallback>,
  layer: Option<ZwlrLayerSurfaceV1>,
  configured: bool,
}

/// No requests to answer, or none the tests need.
macro_rules! ignore {
  ($($interface:ty: $data:ty),* $(,)?) => {$(
    impl Dispatch<$interface, $data> for State {
      fn request(
        _: &mut State,
        _: &Client,
        _: &$interface,
        _: <$interface as Resource>::Request,
        _: &$data,
        _: &DisplayHandle,
        _: &mut DataInit<'_, State>,
      ) {
      }
    }
  )*};
}

ignore!(
  WlCallback: (),
  WlBuffer: (),
  XdgWmBase: (),
  WlOutput: (),
  WlTouch: (),
  ExtWorkspaceGroupHandleV1: (),
);

/// Binds a global with no events to send, giving it `()` as its data.
macro_rules! plain_global {
  ($($interface:ty),*) => {$(
    impl GlobalDispatch<$interface, ()> for State {
      fn bind(
        _: &mut State,
        _: &DisplayHandle,
        _: &Client,
        resource: New<$interface>,
        _: &(),
        init: &mut DataInit<'_, State>,
      ) {
        init.init(resource, ());
      }
    }
  )*};
}

plain_global!(WlCompositor, XdgWmBase, ZwlrLayerShellV1);

impl GlobalDispatch<WlShm, ()> for State {
  fn bind(
    _: &mut State,
    _: &DisplayHandle,
    _: &Client,
    resource: New<WlShm>,
    _: &(),
    init: &mut DataInit<'_, State>,
  ) {
    let shm = init.init(resource, ());
    shm.format(wl_shm::Format::Argb8888);
    shm.format(wl_shm::Format::Xrgb8888);
  }
}

impl Dispatch<WlShm, ()> for State {
  fn request(
    _: &mut State,
    _: &Client,
    _: &WlShm,
    request: wl_shm::Request,
    _: &(),
    _: &DisplayHandle,
    init: &mut DataInit<'_, State>,
  ) {
    if let wl_shm::Request::CreatePool { id, .. } = request {
      // The pixels are never read, so the memory is let go at once.
      init.init(id, ());
    }
  }
}

impl Dispatch<WlShmPool, ()> for State {
  fn request(
    _: &mut State,
    _: &Client,
    _: &WlShmPool,
    request: wl_shm_pool::Request,
    _: &(),
    _: &DisplayHandle,
    init: &mut DataInit<'_, State>,
  ) {
    if let wl_shm_pool::Request::CreateBuffer { id, .. } = request {
      init.init(id, ());
    }
  }
}

/// An output's scale, and whether it is `FAKE-2`, described a turn later.
impl GlobalDispatch<WlOutput, (i32, bool)> for State {
  fn bind(
    state: &mut State,
    _: &DisplayHandle,
    _: &Client,
    resource: New<WlOutput>,
    &(scale, late): &(i32, bool),
    init: &mut DataInit<'_, State>,
  ) {
    let output = init.init(resource, ());
    if late {
      state.late.push(output);
    } else {
      describe(&output, scale, "FAKE-1");
      state.outputs.push(output);
    }
  }
}

/// Sends `output`'s description, named `name`.
fn describe(output: &WlOutput, scale: i32, name: &str) {
  let (width, height) = OUTPUT;
  output.geometry(
    0,
    0,
    0,
    0,
    wl_output::Subpixel::Unknown,
    String::from("Fake"),
    String::from("Fake"),
    wl_output::Transform::Normal,
  );
  output.mode(
    wl_output::Mode::Current,
    width * scale,
    height * scale,
    60_000,
  );
  output.scale(scale);
  output.name(String::from(name));
  output.done();
}

impl Dispatch<WlCompositor, ()> for State {
  fn request(
    state: &mut State,
    _: &Client,
    _: &WlCompositor,
    request: wl_compositor::Request,
    _: &(),
    _: &DisplayHandle,
    init: &mut DataInit<'_, State>,
  ) {
    match request {
      wl_compositor::Request::CreateSurface { id } => {
        let surface = init.init(id, Mutex::new(Surface::default()));
        state.surfaces.push(surface);
      }
      wl_compositor::Request::CreateRegion { id } => {
        init.init(id, Mutex::new(Vec::<[i32; 4]>::new()));
      }
      _ => {}
    }
  }
}

impl Dispatch<WlRegion, Mutex<Vec<[i32; 4]>>> for State {
  fn request(
    _: &mut State,
    _: &Client,
    _: &WlRegion,
    request: wl_region::Request,
    rects: &Mutex<Vec<[i32; 4]>>,
    _: &DisplayHandle,
    _: &mut DataInit<'_, State>,
  ) {
    if let wl_region::Request::Add {
      x,
      y,
      width,
      height,
    } = request
    {
      rects.lock().expect("a region").push([x, y, width, height]);
    }
  }
}

impl Dispatch<WlSurface, Mutex<Surface>> for State {
  fn request(
    state: &mut State,
    _: &Client,
    surface: &WlSurface,
    request: wl_surface::Request,
    data: &Mutex<Surface>,
    _: &DisplayHandle,
    init: &mut DataInit<'_, State>,
  ) {
    let mut ours = data.lock().expect("a surface");
    match request {
      wl_surface::Request::Attach { buffer, .. } => ours.buffer = buffer,
      wl_surface::Request::Frame { callback } => {
        let callback = init.init(callback, ());
        ours.callbacks.push(callback);
      }
      wl_surface::Request::Commit => {
        if let Some(layer) = ours.layer.clone()
          && !std::mem::replace(&mut ours.configured, true)
        {
          let size = layer
            .data::<Mutex<(u32, u32)>>()
            .map(|size| *size.lock().expect("a size"))
            .unwrap_or_default();
          let (width, height) = OUTPUT;
          let width = if size.0 == 0 { width as u32 } else { size.0 };
          let height = if size.1 == 0 { height as u32 } else { size.1 };
          for output in &state.outputs {
            surface.enter(output);
          }
          layer.configure(state.serial(), width, height);
        }
        if let Some(buffer) = ours.buffer.take()
          && let Some(old) = ours.shown.replace(buffer)
        {
          old.release();
        }
        if state.copying
          && let Some(copied) = ours.shown.take()
        {
          copied.release();
        }
        for callback in ours.callbacks.drain(..) {
          callback.done(0);
        }
      }
      _ => {}
    }
  }
}

impl Dispatch<ZwlrLayerShellV1, ()> for State {
  fn request(
    _: &mut State,
    _: &Client,
    _: &ZwlrLayerShellV1,
    request: zwlr_layer_shell_v1::Request,
    _: &(),
    _: &DisplayHandle,
    init: &mut DataInit<'_, State>,
  ) {
    if let zwlr_layer_shell_v1::Request::GetLayerSurface { id, surface, .. } = request {
      let layer = init.init(id, Mutex::new((0_u32, 0_u32)));
      let data = surface.data::<Mutex<Surface>>().expect("a surface's data");
      data.lock().expect("a surface").layer = Some(layer);
    }
  }
}

impl Dispatch<ZwlrLayerSurfaceV1, Mutex<(u32, u32)>> for State {
  fn request(
    _: &mut State,
    _: &Client,
    _: &ZwlrLayerSurfaceV1,
    request: zwlr_layer_surface_v1::Request,
    size: &Mutex<(u32, u32)>,
    _: &DisplayHandle,
    _: &mut DataInit<'_, State>,
  ) {
    if let zwlr_layer_surface_v1::Request::SetSize { width, height } = request {
      *size.lock().expect("a size") = (width, height);
    }
  }
}

impl GlobalDispatch<WlSeat, ()> for State {
  fn bind(
    _: &mut State,
    _: &DisplayHandle,
    _: &Client,
    resource: New<WlSeat>,
    _: &(),
    init: &mut DataInit<'_, State>,
  ) {
    let seat = init.init(resource, ());
    seat.capabilities(wl_seat::Capability::Touch);
    seat.name(String::from("seat0"));
  }
}

impl Dispatch<WlSeat, ()> for State {
  fn request(
    state: &mut State,
    _: &Client,
    _: &WlSeat,
    request: wl_seat::Request,
    _: &(),
    _: &DisplayHandle,
    init: &mut DataInit<'_, State>,
  ) {
    if let wl_seat::Request::GetTouch { id } = request {
      state.touches.push(init.init(id, ()));
    }
  }
}

impl GlobalDispatch<ExtBackgroundEffectManagerV1, bool> for State {
  fn bind(
    state: &mut State,
    _: &DisplayHandle,
    _: &Client,
    resource: New<ExtBackgroundEffectManagerV1>,
    now: &bool,
    init: &mut DataInit<'_, State>,
  ) {
    let manager = init.init(resource, ());
    let can = if *now {
      Capability::Blur
    } else {
      Capability::empty()
    };
    manager.capabilities(can);
    state.effects.push(manager);
  }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for State {
  fn request(
    _: &mut State,
    _: &Client,
    _: &ExtBackgroundEffectManagerV1,
    request: ext_background_effect_manager_v1::Request,
    _: &(),
    _: &DisplayHandle,
    init: &mut DataInit<'_, State>,
  ) {
    if let ext_background_effect_manager_v1::Request::GetBackgroundEffect { id, .. } = request {
      init.init(id, ());
    }
  }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, ()> for State {
  fn request(
    state: &mut State,
    _: &Client,
    _: &ExtBackgroundEffectSurfaceV1,
    request: ext_background_effect_surface_v1::Request,
    _: &(),
    _: &DisplayHandle,
    _: &mut DataInit<'_, State>,
  ) {
    if let ext_background_effect_surface_v1::Request::SetBlurRegion { region } = request {
      let rects = region.map(|region| {
        let rects = region.data::<Mutex<Vec<[i32; 4]>>>().expect("a region");
        let rects = rects.lock().expect("a region");
        let rects = rects
          .iter()
          .map(|rect| rect.map(|n| n.to_string()).join(","));
        rects.collect::<Vec<_>>().join(" ")
      });
      state.log(format!("blur {}", rects.as_deref().unwrap_or("none")));
    }
  }
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for State {
  fn bind(
    state: &mut State,
    handle: &DisplayHandle,
    client: &Client,
    resource: New<ExtWorkspaceManagerV1>,
    _: &(),
    init: &mut DataInit<'_, State>,
  ) {
    let manager = init.init(resource, ());
    let group = client
      .create_resource::<ExtWorkspaceGroupHandleV1, (), State>(handle, 1, ())
      .expect("a group");
    manager.workspace_group(&group);
    group.capabilities(GroupCapabilities::empty());
    for (name, can) in state.globals.workspaces.clone().unwrap_or_default() {
      let workspace = client
        .create_resource::<ExtWorkspaceHandleV1, &'static str, State>(handle, 1, name)
        .expect("a workspace");
      manager.workspace(&workspace);
      workspace.name(name.to_owned());
      workspace.capabilities(can);
      workspace.state(ext_workspace_handle_v1::State::empty());
      group.workspace_enter(&workspace);
    }
    manager.done();
  }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for State {
  fn request(
    state: &mut State,
    _: &Client,
    _: &ExtWorkspaceManagerV1,
    request: ext_workspace_manager_v1::Request,
    _: &(),
    _: &DisplayHandle,
    _: &mut DataInit<'_, State>,
  ) {
    if let ext_workspace_manager_v1::Request::Commit = request {
      state.log(String::from("commit"));
    }
  }
}

impl Dispatch<ExtWorkspaceHandleV1, &'static str> for State {
  fn request(
    state: &mut State,
    _: &Client,
    _: &ExtWorkspaceHandleV1,
    request: ext_workspace_handle_v1::Request,
    name: &&'static str,
    _: &DisplayHandle,
    _: &mut DataInit<'_, State>,
  ) {
    let what = match request {
      ext_workspace_handle_v1::Request::Activate => "activate",
      ext_workspace_handle_v1::Request::Deactivate => "deactivate",
      ext_workspace_handle_v1::Request::Remove => "remove",
      ext_workspace_handle_v1::Request::Assign { .. } => "assign",
      _ => return,
    };
    state.log(format!("{what} {name}"));
  }
}
