use std::cell::RefCell;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use zbus::Variant;

use super::{LOGIND, MANAGER, PATH};
use crate::testing::{FakeService, Harness, PrivateBus, capture_logs, offline, reply};
use crate::{Config, ServicesExt};

fn online(bus: &PrivateBus) -> Config {
  Config {
    system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  }
}

/// A fake logind and what it was asked.
struct FakeLogind {
  fake: FakeService,
  /// The read end of each inhibitor's pipe, in the order handed out:
  /// it hangs up once the shell closes the write end.
  inhibitors: Rc<RefCell<Vec<OwnedFd>>>,
  /// Each `Inhibit`'s what, who, why and mode, and each power method
  /// with its `interactive`, in order.
  asked: Rc<RefCell<Vec<String>>>,
}

impl FakeLogind {
  fn start(shell: &mut Harness, bus: &PrivateBus) -> FakeLogind {
    let fake = FakeService::start(shell, bus, LOGIND);
    fake.add(
      PATH,
      MANAGER,
      &[("PreparingForSleep", Variant::from(false))],
    );
    let inhibitors: Rc<RefCell<Vec<OwnedFd>>> = Rc::default();
    let asked: Rc<RefCell<Vec<String>>> = Rc::default();
    let (pipes, log) = (Rc::clone(&inhibitors), Rc::clone(&asked));
    fake.on("Inhibit", move |call| {
      let (what, who, why, mode): (String, String, String, String) =
        call.body().deserialize().expect("ssss");
      log
        .borrow_mut()
        .push(format!("Inhibit {what} {who} {why:?} {mode}"));
      let (read, write) = std::io::pipe().expect("a pipe");
      pipes.borrow_mut().push(read.into());
      reply(call, &zbus::Fd::from(OwnedFd::from(write)))
    });
    for method in ["PowerOff", "Reboot", "Suspend", "Hibernate"] {
      let log = Rc::clone(&asked);
      fake.on(method, move |call| {
        let interactive: bool = call.body().deserialize().expect("b");
        log.borrow_mut().push(format!("{method} {interactive}"));
        reply(call, &())
      });
    }
    fake.own(shell);
    FakeLogind {
      fake,
      inhibitors,
      asked,
    }
  }

  fn prepare_for_sleep(&self, shell: &mut Harness, start: bool) {
    self
      .fake
      .emit(shell, PATH, MANAGER, "PrepareForSleep", start);
  }

  fn inhibitors(&self) -> usize {
    self.inhibitors.borrow().len()
  }

  /// Whether the shell still holds inhibitor `n`.
  fn held(&self, n: usize) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec};

    let pipes = self.inhibitors.borrow();
    let mut fds = [PollFd::new(&pipes[n], PollFlags::IN)];
    rustix::event::poll(&mut fds, Some(&Timespec::default())).expect("poll");
    !fds[0].revents().contains(PollFlags::HUP)
  }
}

fn run(shell: &mut Harness, ms: u64) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(ms))
}

#[test]
fn a_delay_lets_sleep_go_once_ready_and_holds_it_again_after_the_resume() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let logind = FakeLogind::start(&mut shell, &bus);
  let ready = shell.cx.signal(false);
  let lock = shell.cx.child();
  let handle = lock.logind();
  let sleeping = handle.preparing_for_sleep();
  handle.delay_sleep_until(move || ready.get());
  shell.retry_until("an inhibitor", || logind.inhibitors() == 1, |_| {});
  assert_eq!(
    logind.asked.borrow()[0],
    "Inhibit sleep crabshell \"The shell gets ready for sleep\" delay"
  );
  run(&mut shell, 100);
  assert!(logind.held(0));

  // Sleep waits while the config isn't ready.
  logind.prepare_for_sleep(&mut shell, true);
  shell.retry_until("PrepareForSleep(true)", || sleeping.get_untracked(), |_| {});
  run(&mut shell, 200);
  assert!(logind.held(0), "let go before ready");
  ready.set(true);
  shell.retry_until("the inhibitor let go", || !logind.held(0), |_| {});

  // After the resume a new one is held, and let go at once next time,
  // since the config is ready already.
  logind.prepare_for_sleep(&mut shell, false);
  shell.retry_until("a new inhibitor", || logind.inhibitors() == 2, |_| {});
  assert!(!sleeping.get_untracked());
  run(&mut shell, 100);
  assert!(logind.held(1));
  logind.prepare_for_sleep(&mut shell, true);
  shell.retry_until("the second let go", || !logind.held(1), |_| {});
  logind.prepare_for_sleep(&mut shell, false);
  shell.retry_until("a third inhibitor", || logind.inhibitors() == 3, |_| {});
  run(&mut shell, 100);
  assert!(logind.held(2));

  // The context going lets go, and leaves no node behind.
  lock.owner().dispose();
  run(&mut shell, 100);
  assert!(!logind.held(2), "held after its context went");
  assert_eq!(logind.inhibitors(), 3);
}

#[test]
fn each_delay_holds_its_own_and_strangers_cant_start_a_sleep() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let logind = FakeLogind::start(&mut shell, &bus);
  let first = shell.cx.signal(false);
  let second = shell.cx.signal(false);
  let handle = shell.cx.logind();
  handle.delay_sleep_until(move || first.get());
  handle.delay_sleep_until(move || second.get());
  shell.retry_until("two inhibitors", || logind.inhibitors() == 2, |_| {});

  let stranger = shell.block_on(bus.connect(&[]));
  shell
    .block_on(async move {
      stranger
        .emit_signal(None::<&str>, PATH, MANAGER, "PrepareForSleep", &true)
        .await
    })
    .expect("the stranger's signal");
  run(&mut shell, 200);
  assert!(!handle.preparing_for_sleep().get_untracked());

  logind.prepare_for_sleep(&mut shell, true);
  run(&mut shell, 200);
  first.set(true);
  shell.retry_until("the first let go", || !logind.held(0), |_| {});
  run(&mut shell, 100);
  assert!(logind.held(1), "the second let go with the first");
  second.set(true);
  shell.retry_until("the second let go", || !logind.held(1), |_| {});
}

#[test]
fn a_panicking_ready_is_logged_and_lets_sleep_go() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let logind = FakeLogind::start(&mut shell, &bus);
  let handle = shell.cx.logind();
  handle.delay_sleep_until(|| -> bool { panic!("the lock broke") });
  let ready = shell.cx.signal(false);
  handle.delay_sleep_until(move || ready.get());
  shell.retry_until("two inhibitors", || logind.inhibitors() == 2, |_| {});
  let logs = capture_logs(|| {
    logind.prepare_for_sleep(&mut shell, true);
    shell.retry_until("the broken one let go", || !logind.held(0), |_| {});
  });
  assert!(
    logs.contains("Logind::delay_sleep_until (called at")
      && logs.contains("logind/tests.rs")
      && logs.contains("the lock broke"),
    "{logs}"
  );
  // The other delay goes on.
  assert!(logind.held(1));
  ready.set(true);
  shell.retry_until("the other let go", || !logind.held(1), |_| {});
}

#[test]
fn the_power_actions_ask_logind_interactively() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let logind = FakeLogind::start(&mut shell, &bus);
  let handle = shell.cx.logind();
  // Once logind is reached: a delay's inhibitor says so.
  handle.delay_sleep_until(true);
  shell.retry_until("logind reached", || logind.inhibitors() == 1, |_| {});
  handle.power_off();
  handle.reboot();
  handle.suspend();
  handle.hibernate();
  shell.retry_until(
    "the power calls",
    || logind.asked.borrow().len() == 5,
    |_| {},
  );
  assert_eq!(
    logind.asked.borrow()[1..],
    [
      "PowerOff true",
      "Reboot true",
      "Suspend true",
      "Hibernate true"
    ]
  );
}

#[test]
fn an_idle_logind_wakes_nothing_and_none_at_all_is_quiet() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let logind = FakeLogind::start(&mut shell, &bus);
  let handle = shell.cx.logind();
  handle.delay_sleep_until(false);
  shell.retry_until("an inhibitor", || logind.inhibitors() == 1, |_| {});
  run(&mut shell, 200);
  assert_eq!(run(&mut shell, 2000), 1);

  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let handle = shell.cx.logind();
    handle.delay_sleep_until(false);
    handle.suspend();
    assert_eq!(run(&mut shell, 200), 1);
    assert!(!handle.preparing_for_sleep().get_untracked());
  });
  assert!(logs.contains("no system bus"), "{logs}");
  assert!(logs.contains("logind isn't running"), "{logs}");
}

#[test]
fn a_delay_and_its_context_leave_no_node_behind() {
  let runtime = Runtime::current();
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let logind = FakeLogind::start(&mut shell, &bus);
  shell.cx.logind().delay_sleep_until(false);
  // Once following logind has made what it keeps under the root.
  shell.retry_until("an inhibitor", || logind.inhibitors() == 1, |_| {});
  let before = runtime.node_count();
  let lock = shell.cx.child();
  lock.logind().delay_sleep_until(false);
  run(&mut shell, 100);
  lock.owner().dispose();
  assert_eq!(runtime.node_count(), before);
  // On a context that is gone, it says so and makes nothing.
  let logs = capture_logs(|| lock.logind().delay_sleep_until(false));
  assert!(logs.contains("on a context that is gone"), "{logs}");
  assert_eq!(runtime.node_count(), before);
}

#[test]
fn an_inhibitor_that_arrives_once_sleep_is_near_goes_as_ready_says() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let logind = FakeLogind::start(&mut shell, &bus);
  shell.cx.logind().delay_sleep_until(true);
  shell.retry_until("an inhibitor", || logind.inhibitors() == 1, |_| {});
  logind.prepare_for_sleep(&mut shell, true);
  shell.retry_until("the first let go", || !logind.held(0), |_| {});

  // The resume's new inhibitor is answered only after the next sleep
  // began, by a slow logind.
  logind.fake.hold();
  logind.prepare_for_sleep(&mut shell, false);
  shell.retry_until(
    "a second Inhibit",
    || logind.asked.borrow().len() == 2,
    |_| {},
  );
  logind.prepare_for_sleep(&mut shell, true);
  run(&mut shell, 100);
  logind.fake.release(&mut shell);
  shell.retry_until("the late one let go", || !logind.held(1), |_| {});
}
