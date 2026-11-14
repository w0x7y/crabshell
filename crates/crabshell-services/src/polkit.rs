//! A polkit authentication agent, as Quickshell's `PolkitAgent`: when a
//! program asks polkit for something only some users may do, polkit asks
//! the shell, and a config shows the "authenticate to do X" dialog.
//!
//! The shell registers with polkit on the system bus the first time a
//! config asks for the agent. zbus has no object server here, so polkit's
//! calls are answered by hand on the main thread. Each try runs polkit's
//! setuid helper, which asks PAM: its prompts go into the request's state
//! and the config's answers into its input. Nothing polls: the shell
//! wakes when polkit calls or the helper prints.

mod helper;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Once;

use calloop::Interest;
use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use zbus::export::futures_core::Stream;
use zbus::message::{Flags, Type as Kind};
use zbus::{Connection, Message, MessageStream, OwnedVariant, Variant};

use self::helper::{Helper, Line, Secret};
use crate::Shared;
use crate::dbus::{CALL_TIMEOUT, bus_call, owner_changed, owner_rule, set_changed};

const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";
const AGENT_INTERFACE: &str = "org.freedesktop.PolicyKit1.AuthenticationAgent";
/// Where the agent answers polkit: any path of the shell's own.
const AGENT_PATH: &str = "/org/crabshell/PolkitAgent";

/// Where polkit's helper is on most distributions.
/// shortcut: one path, so a distribution that keeps it in
/// `/usr/libexec` gets no dialogs; look in both if one turns up.
pub(crate) const HELPER: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";

const FAILED: &str = "org.freedesktop.PolicyKit1.Error.Failed";
const CANCELLED: &str = "org.freedesktop.PolicyKit1.Error.Cancelled";
const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";
const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";

/// The most bytes kept of a string from polkit or its helper.
const MAX_TEXT: usize = 4 * 1024;
/// The most wrong answers to one request before polkit is told it failed.
const MAX_TRIES: u32 = 3;
/// The most identities kept of a request.
const MAX_IDENTITIES: usize = 64;
/// The most requests waiting behind the one answered.
const MAX_QUEUED: usize = 8;
/// The most info and error lines a request keeps, the newest.
const MAX_LINES: usize = 16;

/// A request from polkit: a program asked to do `action_id`, and polkit
/// wants a user to prove they may.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthRequest {
  /// Which request this is, so two alike still differ. polkit's cookie
  /// stays in the agent.
  id: u64,
  /// What was asked for, such as `org.freedesktop.login1.reboot`.
  pub action_id: String,
  /// What to tell the user, in the shell's language, at most 4 KiB.
  pub message: String,
  /// A themed icon for the action, or empty.
  pub icon_name: String,
  /// The users who may answer, by name. The first is the one asked now;
  /// [`PolkitAgent::pick_identity`] puts another first.
  pub identities: Vec<String>,
  /// Where the conversation is.
  pub state: AuthState,
  /// What PAM told the user this try, such as to touch a key, oldest
  /// first. At most 16 lines of 4 KiB.
  pub info: Vec<String>,
  /// What PAM said went wrong this try, oldest first, kept as `info` is.
  pub errors: Vec<String>,
}

/// Where a request's conversation is.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthState {
  /// polkit's helper is starting, or has only told the user something.
  Waiting,
  /// PAM asks `message`, such as `Password: `, for
  /// [`PolkitAgent::respond`]. `visible` is whether the answer may show
  /// as it is typed.
  Asking {
    /// The question, at most 4 KiB.
    message: String,
    /// `false` for a password.
    visible: bool,
  },
  /// The answer is being checked.
  Busy,
  /// The answer was wrong. [`PolkitAgent::respond`] tries again; after
  /// the third failure polkit is told, and the request goes.
  Failed {
    /// What PAM said, or a plain "Authentication failed".
    message: String,
  },
}

/// The shell's polkit agent, from `cx.polkit_agent()`. A clone is cheap
/// and shares the one agent.
#[derive(Clone)]
pub struct PolkitAgent {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  request: ReadSignal<Option<AuthRequest>>,
}

impl PolkitAgent {
  /// The request being answered now, or `None`. One shows at a time; up
  /// to 8 more wait their turn, and polkit is told any past that failed.
  pub fn request(&self) -> ReadSignal<Option<AuthRequest>> {
    self.request
  }

  /// Answers the request's question, as the user typed it. After a
  /// failure, it tries again and answers the new try's first question.
  /// At any other time, or with a line break in it, it is dropped. It is
  /// never logged, and its bytes are zeroed once sent.
  pub fn respond(&self, text: impl Into<String>) {
    let secret = Secret(text.into().into_bytes());
    if let Some(service) = &self.service {
      service.respond(secret);
    }
  }

  /// Gives up on the request, as the user's cancel button does, and tells
  /// polkit so.
  pub fn cancel(&self) {
    if let Some(service) = &self.service {
      service.finish(Some((CANCELLED, "the user cancelled")));
    }
  }

  /// Asks `name`, one of the request's identities, instead: it moves to
  /// the front, and the conversation starts again as that user. An
  /// unknown name is logged.
  pub fn pick_identity(&self, name: &str) {
    if let Some(service) = &self.service {
      service.pick(name);
    }
  }
}

/// `cx.polkit_agent()`.
pub(crate) fn polkit_agent(cx: &Cx) -> PolkitAgent {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.polkit_agent() needs the services the shell provides; no requests arrive");
    });
    return PolkitAgent {
      service: None,
      request: cx.signal(None).read_only(),
    };
  };
  let request = service.start();
  PolkitAgent {
    service: Some(service),
    request,
  }
}

/// The polkit agent of one shell, started on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
  /// The number the next helper run and request get.
  next: Cell<u64>,
}

struct State {
  request: Signal<Option<AuthRequest>>,
  /// The system bus, once connected.
  connection: Option<Connection>,
  /// polkit's unique name, once it took the registration: the only
  /// caller the agent answers.
  authority: Option<String>,
  current: Option<Current>,
  /// Requests that came while one was answered, oldest first.
  queue: VecDeque<Begin>,
}

/// Stops the helper and forgets the requests when the root goes. The
/// agent's connection closes at the next message on it, or with the shell.
struct StopOnDrop(Rc<Service>);

impl Drop for StopOnDrop {
  fn drop(&mut self) {
    self.0.forget();
  }
}

/// A `BeginAuthentication` call, kept until it is answered.
struct Begin {
  call: Message,
  cookie: String,
  shown: AuthRequest,
}

/// The request being answered.
struct Current {
  begin: Begin,
  failures: u32,
  helper: Option<Helper>,
  /// An answer given after a failure, for the next try's first question.
  answer: Option<Secret>,
}

impl Current {
  fn new(begin: Begin) -> Current {
    Current {
      begin,
      failures: 0,
      helper: None,
      answer: None,
    }
  }
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
      next: Cell::new(1),
    }
  }

  fn next(&self) -> u64 {
    let next = self.next.get();
    self.next.set(next + 1);
    next
  }

  /// The request, registering with polkit if this is the first ask.
  fn start(self: &Rc<Self>) -> ReadSignal<Option<AuthRequest>> {
    if let Some(state) = &*self.state.borrow() {
      return state.request.read_only();
    }
    let request = self.shared.root.signal(None);
    *self.state.borrow_mut() = Some(State {
      request,
      connection: None,
      authority: None,
      current: None,
      queue: VecDeque::new(),
    });
    self.shared.root.signal(StopOnDrop(Rc::clone(self)));
    match self.shared.config.system_bus.clone() {
      Some(address) => self.shared.spawn(run(Rc::clone(self), address)),
      None => tracing::info!("no system bus, so no polkit requests arrive"),
    }
    request.read_only()
  }

  /// Drops the connection and every request unanswered, as when the root
  /// or the bus goes. Touches no signal, since the root may be going.
  fn forget(&self) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      state.connection = None;
    }
    self.drop_requests();
  }

  /// Drops every request unanswered, stopping the helper, as when polkit
  /// goes and nobody could answer. Touches no signal.
  fn drop_requests(&self) {
    let current = self.state.borrow_mut().as_mut().and_then(|state| {
      state.queue.clear();
      state.authority = None;
      state.current.take()
    });
    if let Some(helper) = current.and_then(|current| current.helper) {
      helper.stop(&self.shared, true);
    }
  }

  /// Whether the request is still there: its owner, the root, isn't
  /// disposed.
  fn alive(&self) -> bool {
    let state = self.state.borrow();
    let alive = state
      .as_ref()
      .and_then(|state| state.request.try_with(|_| ()));
    alive.is_some()
  }

  /// Runs `f` on the request being answered, if there is one.
  fn with_current<R>(&self, f: impl FnOnce(&mut Current) -> R) -> Option<R> {
    let mut state = self.state.borrow_mut();
    state.as_mut()?.current.as_mut().map(f)
  }

  /// Shows the request as it is now.
  fn publish(&self) {
    let shown = self.state.borrow().as_ref().map(|state| {
      let current = state.current.as_ref();
      (
        state.request,
        current.map(|current| current.begin.shown.clone()),
      )
    });
    if let Some((signal, shown)) = shown {
      set_changed(signal, shown);
    }
  }

  /// One method call from the bus.
  fn on_call(self: &Rc<Self>, call: Message) {
    let header = call.header();
    let sender = header.sender().map(|sender| sender.as_str());
    let authority = self
      .state
      .borrow()
      .as_ref()
      .and_then(|state| state.authority.clone());
    if sender.is_none() || sender != authority.as_deref() {
      tracing::warn!(
        ?sender,
        "a program other than polkit called the agent; refused"
      );
      return self.reply(
        &call,
        Some((ACCESS_DENIED, "only polkit may call the agent")),
      );
    }
    let path = header.path().map(|path| path.as_str());
    let interface = header.interface().map(|interface| interface.as_str());
    let member = header.member().map(|member| member.as_str());
    match (path, interface, member) {
      (Some(AGENT_PATH), Some(AGENT_INTERFACE), Some("BeginAuthentication")) => self.begin(call),
      (Some(AGENT_PATH), Some(AGENT_INTERFACE), Some("CancelAuthentication")) => {
        self.cancelled(call);
      }
      _ => self.reply(&call, Some((UNKNOWN_METHOD, "no such method"))),
    }
  }

  /// polkit asks the user to authenticate.
  fn begin(self: &Rc<Self>, call: Message) {
    let begin = match begin_args(&call, self.next()) {
      Ok(begin) => begin,
      Err((error, why)) => {
        tracing::warn!(why, "a polkit request the agent can't answer");
        return self.reply(&call, Some((error, why)));
      }
    };
    let refused = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      let action = &begin.shown.action_id;
      tracing::info!(action, "polkit asks to authenticate");
      if state.current.is_none() {
        state.current = Some(Current::new(begin));
        None
      } else if state.queue.len() < MAX_QUEUED {
        state.queue.push_back(begin);
        return;
      } else {
        Some(begin)
      }
    };
    match refused {
      Some(begin) => {
        tracing::warn!("{MAX_QUEUED} polkit requests wait already; refused another");
        self.reply(&begin.call, Some((FAILED, "too many requests wait")));
      }
      None => self.run_helper(),
    }
  }

  /// polkit takes back the request `cookie` names.
  fn cancelled(self: &Rc<Self>, call: Message) {
    let Ok(cookie) = call.body().deserialize::<String>() else {
      return self.reply(&call, Some((INVALID_ARGS, "not a cookie")));
    };
    self.reply(&call, None);
    let why = "polkit cancelled it";
    let current = self.with_current(|current| current.begin.cookie == cookie);
    if current == Some(true) {
      tracing::info!("polkit cancelled the request");
      return self.finish(Some((CANCELLED, why)));
    }
    let queued = self.state.borrow_mut().as_mut().and_then(|state| {
      let at = state
        .queue
        .iter()
        .position(|begin| begin.cookie == cookie)?;
      state.queue.remove(at)
    });
    if let Some(begin) = queued {
      self.reply(&begin.call, Some((CANCELLED, why)));
    }
  }

  /// The config picks identity `name`.
  fn pick(self: &Rc<Self>, name: &str) {
    let picked = self.with_current(|current| {
      let identities = &mut current.begin.shown.identities;
      let at = identities.iter().position(|known| known == name)?;
      identities[..=at].rotate_right(1);
      current.answer = None;
      Some(current.helper.take())
    });
    let Some(helper) = picked.flatten() else {
      return tracing::warn!(name, "no such identity to pick for polkit");
    };
    if let Some(helper) = helper {
      helper.stop(&self.shared, true);
    }
    self.run_helper();
  }

  /// Starts a try: polkit's helper, for the first identity.
  fn run_helper(self: &Rc<Self>) {
    let Some(event_loop) = self.shared.event_loop() else {
      return;
    };
    let run = self.next();
    let started = self.with_current(|current| {
      let shown = &mut current.begin.shown;
      shown.state = AuthState::Waiting;
      shown.info.clear();
      shown.errors.clear();
      let program = &self.shared.config.polkit_helper;
      let mut helper = Helper::start(program, &shown.identities[0], run)?;
      if let Err(error) = helper.send(current.begin.cookie.as_bytes()) {
        helper.stop(&self.shared, true);
        return Err(error);
      }
      let service = Rc::clone(self);
      let ready = Box::new(move || service.on_output(run));
      helper.token = helper
        .stdout_fd()
        .and_then(|fd| event_loop.insert_fd(fd, Interest::READ, ready));
      if helper.token.is_none() {
        helper.stop(&self.shared, true);
        return Err(std::io::Error::other("its output couldn't be watched"));
      }
      current.helper = Some(helper);
      Ok(())
    });
    match started {
      Some(Ok(())) => self.publish(),
      Some(Err(error)) => {
        let program = self.shared.config.polkit_helper.display();
        tracing::warn!(%error, %program, "couldn't start polkit's helper");
        self.finish(Some((FAILED, "polkit's helper couldn't start")));
      }
      None => {}
    }
  }

  /// What the helper of run `run` printed.
  fn on_output(self: &Rc<Self>, run: u64) {
    let running = |service: &Service| {
      let running =
        service.with_current(|current| current.helper.as_ref().map(|helper| helper.run));
      running.flatten() == Some(run)
    };
    if !running(self) {
      return;
    }
    let read = self.with_current(|current| current.helper.as_mut()?.read());
    let Some((lines, ended)) = read.flatten() else {
      return;
    };
    for line in lines {
      if !running(self) {
        return;
      }
      self.on_line(helper::parse(&line));
    }
    if ended && running(self) {
      tracing::warn!("polkit's helper ended without an answer");
      self.failed();
    }
  }

  fn on_line(self: &Rc<Self>, line: Line) {
    let keep = |lines: &mut Vec<String>, text| {
      if lines.len() == MAX_LINES {
        lines.remove(0);
      }
      lines.push(text);
    };
    let verdict = line_verdict(&line);
    let answer = self.with_current(|current| {
      let shown = &mut current.begin.shown;
      match line {
        Line::Prompt { message, visible } => {
          shown.state = AuthState::Asking { message, visible };
          return current.answer.take();
        }
        Line::Info(text) => keep(&mut shown.info, text),
        Line::Error(text) => keep(&mut shown.errors, text),
        Line::Unknown => tracing::debug!("polkit's helper printed a line it doesn't know; skipped"),
        Line::Success | Line::Failure => {}
      }
      None
    });
    match verdict {
      Some(true) => {
        tracing::info!("authenticated for polkit");
        self.finish(None);
      }
      Some(false) => self.failed(),
      None => match answer.flatten() {
        // A try after a failure answers its first question at once.
        Some(secret) => self.send(secret),
        None => self.publish(),
      },
    }
  }

  /// A wrong answer, or a helper that ended without saying: shows it, or
  /// after the third tells polkit.
  fn failed(self: &Rc<Self>) {
    let gave_up = self.with_current(|current| {
      if let Some(helper) = current.helper.take() {
        helper.stop(&self.shared, true);
      }
      current.answer = None;
      current.failures += 1;
      let shown = &mut current.begin.shown;
      let message = shown.errors.last().cloned();
      let message = message.unwrap_or_else(|| String::from("Authentication failed"));
      shown.state = AuthState::Failed { message };
      current.failures >= MAX_TRIES
    });
    match gave_up {
      Some(true) => {
        tracing::info!("polkit authentication failed {MAX_TRIES} times");
        self.finish(Some((FAILED, "authentication failed")));
      }
      Some(false) => self.publish(),
      None => {}
    }
  }

  /// The config's answer.
  fn respond(self: &Rc<Self>, secret: Secret) {
    // A line break would end the line early, and the helper, in C, would
    // read a NUL as the end.
    if secret.0.contains(&b'\n') || secret.0.contains(&0) {
      return tracing::warn!("a polkit answer with a line break or NUL was dropped");
    }
    let state = self.with_current(|current| current.begin.shown.state.clone());
    match state {
      Some(AuthState::Asking { .. }) => self.send(secret),
      Some(AuthState::Failed { .. }) => {
        self.with_current(|current| current.answer = Some(secret));
        self.run_helper();
      }
      _ => tracing::debug!("no polkit question to answer; an answer was dropped"),
    }
  }

  /// Sends `secret` to the helper, which then checks it. It goes when
  /// this returns.
  fn send(self: &Rc<Self>, secret: Secret) {
    let sent = self.with_current(|current| {
      let sent = current.helper.as_mut().map(|helper| helper.send(&secret.0));
      if let Some(Ok(())) = sent {
        current.begin.shown.state = AuthState::Busy;
      }
      sent
    });
    match sent.flatten() {
      Some(Ok(())) => self.publish(),
      Some(Err(error)) => {
        tracing::warn!(%error, "couldn't answer polkit's helper");
        self.failed();
      }
      None => {}
    }
  }

  /// Ends the request being answered and tells polkit: `None` for
  /// authenticated, else the error's name and why. The next waiting
  /// request starts.
  fn finish(self: &Rc<Self>, error: Option<(&'static str, &str)>) {
    let current = self.state.borrow_mut().as_mut().and_then(|state| {
      let current = state.current.take()?;
      state.current = state.queue.pop_front().map(Current::new);
      Some(current)
    });
    let Some(current) = current else {
      return;
    };
    if let Some(helper) = current.helper {
      // After a success it is telling polkit, so it is left to end.
      helper.stop(&self.shared, error.is_some());
    }
    self.reply(&current.begin.call, error);
    match self.with_current(|_| ()) {
      Some(()) => self.run_helper(),
      None => self.publish(),
    }
  }

  /// Answers `call`: with an empty return for `None`, else with the error
  /// and why.
  fn reply(&self, call: &Message, error: Option<(&str, &str)>) {
    let header = call.header();
    if header.primary().flags().contains(Flags::NO_REPLY_EXPECTED) {
      return;
    }
    let reply = match error {
      None => Message::method_return(&header).build(&()),
      Some((name, why)) => Message::error(&header, name).build(&why),
    };
    let connection = self
      .state
      .borrow()
      .as_ref()
      .and_then(|state| state.connection.clone());
    let Some(connection) = connection else {
      return;
    };
    self.shared.spawn(async move {
      let sent = match reply {
        Ok(reply) => connection.send(&reply).await,
        Err(error) => Err(error),
      };
      if let Err(error) = sent {
        tracing::debug!(%error, "the polkit agent's answer wasn't sent");
      }
    });
  }
}

/// Whether `line` ends the try: `true` for a success, `false` for a
/// failure.
fn line_verdict(line: &Line) -> Option<bool> {
  match line {
    Line::Success => Some(true),
    Line::Failure => Some(false),
    _ => None,
  }
}

/// `BeginAuthentication`'s arguments: the action, the message, the icon,
/// details, the cookie and the identities.
type BeginArgs<'a> = (
  &'a str,
  &'a str,
  &'a str,
  HashMap<&'a str, &'a str>,
  &'a str,
  Vec<(&'a str, HashMap<&'a str, OwnedVariant>)>,
);

/// The request `call` asks for, numbered `id`, or the error to answer.
fn begin_args(call: &Message, id: u64) -> Result<Begin, (&'static str, &'static str)> {
  let body = call.body();
  let Ok((action_id, message, icon_name, _, cookie, identities)) =
    body.deserialize::<BeginArgs<'_>>()
  else {
    return Err((INVALID_ARGS, "not a polkit request"));
  };
  // It is the helper's first line of input, so it can't hold another.
  if cookie.is_empty() || cookie.len() > MAX_TEXT || cookie.contains('\n') {
    return Err((FAILED, "a bad cookie"));
  }
  let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
  let mut users: Vec<(u32, String)> = identities
    .iter()
    .take(MAX_IDENTITIES)
    .filter(|(kind, _)| *kind == "unix-user")
    .filter_map(|(_, details)| u32::try_from(details.get("uid")?).ok())
    .filter_map(|uid| Some((uid, user_name(&passwd, uid)?)))
    .collect();
  // The session's user is asked first: polkit may list root first, as for
  // an admin group.
  let own = rustix::process::getuid().as_raw();
  if let Some(at) = users.iter().position(|(uid, _)| *uid == own) {
    users[..=at].rotate_right(1);
  }
  let names: Vec<String> = users.into_iter().map(|(_, name)| name).collect();
  if names.is_empty() {
    return Err((FAILED, "no user the agent knows may answer"));
  }
  Ok(Begin {
    call: call.clone(),
    cookie: cookie.to_owned(),
    shown: AuthRequest {
      id,
      action_id: cut(action_id, MAX_TEXT),
      message: cut(message, MAX_TEXT),
      icon_name: cut(icon_name, MAX_TEXT),
      identities: names,
      state: AuthState::Waiting,
      info: Vec::new(),
      errors: Vec::new(),
    },
  })
}

/// `text`'s first `max` bytes at most, cut at a char boundary.
fn cut(text: &str, max: usize) -> String {
  text[..text.floor_char_boundary(max)].to_owned()
}

/// The name of user `uid`, from `passwd`, the text of `/etc/passwd`, or
/// for this shell's own user from `USER`.
/// shortcut: reads only `/etc/passwd`, so another user from LDAP or
/// systemd-homed isn't offered; ask NSS through a crate if that matters.
fn user_name(passwd: &str, uid: u32) -> Option<String> {
  let listed = passwd.lines().find_map(|line| {
    let mut fields = line.split(':');
    let name = fields.next()?;
    let uid_field = fields.nth(1)?;
    (uid_field.parse() == Ok(uid)).then(|| name.to_owned())
  });
  let own = || {
    let ours = rustix::process::getuid().as_raw() == uid;
    ours.then(|| std::env::var("USER").ok()).flatten()
  };
  listed.or_else(own).filter(|name| !name.is_empty())
}

/// Who the agent answers for: this login session when `session` names
/// one, as `XDG_SESSION_ID` does, else this process.
fn subject(session: Option<String>) -> (&'static str, HashMap<&'static str, Variant<'static>>) {
  match session {
    Some(id) => (
      "unix-session",
      HashMap::from([("session-id", Variant::from(id))]),
    ),
    None => (
      "unix-process",
      HashMap::from([
        ("pid", Variant::from(std::process::id())),
        ("start-time", Variant::from(start_time().unwrap_or(0))),
      ]),
    ),
  }
}

/// When this process started, in clock ticks after boot, as polkit names
/// a process: the 22nd field of `/proc/self/stat`.
fn start_time() -> Option<u64> {
  let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
  // Fields after the name in parentheses start at the 3rd.
  stat
    .rsplit_once(')')?
    .1
    .split_whitespace()
    .nth(19)?
    .parse()
    .ok()
}

/// Joins the system bus at `address`, registers the agent with polkit,
/// again whenever polkit starts again, and answers polkit's calls, until
/// the bus closes or the request is disposed.
async fn run(service: Rc<Service>, address: zbus::Address) {
  let connection = match zbus::connection::Builder::address(address).build().await {
    Ok(connection) => connection,
    Err(error) => {
      return tracing::info!(%error, "can't reach the system bus, so no polkit requests arrive");
    }
  };
  // Made before polkit can call, so no call is missed.
  let mut messages = MessageStream::from(&connection);
  if let Err(error) = bus_call(&connection, "AddMatch", &owner_rule(AUTHORITY)).await {
    return tracing::warn!(%error, "can't follow polkit on the system bus, so no requests arrive");
  }
  if let Some(state) = service.state.borrow_mut().as_mut() {
    state.connection = Some(connection.clone());
  }
  register(&service, &connection).await;
  loop {
    let next = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await;
    if !service.alive() {
      return;
    }
    match next {
      Some(Ok(message)) if message.header().message_type() == Kind::MethodCall => {
        service.on_call(message);
      }
      Some(Ok(message)) => {
        // The bus daemon sends this, so nobody else can fake it.
        if let Some((name, owner)) = owner_changed(&message)
          && name == AUTHORITY
        {
          // Its requests went with it.
          service.drop_requests();
          service.publish();
          if !owner.is_empty() {
            tracing::info!("polkit started again; registering the agent with it");
            register(&service, &connection).await;
          }
        }
      }
      Some(Err(error)) => tracing::warn!(%error, "a bad message on the system bus"),
      None => break,
    }
  }
  tracing::warn!("the system bus closed, so no more polkit requests arrive");
  service.forget();
  service.publish();
}

/// Registers the agent with the polkit on `connection`, which takes its
/// calls only from the unique name that answered. A refusal is logged.
async fn register(service: &Rc<Service>, connection: &Connection) {
  let session = std::env::var("XDG_SESSION_ID")
    .ok()
    .filter(|id| !id.is_empty());
  // The locale desktop entries are read in, so polkit's message matches.
  let locale = service.shared.config.desktop_entries.locale.as_str();
  let body = (subject(session), locale, AGENT_PATH);
  let call = connection.call_method(
    Some(AUTHORITY),
    AUTHORITY_PATH,
    Some(AUTHORITY_INTERFACE),
    "RegisterAuthenticationAgent",
    &body,
  );
  let authority = match service.shared.timeout(CALL_TIMEOUT, call).await {
    Ok(reply) => reply.header().sender().map(|sender| sender.to_string()),
    Err(error) => return tracing::warn!(%error, "polkit refused the agent, so no requests arrive"),
  };
  tracing::info!("registered as the polkit agent");
  if let Some(state) = service.state.borrow_mut().as_mut() {
    state.authority = authority;
  }
}
