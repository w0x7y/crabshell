//! PAM authentication, as Quickshell's `PamContext`, for lock screens.

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::rc::Rc;
use std::sync::{Arc, Once, mpsc};

use calloop::RegistrationToken;
use calloop::channel::Event as Arrived;
use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::Cx;
use nonstick::{AuthnFlags, ConversationAdapter, ErrorCode, Transaction, TransactionBuilder};

use crate::Shared;

/// The longest question or message passed on, in bytes. The rest is cut.
const MAX_TEXT: usize = 4096;

/// The most messages one attempt keeps; older ones go.
pub(crate) const MAX_MESSAGES: usize = 64;

/// What `cx.pam` needs from the shell: the loop and the backend.
pub(crate) struct Service(pub(crate) Rc<Shared>);

/// Where an authentication is, from [`Pam::state`].
#[derive(Clone, Debug, PartialEq)]
pub enum PamState {
  /// Nothing started, or the last attempt was aborted.
  Idle,
  /// PAM asks `message`, such as `Password: `, and waits for
  /// [`Pam::respond`].
  Asking {
    /// The question, cut to 4 KiB.
    message: String,
    /// Whether the answer may show as it is typed: `false` for a
    /// password.
    visible: bool,
  },
  /// PAM is working, as between an answer and the next question.
  Busy,
  /// The attempt ended.
  Done(PamResult),
}

/// How an attempt ended.
#[derive(Clone, Debug, PartialEq)]
pub enum PamResult {
  /// The user is who they said.
  Success,
  /// PAM refused the answers, as for a wrong password.
  Failure,
  /// Something kept PAM from deciding, such as an unknown user.
  Error {
    /// Where it went wrong.
    kind: PamError,
    /// What went wrong, in PAM's words or the shell's.
    message: String,
  },
}

impl PamResult {
  pub(crate) fn error(kind: PamError, message: impl Into<String>) -> PamResult {
    let message = message.into();
    PamResult::Error { kind, message }
  }
}

/// Where an attempt that ended in [`PamResult::Error`] went wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PamError {
  /// It never began: libpam refused to start the service, a service or
  /// user name held a NUL, or NSS doesn't know the shell's own user.
  StartFailed,
  /// PAM couldn't try the answers, which isn't the user failing to
  /// authenticate: `pam_authenticate` returned an error but
  /// `PAM_AUTH_ERR`, which is [`Failure`](PamResult::Failure), and
  /// `PAM_CONV_ERR`, such as `PAM_USER_UNKNOWN`, `PAM_AUTHINFO_UNAVAIL`
  /// or `PAM_MAXTRIES`.
  TryAuthFailed,
  /// The shell's side failed: it refused PAM an answer, as one with a
  /// NUL, or the attempt ended without a result.
  InternalError,
}

/// A line PAM sent during an attempt, from [`Pam::messages`], cut to
/// 4 KiB.
#[derive(Clone, Debug, PartialEq)]
pub enum PamMessage {
  /// Something to show, such as how to use a fingerprint reader.
  Info(String),
  /// Something that went wrong, such as a locked account.
  Error(String),
}

/// Authentication through a PAM service, from `cx.pam(service)`, as a lock
/// screen does. A clone is cheap and shares it.
///
/// libpam blocks, so each attempt runs on a thread of its own, and the
/// loop hears from it only while it runs: an idle `Pam` wakes nothing. When
/// the context that made it goes, an attempt still running is refused its
/// answer, so its thread ends.
#[derive(Clone)]
pub struct Pam {
  inner: Rc<Inner>,
}

struct Inner {
  /// `None` when the shell provided no services.
  shared: Option<Rc<Shared>>,
  service: String,
  /// These live under the asking context's owner.
  state: Signal<PamState>,
  messages: Signal<Vec<PamMessage>>,
  attempt: RefCell<Option<Attempt>>,
  /// The loop source of an aborted attempt whose thread is still inside
  /// PAM, as one a fingerprint module holds, until that thread returns.
  abandoned: RefCell<Option<RegistrationToken>>,
  /// The user a start named while that thread ran, which begins once it
  /// returns.
  waiting: RefCell<Option<Option<String>>>,
  /// The number the next attempt gets.
  next: Cell<u64>,
}

/// The attempt running now.
struct Attempt {
  /// Which attempt this is, so the events of an aborted one are told
  /// apart.
  number: u64,
  /// The loop source its thread's events arrive on.
  source: RegistrationToken,
  /// Where answers go. Dropping it refuses the question its thread waits
  /// on.
  answers: mpsc::Sender<Secret>,
  /// A question waits for its answer.
  asking: bool,
}

/// What an attempt's thread tells the loop.
pub(crate) enum Event {
  Ask { message: String, visible: bool },
  Tell(PamMessage),
  Done(PamResult),
}

/// What authenticates, off the loop's thread: libpam, or a test's script.
pub(crate) trait Backend: Send + Sync {
  /// Authenticates `user` for `service`, asking and telling through
  /// `talk`.
  fn authenticate(&self, service: &str, user: &str, talk: &Talk) -> PamResult;
}

/// An attempt's end of the conversation, on its thread.
pub(crate) struct Talk {
  events: calloop::channel::Sender<Event>,
  answers: mpsc::Receiver<Secret>,
}

impl Talk {
  /// Asks the config `question`, cut to 4 KiB, and waits for the answer.
  /// `None` when it was refused: the attempt was aborted, its context
  /// went, or the shell is gone.
  pub(crate) fn ask(&self, question: &OsStr, visible: bool) -> Option<Secret> {
    let message = cut(question);
    self.events.send(Event::Ask { message, visible }).ok()?;
    self.answers.recv().ok()
  }

  /// Passes `text`, cut to 4 KiB, on as a message.
  pub(crate) fn tell(&self, kind: fn(String) -> PamMessage, text: &OsStr) {
    // Refused once the attempt is gone, when nobody reads it.
    let _ = self.events.send(Event::Tell(kind(cut(text))));
  }
}

/// `text` as UTF-8 with bad bytes replaced, cut to [`MAX_TEXT`] bytes on
/// a character's boundary.
fn cut(text: &OsStr) -> String {
  let mut text = text.to_string_lossy().into_owned();
  text.truncate(text.floor_char_boundary(MAX_TEXT));
  text
}

/// An answer on its way to libpam. Its bytes are zeroed when it drops, and
/// it has no `Debug`, so it can't be logged.
pub(crate) struct Secret(Vec<u8>);

impl Secret {
  pub(crate) fn bytes(&self) -> &[u8] {
    &self.0
  }
}

impl Drop for Secret {
  fn drop(&mut self) {
    wipe(&mut self.0);
  }
}

/// Zeroes every byte `bytes` holds room for.
pub(crate) fn wipe(bytes: &mut Vec<u8>) {
  bytes.clear();
  bytes.resize(bytes.capacity(), 0);
  // So the compiler can't drop the writes as dead before the free.
  std::hint::black_box(bytes);
}

/// `cx.pam(service)`.
pub(crate) fn pam(cx: &Cx, service: &str) -> Pam {
  let shared = cx.service::<Service>().map(|service| Rc::clone(&service.0));
  if shared.is_none() {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.pam() needs the services the shell provides; it never authenticates");
    });
  }
  let inner = Rc::new(Inner {
    shared,
    service: service.to_owned(),
    state: cx.signal(PamState::Idle),
    messages: cx.signal(Vec::new()),
    attempt: RefCell::default(),
    abandoned: RefCell::default(),
    waiting: RefCell::default(),
    next: Cell::new(0),
  });
  // Dropped with the asking context's owner, which ends the attempt.
  cx.owner().signal(EndOnDrop(Rc::clone(&inner)));
  Pam { inner }
}

/// Ends the attempt when the asking context's owner drops it.
struct EndOnDrop(Rc<Inner>);

impl Drop for EndOnDrop {
  fn drop(&mut self) {
    self.0.end();
    self.0.waiting.take();
    if let Some(token) = self.0.abandoned.take() {
      self.0.remove(token);
    }
  }
}

impl Pam {
  /// Starts authenticating `user`, or with `None` the user the shell runs
  /// as, found by its uid. An attempt already running is aborted first.
  ///
  /// PAM can't be stopped while a module blocks without asking, as a
  /// fingerprint reader waits for a finger, so an aborted attempt's
  /// thread stays inside PAM until the module returns. A start meanwhile
  /// shows [`Busy`](PamState::Busy) and begins once it has, so two
  /// transactions never run side by side.
  ///
  /// Ending an attempt while PAM waits for an answer, by aborting it,
  /// starting another or dropping its context, counts as a failed login
  /// where `pam_faillock` counts failures, and enough of them lock the
  /// account. So take the password first, start on Enter, and answer as
  /// soon as PAM asks.
  pub fn start(&self, user: Option<&str>) {
    self.inner.abort();
    self.inner.start(user.map(str::to_owned));
  }

  /// Answers the question asked, and the state turns
  /// [`Busy`](PamState::Busy). The answer is never logged, and its bytes
  /// are zeroed once handed on, but the copy made for libpam is freed
  /// unzeroed. With no question waiting, it is dropped, and that is
  /// logged.
  pub fn respond(&self, text: impl Into<String>) {
    self.inner.respond(Secret(text.into().into_bytes()));
  }

  /// Stops the attempt running: the state turns [`Idle`](PamState::Idle)
  /// at once, its question is refused and its result is never shown.
  /// Its thread ends at PAM's next question, or once a module that
  /// blocks without asking returns. A start waiting for that is dropped.
  /// With nothing running, nothing changes. A refused question counts as
  /// a failed login, as [`start`](Self::start) says.
  pub fn abort(&self) {
    self.inner.abort();
  }

  /// Where the attempt is.
  pub fn state(&self) -> ReadSignal<PamState> {
    self.inner.state.read_only()
  }

  /// The info and error lines PAM sent during the attempt, oldest first.
  /// Each start empties it.
  pub fn messages(&self) -> ReadSignal<Vec<PamMessage>> {
    self.inner.messages.read_only()
  }
}

impl Inner {
  /// Whether the asking context is still there. Untracked, so an effect
  /// that starts an attempt doesn't run again for it.
  fn alive(&self) -> bool {
    untrack(|| self.state.try_with(|_| ()).is_some())
  }

  fn start(self: &Rc<Self>, user: Option<String>) {
    let Some(shared) = &self.shared else {
      return;
    };
    let Some(event_loop) = shared.event_loop() else {
      return;
    };
    if !self.alive() {
      return;
    }
    if self.abandoned.borrow().is_some() {
      *self.waiting.borrow_mut() = Some(user);
      set(self.messages, Vec::new());
      return set(self.state, PamState::Busy);
    }
    let (events, channel) = calloop::channel::channel();
    let (answers, inbox) = mpsc::channel();
    let inner = Rc::clone(self);
    let number = self.next.replace(self.next.get() + 1);
    let arrived = Box::new(move |event| inner.arrived(number, event));
    let Some(source) = event_loop.insert_pam_channel(channel, arrived) else {
      return;
    };
    let backend = Arc::clone(&shared.config.pam);
    let service = self.service.clone();
    let talk = Talk {
      events,
      answers: inbox,
    };
    let spawned = std::thread::Builder::new()
      .name(String::from("crabshell-pam"))
      .spawn(move || {
        let result = match user.map_or_else(own_user, Ok) {
          Ok(user) => backend.authenticate(&service, &user, &talk),
          Err(error) => PamResult::error(PamError::StartFailed, error),
        };
        // Refused when the attempt was aborted meanwhile.
        let _ = talk.events.send(Event::Done(result));
      });
    if let Err(error) = spawned {
      // Its channel closed with it, which ends the attempt.
      tracing::error!(%error, "PAM couldn't start a thread");
    }
    *self.attempt.borrow_mut() = Some(Attempt {
      number,
      source,
      answers,
      asking: false,
    });
    set(self.messages, Vec::new());
    set(self.state, PamState::Busy);
  }

  fn arrived(self: &Rc<Self>, number: u64, event: Arrived<Event>) {
    let live = self.attempt.borrow().as_ref().map(|attempt| attempt.number);
    if live != Some(number) {
      // An aborted attempt's: only its end matters.
      if let Arrived::Closed = event {
        self.returned();
      }
      return;
    }
    match event {
      Arrived::Msg(Event::Ask { message, visible }) => {
        if let Some(attempt) = self.attempt.borrow_mut().as_mut() {
          attempt.asking = true;
        }
        set(self.state, PamState::Asking { message, visible });
      }
      Arrived::Msg(Event::Tell(message)) => {
        self.messages.try_update(|messages| {
          if messages.len() == MAX_MESSAGES {
            messages.remove(0);
          }
          messages.push(message);
        });
      }
      Arrived::Msg(Event::Done(result)) => self.finish(result),
      // The thread ended before its result, as when it panicked.
      Arrived::Closed => self.finish(PamResult::error(
        PamError::InternalError,
        "the attempt ended without a result",
      )),
    }
  }

  fn respond(&self, secret: Secret) {
    {
      let mut attempt = self.attempt.borrow_mut();
      let Some(attempt) = attempt.as_mut().filter(|attempt| attempt.asking) else {
        // `secret` is zeroed as it drops.
        return tracing::warn!("PAM asked no question, so a response was dropped");
      };
      attempt.asking = false;
      // Fails only once the thread is gone, and drops `secret`, zeroed.
      let _ = attempt.answers.send(secret);
    }
    set(self.state, PamState::Busy);
  }

  fn finish(&self, result: PamResult) {
    if self.end() {
      set(self.state, PamState::Done(result));
    }
  }

  /// Abandons the attempt running: dropping its sender refuses the
  /// question its thread waits on, and its source stays only to hear that
  /// thread return. A start waiting is dropped.
  fn abort(&self) {
    let waited = self.waiting.take().is_some();
    let attempt = self.attempt.borrow_mut().take();
    if let Some(attempt) = &attempt {
      *self.abandoned.borrow_mut() = Some(attempt.source);
    }
    if waited || attempt.is_some() {
      set(self.state, PamState::Idle);
    }
  }

  /// An aborted attempt's thread returned: its source leaves the loop, and
  /// a start that waited for it begins.
  fn returned(self: &Rc<Self>) {
    if let Some(token) = self.abandoned.take() {
      self.remove(token);
    }
    let waiting = self.waiting.take();
    if let Some(user) = waiting {
      self.start(user);
    }
  }

  /// Ends the attempt running, if one is: its source leaves the loop, and
  /// dropping its sender refuses the question its thread waits on. Whether
  /// one ran.
  fn end(&self) -> bool {
    let Some(attempt) = self.attempt.borrow_mut().take() else {
      return false;
    };
    self.remove(attempt.source);
    true
  }

  fn remove(&self, token: RegistrationToken) {
    if let Some(event_loop) = self.shared.as_ref().and_then(|shared| shared.event_loop()) {
      event_loop.remove(token);
    }
  }
}

/// The name of the user the shell runs as, found by its uid through NSS,
/// since `$USER` is whatever the shell's parent set.
fn own_user() -> Result<String, String> {
  let uid = rustix::process::getuid().as_raw();
  crate::user_name(uid).ok_or_else(|| format!("no user with uid {uid}"))
}

/// The system's libpam.
pub(crate) struct LibPam;

impl Backend for LibPam {
  fn authenticate(&self, service: &str, user: &str, talk: &Talk) -> PamResult {
    // libpam takes C strings, and nonstick panics on a NUL.
    if service.contains('\0') || user.contains('\0') {
      return PamResult::error(PamError::StartFailed, "a service or user name with a NUL");
    }
    let built = TransactionBuilder::new_with_service(service)
      .username(user)
      .build(Adapter(talk).into_conversation());
    let mut transaction = match built {
      Ok(transaction) => transaction,
      Err(error) => return PamResult::error(PamError::StartFailed, error.to_string()),
    };
    match transaction.authenticate(AuthnFlags::empty()) {
      Ok(()) => {
        // The account step is where `pam_faillock` forgets past failures.
        // Its refusal, as for an expired password, is only logged.
        if let Err(error) = transaction.account_management(AuthnFlags::empty()) {
          tracing::warn!(%error, "PAM's account check refused after authenticating");
        }
        PamResult::Success
      }
      Err(error) => tried(error),
    }
  }
}

/// What `pam_authenticate` failing with `error` means.
fn tried(error: ErrorCode) -> PamResult {
  let kind = match error {
    ErrorCode::AuthenticationError => return PamResult::Failure,
    // Only the shell's conversation fails so: it refused an answer.
    ErrorCode::ConversationError => PamError::InternalError,
    _ => PamError::TryAuthFailed,
  };
  PamResult::error(kind, error.to_string())
}

/// libpam's questions and messages, through an attempt's [`Talk`]. It runs
/// inside libpam's callback, where a panic aborts the shell, so nothing in
/// it panics.
struct Adapter<'a>(&'a Talk);

impl Adapter<'_> {
  /// The config's answer to `question`, copied for libpam. Ours is zeroed
  /// as it drops.
  fn answer(&self, question: &OsStr, visible: bool) -> nonstick::Result<OsString> {
    let secret = self
      .0
      .ask(question, visible)
      .ok_or(ErrorCode::ConversationError)?;
    // nonstick panics on a NUL, and libpam would read only up to it.
    if secret.bytes().contains(&0) {
      tracing::warn!("PAM was refused a response with a NUL in it");
      return Err(ErrorCode::ConversationError);
    }
    // shortcut: nonstick frees this copy without zeroing it; zero it too if
    // nonstick ever takes answers in a buffer it wipes.
    Ok(OsStr::from_bytes(secret.bytes()).to_owned())
  }
}

impl ConversationAdapter for Adapter<'_> {
  fn prompt(&self, request: impl AsRef<OsStr>) -> nonstick::Result<OsString> {
    self.answer(request.as_ref(), true)
  }

  fn masked_prompt(&self, request: impl AsRef<OsStr>) -> nonstick::Result<OsString> {
    self.answer(request.as_ref(), false)
  }

  fn error_msg(&self, message: impl AsRef<OsStr>) {
    self.0.tell(PamMessage::Error, message.as_ref());
  }

  fn info_msg(&self, message: impl AsRef<OsStr>) {
    self.0.tell(PamMessage::Info, message.as_ref());
  }
}

/// Sets `signal` to `value` unless it holds it already, so nothing wakes
/// for a value that didn't change.
fn set<T: PartialEq + 'static>(signal: Signal<T>, value: T) {
  if untrack(|| signal.try_with(|old| *old != value)) == Some(true) {
    signal.try_set(value);
  }
}
