use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use zbus::{Connection, Message, OwnedVariant, Variant};

use super::{
  AGENT_INTERFACE, AGENT_PATH, AUTHORITY, AuthRequest, AuthState, CANCELLED, FAILED, PolkitAgent,
  subject, user_name,
};
use crate::testing::{
  FakeService, Harness, PrivateBus, TempDir, capture_logs, capture_logs_from, offline, reply,
};
use crate::{Config, ServicesExt};

const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

/// PAM as the tests have it: one line of info, a password question, and
/// `hunter2` is right.
const PASSWORD: &str = "echo 'PAM_TEXT_INFO Use your password'
echo 'PAM_PROMPT_ECHO_OFF Password: '
read -r answer
if [ \"$answer\" = hunter2 ]; then echo SUCCESS; else echo 'PAM_ERROR_MSG Sorry, try again.'; echo FAILURE; fi
";

/// What a registration sent: the agent's unique name, the subject's kind
/// and details, the locale and the object path.
struct Registration {
  agent: String,
  kind: String,
  details: HashMap<String, OwnedVariant>,
  path: String,
}

type Registrations = Rc<RefCell<Vec<Registration>>>;

/// Where an agent's answer to a `BeginAuthentication` lands: `Ok` for an
/// empty return, else the error's name.
type Answer = Rc<RefCell<Option<Result<(), String>>>>;

/// A shell on a private system bus with a fake polkit, and a fake helper
/// that plays `conversation` after reading the cookie.
struct Polkit {
  shell: Harness,
  bus: PrivateBus,
  authority: FakeService,
  registrations: Registrations,
  dir: TempDir,
}

impl Polkit {
  fn start(conversation: &str) -> Polkit {
    Polkit::with(conversation, |config| config)
  }

  fn with(conversation: &str, config: impl FnOnce(Config) -> Config) -> Polkit {
    let bus = PrivateBus::start();
    let dir = TempDir::new();
    let helper = dir.path().join("helper");
    // It records its user and cookie a line a run, and its pid.
    let script = format!(
      "#!/bin/sh\nread -r cookie\necho \"$1 $cookie\" >> \"$0.runs\"\necho $$ > \"$0.pid\"\n{conversation}"
    );
    std::fs::write(&helper, script).expect("the fake helper");
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&helper, mode).expect("its mode");
    let mut shell = Harness::new(config(Config {
      system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
      polkit_helper: helper,
      ..offline()
    }));
    let (authority, registrations) = authority(&mut shell, &bus, true);
    Polkit {
      shell,
      bus,
      authority,
      registrations,
      dir,
    }
  }

  /// `cx.polkit_agent()`, once polkit took it, and its unique name.
  fn agent(&mut self) -> (PolkitAgent, String) {
    let agent = self.shell.cx.polkit_agent();
    let service = Rc::clone(agent.service.as_ref().expect("the shell's agent"));
    let registered = move || {
      let state = service.state.borrow();
      state
        .as_ref()
        .is_some_and(|state| state.authority.is_some())
    };
    self.wait("the registration", registered);
    let name = self.registrations.borrow()[0].agent.clone();
    (agent, name)
  }

  fn wait(&mut self, what: &str, done: impl Fn() -> bool) {
    self.shell.retry_until(what, done, |_| {});
  }

  /// Calls `BeginAuthentication` on `agent` from `from`, for `cookie`
  /// and the users `uids`.
  fn begin_from(&mut self, from: &Connection, agent: &str, cookie: &str, uids: &[u32]) -> Answer {
    let (from, agent, cookie) = (from.clone(), agent.to_owned(), cookie.to_owned());
    let identities: Vec<(&str, HashMap<&str, Variant<'static>>)> = uids
      .iter()
      .map(|uid| ("unix-user", HashMap::from([("uid", Variant::from(*uid))])))
      .collect();
    let answer = Answer::default();
    let sink = Rc::clone(&answer);
    self.shell.spawn(async move {
      let details = HashMap::from([("polkit.subject-pid", "1")]);
      let action = format!("org.example.{cookie}");
      let body = (
        action.as_str(),
        "Authentication is needed to reboot",
        "system-reboot",
        details,
        cookie.as_str(),
        identities,
      );
      let call = from.call_method(
        Some(agent.as_str()),
        AGENT_PATH,
        Some(AGENT_INTERFACE),
        "BeginAuthentication",
        &body,
      );
      *sink.borrow_mut() = Some(call.await.map(drop).map_err(error_name));
    });
    answer
  }

  /// `BeginAuthentication` from polkit.
  fn begin(&mut self, agent: &str, cookie: &str, uids: &[u32]) -> Answer {
    let from = self.authority.connection.clone();
    self.begin_from(&from, agent, cookie, uids)
  }

  /// `CancelAuthentication` from `from`.
  fn cancel_from(&mut self, from: &Connection, agent: &str, cookie: &str) -> Result<(), String> {
    let (from, agent, cookie) = (from.clone(), agent.to_owned(), cookie.to_owned());
    self.shell.block_on(async move {
      let call = from.call_method(
        Some(agent.as_str()),
        AGENT_PATH,
        Some(AGENT_INTERFACE),
        "CancelAuthentication",
        &cookie,
      );
      call.await.map(drop).map_err(error_name)
    })
  }

  /// polkit's answer, once the agent gave it.
  fn answered(&mut self, answer: &Answer) -> Result<(), String> {
    let done = Rc::clone(answer);
    self.wait("the agent's answer", move || done.borrow().is_some());
    answer.borrow_mut().take().expect("an answer")
  }

  /// Waits for the request to be in a state like `like`.
  fn wait_state(&mut self, agent: &PolkitAgent, like: fn(&AuthState) -> bool) {
    let request = agent.request();
    let shown = move || {
      request
        .get_untracked()
        .is_some_and(|shown| like(&shown.state))
    };
    self.wait("the request's state", shown);
  }

  /// The user and cookie of each run of the helper.
  fn runs(&self) -> Vec<String> {
    let runs = std::fs::read_to_string(self.dir.path().join("helper.runs")).unwrap_or_default();
    runs.lines().map(str::to_owned).collect()
  }

  /// Waits for the last helper started to be gone and reaped.
  fn wait_helper_gone(&mut self) {
    let pid = std::fs::read_to_string(self.dir.path().join("helper.pid")).expect("a helper ran");
    let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
    self.wait("the helper gone", move || !proc.exists());
  }
}

/// A fake polkit on `bus` that records each registration and takes it,
/// or refuses it unless `take`.
fn authority(shell: &mut Harness, bus: &PrivateBus, take: bool) -> (FakeService, Registrations) {
  let authority = FakeService::start(shell, bus, AUTHORITY);
  let registrations = Registrations::default();
  let sink = Rc::clone(&registrations);
  authority.on("RegisterAuthenticationAgent", move |call| {
    type Body = ((String, HashMap<String, OwnedVariant>), String, String);
    let ((kind, details), _locale, path) = call.body().deserialize::<Body>().ok()?;
    let agent = call.header().sender()?.to_string();
    sink.borrow_mut().push(Registration {
      agent,
      kind,
      details,
      path,
    });
    if !take {
      let refusal = Message::error(&call.header(), FAILED);
      return Some(refusal.build(&"no agents today"));
    }
    reply(call, &())
  });
  authority.own(shell);
  (authority, registrations)
}

fn error_name(error: zbus::Error) -> String {
  use zbus::DBusError;

  match error {
    zbus::Error::MethodError(name, _, _) => name.to_string(),
    // zbus reads the bus's own errors, AccessDenied among them, as these.
    zbus::Error::FDO(error) => error.name().to_string(),
    other => other.to_string(),
  }
}

fn state(request: Option<AuthRequest>) -> Option<AuthState> {
  request.map(|shown| shown.state)
}

fn asking(state: &AuthState) -> bool {
  matches!(state, AuthState::Asking { .. })
}

fn failed(state: &AuthState) -> bool {
  matches!(state, AuthState::Failed { .. })
}

#[test]
fn the_agent_registers_on_the_first_call_only() {
  let mut polkit = Polkit::start(PASSWORD);
  polkit
    .shell
    .run_until(Instant::now() + Duration::from_millis(300));
  assert!(polkit.registrations.borrow().is_empty(), "not before");

  let (agent, _) = polkit.agent();
  let again = polkit.shell.cx.child().polkit_agent();
  assert_eq!(again.request(), agent.request());
  polkit
    .shell
    .run_until(Instant::now() + Duration::from_millis(300));
  let registrations = polkit.registrations.borrow();
  assert_eq!(registrations.len(), 1);
  let registration = &registrations[0];
  assert_eq!(registration.path, AGENT_PATH);
  let session = std::env::var("XDG_SESSION_ID")
    .ok()
    .filter(|id| !id.is_empty());
  let kind = if session.is_some() {
    "unix-session"
  } else {
    "unix-process"
  };
  assert_eq!(registration.kind, kind);
  assert!(!registration.details.is_empty());

  // The subject without a session: this process, as polkit names one.
  let (kind, details) = subject(None);
  assert_eq!(kind, "unix-process");
  assert_eq!(details["pid"], Variant::from(std::process::id()));
  let Variant::U64(start) = details["start-time"] else {
    panic!("a start time");
  };
  assert!(start > 0);
  let (kind, details) = subject(Some("c2".to_owned()));
  assert_eq!(
    (kind, &details["session-id"]),
    ("unix-session", &Variant::from("c2"))
  );
}

#[test]
fn a_refused_registration_is_logged_and_nothing_arrives() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(Config {
    system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  });
  let (_authority, registrations) = authority(&mut shell, &bus, false);
  let logs = capture_logs(|| {
    let agent = shell.cx.polkit_agent();
    shell.retry_until(
      "the registration",
      || !registrations.borrow().is_empty(),
      |_| {},
    );
    shell.run_until(Instant::now() + Duration::from_millis(200));
    assert_eq!(agent.request().get_untracked(), None);
  });
  assert!(logs.contains("polkit refused the agent"), "{logs}");
  let woke = shell.run_until(Instant::now() + Duration::from_secs(1));
  assert_eq!(woke, 1, "a refused agent sleeps");

  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    shell.cx.polkit_agent();
    shell.run_until(Instant::now() + Duration::from_millis(100));
  });
  assert!(logs.contains("no system bus"), "{logs}");
}

#[test]
fn a_password_question_then_success_answers_polkit() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let request = agent.request();
  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  let shown = request.get_untracked().expect("a request");
  assert_eq!(shown.action_id, "org.example.cookie-1");
  assert_eq!(shown.message, "Authentication is needed to reboot");
  assert_eq!(shown.icon_name, "system-reboot");
  assert_eq!(shown.identities, ["root"]);
  let question = AuthState::Asking {
    message: "Password: ".to_owned(),
    visible: false,
  };
  assert_eq!(shown.state, question);
  assert_eq!(shown.info, ["Use your password"]);
  assert_eq!(polkit.runs(), ["root cookie-1"]);

  agent.respond("hunter2");
  assert_eq!(state(request.get_untracked()), Some(AuthState::Busy));
  assert_eq!(polkit.answered(&answer), Ok(()));
  assert_eq!(request.get_untracked(), None);
  polkit.wait_helper_gone();
}

#[test]
fn a_failure_shows_and_the_next_answer_tries_again() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let request = agent.request();
  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  agent.respond("wrong");
  polkit.wait_state(&agent, failed);
  let shown = request.get_untracked().expect("a request");
  let failure = AuthState::Failed {
    message: "Sorry, try again.".to_owned(),
  };
  assert_eq!(shown.state, failure);
  assert_eq!(shown.errors, ["Sorry, try again."]);
  assert_eq!(*answer.borrow(), None, "polkit still waits");

  agent.respond("hunter2");
  assert_eq!(polkit.answered(&answer), Ok(()));
  assert_eq!(polkit.runs(), ["root cookie-1", "root cookie-1"]);
}

#[test]
fn three_failures_answer_polkit_failed() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let request = agent.request();
  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  for _ in 0..2 {
    agent.respond("wrong");
    polkit.wait_state(&agent, failed);
  }
  agent.respond("wrong");
  assert_eq!(polkit.answered(&answer), Err(FAILED.to_owned()));
  assert_eq!(request.get_untracked(), None);
  assert_eq!(polkit.runs().len(), 3);
}

#[test]
fn cancel_from_the_config_answers_cancelled_and_stops_the_helper() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  agent.cancel();
  assert_eq!(agent.request().get_untracked(), None);
  assert_eq!(polkit.answered(&answer), Err(CANCELLED.to_owned()));
  polkit.wait_helper_gone();
  // Nothing is left to answer.
  agent.respond("hunter2");
  agent.cancel();
}

#[test]
fn polkit_cancelling_stops_the_helper_and_clears_the_request() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  let from = polkit.authority.connection.clone();
  assert_eq!(polkit.cancel_from(&from, &name, "another"), Ok(()));
  assert!(agent.request().get_untracked().is_some(), "not this one");

  assert_eq!(polkit.cancel_from(&from, &name, "cookie-1"), Ok(()));
  assert_eq!(polkit.answered(&answer), Err(CANCELLED.to_owned()));
  assert_eq!(agent.request().get_untracked(), None);
  polkit.wait_helper_gone();
}

#[test]
fn only_polkit_may_call_the_agent() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let intruder = polkit.shell.block_on(polkit.bus.connect(&[]));
  let logs = capture_logs(|| {
    let answer = polkit.begin_from(&intruder, &name, "cookie-1", &[0]);
    assert_eq!(polkit.answered(&answer), Err(ACCESS_DENIED.to_owned()));
    assert_eq!(agent.request().get_untracked(), None);
    assert!(polkit.runs().is_empty(), "no helper ran");

    let answer = polkit.begin(&name, "cookie-2", &[0]);
    polkit.wait_state(&agent, asking);
    let cancel = polkit.cancel_from(&intruder, &name, "cookie-2");
    assert_eq!(cancel, Err(ACCESS_DENIED.to_owned()));
    assert!(agent.request().get_untracked().is_some());
    agent.respond("hunter2");
    assert_eq!(polkit.answered(&answer), Ok(()));
  });
  assert!(logs.contains("other than polkit"), "{logs}");
}

#[test]
fn requests_wait_their_turn_and_past_eight_are_refused() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let service = Rc::clone(agent.service.as_ref().expect("the shell's agent"));
  let mut answers = Vec::new();
  for n in 1..=9 {
    answers.push(polkit.begin(&name, &format!("cookie-{n}"), &[0]));
    let service = Rc::clone(&service);
    // Each in before the next, so they come in order.
    polkit.wait("the request", move || {
      let state = service.state.borrow();
      let state = state.as_ref().expect("started");
      state.current.iter().count() + state.queue.len() == n
    });
  }
  let tenth = polkit.begin(&name, "cookie-10", &[0]);
  assert_eq!(polkit.answered(&tenth), Err(FAILED.to_owned()));
  let action = || agent.request().get_untracked().map(|shown| shown.action_id);
  assert_eq!(action().as_deref(), Some("org.example.cookie-1"));
  assert!(answers.iter().all(|answer| answer.borrow().is_none()));

  let from = polkit.authority.connection.clone();
  assert_eq!(polkit.cancel_from(&from, &name, "cookie-5"), Ok(()));
  assert_eq!(polkit.answered(&answers[4]), Err(CANCELLED.to_owned()));
  assert_eq!(action().as_deref(), Some("org.example.cookie-1"));
  agent.cancel();
  assert_eq!(polkit.answered(&answers[0]), Err(CANCELLED.to_owned()));
  assert_eq!(action().as_deref(), Some("org.example.cookie-2"));
  polkit.wait_state(&agent, asking);
  agent.respond("hunter2");
  assert_eq!(polkit.answered(&answers[1]), Ok(()));
  assert_eq!(action().as_deref(), Some("org.example.cookie-3"));
}

#[test]
fn picking_an_identity_asks_as_that_user() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let ours = rustix::process::getuid().as_raw();
  let passwd = std::fs::read_to_string("/etc/passwd").expect("the users");
  let user = user_name(&passwd, ours).expect("this user's name");
  let answer = polkit.begin(&name, "cookie-1", &[0, ours]);
  polkit.wait_state(&agent, asking);
  let identities = || {
    agent
      .request()
      .get_untracked()
      .expect("a request")
      .identities
  };
  // The session's user first, though polkit listed root first.
  assert_eq!(identities(), [user.as_str(), "root"]);

  let logs = capture_logs(|| agent.pick_identity("nobody-here"));
  assert!(logs.contains("no such identity"), "{logs}");
  agent.pick_identity("root");
  assert_eq!(identities(), ["root", user.as_str()]);
  polkit.wait_state(&agent, asking);
  agent.respond("hunter2");
  assert_eq!(polkit.answered(&answer), Ok(()));
  let first = format!("{user} cookie-1");
  assert_eq!(polkit.runs(), [first.as_str(), "root cookie-1"]);
}

#[test]
fn disposing_the_root_stops_the_helper_and_frees_the_agent() {
  let runtime = Runtime::current();
  let before = runtime.node_count();
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let _answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  polkit.shell.cx.owner().dispose();
  assert_eq!(runtime.node_count(), before);
  polkit.wait_helper_gone();
  let woke = polkit
    .shell
    .run_until(Instant::now() + Duration::from_millis(500));
  assert_eq!(woke, 1);
}

#[test]
fn the_bus_closing_clears_the_request_and_stops_the_helper() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let _answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  let Polkit {
    mut shell,
    bus,
    authority,
    dir,
    ..
  } = polkit;
  drop((authority, bus));
  let request = agent.request();
  shell.retry_until(
    "the request gone",
    || request.get_untracked().is_none(),
    |_| {},
  );
  let pid = std::fs::read_to_string(dir.path().join("helper.pid")).expect("a helper ran");
  let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
  shell.retry_until("the helper gone", || !proc.exists(), |_| {});
}

#[test]
fn a_missing_helper_answers_failed_and_a_crashing_one_shows_failed() {
  let mut polkit = Polkit::with(PASSWORD, |config| Config {
    polkit_helper: PathBuf::from("/nonexistent/crabshell-polkit-helper"),
    ..config
  });
  let (agent, name) = polkit.agent();
  let logs = capture_logs(|| {
    let answer = polkit.begin(&name, "cookie-1", &[0]);
    assert_eq!(polkit.answered(&answer), Err(FAILED.to_owned()));
  });
  assert!(logs.contains("couldn't start polkit's helper"), "{logs}");
  assert_eq!(agent.request().get_untracked(), None);

  let mut polkit =
    Polkit::start("echo 'PAM_PROMPT_ECHO_OFF Password: '\nread -r answer\nkill -KILL $$\n");
  let (agent, name) = polkit.agent();
  let logs = capture_logs(|| {
    let answer = polkit.begin(&name, "cookie-1", &[0]);
    polkit.wait_state(&agent, asking);
    agent.respond("hunter2");
    polkit.wait_state(&agent, failed);
    let failure = AuthState::Failed {
      message: "Authentication failed".to_owned(),
    };
    assert_eq!(state(agent.request().get_untracked()), Some(failure));
    assert_eq!(*answer.borrow(), None, "it may be tried again");
  });
  assert!(logs.contains("ended without an answer"), "{logs}");
}

#[test]
fn user_names_come_from_passwd_and_this_user_from_user() {
  let passwd =
    "root:x:0:0:root:/root:/bin/bash\n# not a user\nbad\nalice:x:1000:1000::/home/alice:/bin/sh\n";
  assert_eq!(user_name(passwd, 0).as_deref(), Some("root"));
  assert_eq!(user_name(passwd, 1000).as_deref(), Some("alice"));
  let ours = rustix::process::getuid().as_raw();
  let user = std::env::var("USER").ok().filter(|user| !user.is_empty());
  assert_eq!(user_name("", ours), user);
  assert_eq!(user_name(passwd, ours.wrapping_add(4242)), None);
}

#[test]
fn a_request_the_agent_cant_answer_is_refused() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let cookie = polkit.begin(&name, "two\nlines", &[0]);
  assert_eq!(polkit.answered(&cookie), Err(FAILED.to_owned()));
  // No user has that uid.
  let nobody = polkit.begin(&name, "cookie-1", &[4_000_000_000]);
  assert_eq!(polkit.answered(&nobody), Err(FAILED.to_owned()));
  assert_eq!(agent.request().get_untracked(), None);
  assert!(polkit.runs().is_empty(), "no helper ran");
}

#[test]
fn answers_never_reach_the_logs() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let logs = capture_logs_from(tracing::Level::TRACE, || {
    let answer = polkit.begin(&name, "cookie-1", &[0]);
    polkit.wait_state(&agent, asking);
    agent.respond("wrong-secret");
    polkit.wait_state(&agent, failed);
    let shown = format!("{:?}", agent.request().get_untracked());
    assert!(!shown.contains("wrong-secret"), "{shown}");
    agent.respond("two\nsecret-lines");
    agent.respond("hunter2");
    assert_eq!(polkit.answered(&answer), Ok(()));
    agent.respond("late-secret");
  });
  for secret in ["wrong-secret", "secret-lines", "hunter2", "late-secret"] {
    assert!(!logs.contains(secret), "{secret} in {logs}");
  }
  assert!(logs.contains("line break or NUL was dropped"), "{logs}");
  assert!(logs.contains("an answer was dropped"), "{logs}");
}

#[test]
fn an_idle_agent_wakes_nothing() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let quiet = |polkit: &mut Polkit, what: &str| {
    let shell = &mut polkit.shell;
    shell.run_until(Instant::now() + Duration::from_millis(200));
    let woke = shell.run_until(Instant::now() + Duration::from_secs(1));
    assert_eq!(woke, 1, "{what}");
  };
  quiet(&mut polkit, "registered, nothing asked");

  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  quiet(&mut polkit, "a question waits for the user");
  agent.respond("hunter2");
  assert_eq!(polkit.answered(&answer), Ok(()));
  polkit.wait_helper_gone();
  quiet(&mut polkit, "after a request");
}

#[test]
fn a_polkit_that_starts_again_gets_the_agent_again() {
  let mut polkit = Polkit::start(PASSWORD);
  let (agent, name) = polkit.agent();
  let _old_request = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  let old = polkit.authority.connection.clone();
  polkit.shell.block_on(async move {
    let bus = zbus::fdo::DBusProxy::new(&old).await.expect("the bus");
    let name = AUTHORITY.try_into().expect("a bus name");
    bus.release_name(name).await.expect("released");
  });
  let (new, registrations) = authority(&mut polkit.shell, &polkit.bus, true);
  let again = Rc::clone(&registrations);
  polkit.wait("the registration with the new polkit", move || {
    again.borrow().len() == 1
  });
  assert_eq!(
    agent.request().get_untracked(),
    None,
    "the old request went"
  );
  polkit.wait_helper_gone();
  let from = new.connection.clone();
  let answer = polkit.begin_from(&from, &name, "cookie-2", &[0]);
  polkit.wait_state(&agent, asking);
  agent.respond("hunter2");
  assert_eq!(polkit.answered(&answer), Ok(()));
}

#[test]
fn a_cancel_kills_a_helper_that_ignores_its_input() {
  let mut polkit = Polkit::start("echo 'PAM_PROMPT_ECHO_OFF Password: '\nexec sleep 30\n");
  let (agent, name) = polkit.agent();
  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  agent.cancel();
  assert_eq!(polkit.answered(&answer), Err(CANCELLED.to_owned()));
  polkit.wait_helper_gone();
}

#[test]
fn an_answer_with_a_nul_never_reaches_the_helper() {
  let conversation =
    "echo 'PAM_PROMPT_ECHO_OFF Password: '\nhead -n 1 > \"$0.answer\"\necho SUCCESS\n";
  let mut polkit = Polkit::start(conversation);
  let (agent, name) = polkit.agent();
  let _answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  let logs = capture_logs(|| agent.respond("hun\0ter2"));
  assert!(logs.contains("line break or NUL"), "{logs}");
  polkit
    .shell
    .run_until(Instant::now() + Duration::from_millis(200));
  let sent = std::fs::read(polkit.dir.path().join("helper.answer")).unwrap_or_default();
  assert!(sent.is_empty(), "the helper read {sent:?}");
  assert!(matches!(
    state(agent.request().get_untracked()),
    Some(AuthState::Asking { .. })
  ));
}

#[test]
fn polkits_socket_is_used_when_something_listens_there() {
  use std::io::{BufRead, BufReader, Write};

  let dir = TempDir::new();
  let socket = dir.path().join("agent-helper.socket");
  let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the fake socket");
  // polkit's socket-started helper: the user and cookie, then PAM.
  let helper = std::thread::spawn(move || {
    let (stream, _) = listener.accept().expect("the agent connects");
    let mut lines = BufReader::new(stream.try_clone().expect("its copy")).lines();
    let mut line = || lines.next().map(|line| line.expect("a line"));
    let (user, cookie) = (line(), line());
    let mut out = &stream;
    out
      .write_all(b"PAM_PROMPT_ECHO_OFF Password: \n")
      .expect("the prompt");
    let answer = line();
    out.write_all(b"SUCCESS\n").expect("the result");
    (user, cookie, answer, line())
  });
  let mut polkit = Polkit::with(PASSWORD, |config| Config {
    polkit_socket: socket,
    ..config
  });
  let (agent, name) = polkit.agent();
  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_state(&agent, asking);
  agent.respond("hunter2");
  assert_eq!(polkit.answered(&answer), Ok(()));
  // Before the join, which would wait for good if the socket went unused.
  assert!(polkit.runs().is_empty(), "the spawned helper never ran");
  let (user, cookie, answer, after) = helper.join().expect("the fake helper");
  assert_eq!(user.as_deref(), Some("root"));
  assert_eq!(cookie.as_deref(), Some("cookie-1"));
  assert_eq!(answer.as_deref(), Some("hunter2"));
  assert_eq!(after, None, "the agent hung up");
}

#[test]
fn a_socket_nothing_listens_on_falls_back_to_the_spawned_helper() {
  let dir = TempDir::new();
  let socket = dir.path().join("agent-helper.socket");
  drop(std::os::unix::net::UnixListener::bind(&socket).expect("the fake socket"));
  let mut polkit = Polkit::with(PASSWORD, |config| Config {
    polkit_socket: socket,
    ..config
  });
  let (agent, name) = polkit.agent();
  let logs = capture_logs(|| {
    let answer = polkit.begin(&name, "cookie-1", &[0]);
    polkit.wait_state(&agent, asking);
    agent.respond("hunter2");
    assert_eq!(polkit.answered(&answer), Ok(()));
  });
  assert!(logs.contains("spawning it instead"), "{logs}");
  assert_eq!(polkit.runs(), ["root cookie-1"]);
}

/// polkit itself with its socket-started helper, as polkit 127 and later
/// have them, and a real PAM. It asks the system bus's polkit to let this
/// process use `pkexec`, answers once wrong and then with
/// `CRABSHELL_REAL_POLKIT_PASSWORD`, and expects the authorization. Never
/// on a real desktop: it registers an agent on the real system bus, and
/// the wrong answer counts toward `pam_faillock`. It is for a throwaway
/// container, run as a user in `wheel` with that password.
#[test]
#[ignore = "needs a throwaway system with polkit 127 or later"]
fn the_real_polkit_authorizes_through_its_helper_socket() {
  let Ok(password) = std::env::var("CRABSHELL_REAL_POLKIT_PASSWORD") else {
    eprintln!("CRABSHELL_REAL_POLKIT_PASSWORD is unset, so nothing ran");
    return;
  };
  let mut shell = Harness::new(Config {
    system_bus: Some(zbus::Address::system().expect("the system bus's address")),
    polkit_helper: PathBuf::from(super::HELPER),
    polkit_socket: PathBuf::from(super::HELPER_SOCKET),
    ..offline()
  });
  // Real PAM pauses after a wrong answer, so this waits longer than 5 s.
  let wait = |shell: &mut Harness, what: &str, done: &dyn Fn() -> bool| {
    let give_up = Instant::now() + Duration::from_secs(30);
    while !done() {
      assert!(Instant::now() < give_up, "no {what} after 30 s");
      shell.run_until(Instant::now() + Duration::from_millis(100));
    }
  };
  let agent = shell.cx.polkit_agent();
  let service = Rc::clone(agent.service.as_ref().expect("the shell's agent"));
  let registered = move || {
    let state = service.state.borrow();
    state
      .as_ref()
      .is_some_and(|state| state.authority.is_some())
  };
  wait(&mut shell, "registration", &registered);

  let result: Rc<RefCell<Option<Result<bool, String>>>> = Rc::default();
  let sink = Rc::clone(&result);
  shell.spawn(async move {
    let check = async {
      let connection = zbus::connection::Builder::system().build().await?;
      let details: HashMap<&str, &str> = HashMap::new();
      // 1 allows interaction, which is what reaches the agent.
      let body = (
        subject(None),
        "org.freedesktop.policykit.exec",
        details,
        1_u32,
        "",
      );
      let reply = connection
        .call_method(
          Some(AUTHORITY),
          super::AUTHORITY_PATH,
          Some(super::AUTHORITY_INTERFACE),
          "CheckAuthorization",
          &body,
        )
        .await?;
      let (authorized, _, _): (bool, bool, HashMap<String, String>) = reply.body().deserialize()?;
      Ok::<_, zbus::Error>(authorized)
    };
    *sink.borrow_mut() = Some(check.await.map_err(|error| error.to_string()));
  });
  let request = agent.request();
  let state_is = |like: fn(&AuthState) -> bool| {
    move || {
      request
        .get_untracked()
        .is_some_and(|shown| like(&shown.state))
    }
  };
  wait(&mut shell, "question", &state_is(asking));
  agent.respond("not the password");
  wait(&mut shell, "failure", &state_is(failed));
  agent.respond(password);
  let answered = Rc::clone(&result);
  wait(&mut shell, "answer", &move || answered.borrow().is_some());
  assert_eq!(result.borrow_mut().take(), Some(Ok(true)));
  assert_eq!(agent.request().get_untracked(), None);
}
