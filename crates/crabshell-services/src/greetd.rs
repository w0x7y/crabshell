//! greetd, the login manager, for a greeter config: a session is made for
//! a user, its questions are answered, and it is started once the greeter
//! exits. The protocol is greetd-ipc(7): over the socket `GREETD_SOCK`
//! names, each message is its length as a native-endian `u32`, then that
//! much JSON.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::Once;

use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::Cx;
use serde::Deserialize;
use serde_json::json;

use crate::Shared;

/// The largest message read from greetd; a larger one drops the
/// connection.
pub(crate) const MAX_MESSAGE: usize = 64 * 1024;

/// The longest text kept from greetd, cut at a character.
const MAX_TEXT: usize = 4 * 1024;

/// Where a session being made stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum GreetState {
  /// No session is being made.
  #[default]
  Idle,
  /// A request is on its way to greetd.
  Busy,
  /// greetd asks a question, such as for the password; `visible` is
  /// false for a secret.
  Asking {
    /// The question, such as `Password:`.
    message: String,
    /// Whether the answer may show as it is typed.
    visible: bool,
  },
  /// The user is authenticated, and the session can be started.
  Authenticated,
  /// The session will start once the greeter exits.
  Started,
  /// A request failed. `auth` says it was the credentials, which the
  /// user can try again.
  Failed {
    /// greetd's description.
    message: String,
    /// Whether authentication failed, rather than something else.
    auth: bool,
  },
}

/// An informative or error message greetd sent while a session was made.
#[derive(Clone, Debug, PartialEq)]
pub struct GreetMessage {
  /// The message.
  pub text: String,
  /// Whether it is an error, rather than information.
  pub error: bool,
}

/// greetd, from `cx.greetd()`. A clone is cheap and shares it.
///
/// A greeter makes a session for a user with
/// [`create_session`](Self::create_session), answers each question in
/// [`state`](Self::state) with [`respond`](Self::respond), and once it is
/// [`Authenticated`](GreetState::Authenticated) starts it with
/// [`start_session`](Self::start_session), then exits so greetd can.
#[derive(Clone)]
pub struct Greetd {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  state: ReadSignal<GreetState>,
  messages: ReadSignal<Vec<GreetMessage>>,
}

impl Greetd {
  /// Where the session being made stands.
  pub fn state(&self) -> ReadSignal<GreetState> {
    self.state
  }

  /// The informative and error messages greetd sent since the last
  /// [`create_session`](Self::create_session), oldest first. They need no
  /// answer: each is acknowledged as it comes.
  pub fn messages(&self) -> ReadSignal<Vec<GreetMessage>> {
    self.messages
  }

  /// Makes a session for `user` and starts authenticating them, ending
  /// any session being made before.
  pub fn create_session(&self, user: &str) {
    if let Some(service) = &self.service {
      service.create_session(user);
    }
  }

  /// Answers greetd's question. The answer is never logged, and its bytes
  /// are zeroed once written.
  pub fn respond(&self, answer: &str) {
    if let Some(service) = &self.service {
      service.respond(Some(answer));
    }
  }

  /// Starts the authenticated session with `command`, such as
  /// `["sway"]`, and `env` lines such as `XDG_SESSION_TYPE=wayland`
  /// added to what PAM sets. It starts once the greeter exits.
  pub fn start_session(&self, command: Vec<String>, env: Vec<String>) {
    if let Some(service) = &self.service {
      service.send(
        Kind::Start,
        json!({"type": "start_session", "cmd": command, "env": env}),
      );
    }
  }

  /// Ends the session being made.
  pub fn cancel(&self) {
    if let Some(service) = &self.service {
      service.send(
        Kind::Cancel { asked: true },
        json!({"type": "cancel_session"}),
      );
    }
  }
}

/// `cx.greetd()`.
pub(crate) fn greetd(cx: &Cx) -> Greetd {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.greetd() needs the services the shell provides; it never connects");
    });
    return Greetd {
      service: None,
      state: cx.signal(GreetState::Idle).read_only(),
      messages: cx.signal(Vec::new()).read_only(),
    };
  };
  let (state, messages) = service.signals();
  Greetd {
    service: Some(service),
    state: state.read_only(),
    messages: messages.read_only(),
  }
}

/// greetd for one shell: its signals and connection, made on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  inner: RefCell<Inner>,
}

#[derive(Default)]
struct Inner {
  signals: Option<(Signal<GreetState>, Signal<Vec<GreetMessage>>)>,
  connection: Option<Connection>,
  /// Requests waiting their turn: greetd answers one at a time.
  queue: VecDeque<(Kind, Vec<u8>)>,
  /// The request greetd is answering.
  asked: Option<Kind>,
}

struct Connection {
  stream: UnixStream,
  reader: RegistrationToken,
  writer: Option<RegistrationToken>,
  inbox: Vec<u8>,
  outbox: Vec<u8>,
}

/// What a request was, to read its answer.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
  Create,
  Respond,
  Start,
  /// `asked` when the config asked; otherwise it ends a session that
  /// failed, and changes nothing shown.
  Cancel {
    asked: bool,
  },
}

/// greetd's answer to a request.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Reply {
  Success,
  Error {
    error_type: String,
    description: String,
  },
  AuthMessage {
    auth_message_type: String,
    auth_message: String,
  },
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      inner: RefCell::default(),
    }
  }

  /// The signals, made under the root on first use.
  fn signals(&self) -> (Signal<GreetState>, Signal<Vec<GreetMessage>>) {
    let mut inner = self.inner.borrow_mut();
    *inner.signals.get_or_insert_with(|| {
      (
        self.shared.root.signal(GreetState::Idle),
        self.shared.root.signal(Vec::new()),
      )
    })
  }

  fn create_session(self: &Rc<Self>, user: &str) {
    let in_flight = {
      let inner = self.inner.borrow();
      inner.asked.is_some() || !inner.queue.is_empty()
    };
    let busy = in_flight || !matches!(self.state(), GreetState::Idle | GreetState::Failed { .. });
    if busy {
      // greetd makes one session at a time.
      self.send(
        Kind::Cancel { asked: false },
        json!({"type": "cancel_session"}),
      );
    }
    let (_, messages) = self.signals();
    set(messages, Vec::new());
    self.send(
      Kind::Create,
      json!({"type": "create_session", "username": user}),
    );
  }

  fn respond(self: &Rc<Self>, answer: Option<&str>) {
    let request = json!({"type": "post_auth_message_response", "response": answer});
    let mut body = request.to_string().into_bytes();
    let bytes = frame(&body);
    // This copy of the answer goes now, the framed one once sent.
    zero(&mut body);
    self.queue(Kind::Respond, bytes);
  }

  fn send(self: &Rc<Self>, kind: Kind, body: serde_json::Value) {
    self.queue(kind, frame(body.to_string().as_bytes()));
  }

  fn state(&self) -> GreetState {
    let (state, _) = self.signals();
    untrack(|| state.try_with(Clone::clone)).unwrap_or_default()
  }

  fn queue(self: &Rc<Self>, kind: Kind, bytes: Vec<u8>) {
    let (state, _) = self.signals();
    if kind != (Kind::Cancel { asked: false }) {
      set(state, GreetState::Busy);
    }
    self.inner.borrow_mut().queue.push_back((kind, bytes));
    self.next();
  }

  /// Sends the next request, once greetd answered the last.
  fn next(self: &Rc<Self>) {
    if self.inner.borrow().asked.is_some() {
      return;
    }
    if self.inner.borrow().connection.is_none() && !self.connect() {
      return;
    }
    let mut inner = self.inner.borrow_mut();
    let Some((kind, bytes)) = inner.queue.pop_front() else {
      return;
    };
    inner.asked = Some(kind);
    let connection = inner
      .connection
      .as_mut()
      .expect("invariant: connected above");
    connection.outbox.extend_from_slice(&bytes);
    let mut bytes = bytes;
    zero(&mut bytes);
    drop(inner);
    self.flush();
  }

  /// Connects to greetd. `false`, with the state failed and the queue
  /// dropped, if it isn't there.
  fn connect(self: &Rc<Self>) -> bool {
    let Some(event_loop) = self.shared.event_loop() else {
      return false;
    };
    let path = self.shared.config.greetd.clone();
    let opened = path
      .as_ref()
      .ok_or_else(|| std::io::Error::other("GREETD_SOCK isn't set"))
      .and_then(|path| {
        let stream = UnixStream::connect(path)?;
        stream.set_nonblocking(true)?;
        let fd = stream.as_fd().try_clone_to_owned()?;
        Ok((stream, fd))
      });
    let (stream, fd) = match opened {
      Ok(opened) => opened,
      Err(error) => {
        let path = path.as_ref().map(|path| path.display().to_string());
        tracing::warn!(?path, %error, "can't reach greetd");
        self.fail(format!("can't reach greetd: {error}"), false);
        return false;
      }
    };
    let service = Rc::clone(self);
    let Some(reader) = event_loop.insert_fd(fd, Interest::READ, Box::new(move || service.read()))
    else {
      self.fail(String::from("can't follow greetd's socket"), false);
      return false;
    };
    self.inner.borrow_mut().connection = Some(Connection {
      stream,
      reader,
      writer: None,
      inbox: Vec::new(),
      outbox: Vec::new(),
    });
    true
  }

  /// Writes what the socket takes, zeroing what went, and waits for room
  /// for the rest.
  fn flush(self: &Rc<Self>) {
    let Some(event_loop) = self.shared.event_loop() else {
      return;
    };
    let mut inner = self.inner.borrow_mut();
    let Some(connection) = inner.connection.as_mut() else {
      return;
    };
    let mut broken = None;
    while !connection.outbox.is_empty() {
      match connection.stream.write(&connection.outbox) {
        Ok(written) => {
          zero(&mut connection.outbox[..written]);
          connection.outbox.drain(..written);
        }
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
        Err(error) => {
          broken = Some(error.to_string());
          break;
        }
      }
    }
    if broken.is_none() {
      if connection.outbox.is_empty() {
        if let Some(writer) = connection.writer.take() {
          event_loop.remove(writer);
        }
      } else if connection.writer.is_none() {
        match connection.stream.as_fd().try_clone_to_owned() {
          Ok(fd) => {
            let service = Rc::clone(self);
            let write = Box::new(move || service.flush());
            connection.writer = event_loop.insert_fd(fd, Interest::WRITE, write);
          }
          Err(error) => broken = Some(error.to_string()),
        }
      }
    }
    drop(inner);
    if let Some(why) = broken {
      self.lost(&why);
    }
  }

  /// Reads what greetd sent and acts on each whole message.
  fn read(self: &Rc<Self>) {
    let mut chunk = [0; 16 * 1024];
    let mut inner = self.inner.borrow_mut();
    let Some(connection) = inner.connection.as_mut() else {
      return;
    };
    let why = match connection.stream.read(&mut chunk) {
      Ok(0) => Some(String::from("greetd closed the socket")),
      Ok(read) => {
        connection.inbox.extend_from_slice(&chunk[..read]);
        None
      }
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      Err(error) => Some(error.to_string()),
    };
    if let Some(why) = why {
      drop(inner);
      return self.lost(&why);
    }
    let mut replies = Vec::new();
    let mut broken = None;
    while connection.inbox.len() >= 4 {
      let length = u32::from_ne_bytes(connection.inbox[..4].try_into().expect("four bytes"));
      let length = length as usize;
      if length > MAX_MESSAGE {
        broken = Some(format!("greetd sent a message of {length} bytes"));
        break;
      }
      if connection.inbox.len() < 4 + length {
        break;
      }
      let body: Vec<u8> = connection.inbox.drain(..4 + length).skip(4).collect();
      replies.push(body);
    }
    drop(inner);
    if let Some(why) = broken {
      return self.lost(&why);
    }
    for body in replies {
      match serde_json::from_slice::<Reply>(&body) {
        Ok(reply) => self.answered(reply),
        Err(error) => {
          tracing::warn!(%error, "a message greetd sent doesn't parse");
          return self.lost("greetd sent something it shouldn't");
        }
      }
    }
  }

  /// Acts on greetd's answer to the request it was answering.
  fn answered(self: &Rc<Self>, reply: Reply) {
    let (state, messages) = self.signals();
    let asked = self.inner.borrow_mut().asked.take();
    let Some(kind) = asked else {
      tracing::warn!("greetd answered nothing asked");
      return;
    };
    match (kind, reply) {
      (Kind::Create | Kind::Respond, Reply::Success) => set(state, GreetState::Authenticated),
      (Kind::Start, Reply::Success) => set(state, GreetState::Started),
      (Kind::Cancel { asked }, Reply::Success) => {
        if asked {
          set(state, GreetState::Idle);
        }
      }
      (
        kind,
        Reply::Error {
          error_type,
          description,
        },
      ) => {
        let auth = error_type == "auth_error";
        if !matches!(kind, Kind::Cancel { asked: false }) {
          set(
            state,
            GreetState::Failed {
              message: cut(description),
              auth,
            },
          );
        }
        // A failed attempt is ended, so another can begin.
        if matches!(kind, Kind::Create | Kind::Respond) {
          self.send(
            Kind::Cancel { asked: false },
            json!({"type": "cancel_session"}),
          );
        }
      }
      (
        _,
        Reply::AuthMessage {
          auth_message_type,
          auth_message,
        },
      ) => match auth_message_type.as_str() {
        kind @ ("visible" | "secret") => set(
          state,
          GreetState::Asking {
            message: cut(auth_message),
            visible: kind == "visible",
          },
        ),
        kind => {
          let message = GreetMessage {
            text: cut(auth_message),
            error: kind == "error",
          };
          messages.try_update(|all| all.push(message));
          // Informative: acknowledged with no answer.
          self.respond(None);
        }
      },
    }
    self.next();
  }

  /// The request in flight failed for `message`.
  fn fail(&self, message: String, auth: bool) {
    let (state, _) = self.signals();
    set(state, GreetState::Failed { message, auth });
    let mut inner = self.inner.borrow_mut();
    inner.queue.clear();
    inner.asked = None;
  }

  /// Drops the connection, which ends the session being made.
  fn lost(&self, why: &str) {
    tracing::warn!(why, "lost the connection to greetd");
    let connection = self.inner.borrow_mut().connection.take();
    if let (Some(mut connection), Some(event_loop)) = (connection, self.shared.event_loop()) {
      event_loop.remove(connection.reader);
      if let Some(writer) = connection.writer {
        event_loop.remove(writer);
      }
      zero(&mut connection.outbox);
    }
    self.fail(format!("lost greetd: {why}"), false);
  }
}

/// `body` with its length before it, as greetd reads it.
fn frame(body: &[u8]) -> Vec<u8> {
  let length = u32::try_from(body.len()).unwrap_or(u32::MAX);
  let mut framed = Vec::with_capacity(4 + body.len());
  framed.extend_from_slice(&length.to_ne_bytes());
  framed.extend_from_slice(body);
  framed
}

/// Overwrites `bytes` with zeros, as for an answer once sent.
fn zero(bytes: &mut [u8]) {
  for byte in bytes.iter_mut() {
    *byte = 0;
  }
  std::hint::black_box(bytes);
}

/// `text` cut to [`MAX_TEXT`] bytes at a character.
fn cut(mut text: String) -> String {
  if text.len() > MAX_TEXT {
    text.truncate(text.floor_char_boundary(MAX_TEXT));
  }
  text
}

/// Sets `signal` unless it holds `value` or is gone.
fn set<T: PartialEq + 'static>(signal: Signal<T>, value: T) {
  if untrack(|| signal.try_with(|old| *old != value)) == Some(true) {
    signal.try_set(value);
  }
}
