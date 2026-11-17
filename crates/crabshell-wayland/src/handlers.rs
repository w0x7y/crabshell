//! What [`Platform`] does with each compositor event. sctk calls these
//! trait methods from its dispatch code; each finds the window the event
//! is for and updates it. Nothing here trusts the compositor's numbers
//! without checking them.

use crabshell_ui::{Modifiers, Size};
use smithay_client_toolkit::compositor::CompositorHandler;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::protocol::{
  wl_output::{self, WlOutput},
  wl_pointer::WlPointer,
  wl_seat::WlSeat,
  wl_surface::WlSurface,
  wl_touch::WlTouch,
};
use smithay_client_toolkit::reexports::client::{
  Connection, Dispatch, Proxy, QueueHandle, delegate_noop,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
  LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
  self, WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::events::Event;
use crate::keyboard::SeatKeyboard;
use crate::outputs::replace;
use crate::platform::{Platform, SeatPointer, SeatTouch};
use crate::pointer::{self, Cursor, Input, Touch};
use crate::scale::Scale;
use crate::surface::{Role, Surface};
use crate::windows::{Entry, WindowId};

/// A window the platform holds, with its real surface and output.
type Held = Entry<Surface, WlOutput>;

impl Platform {
  fn by_surface(&mut self, surface: &WlSurface) -> Option<&mut Held> {
    self.windows.entries.iter_mut().find(|entry| {
      entry
        .surface
        .as_ref()
        .is_some_and(|ours| ours.wl_surface() == surface)
    })
  }

  fn by_layer(&mut self, layer: &LayerSurface) -> Option<&mut Held> {
    self.windows.entries.iter_mut().find(|entry| {
      entry
        .surface
        .as_ref()
        .is_some_and(|ours| matches!(&ours.role, Role::Layer(ours) if ours == layer))
    })
  }

  /// Lets go of a seat's pointer, unhovering the window it was over.
  fn drop_pointer(&mut self, index: usize) {
    let pointer = self.pointers.remove(index);
    if self
      .last_button
      .as_ref()
      .is_some_and(|(seat, _)| *seat == pointer.seat)
    {
      self.last_button = None;
    }
    if let Some(entry) = pointer.cursor.focus().and_then(|id| self.windows.get(id)) {
      entry.window.pointer_leave();
    }
    if let Some(shapes) = pointer.shapes {
      shapes.destroy();
    }
    // `release` came in version 3; older pointers just stop being used.
    if pointer.pointer.version() >= 3 {
      pointer.pointer.release();
    }
  }
}

impl CompositorHandler for Platform {
  /// A whole scale, from `wl_surface.preferred_buffer_scale` or the
  /// outputs the surface is on. Only used without fractional scaling.
  fn scale_factor_changed(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    surface: &WlSurface,
    new_factor: i32,
  ) {
    let Some(Entry {
      surface: Some(ours),
      ..
    }) = self.by_surface(surface)
    else {
      return;
    };
    if ours.fractional.is_some() {
      return;
    }
    match Scale::integer(new_factor) {
      Some(scale) => ours.frames.set_scale(scale),
      None => tracing::warn!(new_factor, "ignoring a scale out of range"),
    }
  }

  fn transform_changed(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    _: &WlSurface,
    _: wl_output::Transform,
  ) {
  }

  fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
    if let Some(Entry {
      surface: Some(ours),
      ..
    }) = self.by_surface(surface)
    {
      ours.frames.pacing.frame_done();
    }
  }

  fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}

  fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
}

impl LayerShellHandler for Platform {
  /// The compositor won't show the surface again, as when its output is
  /// unplugged. The window is dropped with its owner, then the listener
  /// hears of it.
  fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
    let Some(id) = self.by_layer(layer).map(|entry| entry.id) else {
      return;
    };
    tracing::info!(?id, "the compositor closed a window");
    self.remove_window(id);
    self.notify(Event::WindowClosed(id));
  }

  /// sctk has acknowledged the configure; the next commit applies it.
  fn configure(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    layer: &LayerSurface,
    configure: LayerSurfaceConfigure,
    _: u32,
  ) {
    let Some(Entry {
      window,
      surface: Some(ours),
      ..
    }) = self.by_layer(layer)
    else {
      return;
    };
    match ours.configure(configure.new_size, &window.surface()) {
      Some((width, height)) => window.set_size(Size::new(width as f32, height as f32)),
      None => tracing::warn!(
          size = ?configure.new_size,
          "ignoring a configure with a size that can't be drawn"
      ),
    }
  }
}

impl OutputHandler for Platform {
  fn output_state(&mut self) -> &mut OutputState {
    &mut self.outputs
  }

  fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
    let Some(info) = self.describe(&output) else {
      tracing::warn!("an output without a name can't hold windows; it needs wl_output version 4");
      return;
    };
    tracing::info!(name = info.name, mode = ?info.mode, scale = info.scale, "output added");
    self.foreign.output_changed(&output);
    self.windowsets.output_changed(&output);
    self.known.push((output.clone(), info.clone()));
    self.lock_output(&output);
    self.notify(Event::OutputAdded(info));
  }

  /// A new mode, scale, place or anything else. A mode change reaches
  /// the windows as a configure, and a scale change as a preferred scale,
  /// so this keeps the information current and tells the listener.
  fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
    let Some(info) = self.describe(&output) else {
      return;
    };
    self.foreign.output_changed(&output);
    self.windowsets.output_changed(&output);
    match self.known.iter_mut().find(|(known, _)| *known == output) {
      Some((_, known)) => {
        if let Some(info) = replace(known, info) {
          self.notify(Event::OutputChanged(info));
        }
      }
      // It has a name only now.
      None => {
        self.known.push((output.clone(), info.clone()));
        self.lock_output(&output);
        self.notify(Event::OutputAdded(info));
      }
    }
  }

  /// The output is gone: the windows on it are dropped, with their
  /// owners, before the listener hears of it.
  fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
    let Some(index) = self.known.iter().position(|(known, _)| *known == output) else {
      return;
    };
    let (_, info) = self.known.remove(index);
    self.foreign.output_changed(&output);
    self.windowsets.output_changed(&output);
    let dropped = self.windows.drop_output(&output, &info.name);
    tracing::info!(name = info.name, windows = dropped, "output removed");
    self.notify(Event::OutputRemoved(info));
  }
}

impl SeatHandler for Platform {
  fn seat_state(&mut self) -> &mut SeatState {
    &mut self.seats
  }

  fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
    self.add_text_input(qh, &seat);
    self.add_data_device(qh, &seat);
  }

  fn new_capability(
    &mut self,
    _: &Connection,
    qh: &QueueHandle<Self>,
    seat: WlSeat,
    capability: Capability,
  ) {
    match capability {
      Capability::Pointer => self.add_pointer(qh, seat),
      Capability::Keyboard => self.add_keyboard(qh, seat),
      Capability::Touch => self.add_touch(qh, seat),
      _ => {}
    }
  }

  fn remove_capability(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    seat: WlSeat,
    capability: Capability,
  ) {
    match capability {
      Capability::Pointer => self.drop_pointers(&seat),
      Capability::Keyboard => self.drop_keyboards(&seat),
      Capability::Touch => self.drop_touches(&seat),
      _ => {}
    }
  }

  fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
    self.drop_pointers(&seat);
    self.drop_keyboards(&seat);
    self.drop_touches(&seat);
    self.drop_text_input(&seat);
    self.drop_data_device(&seat);
  }
}

impl Platform {
  fn add_pointer(&mut self, qh: &QueueHandle<Self>, seat: WlSeat) {
    if self.pointers.iter().any(|ours| ours.seat == seat) {
      return;
    }
    let pointer = match self.seats.get_pointer(qh, &seat) {
      Ok(pointer) => pointer,
      Err(error) => {
        tracing::warn!(%error, "couldn't get the seat's pointer");
        return;
      }
    };
    let shapes = self
      .cursor_shapes
      .as_ref()
      .map(|manager| manager.get_shape_device(&pointer, qh));
    self.pointers.push(SeatPointer {
      seat,
      pointer,
      shapes,
      cursor: Cursor::default(),
    });
  }

  fn add_keyboard(&mut self, qh: &QueueHandle<Self>, seat: WlSeat) {
    if self.keyboards.iter().any(|ours| ours.seat == seat) {
      return;
    }
    let keyboard = self.seats.get_keyboard_with_repeat(
      qh,
      &seat,
      None,
      self.loop_handle(),
      Box::new(|platform: &mut Platform, keyboard, event| platform.key(keyboard, &event, true)),
    );
    match keyboard {
      Ok(keyboard) => self.keyboards.push(SeatKeyboard {
        seat,
        keyboard,
        focus: None,
        modifiers: Modifiers::default(),
        serial: 0,
      }),
      Err(error) => tracing::warn!(%error, "couldn't get the seat's keyboard"),
    }
  }

  fn drop_pointers(&mut self, seat: &WlSeat) {
    while let Some(index) = self.pointers.iter().position(|ours| ours.seat == *seat) {
      self.drop_pointer(index);
    }
  }

  fn add_touch(&mut self, qh: &QueueHandle<Self>, seat: WlSeat) {
    if self.touches.iter().any(|ours| ours.seat == seat) {
      return;
    }
    match self.seats.get_touch(qh, &seat) {
      Ok(touch) => self.touches.push(SeatTouch {
        seat,
        touch,
        state: Touch::default(),
      }),
      Err(error) => tracing::warn!(%error, "couldn't get the seat's touchscreen"),
    }
  }

  /// Lets go of a seat's touchscreen, ending a touch held on a window.
  fn drop_touches(&mut self, seat: &WlSeat) {
    while let Some(index) = self.touches.iter().position(|ours| ours.seat == *seat) {
      let mut ours = self.touches.remove(index);
      if let Some(window) = ours.state.window() {
        let inputs = ours.state.cancel();
        self.touch_inputs(window, inputs);
      }
      // `release` came in version 3; older ones just stop being used.
      if ours.touch.version() >= 3 {
        ours.touch.release();
      }
    }
  }

  /// Hands a touch's inputs to `window`, if it is still held.
  fn touch_inputs(&mut self, window: WindowId, inputs: Vec<Input>) {
    if let Some(entry) = self.windows.get(window) {
      for input in inputs {
        pointer::apply(&entry.window, input);
      }
    }
  }

  fn drop_keyboards(&mut self, seat: &WlSeat) {
    while let Some(index) = self.keyboards.iter().position(|ours| ours.seat == *seat) {
      self.drop_keyboard(index);
    }
  }
}

impl PointerHandler for Platform {
  fn pointer_frame(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    pointer: &WlPointer,
    events: &[PointerEvent],
  ) {
    let Some(ours) = self
      .pointers
      .iter_mut()
      .find(|ours| &ours.pointer == pointer)
    else {
      return;
    };
    for event in events {
      let Some(entry) = self.windows.entries.iter().find(|entry| {
        entry
          .surface
          .as_ref()
          .is_some_and(|surface| surface.wl_surface() == &event.surface)
      }) else {
        continue;
      };
      match event.kind {
        PointerEventKind::Enter { serial } => ours.cursor.enter(serial, entry.id),
        PointerEventKind::Leave { .. } => ours.cursor.leave(),
        PointerEventKind::Press { serial, .. } | PointerEventKind::Release { serial, .. } => {
          if let PointerEventKind::Press { .. } = event.kind {
            self.last_input = Some((ours.seat.clone(), serial, entry.id));
          }
          self.last_button = Some((ours.seat.clone(), serial));
          self.button_held = matches!(event.kind, PointerEventKind::Press { .. });
          // Shift and a press extend a selection.
          if let Some(keyboard) = self.keyboards.iter().find(|k| k.seat == ours.seat) {
            entry.window.set_modifiers(keyboard.modifiers);
          }
        }
        _ => {}
      }
      let mut cursor = None;
      for input in pointer::inputs(&event.kind, event.position) {
        cursor = Some(pointer::apply(&entry.window, input));
      }
      // Nothing is sent after a leave: the cursor has no window.
      if let Some(cursor) = cursor {
        ours.show(cursor);
      }
    }
  }
}

/// One finger at a time acts as the pointer, so a tap clicks and a
/// moving finger drags. A touch has no cursor to set.
impl TouchHandler for Platform {
  fn down(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    touch: &WlTouch,
    serial: u32,
    time: u32,
    surface: WlSurface,
    id: i32,
    position: (f64, f64),
  ) {
    let Some(window) = self.by_surface(&surface).map(|entry| entry.id) else {
      return;
    };
    let Some(ours) = self.touches.iter_mut().find(|ours| ours.touch == *touch) else {
      return;
    };
    let inputs = ours.state.down(id, window, position, time);
    if !inputs.is_empty() {
      // A popup opened by a tap grabs with the down's serial.
      self.last_button = Some((ours.seat.clone(), serial));
      self.last_input = Some((ours.seat.clone(), serial, window));
      self.button_held = true;
    }
    self.touch_inputs(window, inputs);
  }

  fn up(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    touch: &WlTouch,
    serial: u32,
    time: u32,
    id: i32,
  ) {
    let Some(ours) = self.touches.iter_mut().find(|ours| ours.touch == *touch) else {
      return;
    };
    let window = ours.state.window();
    let inputs = ours.state.up(id, time);
    if !inputs.is_empty() {
      self.last_button = Some((ours.seat.clone(), serial));
      self.button_held = false;
    }
    if let Some(window) = window {
      self.touch_inputs(window, inputs);
    }
  }

  fn motion(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    touch: &WlTouch,
    time: u32,
    id: i32,
    position: (f64, f64),
  ) {
    let Some(ours) = self.touches.iter_mut().find(|ours| ours.touch == *touch) else {
      return;
    };
    let window = ours.state.window();
    let inputs = ours.state.motion(id, position, time);
    if let Some(window) = window {
      self.touch_inputs(window, inputs);
    }
  }

  fn shape(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: i32, _: f64, _: f64) {}

  fn orientation(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: i32, _: f64) {}

  fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, touch: &WlTouch) {
    let Some(ours) = self.touches.iter_mut().find(|ours| ours.touch == *touch) else {
      return;
    };
    let window = ours.state.window();
    let inputs = ours.state.cancel();
    if let Some(window) = window {
      self.button_held = false;
      self.touch_inputs(window, inputs);
    }
  }
}

impl ShmHandler for Platform {
  fn shm_state(&mut self) -> &mut Shm {
    &mut self.shm
  }
}

impl ProvidesRegistryState for Platform {
  fn registry(&mut self) -> &mut RegistryState {
    &mut self.registry
  }

  registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpFractionalScaleV1, ()> for Platform {
  fn event(
    platform: &mut Platform,
    proxy: &WpFractionalScaleV1,
    event: wp_fractional_scale_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let wp_fractional_scale_v1::Event::PreferredScale { scale } = event else {
      return;
    };
    let Some(ours) = platform.windows.entries.iter_mut().find_map(|entry| {
      entry.surface.as_mut().filter(|surface| {
        surface
          .fractional
          .as_ref()
          .is_some_and(|(ours, _)| ours == proxy)
      })
    }) else {
      return;
    };
    match Scale::fractional(scale) {
      Some(scale) => ours.frames.set_scale(scale),
      None => tracing::warn!(scale, "ignoring a fractional scale out of range"),
    }
  }
}

// These have no events.
delegate_noop!(Platform: WpFractionalScaleManagerV1);
delegate_noop!(Platform: WpViewporter);
delegate_noop!(Platform: WpViewport);

// sctk 0.21: routes the events of every object sctk made to the handler
// traits above.
delegate_dispatch2!(Platform);
delegate_registry!(Platform);
