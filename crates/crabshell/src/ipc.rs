//! `cx.ipc`: functions a config offers the command line, which
//! `crabshell ipc <target> <function> [args...]` calls through a socket in
//! `$XDG_RUNTIME_DIR/crabshell`. One request per connection and one line
//! of JSON each way, all on the loop's thread with nonblocking sockets.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{self, AssertUnwindSafe, Location};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use crabshell_ui::Cx;
use serde::Deserialize;

use crate::persist;
use crate::shell::{OnDispose, panic_message};

/// A handler: the call's arguments in, the reply's text out.
pub(crate) type Handler = dyn Fn(&[String]) -> Result<String, String>;

/// How long a connection stays open, to send its request and read the
/// reply.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);

/// How long the listener rests after an error that leaves a connection
/// waiting, such as running out of file descriptors.
const BACK_OFF: Duration = Duration::from_secs(1);
const MAX_NAME: usize = 64;
const MAX_REQUEST: usize = 64 * 1024;
const MAX_REPLY: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 16;

/// `<runtime_dir>/crabshell/ipc-<display>.sock`, where `<display>` is the
/// last part of `WAYLAND_DISPLAY`, or `wayland-0`. `None` without an
/// absolute runtime dir. `crabshell ipc` finds it the same way.
pub(crate) fn socket_path(runtime_dir: Option<&OsStr>, display: Option<&OsStr>) -> Option<PathBuf> {
  let dir = Path::new(runtime_dir?);
  if !dir.is_absolute() {
    return None;
  }
  let display = display
    .and_then(|display| Path::new(display).file_name())
    .unwrap_or(OsStr::new("wayland-0"));
  let mut name = OsString::from("ipc-");
  name.push(display);
  name.push(".sock");
  Some(dir.join("crabshell").join(name))
}

/// 1 to 64 bytes of ASCII letters, digits, `_`, `-` and `.`.
pub(crate) fn valid_name(name: &str) -> bool {
  (1..=MAX_NAME).contains(&name.len())
    && name
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// What a client asks.
#[derive(Debug, PartialEq)]
pub(crate) enum Request {
  List,
  Call {
    target: String,
    function: String,
    args: Vec<String>,
  },
}

/// Reads a request line: `{"target":…,"function":…,"args":[…]}` or
/// `{"list":true}`.
pub(crate) fn parse(line: &[u8]) -> Result<Request, String> {
  #[derive(Deserialize)]
  struct Wire {
    target: Option<String>,
    function: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    list: bool,
  }
  let wire: Wire =
    serde_json::from_slice(line).map_err(|error| format!("the request doesn't parse: {error}"))?;
  let args = wire.args;
  match (wire.list, wire.target, wire.function) {
    (true, None, None) => Ok(Request::List),
    (false, Some(target), Some(function)) => Ok(Request::Call {
      target,
      function,
      args,
    }),
    _ => Err(String::from(
      "a request names a target and a function, or asks for the list",
    )),
  }
}

/// The reply line: `{"ok":…}` or `{"error":…}`.
pub(crate) fn reply(answer: Result<String, String>) -> Vec<u8> {
  let value = match answer {
    Ok(text) => serde_json::json!({ "ok": text }),
    Err(text) => serde_json::json!({ "error": text }),
  };
  let mut line = value.to_string().into_bytes();
  line.push(b'\n');
  line
}

/// The shell's handlers, and the socket that serves them.
pub(crate) struct Ipc<D> {
  /// Where to listen; `None` without `XDG_RUNTIME_DIR`.
  path: Option<PathBuf>,
  /// The loop, if it is still there. Held weakly, so the sources that
  /// hold this don't keep their own loop alive.
  handle: Box<dyn Fn() -> Option<LoopHandle<'static, D>>>,
  handlers: RefCell<BTreeMap<(String, String), Rc<Entry>>>,
  next_id: Cell<u64>,
  socket: Cell<Socket>,
  pub(crate) open: Cell<usize>,
  /// The listener's source, to turn back on after it backed off.
  listener: Cell<Option<RegistrationToken>>,
}

#[derive(Clone, Copy)]
enum Socket {
  Unbound,
  /// Its file's device and inode, so only ours is ever removed.
  Bound(u64, u64),
  Off,
}

struct Entry {
  /// Which registration, so an older one's owner going leaves a newer one.
  id: u64,
  at: &'static Location<'static>,
  f: Box<Handler>,
  disabled: Cell<bool>,
}

/// A connection's two sources: its socket and its deadline.
#[derive(Default)]
struct Tokens {
  socket: Cell<Option<RegistrationToken>>,
  deadline: Cell<Option<RegistrationToken>>,
}

impl<D: 'static> Ipc<D> {
  pub(crate) fn new(handle: &LoopHandle<'static, D>, path: Option<PathBuf>) -> Rc<Ipc<D>> {
    let weak = handle.downgrade();
    Rc::new(Ipc {
      path,
      handle: Box::new(move || weak.upgrade()),
      handlers: RefCell::default(),
      next_id: Cell::new(0),
      socket: Cell::new(Socket::Unbound),
      open: Cell::new(0),
      listener: Cell::new(None),
    })
  }

  /// Serves `f` as `target function` while `cx`'s owner lives, binding
  /// the socket if it is the first.
  pub(crate) fn register(
    self: &Rc<Self>,
    cx: &Cx,
    target: &str,
    function: &str,
    f: Box<Handler>,
    at: &'static Location<'static>,
  ) {
    for (what, name) in [("target", target), ("function", function)] {
      if !valid_name(name) {
        return tracing::error!(
          "cx.ipc at {at}: {name:?} isn't a {what} name (1 to 64 ASCII letters, digits, _, - and .); nothing is registered"
        );
      }
    }
    let id = self.next_id.get();
    self.next_id.set(id + 1);
    let key = (target.to_owned(), function.to_owned());
    let (weak, held) = (Rc::downgrade(self), key.clone());
    let gone = OnDispose(Some(Box::new(move || {
      if let Some(ipc) = weak.upgrade() {
        ipc.unregister(&held, id);
      }
    })));
    cx.owner().signal(gone);
    let entry = Rc::new(Entry {
      id,
      at,
      f,
      disabled: Cell::new(false),
    });
    let older = self.handlers.borrow_mut().insert(key, entry);
    if let Some(older) = &older {
      tracing::warn!(
        "ipc {target}.{function} (registered at {at}) replaces the one registered at {}",
        older.at
      );
    }
    // Dropped after the map is released: what it holds may use cx.ipc,
    // and its drop is the config's code.
    if let Some(older) = older {
      let at = older.at;
      if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(move || drop(older))) {
        let message = panic_message(payload.as_ref());
        tracing::error!(
          "dropping ipc {target}.{function} (registered at {at}) panicked: {message}"
        );
      }
    }
    if let Socket::Unbound = self.socket.get() {
      self.socket.set(Socket::Off);
      self.listen(at);
    }
  }

  fn unregister(&self, key: &(String, String), id: u64) {
    let mut handlers = self.handlers.borrow_mut();
    let ours = handlers.get(key).is_some_and(|entry| entry.id == id);
    let removed = ours.then(|| handlers.remove(key));
    drop(handlers);
    drop(removed);
  }

  fn listen(self: &Rc<Self>, at: &Location) {
    let Some(path) = &self.path else {
      return tracing::warn!(
        "cx.ipc at {at}: the shell has no IPC socket (crabshell::run makes one in XDG_RUNTIME_DIR), so nothing can call it"
      );
    };
    match self.bind(path) {
      Ok(socket) => self.socket.set(socket),
      Err(error) => tracing::error!(
        path = %path.display(),
        %error,
        "can't listen for IPC calls, so nothing can call cx.ipc's handlers"
      ),
    }
  }

  fn bind(self: &Rc<Self>, path: &Path) -> std::io::Result<Socket> {
    if let Some(dir) = path.parent() {
      persist::private_dir(dir)?;
    }
    // A reload runs the new shell beside the old one for a moment; with
    // two shells running at once, the newer one wins.
    match std::fs::remove_file(path) {
      Ok(()) => tracing::info!(path = %path.display(), "replaced the IPC socket that was there"),
      Err(error) if error.kind() == ErrorKind::NotFound => {}
      Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    let meta = std::fs::symlink_metadata(path)?;
    let handle = (self.handle)().ok_or_else(|| std::io::Error::other("the loop is gone"))?;
    let ipc = Rc::clone(self);
    let source = Generic::new(listener, Interest::READ, Mode::Level);
    let token = handle
      .insert_source(source, move |_, listener, _| {
        if ipc.accept(listener) {
          return Ok(PostAction::Continue);
        }
        // The connection stays queued, so the listener would be ready
        // again at once: it rests a while instead of spinning.
        ipc.back_off();
        Ok(PostAction::Disable)
      })
      .map_err(|error| std::io::Error::other(error.error.to_string()))?;
    self.listener.set(Some(token));
    Ok(Socket::Bound(meta.dev(), meta.ino()))
  }

  /// Takes every connection waiting. `false` after an error that leaves
  /// one waiting, such as running out of file descriptors.
  fn accept(self: &Rc<Self>, listener: &UnixListener) -> bool {
    loop {
      match listener.accept() {
        Ok((stream, _)) => self.open_connection(stream),
        Err(error) if error.kind() == ErrorKind::WouldBlock => return true,
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(error) => {
          tracing::warn!(%error, "can't accept an IPC connection; trying again in 1 s");
          return false;
        }
      }
    }
  }

  /// Turns the listener back on after [`BACK_OFF`].
  fn back_off(self: &Rc<Self>) {
    let Some(handle) = (self.handle)() else {
      return;
    };
    let ipc = Rc::clone(self);
    let again = handle.insert_source(Timer::from_duration(BACK_OFF), move |_, _, _| {
      if let (Some(token), Some(handle)) = (ipc.listener.get(), (ipc.handle)())
        && let Err(error) = handle.enable(&token)
      {
        tracing::error!(%error, "can't listen for IPC calls again");
      }
      TimeoutAction::Drop
    });
    if again.is_err() {
      tracing::error!("can't listen for IPC calls again");
    }
  }

  fn open_connection(self: &Rc<Self>, stream: UnixStream) {
    let Some(handle) = (self.handle)() else {
      return;
    };
    if let Err(error) = stream.set_nonblocking(true) {
      return tracing::warn!(%error, "can't serve an IPC connection");
    }
    if self.open.get() >= MAX_CONNECTIONS {
      tracing::warn!("refused an IPC connection: {MAX_CONNECTIONS} are open already");
      let refusal = format!("the shell has {MAX_CONNECTIONS} IPC connections open already");
      // What came already is dropped first, or closing would reset the
      // connection before the client reads why.
      let _ = std::io::copy(
        &mut (&stream).take(MAX_REQUEST as u64),
        &mut std::io::sink(),
      );
      let _ = (&stream).write_all(&reply(Err(refusal)));
      return;
    }
    let tokens = Rc::new(Tokens::default());
    let (ipc, held) = (Rc::clone(self), Rc::clone(&tokens));
    let mut connection = Connection::default();
    // Edge-triggered, so a writable socket doesn't wake the loop while the
    // request is on its way.
    let source = Generic::new(stream, Interest::BOTH, Mode::Edge);
    let inserted = handle.insert_source(source, move |_, stream, _| {
      if connection.serve(&ipc, stream) {
        return Ok(PostAction::Continue);
      }
      held.socket.take();
      ipc.open.set(ipc.open.get() - 1);
      if let (Some(deadline), Some(handle)) = (held.deadline.take(), (ipc.handle)()) {
        handle.remove(deadline);
      }
      Ok(PostAction::Remove)
    });
    let Ok(socket) = inserted else {
      return tracing::warn!("can't serve an IPC connection");
    };
    tokens.socket.set(Some(socket));
    self.open.set(self.open.get() + 1);
    let (ipc, held) = (Rc::clone(self), Rc::clone(&tokens));
    let deadline = handle.insert_source(Timer::from_duration(TIMEOUT), move |_, _, _| {
      held.deadline.take();
      if let (Some(socket), Some(handle)) = (held.socket.take(), (ipc.handle)()) {
        handle.remove(socket);
        ipc.open.set(ipc.open.get() - 1);
      }
      TimeoutAction::Drop
    });
    match deadline {
      Ok(deadline) => tokens.deadline.set(Some(deadline)),
      Err(_) => tracing::warn!("an IPC connection has no deadline"),
    }
  }

  /// The reply's text for a request line.
  fn answer(&self, line: &[u8]) -> Result<String, String> {
    match parse(line)? {
      Request::List => Ok(
        self
          .handlers
          .borrow()
          .keys()
          .map(|(target, function)| format!("{target} {function}"))
          .collect::<Vec<_>>()
          .join("\n"),
      ),
      Request::Call {
        target,
        function,
        args,
      } => self.call(target, function, &args),
    }
  }

  fn call(&self, target: String, function: String, args: &[String]) -> Result<String, String> {
    let name = format!("{target}.{function}");
    let entry = self.handlers.borrow().get(&(target, function)).cloned();
    let Some(entry) = entry else {
      return Err(format!("no IPC handler {name}"));
    };
    if entry.disabled.get() {
      return Err(format!("{name} panicked before and is disabled"));
    }
    let at = entry.at;
    let mut answer = match panic::catch_unwind(AssertUnwindSafe(|| (entry.f)(args))) {
      Ok(answer) => answer,
      Err(payload) => {
        entry.disabled.set(true);
        let message = panic_message(payload.as_ref());
        tracing::error!("ipc {name} (registered at {at}) panicked: {message}");
        Err(format!("{name} panicked: {message}"))
      }
    };
    // The call may have disposed its owner, leaving this the last
    // reference, so dropping it runs the handler's own drop code.
    if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(move || drop(entry))) {
      let message = panic_message(payload.as_ref());
      tracing::error!("dropping ipc {name} (registered at {at}) panicked: {message}");
    }
    let (Ok(text) | Err(text)) = &mut answer;
    if text.len() > MAX_REPLY {
      text.truncate(text.floor_char_boundary(MAX_REPLY));
      tracing::warn!("ipc {name}'s reply is over 1 MiB; it is cut");
    }
    answer
  }
}

impl<D> Ipc<D> {
  /// Removes the socket file, if it is still ours, and serves no more.
  pub(crate) fn remove_socket(&self) {
    if let (Socket::Bound(dev, ino), Some(path)) = (self.socket.replace(Socket::Off), &self.path) {
      let ours =
        std::fs::symlink_metadata(path).is_ok_and(|meta| (meta.dev(), meta.ino()) == (dev, ino));
      if ours {
        let _ = std::fs::remove_file(path);
      }
    }
  }
}

impl<D> Drop for Ipc<D> {
  fn drop(&mut self) {
    self.remove_socket();
  }
}

/// One client's request as it arrives, then the reply as the socket takes
/// it.
#[derive(Default)]
struct Connection {
  request: Vec<u8>,
  reply: Vec<u8>,
  written: usize,
}

impl Connection {
  /// Reads, answers and writes what the socket allows. `false` once the
  /// connection is done.
  fn serve<D: 'static>(&mut self, ipc: &Ipc<D>, mut stream: &UnixStream) -> bool {
    if self.reply.is_empty() {
      let mut chunk = [0_u8; 4096];
      let request = loop {
        match stream.read(&mut chunk) {
          Ok(0) if self.request.is_empty() => return false,
          Ok(0) => break std::mem::take(&mut self.request),
          Ok(read) => {
            let end = chunk[..read].iter().position(|&byte| byte == b'\n');
            // Past the limit the rest is read and dropped: closing with
            // it unread would reset the connection before the error.
            if self.request.len() <= MAX_REQUEST {
              self
                .request
                .extend_from_slice(&chunk[..end.unwrap_or(read)]);
            }
            if end.is_some() {
              break std::mem::take(&mut self.request);
            }
          }
          Err(error) if error.kind() == ErrorKind::WouldBlock => return true,
          Err(error) if error.kind() == ErrorKind::Interrupted => {}
          Err(_) => return false,
        }
      };
      self.reply = reply(if request.len() > MAX_REQUEST {
        Err(format!("the request is over {} KiB", MAX_REQUEST / 1024))
      } else {
        ipc.answer(&request)
      });
    }
    while self.written < self.reply.len() {
      match stream.write(&self.reply[self.written..]) {
        Ok(written) => self.written += written,
        Err(error) if error.kind() == ErrorKind::WouldBlock => return true,
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(_) => return false,
      }
    }
    false
  }
}
