use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use nonstick::{ConversationAdapter, ErrorCode};

use super::{
  Adapter, LibPam, Pam, PamMessage, PamResult, PamState, Secret, Talk, own_user, user_name, wipe,
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

const REFUSED: &str = "ended Error(\"refused\")";

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
  let user = own_user().expect("the test's user is in /etc/passwd");
  assert_eq!(log.lock().unwrap()[0], format!("lock for {user}"));
}

#[test]
fn the_own_user_is_found_by_uid() {
  let passwd = "root:x:0:0:root:/root:/bin/sh\n+::::::\nalice:x:1000:1000::/home/alice:/bin/sh\n";
  assert_eq!(user_name(passwd, 1000).as_deref(), Some("alice"));
  assert_eq!(user_name(passwd, 0).as_deref(), Some("root"));
  assert_eq!(user_name(passwd, 7), None);
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
