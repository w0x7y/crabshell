//! What the services' tests share: a shell with no compositor, and a
//! private D-Bus daemon.

use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use calloop::futures::Scheduler;
use crabshell_reactive::Runtime;
use crabshell_ui::Cx;

use crate::Config;

/// What a shell gives the services, with no compositor: a loop and a root
/// context.
pub(crate) struct Harness {
  pub(crate) event_loop: EventLoop<'static, ()>,
  pub(crate) cx: Cx,
  /// Runs the tests' own futures, such as a fake logind's.
  scheduler: Scheduler<()>,
}

impl Harness {
  pub(crate) fn new(config: Config) -> Harness {
    let event_loop = EventLoop::try_new().expect("an event loop");
    let cx = Cx::new(Runtime::current().new_owner());
    crate::provide_with(&cx, &event_loop.handle(), config);
    let (executor, scheduler) = calloop::futures::executor().expect("an executor");
    event_loop
      .handle()
      .insert_source(executor, |(), _, _| {})
      .expect("the tests' executor in the loop");
    Harness {
      event_loop,
      cx,
      scheduler,
    }
  }

  /// Runs the loop as the shell does, until `deadline`. Returns how many
  /// times it woke.
  pub(crate) fn run_until(&mut self, deadline: Instant) -> usize {
    let mut wakeups = 0;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
      if left.is_zero() {
        break;
      }
      self
        .event_loop
        .dispatch(Some(left), &mut ())
        .expect("the loop runs");
      Runtime::current().flush();
      wakeups += 1;
    }
    wakeups
  }

  /// Starts `future` on the loop, alongside the shell, and returns at once.
  pub(crate) fn spawn(&self, future: impl Future<Output = ()> + 'static) {
    self
      .scheduler
      .schedule(future)
      .expect("the tests' executor runs");
  }

  /// Runs `future` to its end on the loop, which runs everything else
  /// meanwhile. Gives up after 5 s.
  pub(crate) fn block_on<R: 'static>(&mut self, future: impl Future<Output = R> + 'static) -> R {
    let result = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&result);
    self.spawn(async move { *sink.borrow_mut() = Some(future.await) });
    let give_up = Instant::now() + Duration::from_secs(5);
    loop {
      if let Some(result) = result.borrow_mut().take() {
        return result;
      }
      assert!(Instant::now() < give_up, "a future still running after 5 s");
      self.run_until(Instant::now() + Duration::from_millis(10));
    }
  }

  /// Does `act`, then runs the loop for 100 ms, until `done`. Gives up
  /// after 5 s.
  pub(crate) fn retry_until(
    &mut self,
    what: &str,
    done: impl Fn() -> bool,
    mut act: impl FnMut(&mut Harness),
  ) {
    let give_up = Instant::now() + Duration::from_secs(5);
    while !done() {
      assert!(Instant::now() < give_up, "no {what} after 5 s");
      act(self);
      self.run_until(Instant::now() + Duration::from_millis(100));
    }
  }
}

/// A config with no system parts: the system zone through jiff, and no
/// bus, Sway or audio server.
pub(crate) fn offline() -> Config {
  Config {
    localtime: None,
    system_bus: None,
    sway: None,
    audio: None,
  }
}

/// A private bus standing in for the system or session bus:
/// `dbus-daemon --session` on an address of its own. It stops on drop.
pub(crate) struct PrivateBus {
  daemon: Child,
  pub(crate) address: String,
}

impl PrivateBus {
  pub(crate) fn start() -> PrivateBus {
    let mut daemon = Command::new("dbus-daemon")
      .args(["--session", "--nofork", "--print-address"])
      .stdout(Stdio::piped())
      .spawn()
      .expect("dbus-daemon starts (it comes with dbus)");
    let stdout = daemon.stdout.take().expect("dbus-daemon's stdout");
    let mut address = String::new();
    BufReader::new(stdout)
      .read_line(&mut address)
      .expect("dbus-daemon prints its address");
    let address = address.trim().to_owned();
    PrivateBus { daemon, address }
  }

  /// A connection to it, owning `names`.
  pub(crate) fn connect(
    &self,
    names: &[&'static str],
  ) -> impl Future<Output = zbus::Connection> + 'static {
    let mut builder = zbus::connection::Builder::address(self.address.as_str());
    for &name in names {
      builder = builder.name(name);
    }
    async move {
      builder
        .build()
        .await
        .expect("a connection to the private bus")
    }
  }
}

impl Drop for PrivateBus {
  fn drop(&mut self) {
    let _ = self.daemon.kill();
    let _ = self.daemon.wait();
  }
}
