//! niri's workspaces, windows, keyboard layouts and overview, over its IPC
//! socket at `$NIRI_SOCKET`. A request is one JSON line, answered with
//! one, and `"EventStream"` turns a connection into a stream of events,
//! a JSON line each, the first ones giving the whole state.

#[cfg(test)]
mod tests;

use std::cell::{RefCell, RefMut};
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe, Location};
use std::path::Path;
use std::rc::Rc;
use std::sync::Once;

use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{Owner, ReadSignal, Signal};
use crabshell_ui::Cx;
use serde::de::{DeserializeOwned, Error as _, IgnoredAny};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::Shared;
use crate::dbus::set_changed;
use crate::hyprland::{cut, open};
use crate::lines::{MAX_LINE, panic_message};

/// The most workspaces, windows or layout names kept; the rest are left
/// out.
pub(crate) const MAX_LIST: usize = 4096;

/// The longest action sent, as a JSON line.
pub(crate) const MAX_REQUEST: usize = 8 * 1024;

/// The most actions waiting for niri's reply; one past it isn't sent, so
/// a config acting in a loop can't pile up connections.
pub(crate) const MAX_ACTIONS: usize = 64;

/// One of niri's workspaces, as its IPC describes it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct NiriWorkspace {
  /// niri's id for it, which stays while it exists: the key to give
  /// `children_keyed`, and what [`Niri::focus_workspace`] takes.
  pub id: u64,
  /// Its place on its output, from 1. It changes as workspaces move.
  pub idx: u32,
  /// Its name, if the config gave it one.
  #[serde(default, deserialize_with = "some_text")]
  pub name: Option<String>,
  /// The output it is on, such as `DP-1`; `None` while no output is
  /// connected.
  #[serde(default, deserialize_with = "some_text")]
  pub output: Option<String>,
  /// Whether it is shown on its output.
  pub is_active: bool,
  /// Whether it has the focus; one workspace across all outputs does.
  pub is_focused: bool,
  /// Whether a window on it asks for attention.
  #[serde(default)]
  pub is_urgent: bool,
  /// The id of the window that is active on it, if any.
  #[serde(default)]
  pub active_window_id: Option<u64>,
}

/// One of niri's windows, as its IPC describes it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct NiriWindow {
  /// niri's id for it, which stays while it is open: the key to give
  /// `children_keyed`, and what [`Niri::focus_window`] takes.
  pub id: u64,
  /// Its title, if it set one.
  #[serde(default, deserialize_with = "some_text")]
  pub title: Option<String>,
  /// Its app id, if it set one.
  #[serde(default, deserialize_with = "some_text")]
  pub app_id: Option<String>,
  /// The id of the workspace it is on, if any.
  #[serde(default)]
  pub workspace_id: Option<u64>,
  /// Whether it has the keyboard focus.
  pub is_focused: bool,
  /// Whether it asks for attention.
  #[serde(default)]
  pub is_urgent: bool,
}

/// niri's keyboard layouts.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct NiriKeyboardLayouts {
  /// Each layout's XKB name, such as `English (US)`.
  #[serde(deserialize_with = "texts")]
  pub names: Vec<String>,
  /// Which of `names` is in use; always one of them.
  pub current_idx: u32,
}

/// The niri service, from `cx.niri()`. A clone is cheap and shares the
/// one connection.
#[derive(Clone)]
pub struct Niri {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  /// The asking context's owner, which [`on_event`](Self::on_event)'s
  /// handlers go with.
  owner: Owner,
  workspaces: ReadSignal<Vec<NiriWorkspace>>,
  windows: ReadSignal<Vec<NiriWindow>>,
  keyboard_layouts: ReadSignal<Option<NiriKeyboardLayouts>>,
  overview_open: ReadSignal<bool>,
}

impl Niri {
  /// Every workspace, by output name and then by `idx`. Empty while niri
  /// isn't connected.
  pub fn workspaces(&self) -> ReadSignal<Vec<NiriWorkspace>> {
    self.workspaces
  }

  /// Every window, by id. Empty while niri isn't connected.
  pub fn windows(&self) -> ReadSignal<Vec<NiriWindow>> {
    self.windows
  }

  /// The keyboard layouts, once niri has said them.
  pub fn keyboard_layouts(&self) -> ReadSignal<Option<NiriKeyboardLayouts>> {
    self.keyboard_layouts
  }

  /// Whether niri's overview is open.
  pub fn overview_open(&self) -> ReadSignal<bool> {
    self.overview_open
  }

  /// Switches to the workspace with the id `id`.
  pub fn focus_workspace(&self, id: u64) {
    self.action(serde_json::json!({ "FocusWorkspace": { "reference": { "Id": id } } }));
  }

  /// Focuses the window with the id `id`.
  pub fn focus_window(&self, id: u64) {
    self.action(serde_json::json!({ "FocusWindow": { "id": id } }));
  }

  /// Sends niri `{"Action": action}`, as `niri msg action` does, on a
  /// connection of its own, and never waits for it. `action` is one of
  /// niri-ipc's `Action`s in JSON, such as
  /// `json!({ "Spawn": { "command": ["foot"] } })`. niri's refusal is
  /// logged, and so is an action over 8 KiB, or past 64 waiting for a
  /// reply, which isn't sent.
  pub fn action(&self, action: Value) {
    match &self.service {
      Some(service) => service.action(action),
      None => tracing::warn!("no niri to send an action to"),
    }
  }

  /// Calls `handler` with every event niri sends, as its JSON, such as
  /// `{"OverviewOpenedOrClosed":{"is_open":true}}`, until the context
  /// `cx.niri()` was called on goes. The lists have taken the event in
  /// by then.
  ///
  /// It runs on the main thread. If it panics, that is logged with the
  /// event's name and where `on_event` was called, and it isn't called
  /// again. Called on a handle whose context went, it logs that and does
  /// nothing.
  #[track_caller]
  pub fn on_event(&self, handler: impl Fn(&Value) + 'static) {
    let at = Location::caller();
    if self.owner.is_disposed() {
      return tracing::warn!(
        "niri on_event (called at {at}) on a context that is gone does nothing"
      );
    }
    if let Some(service) = &self.service {
      let id = service.add_handler(Rc::new(handler), at);
      self.owner.signal(Unregister(Rc::clone(service), id));
    }
  }
}

/// `cx.niri()`.
pub(crate) fn niri(cx: &Cx) -> Niri {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.niri() needs the services the shell provides; it shows nothing");
    });
    return Niri {
      service: None,
      owner: cx.owner(),
      workspaces: cx.signal(Vec::new()).read_only(),
      windows: cx.signal(Vec::new()).read_only(),
      keyboard_layouts: cx.signal(None).read_only(),
      overview_open: cx.signal(false).read_only(),
    };
  };
  let (workspaces, windows, keyboard_layouts, overview_open) = service.start();
  Niri {
    service: Some(service),
    owner: cx.owner(),
    workspaces: workspaces.read_only(),
    windows: windows.read_only(),
    keyboard_layouts: keyboard_layouts.read_only(),
    overview_open: overview_open.read_only(),
  }
}

type Signals = (
  Signal<Vec<NiriWorkspace>>,
  Signal<Vec<NiriWindow>>,
  Signal<Option<NiriKeyboardLayouts>>,
  Signal<bool>,
);

/// The one niri connection of a shell, started on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  workspaces: Signal<Vec<NiriWorkspace>>,
  windows: Signal<Vec<NiriWindow>>,
  keyboard_layouts: Signal<Option<NiriKeyboardLayouts>>,
  overview_open: Signal<bool>,
  /// What niri's events say, shown in the signals after each read.
  raw: Raw,
  /// The event stream, while it is open.
  stream: Option<Stream>,
  /// Whether the stream was opened again since niri last sent an event
  /// that isn't one of [`STATE_EVENTS`].
  reconnected: bool,
  /// Actions waiting for niri's reply.
  actions: Vec<Action>,
  next_action: u64,
  /// What `on_event` was given, in the order it was.
  handlers: Vec<Handler>,
  next_handler: u64,
}

#[derive(Default)]
struct Raw {
  /// By output and `idx`.
  workspaces: Vec<NiriWorkspace>,
  /// By id.
  windows: Vec<NiriWindow>,
  keyboard_layouts: Option<NiriKeyboardLayouts>,
  overview_open: bool,
}

struct Stream {
  stream: UnixStream,
  reader: RegistrationToken,
  /// What was read past the last whole line.
  inbox: Vec<u8>,
  /// Whether niri's reply to `"EventStream"`, its first line, came.
  answered: bool,
}

struct Action {
  id: u64,
  stream: UnixStream,
  reader: RegistrationToken,
  reply: Vec<u8>,
}

/// A config's raw-event handler.
#[derive(Clone)]
struct Handler {
  id: u64,
  /// Where `on_event` was called, for the log if it panics.
  at: &'static Location<'static>,
  handler: Rc<dyn Fn(&Value)>,
}

/// Drops a handler when the context it was given in goes.
struct Unregister(Rc<Service>, u64);

impl Drop for Unregister {
  fn drop(&mut self) {
    let handler = self.0.remove_handler(self.1);
    // Dropped outside the borrow: the handler may hold a `Niri`.
    drop(handler);
  }
}

/// niri's reply to a request.
#[derive(Deserialize)]
enum Reply {
  Ok(IgnoredAny),
  Err(String),
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
    }
  }

  /// The signals, connecting to niri if this is the first ask.
  fn start(self: &Rc<Self>) -> Signals {
    if self.state.borrow().is_none() {
      let root = &self.shared.root;
      *self.state.borrow_mut() = Some(State {
        workspaces: root.signal(Vec::new()),
        windows: root.signal(Vec::new()),
        keyboard_layouts: root.signal(None),
        overview_open: root.signal(false),
        raw: Raw::default(),
        stream: None,
        reconnected: false,
        actions: Vec::new(),
        next_action: 0,
        handlers: Vec::new(),
        next_handler: 0,
      });
      match &self.shared.config.niri {
        None => tracing::info!("NIRI_SOCKET is unset, so there is no niri to show"),
        Some(path) => {
          self.connect(path);
        }
      }
    }
    let state = self.state.borrow();
    let state = state.as_ref().expect("invariant: started above");
    (
      state.workspaces,
      state.windows,
      state.keyboard_layouts,
      state.overview_open,
    )
  }

  /// Opens the event stream. A failure is logged and never retried, so a
  /// stale socket never wakes the shell. `false` if it failed.
  fn connect(self: &Rc<Self>, path: &Path) -> bool {
    let Some(event_loop) = self.shared.event_loop() else {
      return false;
    };
    let opened = open(path).and_then(|mut stream| {
      send(&mut stream, b"\"EventStream\"\n")?;
      let fd = stream.as_fd().try_clone_to_owned()?;
      Ok((stream, fd))
    });
    let (stream, fd) = match opened {
      Ok(opened) => opened,
      Err(error) => {
        let path = path.display();
        tracing::warn!(%path, %error, "can't connect to niri, so its lists stay empty");
        return false;
      }
    };
    let service = Rc::clone(self);
    let Some(reader) =
      event_loop.insert_fd(fd, Interest::READ, Box::new(move || service.on_stream()))
    else {
      return false;
    };
    let mut state = self.state.borrow_mut();
    let state = state
      .as_mut()
      .expect("invariant: started before connecting");
    state.stream = Some(Stream {
      stream,
      reader,
      inbox: Vec::new(),
      answered: false,
    });
    true
  }

  /// Whether the lists are still there: their owner, the root, isn't
  /// disposed.
  fn alive(&self) -> bool {
    let state = self.state.borrow();
    let alive = state
      .as_ref()
      .and_then(|state| state.workspaces.try_with(|_| ()));
    alive.is_some()
  }

  /// Reads what came on the event stream, takes in each whole event and
  /// shows the lists, then hands the events to the handlers.
  fn on_stream(self: &Rc<Self>) {
    if !self.alive() {
      return self.close();
    }
    let mut chunk = [0; 64 * 1024];
    let mut guard = self.state.borrow_mut();
    let state = guard
      .as_mut()
      .expect("invariant: started before connecting");
    let Some(stream) = state.stream.as_mut() else {
      return;
    };
    match stream.stream.read(&mut chunk) {
      Ok(0) => return self.closed(guard, "niri closed the event stream"),
      Ok(read) => stream.inbox.extend_from_slice(&chunk[..read]),
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      Err(error) => return self.closed(guard, &error.to_string()),
    }
    let Some(whole) = whole_lines(&mut stream.inbox) else {
      // Reconnecting would only read it again.
      return self.lost(state, "niri sent a line over 1 MiB");
    };
    let mut events = Vec::new();
    let mut touched = 0;
    for line in whole
      .split(|&byte| byte == b'\n')
      .filter(|line| !line.is_empty())
    {
      let stream = state
        .stream
        .as_mut()
        .expect("invariant: open while reading");
      if !std::mem::replace(&mut stream.answered, true) {
        if let Some(why) = refusal(line) {
          tracing::warn!(why, "niri refused the event stream");
          return self.lost(state, "niri refused the event stream");
        }
        continue;
      }
      let event: Value = match serde_json::from_slice(line) {
        Ok(event) => event,
        Err(error) => {
          tracing::warn!(%error, "a line from niri's event stream that isn't JSON");
          continue;
        }
      };
      let name = event_name(&event);
      if !STATE_EVENTS.contains(&name) {
        // Past niri's state, so a later close is worth a reconnect.
        state.reconnected = false;
      }
      match state.raw.apply(&event) {
        Ok(parts) => touched |= parts,
        Err(error) => tracing::warn!(name, %error, "a bad event from niri"),
      }
      if !state.handlers.is_empty() {
        events.push(event);
      }
    }
    state.show(touched);
    drop(guard);
    for event in events {
      self.deliver(&event);
    }
  }

  /// Calls each raw-event handler with `event`, with nothing borrowed.
  fn deliver(&self, event: &Value) {
    let handlers = match self.state.borrow().as_ref() {
      Some(state) => state.handlers.clone(),
      None => return,
    };
    for Handler { id, at, handler } in handlers {
      let registered = self
        .state
        .borrow()
        .as_ref()
        .is_some_and(|state| state.handlers.iter().any(|handler| handler.id == id));
      if !registered {
        // An earlier handler's context went.
        continue;
      }
      if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| handler(event))) {
        let message = panic_message(payload.as_ref());
        let name = event_name(event);
        tracing::error!("niri on_event (called at {at}) panicked on {name}: {message}");
        drop(self.remove_handler(id));
      }
    }
  }

  fn add_handler(&self, handler: Rc<dyn Fn(&Value)>, at: &'static Location<'static>) -> u64 {
    let mut state = self.state.borrow_mut();
    let state = state.as_mut().expect("invariant: started by cx.niri()");
    let id = state.next_handler;
    state.next_handler += 1;
    state.handlers.push(Handler { id, at, handler });
    id
  }

  /// Takes out the handler `id`, for the caller to drop once nothing is
  /// borrowed.
  fn remove_handler(&self, id: u64) -> Option<Handler> {
    let mut state = self.state.borrow_mut();
    let handlers = &mut state.as_mut()?.handlers;
    let at = handlers.iter().position(|handler| handler.id == id)?;
    Some(handlers.remove(at))
  }

  /// Sends `{"Action": action}` on a connection of its own.
  fn action(self: &Rc<Self>, action: Value) {
    let (Some(path), Some(event_loop)) = (&self.shared.config.niri, self.shared.event_loop())
    else {
      return tracing::warn!("no niri to send an action to");
    };
    let mut request = serde_json::json!({ "Action": action }).to_string();
    request.push('\n');
    if request.len() > MAX_REQUEST {
      return tracing::warn!("a niri action over 8 KiB is too long to send");
    }
    let mut state = self.state.borrow_mut();
    let state = state.as_mut().expect("invariant: started by cx.niri()");
    if state.actions.len() >= MAX_ACTIONS {
      return tracing::warn!("64 actions wait for niri, so this one isn't sent");
    }
    let opened = open(path).and_then(|mut stream| {
      send(&mut stream, request.as_bytes())?;
      // niri answers, then reads the end and closes.
      stream.shutdown(Shutdown::Write)?;
      let fd = stream.as_fd().try_clone_to_owned()?;
      Ok((stream, fd))
    });
    let (stream, fd) = match opened {
      Ok(opened) => opened,
      Err(error) => return tracing::warn!(%error, "can't send niri an action"),
    };
    let id = state.next_action;
    state.next_action += 1;
    let service = Rc::clone(self);
    let read = Box::new(move || service.on_reply(id));
    if let Some(reader) = event_loop.insert_fd(fd, Interest::READ, read) {
      state.actions.push(Action {
        id,
        stream,
        reader,
        reply: Vec::new(),
      });
    }
  }

  /// Reads what came of an action's reply, and logs a refusal once niri
  /// closed the connection.
  fn on_reply(self: &Rc<Self>, id: u64) {
    if !self.alive() {
      return self.close();
    }
    let mut chunk = [0; 4096];
    let mut state = self.state.borrow_mut();
    let state = state.as_mut().expect("invariant: started before acting");
    let Some(at) = state.actions.iter().position(|action| action.id == id) else {
      return;
    };
    let action = &mut state.actions[at];
    let failed = match action.stream.read(&mut chunk) {
      Ok(0) => None,
      Ok(read) if action.reply.len() + read > MAX_LINE => Some(String::from("over 1 MiB")),
      Ok(read) => return action.reply.extend_from_slice(&chunk[..read]),
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      Err(error) => Some(error.to_string()),
    };
    let action = state.actions.remove(at);
    if let Some(event_loop) = self.shared.event_loop() {
      event_loop.remove(action.reader);
    }
    if let Some(why) = failed.or_else(|| refusal(&action.reply)) {
      tracing::warn!(why, "niri refused an action");
    }
  }

  /// The event stream ended for `why`. niri closes a client that falls
  /// behind and runs on, so while its socket is there, this connects once
  /// again and takes in the whole state niri sends first, keeping the
  /// lists meanwhile. A stream that ends again before niri sent an event
  /// past its state is lost.
  fn closed(self: &Rc<Self>, mut guard: RefMut<'_, Option<State>>, why: &str) {
    let state = guard
      .as_mut()
      .expect("invariant: started before connecting");
    let path = self.shared.config.niri.as_deref();
    let Some(path) = path.filter(|path| !state.reconnected && path.exists()) else {
      return self.lost(state, why);
    };
    tracing::warn!(why, "niri's event stream ended; connecting again");
    state.reconnected = true;
    self.drop_stream(state);
    drop(guard);
    if !self.connect(path)
      && let Some(state) = self.state.borrow_mut().as_mut()
    {
      self.lost(state, "niri's socket can't be reached again");
    }
  }

  /// Drops the event stream and empties the lists. A new niri listens at
  /// a new path, so nothing reconnects.
  fn lost(&self, state: &mut State, why: &str) {
    tracing::warn!(why, "lost niri; its lists are empty");
    self.drop_stream(state);
    state.raw = Raw::default();
    state.show(ALL);
  }

  /// Closes every connection for good, once the lists are gone.
  fn close(&self) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      self.drop_stream(state);
      let actions = std::mem::take(&mut state.actions);
      if let Some(event_loop) = self.shared.event_loop() {
        for action in actions {
          event_loop.remove(action.reader);
        }
      }
    }
  }

  fn drop_stream(&self, state: &mut State) {
    if let (Some(stream), Some(event_loop)) = (state.stream.take(), self.shared.event_loop()) {
      event_loop.remove(stream.reader);
    }
  }
}

impl State {
  /// Sets each signal of the `parts` given whose value changed.
  fn show(&self, parts: u8) {
    let raw = &self.raw;
    if parts & WORKSPACES != 0 {
      set_changed(self.workspaces, raw.workspaces.clone());
    }
    if parts & WINDOWS != 0 {
      set_changed(self.windows, raw.windows.clone());
    }
    if parts & LAYOUTS != 0 {
      set_changed(self.keyboard_layouts, raw.keyboard_layouts.clone());
    }
    if parts & OVERVIEW != 0 {
      set_changed(self.overview_open, raw.overview_open);
    }
  }
}

/// The parts of [`Raw`] an event can change, as bits.
const WORKSPACES: u8 = 1;
const WINDOWS: u8 = 2;
const LAYOUTS: u8 = 4;
const OVERVIEW: u8 = 8;
const ALL: u8 = WORKSPACES | WINDOWS | LAYOUTS | OVERVIEW;

/// The events niri sends a new stream as its whole state, from niri-ipc's
/// `EventStreamState::replicate`.
const STATE_EVENTS: [&str; 7] = [
  "WorkspacesChanged",
  "WindowsChanged",
  "KeyboardLayoutsChanged",
  "OverviewOpenedOrClosed",
  "ConfigLoaded",
  "CastsChanged",
  "TouchpadToggled",
];

impl Raw {
  /// Takes in one event, as niri-ipc's `EventStreamState` does. An event
  /// this doesn't know is skipped, since niri adds events between
  /// versions, and so is one about a workspace or window it doesn't have.
  /// Which parts it changed, as bits.
  fn apply(&mut self, event: &Value) -> Result<u8, serde_json::Error> {
    let Some((name, body)) = event
      .as_object()
      .filter(|event| event.len() == 1)
      .and_then(|event| event.iter().next())
    else {
      return Err(serde_json::Error::custom("not an event"));
    };
    match name.as_str() {
      "WorkspacesChanged" => {
        self.workspaces = list("workspace", body, "workspaces")?;
        let order = |workspace: &NiriWorkspace| (workspace.output.clone(), workspace.idx);
        self.workspaces.sort_by_key(order);
      }
      "WorkspaceActivated" => {
        let id: u64 = get(body, "id")?;
        let focused: bool = get(body, "focused")?;
        let Some(output) = self.workspace(id).map(|workspace| workspace.output.clone()) else {
          return Ok(0);
        };
        for workspace in &mut self.workspaces {
          let activated = workspace.id == id;
          if workspace.output == output {
            workspace.is_active = activated;
          }
          if focused {
            workspace.is_focused = activated;
          }
        }
      }
      "WorkspaceActiveWindowChanged" => {
        let id: u64 = get(body, "workspace_id")?;
        let window = get(body, "active_window_id")?;
        if let Some(workspace) = self.workspace(id) {
          workspace.active_window_id = window;
        }
      }
      "WorkspaceUrgencyChanged" => {
        let urgent = get(body, "urgent")?;
        if let Some(workspace) = self.workspace(get(body, "id")?) {
          workspace.is_urgent = urgent;
        }
      }
      "WindowsChanged" => {
        self.windows = list("window", body, "windows")?;
        self.windows.sort_by_key(|window| window.id);
      }
      "WindowOpenedOrChanged" => {
        let window: NiriWindow = get(body, "window")?;
        let id = window.id;
        if window.is_focused {
          for other in &mut self.windows {
            other.is_focused = false;
          }
        }
        match self.windows.binary_search_by_key(&id, |window| window.id) {
          Ok(at) => self.windows[at] = window,
          Err(_) if self.windows.len() >= MAX_LIST => {
            tracing::warn!("niri has more windows than the {MAX_LIST} kept");
          }
          Err(at) => self.windows.insert(at, window),
        }
      }
      "WindowClosed" => {
        let id: u64 = get(body, "id")?;
        self.windows.retain(|window| window.id != id);
      }
      "WindowFocusChanged" => {
        let id: Option<u64> = get(body, "id")?;
        for window in &mut self.windows {
          window.is_focused = Some(window.id) == id;
        }
      }
      "WindowUrgencyChanged" => {
        let (id, urgent): (u64, bool) = (get(body, "id")?, get(body, "urgent")?);
        if let Some(window) = self.windows.iter_mut().find(|window| window.id == id) {
          window.is_urgent = urgent;
        }
      }
      "KeyboardLayoutsChanged" => {
        let layouts: NiriKeyboardLayouts = get(body, "keyboard_layouts")?;
        in_names(&layouts.names, layouts.current_idx)?;
        self.keyboard_layouts = Some(layouts);
      }
      "KeyboardLayoutSwitched" => {
        let idx = get(body, "idx")?;
        if let Some(layouts) = &mut self.keyboard_layouts {
          in_names(&layouts.names, idx)?;
          layouts.current_idx = idx;
        }
      }
      "OverviewOpenedOrClosed" => self.overview_open = get(body, "is_open")?,
      _ => return Ok(0),
    }
    Ok(match name.as_str() {
      name if name.starts_with("Workspace") => WORKSPACES,
      name if name.starts_with("Window") => WINDOWS,
      name if name.starts_with("Keyboard") => LAYOUTS,
      _ => OVERVIEW,
    })
  }

  fn workspace(&mut self, id: u64) -> Option<&mut NiriWorkspace> {
    self
      .workspaces
      .iter_mut()
      .find(|workspace| workspace.id == id)
  }
}

/// Refuses a layout index past `names`, as after their cap, so
/// `names[current_idx]` is always there.
fn in_names(names: &[String], idx: u32) -> Result<(), serde_json::Error> {
  match usize::try_from(idx) {
    Ok(idx) if idx < names.len() => Ok(()),
    _ => Err(serde_json::Error::custom("a layout index past the names")),
  }
}

/// The field `key` of an event's body.
fn get<'a, T: Deserialize<'a>>(body: &'a Value, key: &str) -> Result<T, serde_json::Error> {
  T::deserialize(body.get(key).unwrap_or(&Value::Null))
}

/// The entries of the list at `key`, at most [`MAX_LIST`], leaving out
/// those that don't parse, which is logged.
fn list<T: DeserializeOwned>(
  what: &str,
  body: &Value,
  key: &str,
) -> Result<Vec<T>, serde_json::Error> {
  let Some(entries) = body.get(key).and_then(Value::as_array) else {
    return Err(serde_json::Error::custom(format!("no {key} list")));
  };
  if entries.len() > MAX_LIST {
    let listed = entries.len();
    tracing::warn!(listed, "niri listed more {what}s than the {MAX_LIST} kept");
  }
  let mut bad = 0;
  let list = entries
    .iter()
    .take(MAX_LIST)
    .filter_map(|entry| T::deserialize(entry).inspect_err(|_| bad += 1).ok())
    .collect();
  if bad > 0 {
    tracing::warn!(bad, "niri described some {what}s badly; they are left out");
  }
  Ok(list)
}

/// An event's name, its one key, for a log line.
fn event_name(event: &Value) -> &str {
  let keys = event.as_object().and_then(|event| event.keys().next());
  keys.map_or("an event", String::as_str)
}

/// What `inbox` holds up to its last line break, leaving the rest.
/// `None` when a line in it, or the rest, is over [`MAX_LINE`].
fn whole_lines(inbox: &mut Vec<u8>) -> Option<Vec<u8>> {
  let end = inbox
    .iter()
    .rposition(|&byte| byte == b'\n')
    .map_or(0, |at| at + 1);
  let rest = inbox.split_off(end);
  let whole = std::mem::replace(inbox, rest);
  let long = whole
    .split(|&byte| byte == b'\n')
    .any(|line| line.len() > MAX_LINE);
  (!long && inbox.len() <= MAX_LINE).then_some(whole)
}

/// Why niri's `reply` to a request refuses it, or `None` for `Ok`.
fn refusal(reply: &[u8]) -> Option<String> {
  match serde_json::from_slice(reply.trim_ascii()) {
    Ok(Reply::Ok(_)) => None,
    Ok(Reply::Err(why)) => Some(cut(why)),
    Err(_) => Some(cut(format!(
      "a bad reply: {}",
      String::from_utf8_lossy(reply)
    ))),
  }
}

/// Writes `request` in one write: the socket is new, so it has room.
fn send(stream: &mut UnixStream, request: &[u8]) -> std::io::Result<()> {
  if stream.write(request)? < request.len() {
    return Err(std::io::Error::other("the request didn't fit the socket"));
  }
  Ok(())
}

fn some_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
  Option::<String>::deserialize(deserializer).map(|text| text.map(cut))
}

fn texts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
  let texts = Vec::<String>::deserialize(deserializer)?;
  Ok(texts.into_iter().take(MAX_LIST).map(cut).collect())
}
