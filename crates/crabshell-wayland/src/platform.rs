//! The event loop, and [`Platform`], the state every event reaches.

use std::time::{Duration, Instant};

use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, LoopSignal};
use crabshell_reactive::Runtime;
use crabshell_render::{BufferAge, Canvas, DamageHistory, PixelFormat, PixelRect, Renderer};
use crabshell_ui::{CursorShape, TextSystem, Window};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::{GlobalList, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::{
  wl_output::WlOutput, wl_pointer::WlPointer, wl_seat::WlSeat,
};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::shm::slot::SlotPool;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
  Shape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::blur::Effects;
use crate::clipboard::Offered;
use crate::events::{self, Event, Listener};
use crate::foreign_toplevels::Foreign;
use crate::idle::Idle;
use crate::keyboard::SeatKeyboard;
use crate::lock::Locks;
use crate::outputs::OutputInfo;
use crate::pointer::Cursor;
use crate::scale::Scale;
use crate::screencopy::Captures;
use crate::surface::{self, Role, Surface};
use crate::text_input::SeatTextInput;
use crate::windows::{Entry, WindowId, Windows};
use crate::windowsets::Windowsets;
use smithay_client_toolkit::data_device_manager::DataDeviceManagerState;
use smithay_client_toolkit::data_device_manager::data_device::DataDevice;
use smithay_client_toolkit::shell::xdg::XdgShell;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;

/// A new window's first shared-memory pool, in bytes: a 1920 by 30 bar.
/// sctk grows it as buffers need.
pub(crate) const INITIAL_POOL_BYTES: usize = 1920 * 30 * 4;

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
      shapes.set_shape(
        serial,
        match shape {
          CursorShape::Default => Shape::Default,
          CursorShape::Pointer => Shape::Pointer,
          CursorShape::Text => Shape::Text,
        },
      );
    }
  }
}

/// Everything the event loop's callbacks reach: the compositor's globals,
/// outputs, pointers and windows, and the renderer and text system they
/// draw with.
pub struct Platform {
  pub(crate) connection: Connection,
  pub(crate) qh: QueueHandle<Platform>,
  handle: LoopHandle<'static, Platform>,
  signal: LoopSignal,
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
  pub(crate) keyboards: Vec<SeatKeyboard>,
  /// Present when the compositor offers a clipboard.
  pub(crate) data_devices_manager: Option<DataDeviceManagerState>,
  pub(crate) data_devices: Vec<(WlSeat, DataDevice)>,
  /// The text this shell offers as the clipboard's.
  pub(crate) offered: Option<Offered>,
  /// Present when the compositor relays input methods.
  pub(crate) text_inputs_manager: Option<ZwpTextInputManagerV3>,
  /// The compositor's background effects, such as blur.
  pub(crate) effects: Effects,
  /// The compositor's idle protocols and the watches on them.
  pub(crate) idle: Idle,
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
  /// Whether the animation timer is in the loop.
  ticking: bool,
  pub(crate) renderer: Renderer,
  /// The icon theme set, for snapshots' renderers.
  icon_theme: Option<String>,
  pub(crate) text: TextSystem,
  pub(crate) stats: Stats,
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
    let effects = Effects {
      manager: globals.bind(&qh, 1..=1, ()).ok(),
      blur: false,
      warned: false,
    };
    let data_devices_manager = DataDeviceManagerState::bind(globals, &qh).ok();
    let locks = Locks::new(globals.bind(&qh, 1..=1, ()).ok());
    tracing::debug!(
      fractional = fractional.is_some(),
      text_input = text_inputs_manager.is_some(),
      background_effects = effects.manager.is_some(),
      idle_notify = idle.notifier.is_some(),
      idle_inhibit = idle.inhibit.is_some(),
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
      keyboards: Vec::new(),
      text_inputs_manager,
      effects,
      idle,
      foreign: Foreign::default(),
      windowsets: Windowsets::default(),
      locks,
      captures: Captures::default(),
      data_devices_manager,
      data_devices: Vec::new(),
      offered: None,
      text_inputs: Vec::new(),
      last_button: None,
      button_held: false,
      ticking: false,
      renderer: Renderer::new(),
      icon_theme: None,
      text: TextSystem::new(),
      stats: Stats::default(),
    };
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

  /// Calls `listener` whenever an output is added or removed, or the
  /// compositor closes a window. Outputs already announced aren't
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
    self.take_popups();
    self
      .windows
      .remove_where(|entry| entry.window.wants_close());
    self.create_surfaces();
    self.update_toplevels();
    let Platform {
      windows,
      pointers,
      renderer,
      text,
      qh,
      stats,
      compositor,
      effects,
      idle,
      ..
    } = self;
    // The windows that draw, whose live views may ask for their next
    // frame.
    let mut drawn = Vec::new();
    for entry in windows.entries.iter_mut() {
      if entry.surface.is_some() && entry.window.take_surface_changed() {
        // Settings layer-shell would refuse are never sent; the
        // surface keeps the last good ones.
        let config = entry.window.surface();
        if !refused(entry, &config)
          && let Some(surface) = &mut entry.surface
        {
          // Before `reconfigure`, whose commit applies it.
          surface.send_mask(compositor, config.mask.as_ref());
          surface.send_blur(effects, compositor, entry.window.blur().as_ref(), qh);
          surface.reconfigure(&config);
        }
      }
      let Some(surface) = entry.surface.as_mut() else {
        continue;
      };
      if !surface.frames.pacing.configured() {
        continue;
      }
      // A new size cuts the mask again; the frame drawn at that size
      // commits it.
      if surface.mask_outdated() {
        surface.send_mask(compositor, entry.window.surface().mask.as_ref());
      }
      surface.inhibit_idle(idle, entry.window.inhibits_idle(), qh);
      if surface.blur_outdated() {
        surface.send_blur(effects, compositor, entry.window.blur().as_ref(), qh);
      }
      if entry.window.needs_layout() {
        entry.window.layout(text);
        surface.fit(entry.window.size());
        // A layout can move a button out from under a still
        // pointer, or under it.
        for pointer in pointers
          .iter_mut()
          .filter(|pointer| pointer.cursor.focus() == Some(entry.id))
        {
          pointer.show(entry.window.cursor());
        }
      }
      if surface.frames.pacing.should_draw(entry.window.has_damage()) {
        let started = Instant::now();
        if surface.draw(&entry.window, renderer, text, qh) {
          stats.commits += 1;
          stats.drawing += started.elapsed();
          drawn.push(entry.id);
        }
      }
    }
    self.sync_captures();
    self.paced_captures(&drawn);
    self.sync_text_inputs();
    self.tick_animations();
    self.commit_windowsets();
    if let Err(error) = self.connection.flush() {
      tracing::warn!(%error, "couldn't flush the Wayland connection");
    }
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
      let config = entry.window.surface();
      if refused(entry, &config) {
        continue;
      }
      let (output, output_scale) = (output.clone(), info.scale);
      let pool = match SlotPool::new(INITIAL_POOL_BYTES, &self.shm) {
        Ok(pool) => pool,
        Err(error) => {
          tracing::warn!(%error, "couldn't create a shared-memory pool");
          continue;
        }
      };
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
      let mut surface = Surface::new(Role::Layer(layer), fractional, pool, scale);
      surface.namespace = namespace;
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
