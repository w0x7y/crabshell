use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use zbus::{Connection, Message, OwnedVariant, Variant};

use super::{
  AGENT_INTERFACE, AGENT_PATH, AUTHORITY, AuthState, CANCELLED, FAILED, PolkitAgent, subject,
  user_name,
};
use crate::testing::{FakeService, Harness, PrivateBus, capture_logs, offline, reply};
use crate::{Config, ServicesExt};

const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

/// What a registration sent: the agent's unique name, the subject's kind
/// and details, and the object path.
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

/// A shell on a private system bus with a fake polkit.
struct Polkit {
  shell: Harness,
  bus: PrivateBus,
  authority: FakeService,
  registrations: Registrations,
}

impl Polkit {
  fn start() -> Polkit {
    let bus = PrivateBus::start();
    let mut shell = Harness::new(Config {
      system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
      ..offline()
    });
    let (authority, registrations) = authority(&mut shell, &bus, true);
    Polkit {
      shell,
      bus,
      authority,
      registrations,
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

  /// Waits for a request to show.
  fn wait_request(&mut self, agent: &PolkitAgent) {
    let request = agent.request();
    self.wait("a request", move || request.get_untracked().is_some());
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

#[test]
fn the_agent_registers_on_the_first_call_only() {
  let mut polkit = Polkit::start();
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
fn a_request_shows_and_cancel_from_the_config_answers_cancelled() {
  let mut polkit = Polkit::start();
  let (agent, name) = polkit.agent();
  let request = agent.request();
  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_request(&agent);
  let shown = request.get_untracked().expect("a request");
  assert_eq!(shown.action_id, "org.example.cookie-1");
  assert_eq!(shown.message, "Authentication is needed to reboot");
  assert_eq!(shown.icon_name, "system-reboot");
  assert_eq!(shown.identities, ["root"]);
  assert_eq!(shown.state, AuthState::Waiting);

  agent.cancel();
  assert_eq!(request.get_untracked(), None);
  assert_eq!(polkit.answered(&answer), Err(CANCELLED.to_owned()));
  // Nothing is left to cancel.
  agent.cancel();
}

#[test]
fn polkit_cancelling_clears_the_request() {
  let mut polkit = Polkit::start();
  let (agent, name) = polkit.agent();
  let answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_request(&agent);
  let from = polkit.authority.connection.clone();
  assert_eq!(polkit.cancel_from(&from, &name, "another"), Ok(()));
  assert!(agent.request().get_untracked().is_some(), "not this one");

  assert_eq!(polkit.cancel_from(&from, &name, "cookie-1"), Ok(()));
  assert_eq!(polkit.answered(&answer), Err(CANCELLED.to_owned()));
  assert_eq!(agent.request().get_untracked(), None);
}

#[test]
fn only_polkit_may_call_the_agent() {
  let mut polkit = Polkit::start();
  let (agent, name) = polkit.agent();
  let intruder = polkit.shell.block_on(polkit.bus.connect(&[]));
  let logs = capture_logs(|| {
    let answer = polkit.begin_from(&intruder, &name, "cookie-1", &[0]);
    assert_eq!(polkit.answered(&answer), Err(ACCESS_DENIED.to_owned()));
    assert_eq!(agent.request().get_untracked(), None);

    let answer = polkit.begin(&name, "cookie-2", &[0]);
    polkit.wait_request(&agent);
    let cancel = polkit.cancel_from(&intruder, &name, "cookie-2");
    assert_eq!(cancel, Err(ACCESS_DENIED.to_owned()));
    assert!(agent.request().get_untracked().is_some());
    agent.cancel();
    assert_eq!(polkit.answered(&answer), Err(CANCELLED.to_owned()));
  });
  assert!(logs.contains("other than polkit"), "{logs}");
}

#[test]
fn requests_wait_their_turn_and_past_eight_are_refused() {
  let mut polkit = Polkit::start();
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
  let mut polkit = Polkit::start();
  let (agent, name) = polkit.agent();
  let cookie = polkit.begin(&name, "two\nlines", &[0]);
  assert_eq!(polkit.answered(&cookie), Err(FAILED.to_owned()));
  // No user has that uid.
  let nobody = polkit.begin(&name, "cookie-1", &[4_000_000_000]);
  assert_eq!(polkit.answered(&nobody), Err(FAILED.to_owned()));
  assert_eq!(agent.request().get_untracked(), None);
}

#[test]
fn an_idle_agent_wakes_nothing() {
  let mut polkit = Polkit::start();
  let (agent, name) = polkit.agent();
  let quiet = |polkit: &mut Polkit, what: &str| {
    let shell = &mut polkit.shell;
    shell.run_until(Instant::now() + Duration::from_millis(200));
    let woke = shell.run_until(Instant::now() + Duration::from_secs(1));
    assert_eq!(woke, 1, "{what}");
  };
  quiet(&mut polkit, "registered, nothing asked");

  let _answer = polkit.begin(&name, "cookie-1", &[0]);
  polkit.wait_request(&agent);
  quiet(&mut polkit, "a request waits for the user");
}
