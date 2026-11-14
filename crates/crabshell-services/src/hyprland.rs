//! Hyprland's workspaces, monitors and windows, over its two IPC sockets
//! in `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`:
//! `.socket.sock` takes one request per connection, such as
//! `j/workspaces`, answers and closes, and `.socket2.sock` sends events
//! as lines of `NAME>>DATA`.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe, Location};
use std::path::Path;
use std::rc::Rc;
use std::sync::Once;

use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{Owner, ReadSignal, Signal};
use crabshell_ui::Cx;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

use crate::Shared;
use crate::dbus::set_changed;
use crate::lines::{Lines, panic_message};

/// The largest reply read from one request; a larger one is dropped.
pub(crate) const MAX_REPLY: usize = 8 * 1024 * 1024;

/// The longest request sent; a dispatch past it is refused.
pub(crate) const MAX_REQUEST: usize = 8 * 1024;

/// The most dispatches waiting for Hyprland's reply; one past it is
/// refused, so a config dispatching in a loop can't run out of files.
const MAX_DISPATCHES: usize = 64;

/// The longest text kept, cut at a character.
pub(crate) const MAX_TEXT: usize = 4 * 1024;

/// The most workspaces, monitors or windows kept; the rest are left out.
pub(crate) const MAX_LIST: usize = 4096;

/// One of Hyprland's workspaces, as `j/workspaces` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyprlandWorkspace {
  /// Its number. Named and special workspaces have none from Hyprland
  /// 0.57 on, and a number below zero before it.
  pub id: Option<i64>,
  /// Its name, such as `1`, `web` or `special:magic`: the key to give
  /// `children_keyed`.
  pub name: String,
  /// The name of the monitor it is on, such as `DP-1`.
  pub monitor: String,
  /// How many windows it holds.
  pub windows: u32,
  /// Whether it is shown on the focused monitor.
  pub focused: bool,
  /// Whether it is shown on its monitor, focused or not.
  pub active: bool,
  /// Whether a window on it asks for attention.
  pub urgent: bool,
}

/// One of Hyprland's monitors, as `j/monitors` describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct HyprlandMonitor {
  /// Hyprland's number for it.
  pub id: i64,
  /// Its name, such as `DP-1`, as the output's name.
  pub name: String,
  /// Its make, model and serial, as Hyprland puts them.
  pub description: String,
  /// Whether it has the focus.
  pub focused: bool,
  /// The name of the workspace it shows.
  pub active_workspace: String,
  /// Where its left edge is, in the layout's logical pixels.
  pub x: i32,
  /// Where its top edge is, in the layout's logical pixels.
  pub y: i32,
  /// Its mode's width, in pixels before scaling.
  pub width: i32,
  /// Its mode's height, in pixels before scaling.
  pub height: i32,
  /// Its scale, such as 1.5.
  pub scale: f64,
}

/// One of Hyprland's windows, as `j/clients` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyprlandToplevel {
  /// Its address, such as `0x55d4c8a3b0c0`: the key to give
  /// `children_keyed`, and what a dispatcher names it by, as
  /// `address:0x55d4c8a3b0c0`.
  pub address: String,
  /// Its class, the app id.
  pub class: String,
  /// Its title.
  pub title: String,
  /// The name of the workspace it is on.
  pub workspace: String,
  /// The name of the monitor it is on, or `""` while Hyprland doesn't
  /// list that monitor.
  pub monitor: String,
  /// Whether it floats.
  pub floating: bool,
  /// Whether it is fullscreen; a maximized window isn't, except on
  /// Hyprland 0.41 and older, which say only that it covers the screen.
  pub fullscreen: bool,
  /// Whether it has the keyboard focus.
  pub focused: bool,
  /// Whether it asks for attention: from its `urgent` event until it is
  /// focused.
  pub urgent: bool,
}

/// One line from Hyprland's event socket, such as `workspacev2>>2,2`,
/// as Quickshell's `HyprlandEvent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyprlandEvent {
  /// What happened, such as `workspacev2` or `submap`.
  pub name: String,
  /// What Hyprland sent with it, such as `2,2`: its fields joined by
  /// commas.
  pub data: String,
}

impl HyprlandEvent {
  /// The data split into at most `fields` fields, as Quickshell's
  /// `parse(n)`: the last keeps its commas, as a window title in
  /// `windowtitlev2>>5a1,vim, the editor` does.
  pub fn parse(&self, fields: usize) -> Vec<&str> {
    self.data.splitn(fields, ',').collect()
  }
}

/// The Hyprland service, from `cx.hyprland()`. A clone is cheap and
/// shares the one connection.
#[derive(Clone)]
pub struct Hyprland {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  /// The asking context's owner, which [`on_event`](Self::on_event)'s
  /// handlers go with.
  owner: Owner,
  workspaces: ReadSignal<Vec<HyprlandWorkspace>>,
  monitors: ReadSignal<Vec<HyprlandMonitor>>,
  toplevels: ReadSignal<Vec<HyprlandToplevel>>,
}

impl Hyprland {
  /// Switches to the workspace `target`, as Hyprland names it: a number
  /// such as `2`, or `name:web`. Hyprland 0.57 and later read dispatches
  /// as Lua, and earlier ones as `workspace 2`, so the first call asks
  /// Hyprland's version, and switches wait for its answer. On 0.57 and
  /// later `target` goes as a quoted Lua string, so no name runs as code.
  /// Hyprland's refusal is logged.
  pub fn focus_workspace(&self, target: &str) {
    match &self.service {
      Some(service) => service.focus_workspace(target),
      None => tracing::warn!(target, "no Hyprland to dispatch to"),
    }
  }

  /// Sends Hyprland `dispatch command`, as `hyprctl dispatch` does, on a
  /// connection of its own, and never waits for it. Hyprland's refusal is
  /// logged, and so is a command over 8 KiB, which isn't sent.
  ///
  /// Up to 0.56 `command` is a dispatcher and its arguments, such as
  /// `workspace 2`. From 0.57 on it is a Lua expression that Hyprland runs
  /// inside the compositor, such as `hl.dsp.focus({ workspace = "2" })`,
  /// so never put outside text, such as a window's title or class or a
  /// workspace's name, into it: what it says would run. Switch workspaces
  /// with [`focus_workspace`](Self::focus_workspace), which quotes them.
  pub fn dispatch(&self, command: &str) {
    match &self.service {
      Some(service) => service.dispatch(command),
      None => tracing::warn!(command, "no Hyprland to dispatch to"),
    }
  }

  /// Calls `handler` with every event Hyprland sends, as Quickshell's
  /// `rawEvent`, until the context `cx.hyprland()` was called on goes.
  /// The lists follow once Hyprland answers the reads an event starts,
  /// after `handler` runs.
  ///
  /// It runs on the main thread. If it panics, that is logged with the
  /// event and where `on_event` was called, and it isn't called again.
  #[track_caller]
  pub fn on_event(&self, handler: impl Fn(&HyprlandEvent) + 'static) {
    if let Some(service) = &self.service {
      let id = service.add_handler(Rc::new(handler), Location::caller());
      self.owner.signal(Unregister(Rc::clone(service), id));
    }
  }
  /// Every workspace, numbered ones by number after the named ones.
  /// Empty while Hyprland isn't connected.
  pub fn workspaces(&self) -> ReadSignal<Vec<HyprlandWorkspace>> {
    self.workspaces
  }

  /// Every monitor, in Hyprland's order.
  pub fn monitors(&self) -> ReadSignal<Vec<HyprlandMonitor>> {
    self.monitors
  }

  /// Every mapped window, in Hyprland's order.
  pub fn toplevels(&self) -> ReadSignal<Vec<HyprlandToplevel>> {
    self.toplevels
  }
}

/// `cx.hyprland()`.
pub(crate) fn hyprland(cx: &Cx) -> Hyprland {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.hyprland() needs the services the shell provides; it shows nothing");
    });
    return Hyprland {
      service: None,
      owner: cx.owner(),
      workspaces: cx.signal(Vec::new()).read_only(),
      monitors: cx.signal(Vec::new()).read_only(),
      toplevels: cx.signal(Vec::new()).read_only(),
    };
  };
  let (workspaces, monitors, toplevels) = service.start();
  Hyprland {
    service: Some(service),
    owner: cx.owner(),
    workspaces: workspaces.read_only(),
    monitors: monitors.read_only(),
    toplevels: toplevels.read_only(),
  }
}

/// A list read whole with one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Query {
  Workspaces,
  Monitors,
  Clients,
  ActiveWindow,
}

impl Query {
  const ALL: [Query; 4] = [
    Query::Workspaces,
    Query::Monitors,
    Query::Clients,
    Query::ActiveWindow,
  ];

  fn command(self) -> &'static str {
    match self {
      Query::Workspaces => "j/workspaces",
      Query::Monitors => "j/monitors",
      Query::Clients => "j/clients",
      Query::ActiveWindow => "j/activewindow",
    }
  }

  /// What reads again after the event `name`. Hyprland sends an event's
  /// two versions together, and both count, for a Hyprland that lacks
  /// one.
  fn after(name: &str) -> &'static [Query] {
    use Query::{ActiveWindow, Clients, Monitors, Workspaces};
    match name.strip_suffix("v2").unwrap_or(name) {
      "workspace" | "focusedmon" | "createworkspace" | "destroyworkspace" | "activespecial" => {
        &[Workspaces, Monitors]
      }
      // Windows name their workspace and monitor too.
      "moveworkspace" | "renameworkspace" | "changeworkspaceid" | "monitoradded"
      | "monitorremoved" | "configreloaded" => &[Workspaces, Monitors, Clients],
      "openwindow" | "closewindow" | "movewindow" => &[Workspaces, Clients],
      "windowtitle" | "changefloatingmode" | "fullscreen" => &[Clients],
      "activewindow" => &[ActiveWindow],
      _ => &[],
    }
  }
}

/// Where the reading of one list stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
  Idle,
  Busy,
  /// Busy, and an event came meanwhile, which the answer may predate, so
  /// it reads again when done.
  Stale,
}

type Lists = (
  Signal<Vec<HyprlandWorkspace>>,
  Signal<Vec<HyprlandMonitor>>,
  Signal<Vec<HyprlandToplevel>>,
);

/// The one Hyprland connection of a shell, started on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  workspaces: Signal<Vec<HyprlandWorkspace>>,
  monitors: Signal<Vec<HyprlandMonitor>>,
  toplevels: Signal<Vec<HyprlandToplevel>>,
  /// The event socket, while it is open.
  events: Option<Events>,
  /// By [`Query`].
  reading: [Reading; 4],
  /// Requests waiting for their reply.
  requests: Vec<Request>,
  next_request: u64,
  /// Whether a request that couldn't connect was logged, so a missing
  /// request socket logs once.
  request_failure_logged: bool,
  raw: Raw,
  /// The windows that asked for attention since they were last focused,
  /// by address, as only events say.
  urgent: HashSet<String>,
  /// What `on_event` was given, in the order it was.
  handlers: Vec<Handler>,
  next_handler: u64,
  /// Whether Hyprland reads a dispatch as Lua, from 0.57 on, once
  /// `j/version` said.
  lua: Option<bool>,
  /// Whether `j/version` was asked and not answered yet.
  version_asked: bool,
  /// What `focus_workspace` was given before the version was known.
  focusing: Vec<String>,
}

/// What a request asks Hyprland.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
  List(Query),
  Version,
  Dispatch,
}

/// A config's raw-event handler.
#[derive(Clone)]
struct Handler {
  id: u64,
  /// Where `on_event` was called, for the log if it panics.
  at: &'static Location<'static>,
  handler: Rc<dyn Fn(&HyprlandEvent)>,
}

/// Drops a handler when the context it was given in goes.
struct Unregister(Rc<Service>, u64);

impl Drop for Unregister {
  fn drop(&mut self) {
    let handler = self.0.remove_handler(self.1);
    // Dropped outside the borrow: the handler may hold a `Hyprland`.
    drop(handler);
  }
}

/// What Hyprland last said, the lists are built from.
#[derive(Default)]
struct Raw {
  workspaces: Vec<RawWorkspace>,
  monitors: Vec<RawMonitor>,
  windows: Vec<RawWindow>,
  /// The focused window's address.
  active: Option<String>,
}

struct Events {
  stream: UnixStream,
  reader: RegistrationToken,
  lines: Lines,
}

struct Request {
  id: u64,
  asked: Asked,
  stream: UnixStream,
  reader: RegistrationToken,
  reply: Vec<u8>,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
    }
  }

  /// The lists, connecting to Hyprland if this is the first ask.
  fn start(self: &Rc<Self>) -> Lists {
    if self.state.borrow().is_none() {
      let root = &self.shared.root;
      *self.state.borrow_mut() = Some(State {
        workspaces: root.signal(Vec::new()),
        monitors: root.signal(Vec::new()),
        toplevels: root.signal(Vec::new()),
        events: None,
        reading: [Reading::Idle; 4],
        requests: Vec::new(),
        next_request: 0,
        request_failure_logged: false,
        raw: Raw::default(),
        urgent: HashSet::new(),
        handlers: Vec::new(),
        next_handler: 0,
        lua: None,
        version_asked: false,
        focusing: Vec::new(),
      });
      match &self.shared.config.hyprland {
        None => tracing::info!(
          "HYPRLAND_INSTANCE_SIGNATURE or XDG_RUNTIME_DIR is unset, so there is no Hyprland to show"
        ),
        Some(dir) => self.connect(dir),
      }
    }
    let state = self.state.borrow();
    let state = state.as_ref().expect("invariant: started above");
    (state.workspaces, state.monitors, state.toplevels)
  }

  /// Opens the event socket, then reads every list, so no change between
  /// the two goes unseen. A failure is logged and never retried, so a
  /// stale signature never wakes the shell.
  fn connect(self: &Rc<Self>, dir: &Path) {
    let Some(event_loop) = self.shared.event_loop() else {
      return;
    };
    let path = dir.join(".socket2.sock");
    let opened = UnixStream::connect(&path).and_then(|stream| {
      stream.set_nonblocking(true)?;
      let fd = stream.as_fd().try_clone_to_owned()?;
      Ok((stream, fd))
    });
    let (stream, fd) = match opened {
      Ok(opened) => opened,
      Err(error) => {
        let path = path.display();
        return tracing::warn!(%path, %error, "can't connect to Hyprland, so its lists stay empty");
      }
    };
    let service = Rc::clone(self);
    let read = Box::new(move || service.on_events());
    let Some(reader) = event_loop.insert_fd(fd, Interest::READ, read) else {
      return;
    };
    let mut state = self.state.borrow_mut();
    let state = state
      .as_mut()
      .expect("invariant: started before connecting");
    state.events = Some(Events {
      stream,
      reader,
      lines: Lines::default(),
    });
    for query in Query::ALL {
      self.read(state, query);
    }
  }

  /// Reads `query`'s list, or once more after the read under way.
  fn read(self: &Rc<Self>, state: &mut State, query: Query) {
    let reading = &mut state.reading[query as usize];
    if *reading != Reading::Idle {
      *reading = Reading::Stale;
      return;
    }
    *reading = Reading::Busy;
    if !self.request(state, query.command(), Asked::List(query)) {
      state.reading[query as usize] = Reading::Idle;
    }
  }

  fn dispatch(self: &Rc<Self>, command: &str) {
    let request = format!("dispatch {command}");
    if request.len() > MAX_REQUEST {
      return tracing::warn!("a dispatch over 8 KiB is too long to send");
    }
    let mut state = self.state.borrow_mut();
    let state = state.as_mut().expect("invariant: started by cx.hyprland()");
    let waiting = state
      .requests
      .iter()
      .filter(|request| request.asked == Asked::Dispatch);
    if waiting.count() >= MAX_DISPATCHES {
      return tracing::warn!(
        command,
        "64 dispatches wait for Hyprland, so this one isn't sent"
      );
    }
    if self.shared.config.hyprland.is_none() {
      return tracing::warn!(command, "no Hyprland to dispatch to");
    }
    self.request(state, &request, Asked::Dispatch);
  }

  fn focus_workspace(self: &Rc<Self>, target: &str) {
    if self.shared.config.hyprland.is_none() {
      return tracing::warn!(target, "no Hyprland to dispatch to");
    }
    let mut guard = self.state.borrow_mut();
    let state = guard.as_mut().expect("invariant: started by cx.hyprland()");
    if let Some(lua) = state.lua {
      drop(guard);
      return self.dispatch(&focus(lua, target));
    }
    if state.focusing.len() >= MAX_DISPATCHES {
      return tracing::warn!(
        target,
        "64 switches wait for Hyprland's version, so this one isn't sent"
      );
    }
    state.focusing.push(target.to_owned());
    if !state.version_asked {
      state.version_asked = self.request(state, "j/version", Asked::Version);
      if !state.version_asked {
        // Hyprland can't be reached, which `request` logged.
        state.focusing.clear();
      }
    }
  }

  /// Sends `command` on a connection of its own. `false`, logged once, if
  /// Hyprland can't be reached.
  fn request(self: &Rc<Self>, state: &mut State, command: &str, asked: Asked) -> bool {
    let (Some(dir), Some(event_loop)) = (&self.shared.config.hyprland, self.shared.event_loop())
    else {
      return false;
    };
    let path = dir.join(".socket.sock");
    let command = command.as_bytes();
    let opened = UnixStream::connect(&path).and_then(|mut stream| {
      stream.set_nonblocking(true)?;
      // One write: Hyprland takes what has arrived as the whole request.
      if stream.write(command)? < command.len() {
        return Err(std::io::Error::other("the request didn't fit the socket"));
      }
      let fd = stream.as_fd().try_clone_to_owned()?;
      Ok((stream, fd))
    });
    let (stream, fd) = match opened {
      Ok(opened) => opened,
      Err(error) => {
        if !std::mem::replace(&mut state.request_failure_logged, true) {
          let path = path.display();
          tracing::warn!(%path, %error, "can't send Hyprland a request");
        }
        return false;
      }
    };
    state.request_failure_logged = false;
    let id = state.next_request;
    state.next_request += 1;
    let service = Rc::clone(self);
    let read = Box::new(move || service.on_reply(id));
    let Some(reader) = event_loop.insert_fd(fd, Interest::READ, read) else {
      return false;
    };
    state.requests.push(Request {
      id,
      asked,
      stream,
      reader,
      reply: Vec::new(),
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

  /// Reads what came on the event socket, and reads again what the
  /// events changed.
  fn on_events(self: &Rc<Self>) {
    if !self.alive() {
      return self.close();
    }
    let mut chunk = [0; 64 * 1024];
    let mut guard = self.state.borrow_mut();
    let state = guard
      .as_mut()
      .expect("invariant: started before connecting");
    let Some(events) = state.events.as_mut() else {
      return;
    };
    let lines = match events.stream.read(&mut chunk) {
      Ok(0) => return self.lost(state, "Hyprland closed its event socket"),
      Ok(read) => events
        .lines
        .push(&chunk[..read], &"Hyprland's event socket"),
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      Err(error) => return self.lost(state, &error.to_string()),
    };
    // A burst of events reads each list once.
    let mut stale = [false; 4];
    let mut events = Vec::new();
    let mut urgency = false;
    for line in lines {
      let Some((name, data)) = line.split_once(">>") else {
        let line = cut(line);
        tracing::warn!(line, "a line from Hyprland's event socket without `>>`");
        continue;
      };
      for &query in Query::after(name) {
        stale[query as usize] = true;
      }
      // Events name a window without JSON's `0x`.
      let address = || format!("0x{data}");
      urgency |= match name {
        "urgent" => state.urgent.len() < MAX_LIST && state.urgent.insert(address()),
        // Focus ends a window's urgency, as in Hyprland.
        "activewindowv2" | "closewindow" => state.urgent.remove(&address()),
        _ => false,
      };
      events.push(HyprlandEvent {
        name: cut(name.to_owned()),
        data: cut(data.to_owned()),
      });
    }
    if urgency {
      state.show();
    }
    for query in Query::ALL
      .into_iter()
      .filter(|&query| stale[query as usize])
    {
      self.read(state, query);
    }
    drop(guard);
    for event in events {
      self.deliver(&event);
    }
  }

  /// Calls each raw-event handler with `event`, with nothing borrowed.
  fn deliver(&self, event: &HyprlandEvent) {
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
        let name = &event.name;
        tracing::error!("Hyprland on_event (called at {at}) panicked on {name}: {message}");
        drop(self.remove_handler(id));
      }
    }
  }

  fn add_handler(
    &self,
    handler: Rc<dyn Fn(&HyprlandEvent)>,
    at: &'static Location<'static>,
  ) -> u64 {
    let mut state = self.state.borrow_mut();
    let state = state.as_mut().expect("invariant: started by cx.hyprland()");
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

  /// Reads what came of a reply, and acts on it once Hyprland closed the
  /// connection.
  fn on_reply(self: &Rc<Self>, id: u64) {
    if !self.alive() {
      return self.close();
    }
    let mut chunk = [0; 64 * 1024];
    let mut guard = self.state.borrow_mut();
    let state = guard.as_mut().expect("invariant: started before reading");
    let Some(at) = state.requests.iter().position(|request| request.id == id) else {
      return;
    };
    let request = &mut state.requests[at];
    let failed = match request.stream.read(&mut chunk) {
      Ok(0) => None,
      Ok(read) if request.reply.len() + read > MAX_REPLY => Some(String::from("over 8 MiB")),
      Ok(read) => return request.reply.extend_from_slice(&chunk[..read]),
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      Err(error) => Some(error.to_string()),
    };
    let request = state.requests.swap_remove(at);
    if let Some(event_loop) = self.shared.event_loop() {
      event_loop.remove(request.reader);
    }
    let query = match request.asked {
      Asked::List(query) => query,
      Asked::Dispatch => {
        let reply = String::from_utf8_lossy(&request.reply);
        if failed.is_some() || reply.trim() != "ok" {
          let reply = cut(reply.into_owned());
          tracing::warn!(reply, failed, "Hyprland refused a dispatch");
        }
        return;
      }
      Asked::Version => {
        state.version_asked = false;
        let lua = failed
          .is_none()
          .then(|| reads_lua(&request.reply))
          .flatten();
        if lua.is_none() {
          let reply = cut(String::from_utf8_lossy(&request.reply).into_owned());
          tracing::warn!(
            reply,
            failed,
            "Hyprland's version says nothing; dispatching as before 0.57"
          );
        }
        let lua = lua.unwrap_or(false);
        state.lua = Some(lua);
        let focusing = std::mem::take(&mut state.focusing);
        drop(guard);
        for target in focusing {
          self.dispatch(&focus(lua, &target));
        }
        return;
      }
    };
    match failed {
      Some(why) => {
        tracing::warn!(
          command = query.command(),
          why,
          "Hyprland's reply was dropped"
        );
      }
      None => state.raw.apply(query, &request.reply),
    }
    let reading = std::mem::replace(&mut state.reading[query as usize], Reading::Idle);
    if reading == Reading::Stale {
      self.read(state, query);
    }
    state.show();
  }

  /// Drops every connection and empties the lists. Hyprland doesn't
  /// come back under the same signature, so nothing reconnects.
  fn lost(&self, state: &mut State, why: &str) {
    tracing::warn!(why, "lost Hyprland; its lists are empty");
    self.drop_connections(state);
    state.raw = Raw::default();
    state.urgent.clear();
    state.show();
  }

  /// Closes every connection for good, once the lists are gone.
  fn close(&self) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      self.drop_connections(state);
    }
  }

  fn drop_connections(&self, state: &mut State) {
    let events = state.events.take();
    let requests = std::mem::take(&mut state.requests);
    state.reading = [Reading::Idle; 4];
    let Some(event_loop) = self.shared.event_loop() else {
      return;
    };
    let readers = events.map(|events| events.reader).into_iter();
    for reader in readers.chain(requests.into_iter().map(|request| request.reader)) {
      event_loop.remove(reader);
    }
  }
}

impl State {
  /// Builds the lists from what Hyprland last said, and sets each that
  /// changed.
  fn show(&self) {
    let raw = &self.raw;
    let focused = raw.monitors.iter().find(|monitor| monitor.focused);
    let urgent: Vec<_> = raw
      .windows
      .iter()
      .filter(|window| self.urgent.contains(&window.address))
      .map(|window| &window.workspace)
      .collect();
    let mut workspaces: Vec<_> = raw
      .workspaces
      .iter()
      .map(|workspace| HyprlandWorkspace {
        id: workspace.at.id,
        name: workspace.at.name.clone(),
        monitor: workspace.monitor.clone(),
        windows: workspace.windows,
        focused: focused.is_some_and(|monitor| monitor.active.is(&workspace.at)),
        active: raw
          .monitors
          .iter()
          .any(|monitor| monitor.name == workspace.monitor && monitor.active.is(&workspace.at)),
        urgent: urgent.iter().any(|at| at.is(&workspace.at)),
      })
      .collect();
    // Named ones first, as Hyprland before 0.57 numbers them below zero.
    workspaces.sort_by_key(|workspace| workspace.id.unwrap_or(i64::MIN));
    let monitors = raw.monitors.iter().map(|monitor| HyprlandMonitor {
      id: monitor.id,
      name: monitor.name.clone(),
      description: monitor.description.clone(),
      focused: monitor.focused,
      active_workspace: monitor.active.name.clone(),
      x: monitor.x,
      y: monitor.y,
      width: monitor.width,
      height: monitor.height,
      scale: monitor.scale,
    });
    let toplevels = raw.windows.iter().map(|window| HyprlandToplevel {
      address: window.address.clone(),
      class: window.class.clone(),
      title: window.title.clone(),
      workspace: window.workspace.name.clone(),
      monitor: raw
        .monitors
        .iter()
        .find(|monitor| monitor.id == window.monitor)
        .map_or_else(String::new, |monitor| monitor.name.clone()),
      floating: window.floating,
      fullscreen: window.fullscreen.is_fullscreen(),
      focused: raw.active.as_ref() == Some(&window.address),
      urgent: self.urgent.contains(&window.address),
    });
    set_changed(self.workspaces, workspaces);
    set_changed(self.monitors, monitors.collect());
    set_changed(self.toplevels, toplevels.collect());
  }
}

impl Raw {
  /// Keeps what `reply` says for `query`. A reply that doesn't parse is
  /// logged and the last one kept; an entry that doesn't is left out.
  fn apply(&mut self, query: Query, reply: &[u8]) {
    match query {
      Query::Workspaces => {
        if let Some(list) = list("workspace", reply) {
          self.workspaces = list;
        }
      }
      Query::Monitors => {
        if let Some(list) = list("monitor", reply) {
          self.monitors = list;
        }
      }
      Query::Clients => {
        if let Some(list) = list("window", reply) {
          self.windows = list;
        }
      }
      Query::ActiveWindow => match serde_json::from_slice::<ActiveWindow>(reply) {
        Ok(active) => self.active = active.address,
        Err(error) => tracing::warn!(%error, "a bad focused window from Hyprland"),
      },
    }
  }
}

/// The entries of a JSON list of `what`s, at most [`MAX_LIST`], leaving
/// out those that don't parse. `None`, logged, for a reply that isn't a
/// list.
fn list<T: DeserializeOwned>(what: &str, reply: &[u8]) -> Option<Vec<T>> {
  let entries: Vec<serde_json::Value> = serde_json::from_slice(reply)
    .inspect_err(|error| tracing::warn!(%error, "a bad {what} list from Hyprland"))
    .ok()?;
  if entries.len() > MAX_LIST {
    let listed = entries.len();
    tracing::warn!(
      listed,
      "Hyprland listed more {what}s than the {MAX_LIST} kept"
    );
  }
  let mut bad = 0;
  let list = entries
    .into_iter()
    .take(MAX_LIST)
    .filter_map(|entry| serde_json::from_value(entry).inspect_err(|_| bad += 1).ok())
    .collect();
  if bad > 0 {
    tracing::warn!(
      bad,
      "Hyprland described some {what}s badly; they are left out"
    );
  }
  Some(list)
}

/// A workspace as a monitor or a window names it: by number while it has
/// one, and from Hyprland 0.57 on by address otherwise.
#[derive(Deserialize)]
struct WorkspaceRef {
  id: Option<i64>,
  #[serde(default, deserialize_with = "some_text")]
  address: Option<String>,
  #[serde(deserialize_with = "text")]
  name: String,
}

impl WorkspaceRef {
  fn is(&self, other: &WorkspaceRef) -> bool {
    match (self.id, other.id) {
      (Some(id), Some(other)) => id == other,
      _ => self.address.is_some() && self.address == other.address,
    }
  }
}

#[derive(Deserialize)]
struct RawWorkspace {
  #[serde(flatten)]
  at: WorkspaceRef,
  #[serde(deserialize_with = "text")]
  monitor: String,
  windows: u32,
}

#[derive(Deserialize)]
struct RawMonitor {
  id: i64,
  #[serde(deserialize_with = "text")]
  name: String,
  #[serde(deserialize_with = "text")]
  description: String,
  focused: bool,
  x: i32,
  y: i32,
  width: i32,
  height: i32,
  scale: f64,
  #[serde(rename = "activeWorkspace")]
  active: WorkspaceRef,
}

#[derive(Deserialize)]
struct RawWindow {
  #[serde(deserialize_with = "text")]
  address: String,
  #[serde(deserialize_with = "text")]
  class: String,
  #[serde(deserialize_with = "text")]
  title: String,
  workspace: WorkspaceRef,
  monitor: i64,
  floating: bool,
  fullscreen: Fullscreen,
}

/// A window's fullscreen state: a flag in older Hyprland, then its mode,
/// where 1 is maximized and 2 fullscreen.
#[derive(Deserialize)]
#[serde(untagged)]
enum Fullscreen {
  Flag(bool),
  Mode(i64),
}

impl Fullscreen {
  fn is_fullscreen(&self) -> bool {
    match *self {
      Fullscreen::Flag(flag) => flag,
      Fullscreen::Mode(mode) => mode & 2 != 0,
    }
  }
}

/// `j/activewindow`'s reply: the focused window, or `{}` for none.
#[derive(Deserialize)]
struct ActiveWindow {
  #[serde(default, deserialize_with = "some_text")]
  address: Option<String>,
}

/// A string from Hyprland, cut to [`MAX_TEXT`].
fn text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
  String::deserialize(deserializer).map(cut)
}

fn some_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
  text(deserializer).map(Some)
}

/// `text` cut to [`MAX_TEXT`] bytes at a character.
fn cut(mut text: String) -> String {
  if text.len() > MAX_TEXT {
    text.truncate(text.floor_char_boundary(MAX_TEXT));
  }
  text
}

/// The dispatch that switches to `target`, in the syntax Hyprland reads.
fn focus(lua: bool, target: &str) -> String {
  if lua {
    format!("hl.dsp.focus({{ workspace = {} }})", lua_string(target))
  } else {
    format!("workspace {target}")
  }
}

/// `text` as a Lua string literal: quoted, with backslashes, quotes and
/// control characters escaped, so nothing in it runs.
fn lua_string(text: &str) -> String {
  let mut quoted = String::from("\"");
  for character in text.chars() {
    match character {
      '\\' | '"' => {
        quoted.push('\\');
        quoted.push(character);
      }
      // Three digits, so a digit after it isn't read as part of it.
      control if control.is_ascii_control() => {
        quoted.push_str(&format!("\\{:03}", u32::from(control)));
      }
      other => quoted.push(other),
    }
  }
  quoted.push('"');
  quoted
}

/// What `j/version` says.
#[derive(Deserialize)]
struct Version {
  /// Such as `0.57.2`, from 0.56 on.
  version: Option<String>,
  /// Such as `v0.41.2`.
  tag: Option<String>,
}

/// Whether the Hyprland that sent `reply` to `j/version` reads dispatches
/// as Lua, as 0.57 and later do. `None` when the reply says no version.
fn reads_lua(reply: &[u8]) -> Option<bool> {
  let Version { version, tag } = serde_json::from_slice(reply).ok()?;
  let text = version.filter(|version| !version.is_empty()).or(tag)?;
  let mut parts = text.trim_start_matches('v').split('.').map(|part| {
    let digits = part
      .find(|c: char| !c.is_ascii_digit())
      .unwrap_or(part.len());
    part[..digits].parse::<u32>().ok()
  });
  let (major, minor) = (parts.next()??, parts.next()??);
  Some((major, minor) >= (0, 57))
}
