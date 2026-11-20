//! The event loop, and [`Platform`], the state every event reaches.

use std::time::{Duration, Instant};

use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, LoopSignal, RegistrationToken};
use crabshell_reactive::Runtime;
use crabshell_render::{
  BufferAge, Canvas, DamageHistory, IconLookup, PixelFormat, PixelRect, Renderer,
};
use crabshell_ui::{CursorShape, Size, TextSystem, Window};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::{GlobalList, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::{
  wl_output::WlOutput, wl_pointer::WlPointer, wl_seat::WlSeat, wl_touch::WlTouch,
};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shm::Shm;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::activation::Activation;
use crate::blur::Effects;
use crate::clipboard::{Offered, Primary};
use crate::data_control::Control;
use crate::dnd::Drags;
use crate::events::{self, Event, Listener};
use crate::focus_grab::FocusGrabs;
use crate::foreign_toplevels::Foreign;
use crate::global_shortcuts::GlobalShortcuts;
use crate::hyprland_surface::HyprlandSurfaces;
use crate::idle::Idle;
use crate::keyboard::SeatKeyboard;
use crate::lock::Locks;
use crate::outputs::{self, OutputInfo};
use crate::pointer::Cursor;
use crate::scale::Scale;
use crate::screencopy::Captures;
use crate::shortcuts_inhibit::ShortcutsInhibit;
use crate::surface::{self, FIRST_FRAME_HOLD, Role, Surface};
use crate::text_input::SeatTextInput;
use crate::windows::{Entry, WindowId, Windows};
use crate::windowsets::Windowsets;
use smithay_client_toolkit::data_device_manager::DataDeviceManagerState;
use smithay_client_toolkit::data_device_manager::data_device::DataDevice;
use smithay_client_toolkit::primary_selection::PrimarySelectionManagerState;
use smithay_client_toolkit::shell::xdg::XdgShell;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;

/// The `wl_compositor` version needed: `wl_surface.damage_buffer` came in 4.
const COMPOSITOR_VERSION: u32 = 4;

/// The event loop: one thread, with the Wayland connection as a calloop
/// source and room for more.
///
/// Each iteration dispatches every ready source, runs the reactive flush,
/// lays out the windows that need it, draws each window that is dirty and
/// has its frame callback, then flushes the connection and sleeps.
pub struct EventLoop {
  inner: calloop::EventLoop<'static, Platform>,
  platform: Platform,
}

impl EventLoop {
  /// Connects to the compositor `WAYLAND_DISPLAY` names.
  ///
  /// # Errors
  ///
  /// If there is no compositor to connect to, or it lacks `wl_compositor`
  /// version 4, `wl_shm`, `zwlr_layer_shell_v1` or `xdg_wm_base`.
  pub fn connect() -> Result<EventLoop, Error> {
    let connection =
      Connection::connect_to_env().map_err(|error| Error::Connect(error.to_string()))?;
    EventLoop::with_connection(connection)
  }

  /// Uses an open connection, such as one to a test compositor's socket.
  ///
  /// # Errors
  ///
  /// As for [`connect`](Self::connect).
  pub fn with_connection(connection: Connection) -> Result<EventLoop, Error> {
    let (globals, queue) = registry_queue_init::<Platform>(&connection)
      .map_err(|error| Error::Connect(error.to_string()))?;
    let inner = calloop::EventLoop::try_new().map_err(|error| Error::Loop(error.to_string()))?;
    let platform = Platform::new(
      connection.clone(),
      &globals,
      queue.handle(),
      inner.handle(),
      inner.get_signal(),
    )?;
    inner
      .handle()
      .insert_source(
        WaylandSource::new(connection, queue),
        |_, queue, platform| {
          platform.stats.wayland_wakeups += 1;
          queue.dispatch_pending(platform)
        },
      )
      .map_err(|error| Error::Loop(error.error.to_string()))?;
    Ok(EventLoop { inner, platform })
  }

  /// The handle other sources are added with: timers, D-Bus, IPC sockets.
  /// Their callbacks get the [`Platform`].
  pub fn handle(&self) -> LoopHandle<'static, Platform> {
    self.inner.handle()
  }

  /// Stops [`run`](Self::run) from anywhere, even another thread.
  pub fn signal(&self) -> LoopSignal {
    self.inner.get_signal()
  }

  /// The platform, to add windows and listen for events.
  pub fn platform(&mut self) -> &mut Platform {
    &mut self.platform
  }

  /// Runs one iteration: waits up to `timeout` (forever if `None`) for a
  /// source, dispatches what is ready, then flushes, lays out, draws and
  /// flushes the connection.
  ///
  /// # Errors
  ///
  /// If the connection failed, such as when the compositor quit.
  pub fn dispatch(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
    self
      .inner
      .dispatch(timeout, &mut self.platform)
      .map_err(|error| Error::Loop(error.to_string()))?;
    self.platform.after_dispatch();
    Ok(())
  }

  /// Runs until [`LoopSignal::stop`]. Windows added before it starts are
  /// set up first.
  ///
  /// # Errors
  ///
  /// As for [`dispatch`](Self::dispatch).
  pub fn run(&mut self) -> Result<(), Error> {
    self.platform.after_dispatch();
    self
      .inner
      .run(None, &mut self.platform, Platform::after_dispatch)
      .map_err(|error| Error::Loop(error.to_string()))
  }
}

/// Why the platform couldn't start or keep running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
  /// There was no compositor to connect to, or the connection broke.
  Connect(String),
  /// The compositor doesn't offer a protocol Crabshell needs.
  Missing(&'static str),
  /// The compositor offers a protocol, but too old a version of it.
  TooOld {
    /// The protocol.
    global: &'static str,
    /// The version offered.
    version: u32,
    /// The version needed.
    needed: u32,
  },
  /// The event loop failed.
  Loop(String),
}

impl std::fmt::Display for Error {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Error::Connect(error) => write!(f, "couldn't talk to the compositor: {error}"),
      Error::Missing(global) => write!(f, "the compositor doesn't offer {global}"),
      Error::TooOld {
        global,
        version,
        needed,
      } => write!(
        f,
        "the compositor offers {global} version {version}, but Crabshell needs {needed}"
      ),
      Error::Loop(error) => write!(f, "the event loop failed: {error}"),
    }
  }
}

impl std::error::Error for Error {}

/// Refuses a protocol older than `needed`.
pub(crate) fn require_version(
  global: &'static str,
  version: u32,
  needed: u32,
) -> Result<(), Error> {
  if version < needed {
    Err(Error::TooOld {
      global,
      version,
      needed,
    })
  } else {
    Ok(())
  }
}

/// What the loop has done, to check that an idle bar stays idle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
  /// Loop iterations: each wakeup, plus each [`EventLoop::dispatch`] that
  /// timed out.
  pub iterations: u64,
  /// Times the Wayland connection woke the loop.
  pub wayland_wakeups: u64,
  /// Frames committed, over all windows.
  pub commits: u64,
  /// Time spent drawing the frames committed.
  pub drawing: Duration,
}

/// What a window's surface last showed.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
  /// The output the window is for.
  pub output: String,
  /// Whether the compositor has configured it.
  pub configured: bool,
  /// The logical size from the last configure.
  pub size: Option<(u32, u32)>,
  /// Physical pixels per logical pixel.
  pub scale: f32,
  /// Whether the scale is fractional, through `wp_fractional_scale_v1`.
  pub fractional: bool,
  /// The size of the last buffer committed.
  pub buffer_size: Option<(u32, u32)>,
  /// The buffers it holds: one at rest, up to three while the compositor
  /// reads the ones it drew before.
  pub buffers: usize,
  /// Frames committed.
  pub commits: u64,
  /// Whether it has a surface: false while it waits for its output, or
  /// when layer-shell would refuse its settings.
  pub shown: bool,
  /// The rectangles the last frame reported with `damage_buffer`.
  pub last_damage: Vec<PixelRect>,
  /// For a popup, where the compositor placed it, relative to the window
  /// that opened it, from the last configure. `None` for a panel.
  pub position: Option<(i32, i32)>,
}

/// A window drawn whole into memory, as the compositor should show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
  /// Premultiplied RGBA, row by row.
  pub rgba: Vec<u8>,
  /// The width in pixels.
  pub width: u32,
  /// The height in pixels.
  pub height: u32,
}

/// A seat's pointer.
pub(crate) struct SeatPointer {
  pub(crate) seat: WlSeat,
  pub(crate) pointer: WlPointer,
  /// For setting cursor shapes, when the compositor offers them.
  pub(crate) shapes: Option<WpCursorShapeDeviceV1>,
  pub(crate) cursor: Cursor,
}

impl SeatPointer {
  /// Shows `wanted` if the pointer is over a window that shows something
  /// else. Without `wp_cursor_shape_v1` the compositor's default stays.
  pub(crate) fn show(&mut self, wanted: CursorShape) {
    if let Some((serial, shape)) = self.cursor.update(wanted)
      && let Some(shapes) = &self.shapes
    {
      shapes.set_shape(serial, crate::pointer::shape(shape, shapes.version()));
    }
  }
}

/// A seat's touchscreen.
pub(crate) struct SeatTouch {
  pub(crate) seat: WlSeat,
  pub(crate) touch: WlTouch,
  pub(crate) state: crate::pointer::Touch,
}

/// Everything the event loop's callbacks reach: the compositor's globals,
/// outputs, pointers and windows, and the renderer and text system they
/// draw with.
pub struct Platform {
  pub(crate) connection: Connection,
  pub(crate) qh: QueueHandle<Platform>,
  handle: LoopHandle<'static, Platform>,
  pub(crate) signal: LoopSignal,
  runtime: Runtime,
  pub(crate) registry: RegistryState,
  pub(crate) compositor: CompositorState,
  layer_shell: LayerShell,
  pub(crate) xdg: XdgShell,
  pub(crate) shm: Shm,
  pub(crate) outputs: OutputState,
  pub(crate) seats: SeatState,
  fractional: Option<(WpFractionalScaleManagerV1, WpViewporter)>,
  pub(crate) cursor_shapes: Option<CursorShapeManager>,
  /// Outputs with names, in the order they came.
  pub(crate) known: Vec<(WlOutput, OutputInfo)>,
  listener: Option<Listener<Platform>>,
  pub(crate) windows: Windows<Surface, WlOutput>,
  pub(crate) pointers: Vec<SeatPointer>,
  pub(crate) touches: Vec<SeatTouch>,
  pub(crate) keyboards: Vec<SeatKeyboard>,
  /// Present when the compositor offers a clipboard.
  pub(crate) data_devices_manager: Option<DataDeviceManagerState>,
  pub(crate) data_devices: Vec<(WlSeat, DataDevice)>,
  /// The text this shell offers as the clipboard's.
  pub(crate) offered: Option<Offered>,
  /// The paste from the clipboard or the primary selection under way.
  pub(crate) pasting: Option<RegistrationToken>,
  /// The primary selection, which a middle click pastes.
  pub(crate) primary: Primary,
  /// The drag the shell started and the one over its windows.
  pub(crate) drags: Drags,
  /// The clipboard as configs follow it, once a watch asks.
  pub(crate) control: Control,
  /// Present when the compositor relays input methods.
  pub(crate) text_inputs_manager: Option<ZwpTextInputManagerV3>,
  /// The compositor's background effects, such as blur.
  pub(crate) effects: Effects,
  /// The compositor's idle protocols and the watches on them.
  pub(crate) idle: Idle,
  /// Handing the compositor's shortcuts to windows.
  pub(crate) shortcuts_inhibit: ShortcutsInhibit,
  /// The shortcuts the compositor binds, and their watches.
  pub(crate) global_shortcuts: GlobalShortcuts,
  /// False while the shell runs beside the one a reload replaces: see
  /// [`set_alone`](Platform::set_alone).
  pub(crate) alone: bool,
  /// The windows' focus grabs.
  pub(crate) focus_grabs: FocusGrabs,
  /// Hyprland's surface manager, once a window asks.
  pub(crate) hyprland_surfaces: HyprlandSurfaces,
  /// Every program's windows, once a watch asks.
  pub(crate) foreign: Foreign,
  /// The workspaces, once a watch asks.
  pub(crate) windowsets: Windowsets,
  /// The session locks.
  pub(crate) locks: Locks,
  /// What screencopy views show.
  pub(crate) captures: Captures,
  pub(crate) text_inputs: Vec<SeatTextInput>,
  /// The seat and serial of the last button press or release on one of
  /// the windows, which a popup's grab needs.
  pub(crate) last_button: Option<(WlSeat, u32)>,
  /// Whether that last button event was a press, still held.
  pub(crate) button_held: bool,
  /// The seat and serial of the last button press, touch or key press on
  /// one of the windows, that window, and when, for activation tokens.
  pub(crate) last_input: Option<((WlSeat, u32, WindowId), Instant)>,
  /// Activation tokens for the programs the shell starts.
  pub(crate) activation: Activation,
  /// Whether the animation timer is in the loop.
  ticking: bool,
  /// The one calloop timer for the config's timers and the deadline it is
  /// armed at, in the loop only while one of them runs.
  timers: Option<(RegistrationToken, Instant)>,
  /// The calloop timer for the next frame of the GIFs drawn and the time
  /// it is armed at, in the loop only while one plays.
  images: Option<(RegistrationToken, Instant)>,
  /// The ping the renderer's decoding thread wakes the loop with, in the
  /// loop only while pictures are being decoded.
  decodes: Option<RegistrationToken>,
  pub(crate) renderer: Renderer,
  /// The icon theme set, for snapshots' renderers.
  icon_theme: Option<String>,
  pub(crate) text: TextSystem,
  pub(crate) stats: Stats,
  /// The last ticket [`sync`](Self::sync) handed out, and the last one
  /// the compositor answered.
  pub(crate) syncs_sent: u64,
  pub(crate) syncs_done: u64,
}

impl Platform {
  fn new(
    connection: Connection,
    globals: &GlobalList,
    qh: QueueHandle<Platform>,
    handle: LoopHandle<'static, Platform>,
    signal: LoopSignal,
  ) -> Result<Platform, Error> {
    let compositor =
      CompositorState::bind(globals, &qh).map_err(|_| Error::Missing("wl_compositor"))?;
    require_version(
      "wl_compositor",
      compositor.wl_compositor().version(),
      COMPOSITOR_VERSION,
    )?;
    let layer_shell =
      LayerShell::bind(globals, &qh).map_err(|_| Error::Missing("zwlr_layer_shell_v1"))?;
    let shm = Shm::bind(globals, &qh).map_err(|_| Error::Missing("wl_shm"))?;
    // With the decoration manager, for floating windows' title bars.
    let xdg = XdgShell::bind(globals, &qh).map_err(|_| Error::Missing("xdg_wm_base"))?;
    // Fractional scaling needs both; without either, whole scales.
    let manager = globals.bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ());
    let viewporter = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ());
    let fractional = match (manager, viewporter) {
      (Ok(manager), Ok(viewporter)) => Some((manager, viewporter)),
      (manager, viewporter) => {
        if let Ok(manager) = manager {
          manager.destroy();
        }
        if let Ok(viewporter) = viewporter {
          viewporter.destroy();
        }
        None
      }
    };
    let text_inputs_manager = globals.bind(&qh, 1..=1, ()).ok();
    let idle = Idle::new(
      globals.bind(&qh, 1..=1, ()).ok(),
      globals.bind(&qh, 1..=1, ()).ok(),
    );
    let shortcuts_inhibit =
      ShortcutsInhibit::new(globals.bind(&qh, 1..=1, ()).ok(), signal.clone());
    let global_shortcuts = GlobalShortcuts::new(globals.bind(&qh, 1..=1, ()).ok());
    let focus_grabs = FocusGrabs::new(globals.bind(&qh, 1..=1, ()).ok());
    let effects = Effects {
      manager: globals.bind(&qh, 1..=1, ()).ok(),
      blur: false,
      warned: false,
    };
    let data_devices_manager = DataDeviceManagerState::bind(globals, &qh).ok();
    let primary = Primary::new(PrimarySelectionManagerState::bind(globals, &qh).ok());
    let locks = Locks::new(globals.bind(&qh, 1..=1, ()).ok());
    let activation = Activation::new(globals.bind(&qh, 1..=1, ()).ok());
    tracing::debug!(
      fractional = fractional.is_some(),
      text_input = text_inputs_manager.is_some(),
      background_effects = effects.manager.is_some(),
      idle_notify = idle.notifier.is_some(),
      idle_inhibit = idle.inhibit.is_some(),
      shortcuts_inhibit = shortcuts_inhibit.manager.is_some(),
      global_shortcuts = global_shortcuts.manager.is_some(),
      focus_grab = focus_grabs.manager.is_some(),
      activation = activation.offers(),
      "bound the compositor's globals"
    );
    let mut platform = Platform {
      registry: RegistryState::new(globals),
      outputs: OutputState::new(globals, &qh),
      seats: SeatState::new(globals, &qh),
      cursor_shapes: CursorShapeManager::bind(globals, &qh).ok(),
      connection,
      qh,
      handle,
      signal,
      runtime: Runtime::current(),
      compositor,
      layer_shell,
      xdg,
      shm,
      fractional,
      known: Vec::new(),
      listener: None,
      windows: Windows::new(),
      pointers: Vec::new(),
      touches: Vec::new(),
      keyboards: Vec::new(),
      text_inputs_manager,
      effects,
      idle,
      shortcuts_inhibit,
      global_shortcuts,
      alone: true,
      focus_grabs,
      hyprland_surfaces: HyprlandSurfaces::default(),
      foreign: Foreign::default(),
      windowsets: Windowsets::default(),
      locks,
      captures: Captures::default(),
      data_devices_manager,
      data_devices: Vec::new(),
      offered: None,
      pasting: None,
      primary,
      drags: Drags::default(),
      control: Control::default(),
      text_inputs: Vec::new(),
      last_button: None,
      button_held: false,
      last_input: None,
      activation,
      ticking: false,
      timers: None,
      images: None,
      decodes: None,
      renderer: Renderer::new(),
      icon_theme: None,
      text: TextSystem::new(),
      stats: Stats::default(),
      syncs_sent: 0,
      syncs_done: 0,
    };
    platform.renderer.decode_off_thread();
    // sctk tells `new_seat` only about seats that come later.
    for seat in platform.seats.seats() {
      let qh = platform.qh.clone();
      platform.add_text_input(&qh, &seat);
      platform.add_data_device(&qh, &seat);
    }
    Ok(platform)
  }

  /// Starts the animation timer if an animation runs and it isn't
  /// running: it fires once a frame of the fastest output, advances the
  /// animations, and leaves the loop once none runs, so a shell where
  /// nothing moves isn't woken.
  fn tick_animations(&mut self) {
    if self.ticking || !crabshell_ui::animating() {
      return;
    }
    let frame = self.frame_interval();
    let ticked =
      self
        .handle
        .insert_source(Timer::immediate(), move |_, _, platform: &mut Platform| {
          if crabshell_ui::advance_animations(Instant::now()) {
            TimeoutAction::ToDuration(frame)
          } else {
            platform.ticking = false;
            TimeoutAction::Drop
          }
        });
    match ticked {
      Ok(_) => self.ticking = true,
      Err(error) => tracing::warn!(%error, "couldn't start the animation timer"),
    }
  }

  /// Arms the one calloop timer at the earliest running timer's deadline,
  /// moves it when that changes, and takes it out of the loop when no
  /// timer runs. Called after the flush, which may start or stop timers.
  fn arm_timers(&mut self) {
    let next = crabshell_ui::next_timer();
    if self.timers.map(|(_, deadline)| deadline) == next {
      return;
    }
    if let Some((token, _)) = self.timers.take() {
      self.handle.remove(token);
    }
    let Some(deadline) = next else {
      return;
    };
    let armed = self.handle.insert_source(
      Timer::from_deadline(deadline),
      |_, _, platform: &mut Platform| {
        crabshell_ui::fire_timers(Instant::now());
        // Kept while one runs, so an interval stays one source; the flush
        // after this may still move it.
        match crabshell_ui::next_timer() {
          Some(next) => {
            if let Some((_, deadline)) = &mut platform.timers {
              *deadline = next;
            }
            TimeoutAction::ToInstant(next)
          }
          None => {
            platform.timers = None;
            TimeoutAction::Drop
          }
        }
      },
    );
    match armed {
      Ok(token) => self.timers = Some((token, deadline)),
      Err(error) => tracing::warn!(%error, "couldn't start the timer"),
    }
  }

  /// Arms a calloop timer for when the renderer says the next frame of a
  /// GIF it drew is due, moves it when that changes, and takes it out of
  /// the loop while none is. When it fires, the GIFs due move on a frame
  /// and their boxes repaint; a GIF not drawn then waits for nothing.
  fn arm_images(&mut self) {
    let next = self.renderer.next_image_frame(Instant::now());
    if self.images.map(|(_, due)| due) == next {
      return;
    }
    if let Some((token, _)) = self.images.take() {
      self.handle.remove(token);
    }
    let Some(due) = next else {
      return;
    };
    let armed = self.handle.insert_source(
      Timer::from_deadline(due),
      |_, _, platform: &mut Platform| {
        platform.images = None;
        for id in platform.renderer.advance_images(Instant::now()) {
          for entry in &platform.windows.entries {
            if entry.window.repaint_image(id) {
              break;
            }
          }
        }
        TimeoutAction::Drop
      },
    );
    match armed {
      Ok(token) => self.images = Some((token, due)),
      Err(error) => tracing::warn!(%error, "couldn't start the GIF timer"),
    }
  }

  /// Gives back the spare buffers of each window with nothing to draw,
  /// while nothing animates, once the compositor released them: the
  /// release woke the loop anyway, and the window's next frame takes a
  /// buffer again. An animation or a GIF would draw again within a frame,
  /// so while one runs every window keeps its buffers.
  fn trim_buffers(&mut self) {
    if crabshell_ui::animating() || self.images.is_some() {
      return;
    }
    for entry in &mut self.windows.entries {
      if let Some(surface) = &mut entry.surface
        && !entry.window.has_damage()
      {
        surface.trim_buffers();
      }
    }
  }

  /// Puts a ping in the loop while the renderer decodes pictures, for its
  /// decoding thread to wake the loop with once one is ready. Then the
  /// images waiting for them repaint in every window, and the ping leaves
  /// the loop until a draw sends more work.
  fn arm_decodes(&mut self) {
    if self.decodes.is_some() || !self.renderer.decoding() {
      return;
    }
    let (ping, source) = match calloop::ping::make_ping() {
      Ok(made) => made,
      Err(error) => {
        tracing::warn!(%error, "couldn't wait for decoded pictures");
        return;
      }
    };
    let inserted = self
      .handle
      .insert_source(source, |(), &mut (), platform: &mut Platform| {
        if let Some(token) = platform.decodes.take() {
          platform.handle.remove(token);
        }
        for id in platform.renderer.take_decoded() {
          for entry in &platform.windows.entries {
            entry.window.repaint_image(id);
          }
        }
      });
    match inserted {
      Ok(token) => {
        self.decodes = Some(token);
        self.renderer.wake_when_decoded(move || ping.ping());
      }
      Err(error) => tracing::warn!(%error, "couldn't wait for decoded pictures"),
    }
  }

  /// Gives each window whose first frame is held for pictures a one-shot
  /// timer that ends the wait at [`FIRST_FRAME_HOLD`], and takes it out
  /// once the window commits.
  fn arm_holds(&mut self) {
    for entry in &mut self.windows.entries {
      let id = entry.id;
      let Some(surface) = entry.surface.as_mut() else {
        continue;
      };
      let Some(hold) = &mut surface.held else {
        continue;
      };
      if surface.commits > 0 {
        if let Some(token) = hold.timer.take() {
          self.handle.remove(token);
        }
        surface.held = None;
        continue;
      }
      if hold.timer.is_some() || hold.ended {
        continue;
      }
      let armed = self.handle.insert_source(
        Timer::from_deadline(hold.since + FIRST_FRAME_HOLD),
        move |_, _, platform: &mut Platform| {
          let surface = platform
            .windows
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.surface.as_mut());
          if let Some(surface) = surface {
            if let Some(hold) = &mut surface.held {
              hold.timer = None;
              hold.ended = true;
            }
            surface.frames.pacing.redraw();
          }
          TimeoutAction::Drop
        },
      );
      match armed {
        Ok(token) => hold.timer = Some(token),
        Err(error) => {
          tracing::warn!(%error, "couldn't time a first frame's wait");
          hold.ended = true;
          surface.frames.pacing.redraw();
        }
      }
    }
  }

  /// A frame of the fastest output, or of 60 Hz without one.
  pub(crate) fn frame_interval(&self) -> Duration {
    frame_interval(
      self
        .known
        .iter()
        .filter_map(|(_, info)| info.mode.map(|mode| mode.refresh_mhz)),
    )
  }

  /// The handle other sources are added with.
  pub fn loop_handle(&self) -> LoopHandle<'static, Platform> {
    self.handle.clone()
  }

  /// Stops [`EventLoop::run`] after this iteration.
  pub fn stop(&self) {
    self.signal.stop();
  }

  /// The outputs announced so far, in the order they came.
  pub fn outputs(&self) -> impl Iterator<Item = &OutputInfo> {
    self.known.iter().map(|(_, info)| info)
  }

  /// Calls `listener` whenever an output is added, changes or is removed,
  /// or the compositor closes a window. Outputs already announced aren't
  /// repeated; read them from [`outputs`](Self::outputs). It replaces any
  /// listener set before. If it panics, the panic is logged and the
  /// listener kept.
  pub fn on_event(&mut self, listener: impl FnMut(&mut Platform, &Event) + 'static) {
    self.listener = Some(Box::new(listener));
  }

  /// Shows `window` on its output. Its layer surface is created after the
  /// next flush, once the output is announced; a window for an output
  /// that never comes waits. A window whose settings layer-shell would
  /// refuse, such as a width of 0 without both side anchors, isn't shown;
  /// that is logged. The window is dropped, with its owner, when the
  /// compositor closes it or its output goes away.
  pub fn add_window(&mut self, window: Window) -> WindowId {
    self.windows.add(window)
  }

  /// Drops a window, with its owner, its surface and the popups it
  /// opened. Returns whether it was there.
  pub fn remove_window(&mut self, id: WindowId) -> bool {
    self.windows.remove(id)
  }

  /// The windows held, panels and the popups their widgets opened, in the
  /// order they were added.
  pub fn windows(&self) -> impl Iterator<Item = (WindowId, &Window)> {
    self
      .windows
      .entries
      .iter()
      .map(|entry| (entry.id, &entry.window))
  }

  /// What a window's surface last showed; `None` for a window not held.
  pub fn window_info(&self, id: WindowId) -> Option<WindowInfo> {
    let entry = self.windows.get(id)?;
    let surface = entry.surface.as_ref();
    Some(WindowInfo {
      output: entry.window.output().name(),
      configured: surface.is_some_and(|surface| surface.frames.pacing.configured()),
      size: surface.and_then(|surface| surface.size),
      scale: surface.map_or(1.0, |surface| surface.frames.scale.factor()),
      fractional: surface.is_some_and(|surface| surface.fractional.is_some()),
      buffer_size: surface.and_then(|surface| surface.buffer_size),
      buffers: surface.map_or(0, Surface::buffers),
      commits: surface.map_or(0, |surface| surface.commits),
      shown: surface.is_some(),
      last_damage: surface
        .map(|surface| surface.last_damage.clone())
        .unwrap_or_default(),
      position: surface
        .and_then(Surface::popup)
        .and_then(|popup| popup.position),
    })
  }

  /// Draws a configured window whole into memory, at its scale, with a
  /// renderer of its own. For comparing with screenshots.
  pub fn snapshot(&mut self, id: WindowId) -> Option<Snapshot> {
    let entry = self.windows.get(id)?;
    let surface = entry.surface.as_ref()?;
    let (width, height) = surface.size?;
    let (buffer_width, buffer_height) = surface.frames.scale.buffer_size(width, height)?;
    let mut rgba = vec![0; buffer_width as usize * buffer_height as usize * 4];
    let mut renderer = Renderer::new();
    if let Some(theme) = &self.icon_theme {
      renderer.set_icon_theme(theme.clone());
    }
    renderer
      .draw(
        &entry.window.view(),
        &[],
        &mut self.text,
        &mut DamageHistory::new(),
        Canvas {
          pixels: &mut rgba,
          width: buffer_width,
          height: buffer_height,
          scale: surface.frames.scale.factor(),
          format: PixelFormat::Abgr8888,
          age: &mut BufferAge::default(),
        },
      )
      .ok()?;
    Some(Snapshot {
      rgba,
      width: buffer_width,
      height: buffer_height,
    })
  }

  /// Sets the icon theme names are looked up in, such as `"Adwaita"`;
  /// the default is `hicolor`. Every window is repainted whole, since any
  /// icon may change.
  pub fn set_icon_theme(&mut self, theme: impl Into<String>) {
    let theme = theme.into();
    self.renderer.set_icon_theme(theme.clone());
    for surface in self
      .windows
      .entries
      .iter_mut()
      .filter_map(|entry| entry.surface.as_mut())
    {
      surface.frames.repaint_whole();
    }
    self.icon_theme = Some(theme);
  }

  /// The lookup icon names are drawn through, which follows
  /// [`set_icon_theme`](Platform::set_icon_theme).
  pub fn icon_lookup(&self) -> IconLookup {
    self.renderer.icon_lookup()
  }

  /// The text system windows are laid out and drawn with.
  pub fn text_system(&mut self) -> &mut TextSystem {
    &mut self.text
  }

  /// What the loop has done so far.
  pub fn stats(&self) -> Stats {
    self.stats
  }

  /// The work after the sources are dispatched: report the toplevels and
  /// windowsets, flush, take new popups and close the ones that asked, set
  /// up new surfaces, apply changed settings, lay out, draw, start and end
  /// captures, commit windowset requests, flush the connection.
  pub(crate) fn after_dispatch(&mut self) {
    self.stats.iterations += 1;
    self.report_toplevels();
    self.report_windowsets();
    // Effects created in a flush run in it, so every popup opened so far,
    // by a handler or an effect, has its bindings run.
    self.runtime.flush();
    // What it set or built has bindings to run before the layout.
    if self.update_locks() {
      self.runtime.flush();
    }
    // A shortcut released in the turn it was pressed shows released now.
    if self.global_shortcuts.shortcuts.flushed() {
      self.runtime.flush();
    }
    // The windows that draw, whose live views may ask for their next
    // frame.
    let mut drawn = Vec::new();
    for pass in 0..PASSES {
      if pass > 0 && !self.left_work() {
        break;
      }
      self.take_popups();
      self
        .windows
        .remove_where(|entry| entry.window.wants_close());
      self.create_surfaces();
      self.update_toplevels();
      self.lay_out_and_draw(&mut drawn);
    }
    self.update_focus_grabs();
    self.update_drags();
    // A view that joined a session shows its last frame now.
    if self.sync_captures() {
      self.lay_out_and_draw(&mut drawn);
    }
    self.paced_captures(&drawn);
    self.sync_text_inputs();
    self.tick_animations();
    self.arm_timers();
    self.arm_images();
    self.trim_buffers();
    self.arm_decodes();
    self.arm_holds();
    self.commit_windowsets();
    if let Err(error) = self.connection.flush() {
      tracing::warn!(%error, "couldn't flush the Wayland connection");
    }
  }

  /// One pass over the windows: applies changed settings, lays out what
  /// needs it and draws what can, adding each window drawn to `drawn`.
  fn lay_out_and_draw(&mut self, drawn: &mut Vec<WindowId>) {
    let Platform {
      windows,
      pointers,
      renderer,
      text,
      qh,
      stats,
      shm,
      compositor,
      registry,
      effects,
      hyprland_surfaces,
      idle,
      shortcuts_inhibit,
      seats,
      alone,
      ..
    } = self;
    for entry in windows.entries.iter_mut() {
      if entry.surface.is_some() && entry.window.take_surface_changed() {
        let config = surface::beside(entry.window.surface(), *alone);
        if entry.window.is_popup() {
          // A popup's settings are its hints, which a frame commits.
          if let Some(surface) = &mut entry.surface {
            let mut hinted = surface.send_mask(compositor, config.mask.as_ref());
            hinted |=
              surface.send_hyprland(hyprland_surfaces, registry, compositor, &entry.window, qh);
            if hinted {
              surface.frames.pacing.redraw();
            }
          }
        } else if !refused(entry, &config)
          && let Some(surface) = &mut entry.surface
        {
          // Settings layer-shell would refuse are never sent, and the
          // surface keeps the last good ones. The hints go first, for
          // `reconfigure`'s commit, or without one for the next frame's.
          let mut hinted = surface.send_mask(compositor, config.mask.as_ref());
          hinted |= surface.send_blur(effects, compositor, entry.window.blur().as_ref(), qh);
          hinted |=
            surface.send_hyprland(hyprland_surfaces, registry, compositor, &entry.window, qh);
          if !surface.reconfigure(&config) && hinted {
            surface.frames.pacing.redraw();
          }
        }
      }
      let Some(surface) = entry.surface.as_mut() else {
        continue;
      };
      if !surface.frames.pacing.configured() {
        continue;
      }
      surface.inhibit_idle(idle, entry.window.inhibits_idle(), qh);
      surface.inhibit_shortcuts(shortcuts_inhibit, (entry.id, &entry.window), seats, qh);
      if surface.blur_outdated() {
        surface.send_blur(effects, compositor, entry.window.blur().as_ref(), qh);
      }
      if entry.window.needs_layout() {
        let alone = *alone;
        let settled = lay_out_sized(&entry.window, text, |window| {
          window.surface_changed()
            && surface.resize(window, &surface::beside(window.surface(), alone))
        });
        log_unsettled(&entry.window, &mut entry.unsettled, settled);
        surface.fit(&entry.window);
      }
      // A layout can move a button out from under a still pointer, or
      // under it, and a binding can change a button's cursor or turn it
      // off. Only a change is sent.
      for pointer in pointers
        .iter_mut()
        .filter(|pointer| pointer.cursor.focus() == Some(entry.id))
      {
        pointer.show(entry.window.cursor());
      }
      // A new size, from a configure or a popup's layout, cuts the masks
      // again, as a new scale does the visible one; the frame drawn at
      // that size commits them.
      if surface.mask_outdated() {
        surface.send_mask(compositor, entry.window.surface().mask.as_ref());
      }
      if surface.hyprland_outdated() {
        surface.send_hyprland(hyprland_surfaces, registry, compositor, &entry.window, qh);
      }
      if surface.frames.pacing.should_draw(entry.window.has_damage()) {
        let started = Instant::now();
        if surface.draw(&entry.window, renderer, text, shm, qh) {
          stats.commits += 1;
          stats.drawing += started.elapsed();
          drawn.push(entry.id);
          entry.window.frame_committed();
        }
      }
    }
  }

  /// Whether the last pass's layouts left work for another: a layout
  /// writes `geometry` and flushes, and the effects that run may lay out
  /// or repaint a window already passed, change its settings, open or
  /// close a popup, or change a lock, all of which nothing would wake the
  /// loop for. New popups are taken here, and a lock that changed is
  /// acted on and flushed, as at the start of the iteration.
  fn left_work(&mut self) -> bool {
    let mut work = self.update_locks();
    if work {
      self.runtime.flush();
    }
    let before = self.windows.entries.len();
    self.take_popups();
    work |= self.windows.entries.len() != before;
    work
      || self.windows.entries.iter().any(|entry| {
        let window = &entry.window;
        window.wants_close()
          || entry.surface.as_ref().is_some_and(|surface| {
            window.surface_changed()
              || surface.frames.pacing.configured()
                && (window.needs_layout() || surface.frames.pacing.should_draw(window.has_damage()))
          })
      })
  }

  /// Creates layer surfaces for panels whose output is known, and popups
  /// for those whose opener is on screen. Called after a flush, since a
  /// window's settings are bindings that first run then.
  fn create_surfaces(&mut self) {
    for index in 0..self.windows.entries.len() {
      let entry = &mut self.windows.entries[index];
      if entry.surface.is_some() {
        continue;
      }
      if entry.opener.is_some() {
        self.create_popup(index);
        continue;
      }
      if let Some(config) = entry.window.floating_config() {
        self.create_toplevel(index, &config);
        continue;
      }
      let name = entry.window.output().name();
      let Some((output, info)) = self.known.iter().find(|(_, info)| info.name == name) else {
        if !entry.waiting {
          entry.waiting = true;
          tracing::debug!(
            window = entry.window.view().root().path(),
            output = name,
            "a window waits for its output to be announced"
          );
        }
        continue;
      };
      // Laid out first at the size it asks for, so a size its bindings
      // read from its content's geometry is right in the first configure.
      let output_size = (info.width, info.height);
      ask_size(&entry.window, output_size);
      let settled = lay_out_sized(&entry.window, &mut self.text, |window| {
        ask_size(window, output_size)
      });
      log_unsettled(&entry.window, &mut entry.unsettled, settled);
      let config = surface::beside(entry.window.surface(), self.alone);
      if refused(entry, &config) {
        continue;
      }
      let (output, output_scale) = (output.clone(), info.scale);
      // The settings were read above; later changes are sent as they
      // come.
      self.windows.entries[index].window.take_surface_changed();
      let namespace = surface::namespace(&config.namespace);
      let wl_surface = self.compositor.create_surface(&self.qh);
      let fractional = self.scale_objects(&wl_surface);
      let layer = self.layer_shell.create_layer_surface(
        &self.qh,
        wl_surface,
        surface::layer(config.layer),
        Some(namespace.clone()),
        Some(&output),
      );
      surface::apply(&layer, &config);
      // An empty commit asks for the first configure.
      layer.commit();
      // Until the compositor says otherwise, the output's own scale.
      let scale = Scale::integer(output_scale).unwrap_or(Scale::Integer(1));
      let scale = if fractional.is_some() {
        scale.as_fractional()
      } else {
        scale
      };
      tracing::debug!(output = name, ?config, "created a layer surface");
      let entry = &mut self.windows.entries[index];
      let mut surface = Surface::new(Role::Layer(layer), fractional, scale);
      surface.namespace = namespace;
      surface.layer_sent = Some(config);
      entry.surface = Some(surface);
      entry.output = Some(output);
    }
  }

  /// A new surface's fractional scale and viewport, when the compositor
  /// scales fractionally.
  pub(crate) fn scale_objects(
    &self,
    surface: &WlSurface,
  ) -> Option<(WpFractionalScaleV1, WpViewport)> {
    self.fractional.as_ref().map(|(manager, viewporter)| {
      (
        manager.get_fractional_scale(surface, &self.qh, ()),
        viewporter.get_viewport(surface, &self.qh, ()),
      )
    })
  }

  /// Tells the listener about `event`.
  pub(crate) fn notify(&mut self, event: Event) {
    events::notify(self, listener_slot, &event);
  }

  /// What sctk knows about `output`, or `None` if it has no name.
  pub(crate) fn describe(&self, output: &WlOutput) -> Option<OutputInfo> {
    let info = self.outputs.info(output)?;
    outputs::describe(&info, self.fractional.is_some())
  }
}

/// A frame of the fastest of these refresh rates, in millihertz, or of
/// 60 Hz without one of at least 1 Hz.
pub(crate) fn frame_interval(refresh_mhz: impl Iterator<Item = u32>) -> Duration {
  let fastest = refresh_mhz
    .max()
    .filter(|&mhz| mhz >= 1_000)
    .unwrap_or(60_000);
  Duration::from_micros(1_000_000_000 / u64::from(fastest))
}

fn listener_slot(platform: &mut Platform) -> &mut Option<Listener<Platform>> {
  &mut platform.listener
}

impl Platform {
  /// Says whether this shell runs alone. A shell a reload starts beside
  /// the old one isn't, until the old one has exited: meanwhile its panels
  /// with a positive exclusive zone ask for -1, so they lie over the old
  /// shell's instead of below them and no other window moves, and no
  /// session lock is asked for, since the old shell may hold the session.
  /// Going alone sends the panels' real zones, and locks if a lock wants
  /// to. A shell is alone until told otherwise.
  pub fn set_alone(&mut self, alone: bool) {
    if std::mem::replace(&mut self.alone, alone) == alone {
      return;
    }
    for entry in self.windows.entries.iter_mut() {
      if entry.surface.is_none() || entry.window.is_popup() {
        continue;
      }
      let config = surface::beside(entry.window.surface(), alone);
      if !refused(entry, &config)
        && let Some(surface) = &mut entry.surface
      {
        surface.reconfigure(&config);
      }
    }
  }
}

/// Whether layer-shell would refuse `config`. A refusal is logged once,
/// until the window's settings are good again.
fn refused(entry: &mut Entry<Surface, WlOutput>, config: &crabshell_ui::SurfaceConfig) -> bool {
  match surface::size_problem(config) {
    Some(problem) => {
      if !entry.refused {
        entry.refused = true;
        tracing::error!(
          window = entry.window.view().root().path(),
          ?config,
          "{problem}; the window isn't shown with these settings"
        );
      }
      true
    }
    None => {
      entry.refused = false;
      false
    }
  }
}

/// Gives a panel without a surface the size its settings ask for, each
/// side left to the compositor taken from `output`'s logical size.
/// Returns whether the size changed.
pub(crate) fn ask_size(window: &Window, output: (u32, u32)) -> bool {
  let config = window.surface();
  let Some((width, height)) = surface::configured_size((config.width, config.height), output)
  else {
    return false;
  };
  let size = Size::new(width as f32, height as f32);
  let changed = window.size() != size;
  window.set_size(size);
  changed
}

/// Lays `window` out, and again while `resize` gives it a new size its
/// settings ask for after that layout, so no frame is drawn at a size the
/// window's own bindings already changed. Takes at most [`SIZE_ROUNDS`]
/// new sizes, so at most one more layout than that, and returns whether
/// the size settled; one that didn't goes on at the next pass.
pub(crate) fn lay_out_sized(
  window: &Window,
  text: &mut TextSystem,
  mut resize: impl FnMut(&Window) -> bool,
) -> bool {
  for _ in 0..SIZE_ROUNDS {
    if window.needs_layout() {
      window.layout(text);
    }
    if !resize(window) {
      return true;
    }
  }
  // Laid out at the last size taken, so the frame drawn matches it.
  window.layout(text);
  false
}

/// Logs a window whose size didn't settle, once until it settles again,
/// so a binding that never settles doesn't log every frame.
pub(crate) fn log_unsettled(window: &Window, logged: &mut bool, settled: bool) {
  if settled {
    *logged = false;
  } else if !std::mem::replace(logged, true) {
    tracing::warn!(
      window = window.view().root().path(),
      "a window's size still changed after {} layouts; check the bindings that size it from \
       its content's geometry",
      SIZE_ROUNDS + 1
    );
  }
}

/// The most new sizes [`lay_out_sized`] takes while a window's size
/// follows its content.
pub(crate) const SIZE_ROUNDS: usize = 4;

/// The most passes over the windows one iteration makes while layouts
/// leave work, so a `geometry` feedback that never settles waits for the
/// next event instead of spinning.
const PASSES: usize = 4;
