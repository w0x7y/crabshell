//! Sway's workspaces and outputs, over Sway's IPC socket.

mod ipc;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Display;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Once;
use std::time::{Duration, Instant};

use calloop::timer::TimeoutAction;
use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{Owner, ReadSignal, Signal, untrack};
use crabshell_ui::Cx;
use serde::Deserialize;

use crate::Shared;

/// How long the service waits before each reconnect.
pub(crate) const RETRY: Duration = Duration::from_secs(2);

/// One of Sway's workspaces, as `GET_WORKSPACES` describes it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Workspace {
  /// Sway's id for it, which stays the same while it exists: the key to
  /// give `children_keyed`.
  pub id: i64,
  /// Its name, such as `1` or `3:web`.
  pub name: String,
  /// The output it is on, such as `DP-1`.
  pub output: String,
  /// Whether it has the keyboard focus.
  pub focused: bool,
  /// Whether it is shown on its output.
  pub visible: bool,
  /// Whether a window on it asks for attention.
  pub urgent: bool,
}

/// One of Sway's outputs, a monitor, as `GET_OUTPUTS` describes it and
/// as Quickshell's `I3Monitor` does.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(from = "RawOutput")]
pub struct SwayOutput {
  /// Its name, such as `DP-1`.
  pub name: String,
  /// Whether it shows the focused workspace.
  pub focused: bool,
  /// Whether it is enabled. One that isn't has no workspace and no size.
  pub active: bool,
  /// The name of the workspace it shows. `None` while it isn't active.
  pub current_workspace: Option<String>,
  /// Where its top-left corner is in the layout, in logical pixels.
  pub x: i32,
  /// See [`x`](Self::x).
  pub y: i32,
  /// Its size in logical pixels: its mode's size divided by its scale.
  pub width: u32,
  /// See [`width`](Self::width).
  pub height: u32,
  /// Its scale, such as 1.5. 1 for an output that isn't active, which
  /// Sway gives -1 and SwayFX none.
  pub scale: f64,
}

/// An output as Sway sends it.
#[derive(Deserialize)]
struct RawOutput {
  name: String,
  #[serde(default)]
  focused: bool,
  active: bool,
  current_workspace: Option<String>,
  rect: RawRect,
  scale: Option<f64>,
}

#[derive(Deserialize)]
struct RawRect {
  x: i32,
  y: i32,
  width: u32,
  height: u32,
}

impl From<RawOutput> for SwayOutput {
  fn from(raw: RawOutput) -> SwayOutput {
    SwayOutput {
      name: raw.name,
      focused: raw.focused,
      active: raw.active,
      current_workspace: raw.current_workspace,
      x: raw.rect.x,
      y: raw.rect.y,
      width: raw.rect.width,
      height: raw.rect.height,
      scale: raw.scale.filter(|scale| *scale > 0.0).unwrap_or(1.0),
    }
  }
}

/// Where Sway listens.
pub(crate) struct Socket {
  pub(crate) path: PathBuf,
  /// How long to wait before each reconnect: [`RETRY`], less in tests.
  pub(crate) retry: Duration,
}

/// The Sway service, from `cx.sway()`. A clone is cheap and shares the
/// one connection.
#[derive(Clone)]
pub struct Sway {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  workspaces: ReadSignal<Vec<Workspace>>,
  /// The asking context's owner, for what a config makes through it.
  owner: Owner,
}

impl Sway {
  /// The service if a config has started it with `cx.sway()`, without
  /// starting it, so the shell can read [`changed_at`](Self::changed_at)
  /// and a bar that shows no workspaces never connects.
  #[doc(hidden)]
  pub fn started(cx: &Cx) -> Option<Sway> {
    let service = cx.service::<Service>()?;
    let workspaces = service.state.borrow().as_ref()?.workspaces.read_only();
    Some(Sway {
      service: Some(service),
      workspaces,
      owner: cx.owner(),
    })
  }

  /// Every workspace on every output, in Sway's order. Empty while Sway
  /// isn't connected.
  pub fn workspaces(&self) -> ReadSignal<Vec<Workspace>> {
    self.workspaces
  }

  /// Every output, enabled or not, in Sway's order, kept current from
  /// Sway's output and workspace events. Empty while Sway isn't
  /// connected. Sway is asked for them on the first call, so a bar that
  /// shows only workspaces never wakes for an output's change.
  pub fn outputs(&self) -> ReadSignal<Vec<SwayOutput>> {
    match &self.service {
      Some(service) => service.outputs(),
      None => self.owner.signal(Vec::new()).read_only(),
    }
  }

  /// When the workspace event that led to the current list arrived, so
  /// the shell can log how long a switch took to reach the screen.
  /// `None` for a list that followed no event, such as the first.
  pub fn changed_at(&self) -> Option<Instant> {
    let state = self.service.as_ref()?.state.borrow();
    state.as_ref()?.changed_at
  }

  /// Switches to the workspace called `name`, as clicking it in swaybar
  /// does. A name Sway reads as a keyword, such as `next`, is logged and
  /// not sent, and so is a call while Sway isn't connected. Sway's
  /// refusal is logged too.
  pub fn focus_workspace(&self, name: &str) {
    let Some(command) = ipc::workspace_command(name) else {
      tracing::warn!(name, "Sway can't switch to this workspace by name");
      return;
    };
    self.dispatch(&command);
  }

  /// Runs `command` as Sway commands, as `swaymsg` and Quickshell's
  /// `I3.dispatch` do, such as `layout tabbed` or `[app_id=foot] kill`.
  /// It is sent as written, so quote a name that has spaces. A call while
  /// Sway isn't connected is logged and dropped, and so is a command over
  /// 4 MiB. Sway's refusal is logged too.
  pub fn dispatch(&self, command: &str) {
    if command.len() > ipc::MAX_PAYLOAD {
      let bytes = command.len();
      return tracing::warn!(bytes, "a Sway command over 4 MiB isn't sent");
    }
    let service = self.service.as_ref();
    if !service.is_some_and(|service| service.send(ipc::RUN_COMMAND, command.as_bytes())) {
      tracing::warn!(
        command,
        "can't run a Sway command while Sway isn't connected"
      );
    }
  }
}

/// `cx.sway()`.
pub(crate) fn sway(cx: &Cx) -> Sway {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.sway() needs the services the shell provides; it shows no workspaces");
    });
    return Sway {
      service: None,
      workspaces: cx.signal(Vec::new()).read_only(),
      owner: cx.owner(),
    };
  };
  let workspaces = service.start();
  Sway {
    service: Some(service),
    workspaces,
    owner: cx.owner(),
  }
}

/// The one Sway connection of a shell, started on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  workspaces: Signal<Vec<Workspace>>,
  /// `None` until a config asks for the outputs, so a bar that shows only
  /// workspaces never subscribes to output events.
  outputs: Option<Signal<Vec<SwayOutput>>>,
  changed_at: Option<Instant>,
  connection: Option<Connection>,
  /// Whether the current outage was logged, so retries stay quiet.
  outage_logged: bool,
}

struct Connection {
  stream: UnixStream,
  /// The loop source that waits for something to read.
  reader: RegistrationToken,
  /// The loop source that waits for room to write, while the socket
  /// holds back part of `outbox`.
  writer: Option<RegistrationToken>,
  /// Bytes read that don't make a whole message yet.
  inbox: Vec<u8>,
  /// Bytes not written yet.
  outbox: Vec<u8>,
  /// One entry per `GET_WORKSPACES` sent and not yet answered, in order:
  /// when the event behind it arrived, or `None` for the first.
  fetches: VecDeque<Option<Instant>>,
  /// The events this connection subscribed to. Sway has no way to
  /// unsubscribe.
  subscribed: Vec<&'static str>,
}

/// Sway's answer to a `SUBSCRIBE` or to one command.
#[derive(Deserialize)]
struct Outcome {
  success: bool,
  /// Why not, when `success` is false.
  #[serde(default)]
  error: String,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
    }
  }

  /// The list, connecting to Sway if this is the first ask.
  fn start(self: &Rc<Self>) -> ReadSignal<Vec<Workspace>> {
    if let Some(state) = &*self.state.borrow() {
      return state.workspaces.read_only();
    }
    let workspaces = self.shared.root.signal(Vec::new());
    *self.state.borrow_mut() = Some(State {
      workspaces,
      outputs: None,
      changed_at: None,
      connection: None,
      outage_logged: false,
    });
    if self.shared.config.sway.is_none() {
      tracing::info!("SWAYSOCK is unset, so the bar shows no workspaces");
    } else {
      // A failed first connect isn't retried: SWAYSOCK is stale, or
      // another compositor runs, and retries would wake the shell.
      self.connect();
    }
    workspaces.read_only()
  }

  /// The outputs, asking Sway for them if this is the first ask.
  fn outputs(self: &Rc<Self>) -> ReadSignal<Vec<SwayOutput>> {
    let mut state = self.state.borrow_mut();
    let state = state.as_mut().expect("invariant: started by cx.sway()");
    if let Some(outputs) = state.outputs {
      return outputs.read_only();
    }
    let outputs = self.shared.root.signal(Vec::new());
    state.outputs = Some(outputs);
    let wanted = state.wanted();
    if let Some(connection) = state.connection.as_mut() {
      connection.subscribe(&wanted);
      ipc::encode(ipc::GET_OUTPUTS, b"", &mut connection.outbox);
      self.flush_with(state);
    }
    outputs.read_only()
  }

  /// Connects and asks for the list and its changes. `false` if Sway
  /// isn't there, logged unless a drop already was.
  fn connect(self: &Rc<Self>) -> bool {
    let (Some(socket), Some(event_loop)) = (&self.shared.config.sway, self.shared.event_loop())
    else {
      return false;
    };
    let opened = UnixStream::connect(&socket.path).and_then(|stream| {
      stream.set_nonblocking(true)?;
      let fd = stream.as_fd().try_clone_to_owned()?;
      Ok((stream, fd))
    });
    let mut state = self.state.borrow_mut();
    let state = state
      .as_mut()
      .expect("invariant: started before connecting");
    let (stream, fd) = match opened {
      Ok(opened) => opened,
      Err(error) => {
        if !state.outage_logged {
          let path = socket.path.display();
          tracing::warn!(
              %path,
              %error,
              "can't connect to Sway, so the bar shows no workspaces"
          );
        }
        return false;
      }
    };
    let service = Rc::clone(self);
    let read = Box::new(move || service.on_readable());
    let Some(reader) = event_loop.insert_fd(fd, Interest::READ, read) else {
      return false;
    };
    if std::mem::take(&mut state.outage_logged) {
      tracing::info!("connected to Sway again");
    }
    let mut connection = Connection {
      stream,
      reader,
      writer: None,
      inbox: Vec::new(),
      outbox: Vec::new(),
      fetches: VecDeque::from([None]),
      subscribed: Vec::new(),
    };
    // Subscribe first, so no change between the two goes unseen.
    connection.subscribe(&state.wanted());
    ipc::encode(ipc::GET_WORKSPACES, b"", &mut connection.outbox);
    if state.outputs.is_some() {
      ipc::encode(ipc::GET_OUTPUTS, b"", &mut connection.outbox);
    }
    state.connection = Some(connection);
    self.flush_with(&mut *state);
    true
  }

  /// Tries to connect every `retry` until it works or the list is gone.
  fn retry_later(self: &Rc<Self>) {
    let (Some(socket), Some(event_loop)) = (&self.shared.config.sway, self.shared.event_loop())
    else {
      return;
    };
    let retry = socket.retry;
    let service = Rc::clone(self);
    event_loop.insert_timer(
      Instant::now() + retry,
      Box::new(move || {
        if !service.alive() || service.connect() {
          TimeoutAction::Drop
        } else {
          TimeoutAction::ToDuration(retry)
        }
      }),
    );
  }

  /// Whether the list is still there: its owner, the root, isn't disposed.
  fn alive(&self) -> bool {
    let state = self.state.borrow();
    let alive = state
      .as_ref()
      .and_then(|state| state.workspaces.try_with(|_| ()));
    alive.is_some()
  }

  /// Reads what Sway sent and acts on each whole message.
  fn on_readable(self: &Rc<Self>) {
    if !self.alive() {
      return self.close();
    }
    let mut chunk = [0; 64 * 1024];
    let mut state = self.state.borrow_mut();
    let state = state
      .as_mut()
      .expect("invariant: started before connecting");
    let Some(connection) = state.connection.as_mut() else {
      return;
    };
    let broken = match connection.stream.read(&mut chunk) {
      Ok(0) => Some("Sway closed the socket".to_owned()),
      Ok(read) => {
        connection.inbox.extend_from_slice(&chunk[..read]);
        state.take_messages().err()
      }
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => None,
      Err(error) => Some(error.to_string()),
    };
    match broken {
      None => self.flush_with(state),
      Some(why) => self.disconnect(state, &why),
    }
  }

  /// Writes what the socket takes, and waits for room for the rest.
  fn flush_with(self: &Rc<Self>, state: &mut State) {
    let (Some(connection), Some(event_loop)) =
      (state.connection.as_mut(), self.shared.event_loop())
    else {
      return;
    };
    while !connection.outbox.is_empty() {
      match connection.stream.write(&connection.outbox) {
        Ok(written) => {
          connection.outbox.drain(..written);
        }
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
        Err(error) => return self.disconnect(state, &error),
      }
    }
    if connection.outbox.len() > ipc::MAX_PAYLOAD {
      // Each event queues a fetch, so a Sway that never reads would
      // grow the outbox and `fetches` without end.
      return self.disconnect(state, &"Sway reads nothing the bar sends");
    }
    if connection.outbox.is_empty() {
      if let Some(writer) = connection.writer.take() {
        event_loop.remove(writer);
      }
    } else if connection.writer.is_none() {
      let fd = match connection.stream.as_fd().try_clone_to_owned() {
        Ok(fd) => fd,
        Err(error) => return self.disconnect(state, &error),
      };
      let service = Rc::clone(self);
      let write = Box::new(move || service.flush());
      connection.writer = event_loop.insert_fd(fd, Interest::WRITE, write);
    }
  }

  /// Sends one message. `false` while Sway isn't connected.
  fn send(self: &Rc<Self>, kind: u32, payload: &[u8]) -> bool {
    let mut state = self.state.borrow_mut();
    let Some(state) = state.as_mut() else {
      return false;
    };
    let Some(connection) = state.connection.as_mut() else {
      return false;
    };
    ipc::encode(kind, payload, &mut connection.outbox);
    self.flush_with(state);
    true
  }

  fn flush(self: &Rc<Self>) {
    let mut state = self.state.borrow_mut();
    if let Some(state) = state.as_mut() {
      self.flush_with(state);
    }
  }

  /// Drops the connection, empties the list and reconnects later.
  fn disconnect(self: &Rc<Self>, state: &mut State, why: &dyn Display) {
    tracing::warn!(%why, "lost the connection to Sway; reconnecting");
    state.outage_logged = true;
    self.drop_connection(state);
    state.changed_at = None;
    replace(state.workspaces, Vec::new());
    if let Some(outputs) = state.outputs {
      replace(outputs, Vec::new());
    }
    self.retry_later();
  }

  /// Closes the connection for good, once the list is gone.
  fn close(&self) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      self.drop_connection(state);
    }
  }

  fn drop_connection(&self, state: &mut State) {
    let connection = state.connection.take();
    if let (Some(connection), Some(event_loop)) = (connection, self.shared.event_loop()) {
      event_loop.remove(connection.reader);
      if let Some(writer) = connection.writer {
        event_loop.remove(writer);
      }
    }
  }
}

/// Sets `signal` to `list` unless it holds that already, so nothing reads
/// an unchanged list again. Whether it changed.
fn replace<T: PartialEq + 'static>(signal: Signal<Vec<T>>, list: Vec<T>) -> bool {
  let changed = untrack(|| signal.try_with(|old| *old != list)) == Some(true);
  if changed {
    signal.set(list);
  }
  changed
}

impl Connection {
  /// Subscribes to the events in `wanted` it hasn't yet.
  fn subscribe(&mut self, wanted: &[&'static str]) {
    let missing: Vec<_> = wanted
      .iter()
      .filter(|name| !self.subscribed.contains(name))
      .copied()
      .collect();
    if !missing.is_empty() {
      let payload = serde_json::to_vec(&missing).expect("invariant: a list of names serializes");
      ipc::encode(ipc::SUBSCRIBE, &payload, &mut self.outbox);
      self.subscribed.extend(missing);
    }
  }
}

impl State {
  /// The events the connection needs: workspace ones for the list, and
  /// output ones once a config reads the outputs.
  fn wanted(&self) -> Vec<&'static str> {
    let mut wanted = vec!["workspace"];
    if self.outputs.is_some() {
      wanted.push("output");
    }
    wanted
  }

  /// Acts on each whole message in the inbox. An error means the stream
  /// can't be read any further.
  fn take_messages(&mut self) -> Result<(), String> {
    let Some(connection) = self.connection.as_mut() else {
      return Ok(());
    };
    let now = Instant::now();
    while let Some((kind, payload)) = ipc::decode(&mut connection.inbox)? {
      match kind {
        ipc::GET_WORKSPACES => {
          let event = connection.fetches.pop_front().flatten();
          match serde_json::from_slice::<Vec<Workspace>>(&payload) {
            Ok(list) => {
              if replace(self.workspaces, list) {
                self.changed_at = event;
              }
            }
            Err(error) => tracing::warn!(%error, "a bad workspace list from Sway"),
          }
        }
        ipc::GET_OUTPUTS => match serde_json::from_slice::<Vec<SwayOutput>>(&payload) {
          Ok(list) => {
            if let Some(outputs) = self.outputs {
              replace(outputs, list);
            }
          }
          Err(error) => tracing::warn!(%error, "a bad output list from Sway"),
        },
        ipc::WORKSPACE_EVENT | ipc::OUTPUT_EVENT => {
          if kind == ipc::WORKSPACE_EVENT {
            connection.fetches.push_back(Some(now));
            ipc::encode(ipc::GET_WORKSPACES, b"", &mut connection.outbox);
          }
          // A workspace event can move an output's workspace or focus.
          if self.outputs.is_some() {
            ipc::encode(ipc::GET_OUTPUTS, b"", &mut connection.outbox);
          }
        }
        ipc::SUBSCRIBE => match serde_json::from_slice::<Outcome>(&payload) {
          Ok(Outcome { success: true, .. }) => {}
          Ok(Outcome { error, .. }) => {
            tracing::warn!(
                %error,
                "Sway refused the subscription; the list won't update"
            );
          }
          Err(error) => tracing::warn!(%error, "a bad subscribe reply from Sway"),
        },
        ipc::RUN_COMMAND => match serde_json::from_slice::<Vec<Outcome>>(&payload) {
          Ok(outcomes) => {
            for Outcome { error, .. } in outcomes.into_iter().filter(|outcome| !outcome.success) {
              tracing::warn!(%error, "Sway refused a command");
            }
          }
          Err(error) => tracing::warn!(%error, "a bad command reply from Sway"),
        },
        kind => tracing::debug!(kind, "an unexpected message from Sway"),
      }
    }
    Ok(())
  }
}
