//! HTTP GET through `curl`, as the configs Quickshell's users write fetch
//! the weather, and remote cover art fetched into a cache file.

#[cfg(test)]
mod tests;

pub(crate) mod art;

use std::cell::RefCell;
use std::fs::File;
use std::io::{ErrorKind, PipeReader, Read, Write};
use std::os::fd::AsFd;
use std::panic::{self, AssertUnwindSafe, Location};
use std::process::{Child, Command, ExitStatus};
use std::rc::{Rc, Weak};
use std::sync::Once;
use std::time::Duration;

use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{Memo, ReadSignal, Signal, untrack};
use crabshell_ui::{Cx, IntoProp};
use rustix::process::{Pid, PidfdFlags, Signal as Kill, kill_process_group, pidfd_open};

use crate::lines::panic_message;
use crate::{Shared, spawn};

/// The largest body [`Http`] keeps unless [`Http::max_size`] says
/// otherwise.
pub(crate) const MAX_BODY: u64 = 4 * 1024 * 1024;

/// How long a whole request may take unless [`Http::timeout`] says
/// otherwise, and how long a cover art download may.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

/// The most of curl's standard error read, for its message.
const MAX_STDERR: usize = 4096;

/// What `cx.http_get` and `cx.cover_art` need from the shell: the loop,
/// where curl is, and where art is cached.
pub(crate) struct Service(pub(crate) Rc<Shared>);

/// Why a request failed, from [`Http::error`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpError {
  /// What kind of failure it is.
  pub kind: HttpErrorKind,
  /// What curl or the shell said, such as `The requested URL returned
  /// error: 404`.
  pub message: String,
}

/// The kinds of [`HttpError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpErrorKind {
  /// `curl` isn't installed, or couldn't start.
  NoCurl,
  /// The server answered with this HTTP status, 400 or over.
  Status(u16),
  /// The body was over [`Http::max_size`].
  TooLarge,
  /// The request took longer than [`Http::timeout`].
  TimedOut,
  /// The body isn't UTF-8.
  NotUtf8,
  /// Anything on the way, such as a name that didn't resolve or a refused
  /// connection.
  Network,
  /// The URL, or a redirect, isn't `http:` or `https:`.
  Refused,
}

impl HttpError {
  fn new(kind: HttpErrorKind, message: impl Into<String>) -> HttpError {
    let message = message.into();
    HttpError { kind, message }
  }
}

/// Whether `url` is one curl may fetch.
fn allowed(url: &str) -> bool {
  let scheme = url.split_once("://").map(|(scheme, _)| scheme);
  scheme.is_some_and(|scheme| {
    scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
  })
}

/// Where a running request puts its body.
pub(crate) enum Sink {
  Memory(Vec<u8>),
  File(File),
}

type Done = Box<dyn FnOnce(Result<Sink, HttpError>)>;

/// One curl run. Dropping it stops curl if it still runs, without calling
/// its `done`.
pub(crate) struct Fetch(Rc<RefCell<Run>>);

struct Run {
  shared: Rc<Shared>,
  /// `None` once it ended or was stopped.
  child: Option<Child>,
  out: Option<(PipeReader, RegistrationToken)>,
  err: PipeReader,
  exit: Option<RegistrationToken>,
  sink: Option<Sink>,
  got: u64,
  cap: u64,
  done: Option<Done>,
}

/// Runs curl for `url`, an `http:` or `https:` URL, with its body going to
/// `sink`, up to `cap` bytes, for at most `timeout`. `done` gets the sink
/// back once the body arrived whole, or why it didn't. The URL is an
/// argument, never seen by a shell, and curl reads no `~/.curlrc`.
pub(crate) fn fetch(
  shared: &Rc<Shared>,
  url: &str,
  cap: u64,
  timeout: Duration,
  sink: Sink,
  done: impl FnOnce(Result<Sink, HttpError>) + 'static,
) -> Result<Fetch, HttpError> {
  if !allowed(url) {
    return Err(HttpError::new(
      HttpErrorKind::Refused,
      "only http: and https: URLs are fetched",
    ));
  }
  let Some(event_loop) = shared.event_loop() else {
    return Err(HttpError::new(
      HttpErrorKind::Network,
      "the shell is stopping",
    ));
  };
  let mut command = Command::new(&shared.config.curl);
  // `-q` must come first to skip `~/.curlrc`. `--globoff` keeps `[]` and
  // `{}` in a URL from naming many.
  command.args([
    "-q",
    "--silent",
    "--show-error",
    "--globoff",
    "--fail",
    "--location",
  ]);
  command.args([
    "--max-redirs",
    "5",
    "--proto",
    "=http,https",
    "--proto-redir",
    "=http,https",
  ]);
  // curl reads 0 as no limit, and refuses a number it can't hold.
  let seconds = timeout.as_secs_f64().clamp(0.001, f64::from(u32::MAX));
  command.arg("--max-time").arg(format!("{seconds:.3}"));
  // Past that, the bytes counted here are the only cap.
  if i64::try_from(cap).is_ok() {
    command.arg("--max-filesize").arg(cap.to_string());
  }
  // The status goes last on standard error, after curl's message.
  command.args(["--write-out", "%{stderr}%{response_code}", "--url", url]);
  let spawned = crate::process::spawn_piped(&mut command, false);
  drop(command);
  let (child, out, err, _) = spawned.map_err(|error| {
    let kind = match error.kind() {
      ErrorKind::NotFound | ErrorKind::PermissionDenied => HttpErrorKind::NoCurl,
      _ => HttpErrorKind::Network,
    };
    HttpError::new(kind, format!("couldn't start curl: {error}"))
  })?;
  let pidfd = pidfd_open(Pid::from_child(&child), PidfdFlags::NONBLOCK);
  let run = Rc::new(RefCell::new(Run {
    shared: Rc::clone(shared),
    child: Some(child),
    out: None,
    err,
    exit: None,
    sink: Some(sink),
    got: 0,
    cap,
    done: Some(Box::new(done)),
  }));
  let fetch = Fetch(Rc::clone(&run));
  let fd = out.as_fd().try_clone_to_owned();
  let reader = Rc::clone(&run);
  let token = fd
    .ok()
    .and_then(|fd| event_loop.insert_fd(fd, Interest::READ, Box::new(move || on_output(&reader))));
  let ended = Rc::clone(&run);
  let exit = pidfd
    .ok()
    .and_then(|fd| event_loop.insert_fd(fd, Interest::READ, Box::new(move || on_exit(&ended))));
  let mut state = run.borrow_mut();
  state.out = token.map(|token| (out, token));
  state.exit = exit;
  let followed = state.out.is_some() && state.exit.is_some();
  drop(state);
  if !followed {
    // Logged by the loop; dropping `fetch` stops curl.
    return Err(HttpError::new(
      HttpErrorKind::Network,
      "couldn't follow curl",
    ));
  }
  Ok(fetch)
}

/// Reads what curl printed, and stops it once that is over the cap.
fn on_output(run: &Rc<RefCell<Run>>) {
  let mut state = run.borrow_mut();
  let error = match state.read_out() {
    Chunk::More | Chunk::Wait => return,
    Chunk::End => {
      // The end of the body; its exit is still to come.
      if let Some((_, token)) = state.out.take() {
        state.remove(token);
      }
      return;
    }
    Chunk::Failed(error) => error,
  };
  drop(state);
  finish(run, Err(error));
}

/// curl ended: what is left of its output, then how it went.
fn on_exit(run: &Rc<RefCell<Run>>) {
  let mut state = run.borrow_mut();
  let status = match state.child.as_mut().map(Child::try_wait) {
    Some(Ok(Some(status))) => Some(status),
    Some(Ok(None)) | None => return,
    Some(Err(_)) => None,
  };
  state.child = None;
  // What it printed before it ended is in the pipe now, up to the cap.
  let mut failed = None;
  while failed.is_none() {
    match state.read_out() {
      Chunk::More => {}
      Chunk::Wait | Chunk::End => break,
      Chunk::Failed(error) => failed = Some(error),
    }
  }
  let mut stderr = Vec::new();
  // Non-blocking: what curl wrote before it ended.
  let _ = (&state.err)
    .take(MAX_STDERR as u64)
    .read_to_end(&mut stderr);
  let sink = state.sink.take();
  drop(state);
  let result = match failed {
    Some(error) => Err(error),
    None => outcome(status, &String::from_utf8_lossy(&stderr)),
  };
  let result = result
    .and_then(|()| sink.ok_or_else(|| HttpError::new(HttpErrorKind::Network, "the body was lost")));
  finish(run, result);
}

/// What one read of curl's output gave.
enum Chunk {
  More,
  /// Nothing to read now.
  Wait,
  End,
  Failed(HttpError),
}

impl Run {
  /// Reads one chunk of the body into the sink.
  fn read_out(&mut self) -> Chunk {
    let mut chunk = [0; 64 * 1024];
    let Some((out, _)) = self.out.as_mut() else {
      return Chunk::End;
    };
    let read = match out.read(&mut chunk) {
      Ok(read) => read,
      Err(error) if error.kind() == ErrorKind::Interrupted => return Chunk::More,
      Err(error) if error.kind() == ErrorKind::WouldBlock => return Chunk::Wait,
      Err(_) => 0,
    };
    if read == 0 {
      return Chunk::End;
    }
    self.got += read as u64;
    if self.got > self.cap {
      let cap = self.cap;
      let message = format!("the body is over {cap} bytes");
      return Chunk::Failed(HttpError::new(HttpErrorKind::TooLarge, message));
    }
    match self.sink.as_mut() {
      Some(Sink::Memory(body)) => body.extend_from_slice(&chunk[..read]),
      Some(Sink::File(file)) => {
        if let Err(error) = file.write_all(&chunk[..read]) {
          let message = format!("couldn't write the download: {error}");
          return Chunk::Failed(HttpError::new(HttpErrorKind::Network, message));
        }
      }
      None => {}
    }
    Chunk::More
  }

  fn remove(&self, token: RegistrationToken) {
    if let Some(event_loop) = self.shared.event_loop() {
      event_loop.remove(token);
    }
  }

  /// Stops curl if it runs and takes the run out of the loop.
  fn stop(&mut self) {
    let tokens = [self.exit.take(), self.out.take().map(|(_, token)| token)];
    for token in tokens.into_iter().flatten() {
      self.remove(token);
    }
    if let Some(child) = self.child.take() {
      // Its own group, from `process_group(0)`.
      let _ = kill_process_group(Pid::from_child(&child), Kill::TERM);
      spawn::reap(&self.shared, child, String::from("curl"));
    }
  }
}

/// Ends the run and calls its `done` with nothing borrowed.
fn finish(run: &Rc<RefCell<Run>>, result: Result<Sink, HttpError>) {
  let done = {
    let mut state = run.borrow_mut();
    state.stop();
    state.done.take()
  };
  if let Some(done) = done {
    done(result);
  }
}

impl Drop for Fetch {
  fn drop(&mut self) {
    let done = {
      let mut state = self.0.borrow_mut();
      state.stop();
      state.done.take()
    };
    // Dropped outside the borrow: it may hold what holds this fetch.
    drop(done);
  }
}

/// How curl said it went, from its exit and its standard error, whose
/// last line is the HTTP status.
fn outcome(status: Option<ExitStatus>, stderr: &str) -> Result<(), HttpError> {
  let (said, code) = stderr.rsplit_once('\n').unwrap_or(("", stderr));
  // `curl: (22) The requested URL returned error: 404`
  let said = said.trim();
  let said = said
    .strip_prefix("curl: (")
    .and_then(|rest| rest.split_once(") "))
    .map_or(said, |(_, message)| message);
  let exit = status.and_then(|status| status.code());
  let message = match exit {
    _ if !said.is_empty() => said.to_owned(),
    Some(exit) => format!("curl exited with {exit}"),
    None => String::from("curl was stopped"),
  };
  let kind = match exit {
    Some(0) => return Ok(()),
    Some(22) => HttpErrorKind::Status(code.trim().parse().unwrap_or(0)),
    Some(28) => HttpErrorKind::TimedOut,
    Some(63) => HttpErrorKind::TooLarge,
    // An unsupported protocol, which only a redirect can reach.
    Some(1) => HttpErrorKind::Refused,
    _ => HttpErrorKind::Network,
  };
  Err(HttpError::new(kind, message))
}

/// A GET request, from `cx.http_get(url)`. It is a `Copy` handle, like a
/// signal, and goes with the context that asked.
#[derive(Clone, Copy)]
pub struct Http {
  body: Signal<Option<String>>,
  error: Signal<Option<HttpError>>,
  loading: Signal<bool>,
  /// Read by the effect that starts a request, so bumping it starts one.
  refreshes: Signal<u64>,
  inner: Signal<Keep>,
}

/// Stops the request when the asking context's owner drops it.
struct Keep(Rc<Inner>);

impl Drop for Keep {
  fn drop(&mut self) {
    let fetch = self.0.state.borrow_mut().fetch.take();
    drop(fetch);
  }
}

struct Inner {
  /// `None` when the shell provided no services.
  shared: Option<Rc<Shared>>,
  body: Signal<Option<String>>,
  error: Signal<Option<HttpError>>,
  loading: Signal<bool>,
  state: RefCell<State>,
}

struct State {
  fetch: Option<Fetch>,
  stopped: bool,
  max_size: u64,
  timeout: Duration,
}

/// `cx.http_get(url)`.
#[track_caller]
pub(crate) fn http_get(cx: &Cx, url: impl IntoProp<String>) -> Http {
  let at = Location::caller();
  let shared = cx.service::<Service>().map(|service| Rc::clone(&service.0));
  if shared.is_none() {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.http_get() needs the services the shell provides; nothing is fetched");
    });
  }
  let inner = Rc::new(Inner {
    shared,
    body: cx.signal(None),
    error: cx.signal(None),
    loading: cx.signal(false),
    state: RefCell::new(State {
      fetch: None,
      stopped: false,
      max_size: MAX_BODY,
      timeout: TIMEOUT,
    }),
  });
  let refreshes = cx.signal(0);
  let url = chosen(cx, url, at, "cx.http_get");
  let weak = Rc::downgrade(&inner);
  // Built in an effect, so a signal the URL read fetches again.
  cx.effect(move || {
    refreshes.get();
    let Some(inner) = weak.upgrade().filter(|inner| !inner.state.borrow().stopped) else {
      return;
    };
    let url = url.get();
    untrack(|| inner.start(url));
  });
  Http {
    body: inner.body,
    error: inner.error,
    loading: inner.loading,
    refreshes,
    inner: cx.signal(Keep(inner)),
  }
}

/// The URL a config's property gives, in a memo so a closure that reads
/// other signals fetches again only when the URL itself changes. A panic
/// is logged with where `name` was called and gives `None`.
fn chosen(
  cx: &Cx,
  url: impl IntoProp<String>,
  at: &'static Location<'static>,
  name: &'static str,
) -> Memo<Option<String>> {
  let url = url.into_prop().shared();
  cx.memo(move || {
    panic::catch_unwind(AssertUnwindSafe(|| url()))
      .inspect_err(|payload| {
        let message = panic_message(payload.as_ref());
        tracing::error!("{name} (called at {at}) panicked choosing its URL: {message}");
      })
      .ok()
  })
}

impl Http {
  /// The last body that arrived whole, kept through a later fetch that
  /// fails. `None` until one has.
  pub fn body(&self) -> ReadSignal<Option<String>> {
    self.body.read_only()
  }

  /// Why the last fetch failed, or `None` once one worked.
  pub fn error(&self) -> ReadSignal<Option<HttpError>> {
    self.error.read_only()
  }

  /// `true` while a fetch runs.
  pub fn loading(&self) -> ReadSignal<bool> {
    self.loading.read_only()
  }

  /// Fetches again now, stopping a fetch still running. A
  /// [`stopped`](Self::stopped) request starts.
  pub fn refresh(&self) {
    if let Some(inner) = self.inner() {
      inner.state.borrow_mut().stopped = false;
    }
    self.refreshes.try_update(|count| *count += 1);
  }

  /// Fetches nothing until the first [`refresh`](Self::refresh), for a
  /// request made on demand. Call it right after `cx.http_get`.
  pub fn stopped(self) -> Http {
    if let Some(inner) = self.inner() {
      inner.state.borrow_mut().stopped = true;
      inner.stop();
    }
    self
  }

  /// The largest body taken, 4 MiB by default; a larger one stops the
  /// fetch and is an error. Applies from the next fetch, so call it right
  /// after `cx.http_get`.
  pub fn max_size(self, bytes: u64) -> Http {
    if let Some(inner) = self.inner() {
      inner.state.borrow_mut().max_size = bytes;
    }
    self
  }

  /// How long the whole request may take, 30 s by default. Applies from
  /// the next fetch, so call it right after `cx.http_get`.
  pub fn timeout(self, timeout: Duration) -> Http {
    if let Some(inner) = self.inner() {
      inner.state.borrow_mut().timeout = timeout;
    }
    self
  }

  fn inner(&self) -> Option<Rc<Inner>> {
    untrack(|| self.inner.try_with(|keep| Rc::clone(&keep.0)))
  }
}

impl Inner {
  /// Stops the fetch running now, if one is, and starts one for `url`.
  /// Nothing, or an empty URL, fetches nothing.
  fn start(self: &Rc<Self>, url: Option<String>) {
    let running = self.state.borrow_mut().fetch.take();
    drop(running);
    let (Some(url), Some(shared)) = (url.filter(|url| !url.is_empty()), self.shared.clone()) else {
      set(self.loading, false);
      return;
    };
    let (cap, timeout) = {
      let state = self.state.borrow();
      (state.max_size, state.timeout)
    };
    let weak = Rc::downgrade(self);
    let done = move |result| {
      if let Some(inner) = Weak::upgrade(&weak) {
        inner.done(result);
      }
    };
    match fetch(&shared, &url, cap, timeout, Sink::Memory(Vec::new()), done) {
      Ok(fetch) => {
        self.state.borrow_mut().fetch = Some(fetch);
        set(self.loading, true);
      }
      Err(error) => self.failed(error),
    }
  }

  /// Stops the fetch running now, if one is.
  fn stop(&self) {
    let fetch = self.state.borrow_mut().fetch.take();
    drop(fetch);
    set(self.loading, false);
  }

  fn done(&self, result: Result<Sink, HttpError>) {
    let fetch = self.state.borrow_mut().fetch.take();
    drop(fetch);
    let text = result.and_then(|sink| match sink {
      Sink::Memory(body) => String::from_utf8(body)
        .map_err(|_| HttpError::new(HttpErrorKind::NotUtf8, "the body isn't UTF-8")),
      Sink::File(_) => Err(HttpError::new(HttpErrorKind::Network, "no body")),
    });
    match text {
      Ok(text) => {
        set(self.body, Some(text));
        set(self.error, None);
        set(self.loading, false);
      }
      Err(error) => self.failed(error),
    }
  }

  /// Shows `error`, logged once while the same one stays.
  fn failed(&self, error: HttpError) {
    if untrack(|| self.error.try_with(|old| old.as_ref() != Some(&error))) == Some(true) {
      tracing::info!(error = error.message, "a request failed");
    }
    set(self.error, Some(error));
    set(self.loading, false);
  }
}

/// Sets `signal` unless it holds `value` or is gone, reading it untracked.
fn set<T: PartialEq + 'static>(signal: Signal<T>, value: T) {
  if untrack(|| signal.try_with(|old| *old != value)) == Some(true) {
    signal.try_set(value);
  }
}
