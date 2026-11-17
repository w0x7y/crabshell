use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use nonstick::{ConversationAdapter, ErrorCode};

use super::{
  Adapter, LibPam, MAX_MESSAGES, Pam, PamError, PamMessage, PamResult, PamState, Secret, Talk,
  own_user, tried, wipe,
};
use crate::ServicesExt;
use crate::testing::{FakePam, Harness, Step, capture_logs, offline};

type Log = Arc<Mutex<Vec<String>>>;

/// A shell whose PAM follows `script`, and what the script's runs did.
fn shell(script: Vec<Step>) -> (Harness, Log) {
  let log = Log::default();
  let mut config = offline();
  config.pam = Arc::new(FakePam {
    script,
    log: Arc::clone(&log),
  });
  (Harness::new(config), log)
}

fn password() -> Step {
  Step::Ask(String::from("Password: "), false, "hunter2")
}

fn asking(message: &str, visible: bool) -> PamState {
  PamState::Asking {
    message: message.to_owned(),
    visible,
  }
}

fn state(pam: &Pam) -> PamState {
  pam.state().get_untracked()
}

fn done(pam: &Pam) -> bool {
  matches!(state(pam), PamState::Done(_))
}

fn logged(log: &Log, line: &str) -> bool {
  log.lock().unwrap().iter().any(|logged| logged == line)
}

const REFUSED: &str = "ended Error { kind: InternalError, message: \"refused\" }";

/// Runs the loop until `ready`, for up to 5 s.
fn wait(shell: &mut Harness, what: &str, ready: impl Fn() -> bool) {
  let give_up = Instant::now() + Duration::from_secs(5);
  while !ready() {
    assert!(Instant::now() < give_up, "no {what} after 5 s");
    shell.run_until(Instant::now() + Duration::from_millis(10));
  }
}

fn run(shell: &mut Harness, ms: u64) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(ms))
}

#[test]
fn a_password_question_then_success() {
  let (mut shell, log) = shell(vec![password()]);
  let pam = shell.cx.pam("lock");
  assert_eq!(state(&pam), PamState::Idle);
  pam.start(Some("alice"));
  assert_eq!(state(&pam), PamState::Busy);
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  assert_eq!(state(&pam), asking("Password: ", false));
  pam.respond("hunter2");
  assert_eq!(state(&pam), PamState::Busy);
  wait(&mut shell, "result", || done(&pam));
  assert_eq!(state(&pam), PamState::Done(PamResult::Success));
  assert_eq!(*log.lock().unwrap(), ["lock for alice", "ended Success"]);
}

#[test]
fn a_wrong_answer_then_failure() {
  let (mut shell, _) = shell(vec![password()]);
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  pam.respond(String::from("wrong"));
  wait(&mut shell, "result", || done(&pam));
  assert_eq!(state(&pam), PamState::Done(PamResult::Failure));
}

#[test]
fn info_and_error_messages_arrive_in_order() {
  let (mut shell, _) = shell(vec![
    Step::Info(String::from("one")),
    Step::Error(String::from("two")),
    password(),
    Step::Info(String::from("three")),
  ]);
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  let (one, two) = (
    PamMessage::Info("one".into()),
    PamMessage::Error("two".into()),
  );
  assert_eq!(pam.messages().get_untracked(), [one.clone(), two.clone()]);
  pam.respond("hunter2");
  wait(&mut shell, "result", || done(&pam));
  let three = PamMessage::Info("three".into());
  assert_eq!(pam.messages().get_untracked(), [one, two, three]);
  pam.start(Some("alice"));
  assert_eq!(pam.messages().get_untracked(), [], "a start empties them");
}

#[test]
fn abort_mid_question_refuses_it_and_ends_the_thread() {
  let (mut shell, log) = shell(vec![password()]);
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  pam.abort();
  assert_eq!(state(&pam), PamState::Idle);
  wait(&mut shell, "the thread's end", || logged(&log, REFUSED));
  run(&mut shell, 50);
  assert_eq!(state(&pam), PamState::Idle, "the aborted end isn't shown");
}

#[test]
fn the_context_going_mid_question_ends_the_thread_and_frees_everything() {
  let runtime = Runtime::current();
  let (mut shell, log) = shell(vec![password()]);
  let before = runtime.node_count();
  let child = shell.cx.child();
  let pam = child.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  let weak = Rc::downgrade(&pam.inner);
  drop(pam);
  child.owner().dispose();
  wait(&mut shell, "the thread's end", || logged(&log, REFUSED));
  run(&mut shell, 50);
  assert!(weak.upgrade().is_none(), "the attempt was freed");
  assert_eq!(runtime.node_count(), before);
}

/// A module that blocks without asking, as a fingerprint reader does.
fn blocking() -> Vec<Step> {
  vec![Step::Sleep(Duration::from_millis(300)), password()]
}

#[test]
fn a_start_while_an_aborted_attempt_blocks_waits_for_it_to_return() {
  let (mut shell, log) = shell(blocking());
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "alice's attempt", || {
    logged(&log, "lock for alice")
  });
  pam.abort();
  assert_eq!(state(&pam), PamState::Idle, "at once");
  pam.start(Some("bob"));
  assert_eq!(state(&pam), PamState::Busy);
  run(&mut shell, 100);
  let started = || {
    log
      .lock()
      .unwrap()
      .iter()
      .filter(|line| line.starts_with("lock for"))
      .count()
  };
  assert_eq!(started(), 1, "no second transaction beside the first");
  wait(&mut shell, "bob's question", || {
    state(&pam) != PamState::Busy
  });
  assert_eq!(
    state(&pam),
    asking("Password: ", false),
    "alice's end isn't shown"
  );
  assert_eq!(
    *log.lock().unwrap(),
    ["lock for alice", REFUSED, "lock for bob"]
  );
  pam.respond("hunter2");
  wait(&mut shell, "result", || done(&pam));
  assert_eq!(state(&pam), PamState::Done(PamResult::Success));
}

#[test]
fn an_abort_drops_a_start_that_waits() {
  let (mut shell, log) = shell(blocking());
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "alice's attempt", || {
    logged(&log, "lock for alice")
  });
  pam.start(Some("bob"));
  pam.abort();
  assert_eq!(state(&pam), PamState::Idle);
  wait(&mut shell, "alice's end", || logged(&log, REFUSED));
  run(&mut shell, 100);
  assert!(!logged(&log, "lock for bob"), "{:?}", log.lock().unwrap());
  assert_eq!(state(&pam), PamState::Idle);
}

#[test]
fn the_context_going_while_an_aborted_attempt_blocks_frees_everything() {
  let runtime = Runtime::current();
  let (mut shell, log) = shell(blocking());
  let before = runtime.node_count();
  let child = shell.cx.child();
  let pam = child.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "alice's attempt", || {
    logged(&log, "lock for alice")
  });
  pam.abort();
  pam.start(Some("bob"));
  let weak = Rc::downgrade(&pam.inner);
  drop(pam);
  child.owner().dispose();
  assert!(weak.upgrade().is_none(), "freed at once");
  assert_eq!(runtime.node_count(), before);
  wait(&mut shell, "alice's end", || logged(&log, REFUSED));
  run(&mut shell, 100);
  assert!(!logged(&log, "lock for bob"));
}

#[test]
fn a_second_start_aborts_the_first() {
  let (mut shell, log) = shell(vec![password()]);
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  pam.start(Some("bob"));
  assert_eq!(state(&pam), PamState::Busy);
  wait(&mut shell, "the first thread's end", || {
    logged(&log, REFUSED)
  });
  wait(&mut shell, "bob's question", || {
    state(&pam) != PamState::Busy
  });
  run(&mut shell, 50);
  assert_eq!(
    state(&pam),
    asking("Password: ", false),
    "alice's end isn't shown"
  );
  pam.respond("hunter2");
  wait(&mut shell, "result", || done(&pam));
  assert_eq!(state(&pam), PamState::Done(PamResult::Success));
  assert!(logged(&log, "lock for bob") && logged(&log, "ended Success"));
}

#[test]
fn long_questions_and_messages_are_cut_to_4_kib() {
  let (mut shell, _) = shell(vec![
    Step::Info(format!("a{}", "é".repeat(3000))),
    Step::Ask("x".repeat(5000), true, "y"),
  ]);
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  assert_eq!(state(&pam), asking(&"x".repeat(4096), true));
  let info = format!("a{}", "é".repeat(2047));
  assert_eq!(pam.messages().get_untracked(), [PamMessage::Info(info)]);
}

#[test]
fn a_response_with_no_question_is_dropped_and_never_logged() {
  let (mut shell, _) = shell(vec![password()]);
  let pam = shell.cx.pam("lock");
  let logs = capture_logs(|| pam.respond("early"));
  assert!(
    logs.contains("no question") && !logs.contains("early"),
    "{logs}"
  );
  pam.start(Some("alice"));
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  pam.respond("hunter2");
  let logs = capture_logs(|| pam.respond("twice"));
  assert!(
    logs.contains("no question") && !logs.contains("twice"),
    "{logs}"
  );
  wait(&mut shell, "result", || done(&pam));
  assert_eq!(state(&pam), PamState::Done(PamResult::Success));
}

#[test]
fn a_start_without_a_user_authenticates_the_shells_own() {
  let (mut shell, log) = shell(Vec::new());
  let pam = shell.cx.pam("lock");
  pam.start(None);
  wait(&mut shell, "result", || done(&pam));
  let user = own_user().expect("NSS knows the test's user");
  assert_eq!(log.lock().unwrap()[0], format!("lock for {user}"));
}

#[test]
fn users_are_found_by_uid_through_nss() {
  // id(1) asks NSS too, so a user only LDAP or systemd-homed knows counts.
  let id = std::process::Command::new("id").arg("-un").output();
  let named = String::from_utf8(id.expect("id runs").stdout).expect("UTF-8");
  assert_eq!(own_user().as_deref(), Ok(named.trim()));
  assert_eq!(crate::user_name(0).as_deref(), Some("root"));
  assert_eq!(crate::user_name(4_000_000_000), None);
}

#[test]
fn a_pam_wakes_nothing_before_or_after_an_attempt() {
  let (mut shell, _) = shell(vec![password()]);
  let pam = shell.cx.pam("lock");
  assert_eq!(run(&mut shell, 300), 1, "only the run's own deadline");
  pam.start(Some("alice"));
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  pam.respond("hunter2");
  wait(&mut shell, "result", || done(&pam));
  assert_eq!(run(&mut shell, 1000), 1, "only the run's own deadline");
}

#[test]
fn wipe_zeroes_the_whole_buffer() {
  let mut bytes = Vec::with_capacity(64);
  bytes.extend_from_slice(b"hunter2");
  wipe(&mut bytes);
  assert!(bytes.len() >= 64 && bytes.iter().all(|&byte| byte == 0));
}

#[test]
fn pam_authenticate_failing_otherwise_than_on_the_answers_is_a_failed_try() {
  assert_eq!(tried(ErrorCode::AuthenticationError), PamResult::Failure);
  let kind = |error| match tried(error) {
    PamResult::Error { kind, .. } => Some(kind),
    _ => None,
  };
  // The rest of what pam_authenticate(3) may return, and what else a
  // module might.
  for error in [
    ErrorCode::AuthInfoUnavailable,
    ErrorCode::MaxTries,
    ErrorCode::UserUnknown,
    ErrorCode::CredentialsInsufficient,
    ErrorCode::Abort,
    ErrorCode::SystemError,
  ] {
    assert_eq!(kind(error), Some(PamError::TryAuthFailed), "{error:?}");
  }
  // The shell's own conversation refused an answer.
  let refused = kind(ErrorCode::ConversationError);
  assert_eq!(refused, Some(PamError::InternalError));
}

#[test]
fn an_attempt_that_ends_without_a_result_is_an_internal_error() {
  let (mut shell, _) = shell(vec![Step::Panic]);
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "result", || done(&pam));
  let PamState::Done(PamResult::Error { kind, message }) = state(&pam) else {
    panic!("{:?}", state(&pam));
  };
  assert_eq!(kind, PamError::InternalError);
  assert!(message.contains("without a result"), "{message}");
}

#[test]
fn an_answer_with_a_nul_is_refused_before_libpam() {
  let (events, _channel) = calloop::channel::channel();
  let (answers, inbox) = mpsc::channel();
  let talk = Talk {
    events,
    answers: inbox,
  };
  answers.send(Secret(b"a\0b".to_vec())).unwrap();
  let answer = Adapter(&talk).masked_prompt("Password: ");
  assert_eq!(answer, Err(ErrorCode::ConversationError));
}

/// The one test that reaches libpam. `other` must be `pam_deny` and
/// nothing else, so no wrong answer counts toward locking a real account.
#[test]
fn the_real_other_service_fails_at_once_without_a_question() {
  let other = std::fs::read_to_string("/etc/pam.d/other").unwrap_or_default();
  let mut lines = other
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty() && !line.starts_with('#'))
    .peekable();
  let deny_only = lines.peek().is_some()
    && lines.all(|line| {
      let fields: Vec<&str> = line.split_whitespace().collect();
      fields.len() == 3 && fields[1] == "required" && fields[2] == "pam_deny.so"
    });
  if !deny_only {
    eprintln!("skipped: /etc/pam.d/other isn't pam_deny only, so it might reach a real account");
    return;
  }
  let mut config = offline();
  config.pam = Arc::new(LibPam);
  let mut shell = Harness::new(config);
  let pam = shell.cx.pam("other");
  let seen = Rc::new(RefCell::new(Vec::new()));
  let states = pam.state();
  let kept = Rc::clone(&seen);
  shell
    .cx
    .effect(move || kept.borrow_mut().push(states.get()));
  pam.start(None);
  wait(&mut shell, "result", || done(&pam));
  assert_eq!(state(&pam), PamState::Done(PamResult::Failure));
  let asked = seen
    .borrow()
    .iter()
    .any(|state| matches!(state, PamState::Asking { .. }));
  assert!(!asked, "{:?}", seen.borrow());
  assert_eq!(pam.messages().get_untracked(), []);
}

#[test]
fn only_the_latest_messages_are_kept() {
  let script = (0..MAX_MESSAGES + 5)
    .map(|n| Step::Info(n.to_string()))
    .collect();
  let (mut shell, _) = shell(script);
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "result", || done(&pam));
  let messages = pam.messages().get_untracked();
  assert_eq!(messages.len(), MAX_MESSAGES);
  assert_eq!(
    messages[0],
    PamMessage::Info(String::from("5")),
    "the oldest go"
  );
}

#[test]
fn a_start_after_the_context_went_does_nothing() {
  let (mut shell, log) = shell(vec![password()]);
  let child = shell.cx.child();
  let pam = child.pam("lock");
  child.owner().dispose();
  pam.start(Some("alice"));
  assert_eq!(run(&mut shell, 300), 1, "no attempt wakes the loop");
  assert!(log.lock().unwrap().is_empty(), "{:?}", log.lock().unwrap());
}

#[test]
fn an_abort_leaves_the_loop_asleep() {
  let (mut shell, log) = shell(vec![password()]);
  let pam = shell.cx.pam("lock");
  pam.start(Some("alice"));
  wait(&mut shell, "question", || state(&pam) != PamState::Busy);
  pam.abort();
  // The loop hears the thread end once refused, and then nothing.
  let returned = || pam.inner.abandoned.borrow().is_none();
  wait(&mut shell, "the thread's end", returned);
  assert!(logged(&log, REFUSED));
  assert_eq!(run(&mut shell, 300), 1, "only the run's own deadline");
  assert_eq!(state(&pam), PamState::Idle);
}

/// libpam never sees these, so no real account is reached.
#[test]
fn a_service_or_user_with_a_nul_is_refused_before_libpam() {
  for (service, user) in [("oth\0er", "alice"), ("other", "al\0ice")] {
    let mut config = offline();
    config.pam = Arc::new(LibPam);
    let mut shell = Harness::new(config);
    let pam = shell.cx.pam(service);
    pam.start(Some(user));
    wait(&mut shell, "result", || done(&pam));
    let PamState::Done(PamResult::Error { kind, message }) = state(&pam) else {
      panic!("{:?}", state(&pam));
    };
    assert_eq!(kind, PamError::StartFailed);
    assert!(message.contains("NUL"), "{message}");
  }
}
