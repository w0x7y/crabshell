//! Activation tokens for the programs the shell starts, so the compositor
//! may focus their windows. The shell hands over how to ask for one, so
//! the services know nothing of Wayland.

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use calloop::timer::TimeoutAction;

use crate::Shared;

/// How long a launch waits for its token before it starts without one.
/// A compositor answers within a round trip, a millisecond or two; a
/// program takes far longer than this to show a window, so the wait goes
/// unnoticed even when the answer never comes.
pub(crate) const WAIT: Duration = Duration::from_millis(100);

/// How the shell asks the compositor for a token: for a program with
/// this app id, if known, handing the token to the callback once it
/// comes.
pub(crate) type Request = Box<dyn Fn(Option<&str>, Box<dyn FnOnce(String)>)>;

/// What goes on once a token came or the wait ended.
pub(crate) type Then = Box<dyn FnOnce(Option<String>)>;

/// The launches waiting for their token, each under a number of its own,
/// with the timer that ends its wait.
#[derive(Default)]
pub(crate) struct Waiting {
  next: Cell<u64>,
  launches: RefCell<Vec<(u64, RegistrationToken, Then)>>,
}

impl Waiting {
  /// Takes launch `number` out with its timer, if it still waits.
  fn take(&self, number: u64) -> Option<(RegistrationToken, Then)> {
    let mut launches = self.launches.borrow_mut();
    let index = launches.iter().position(|(ours, ..)| *ours == number)?;
    let (_, timer, then) = launches.swap_remove(index);
    Some((timer, then))
  }
}

/// Calls `then` with a token for a program with app id `app_id` that is
/// about to start: once the compositor gives it, or with `None` after
/// [`WAIT`] or when the shell stops ([`start_waiting`]). With no way to
/// ask, or no timer to bound the wait, it calls `then` with `None` at
/// once. A token that comes after the wait is dropped. The timer is in the
/// loop only while a token is awaited.
pub(crate) fn with_token(
  shared: &Rc<Shared>,
  app_id: Option<&str>,
  then: impl FnOnce(Option<String>) + 'static,
) {
  let (Some(request), Some(event_loop)) = (shared.activation.get(), shared.event_loop()) else {
    return then(None);
  };
  let waiting = &shared.waiting;
  let number = waiting.next.get();
  waiting.next.set(number + 1);
  let late = {
    let shared = Rc::clone(shared);
    Box::new(move || {
      // The timer drops itself.
      if let Some((_, then)) = shared.waiting.take(number) {
        tracing::debug!("no activation token in time; starting without one");
        then(None);
      }
      TimeoutAction::Drop
    })
  };
  let Some(timer) = event_loop.insert_timer(Instant::now() + WAIT, late) else {
    return then(None);
  };
  let then: Then = Box::new(then);
  waiting.launches.borrow_mut().push((number, timer, then));
  let shared = Rc::clone(shared);
  request(
    app_id,
    Box::new(move |token| {
      let Some((timer, then)) = shared.waiting.take(number) else {
        return tracing::debug!("an activation token came too late, so it goes unused");
      };
      if let Some(event_loop) = shared.event_loop() {
        event_loop.remove(timer);
      }
      then(Some(token));
    }),
  );
}

/// Starts every launch still waiting for its token, without one, as the
/// shell does before it exits, when no token or timer would come.
pub(crate) fn start_waiting(shared: &Shared) {
  let launches = std::mem::take(&mut *shared.waiting.launches.borrow_mut());
  let event_loop = shared.event_loop();
  for (_, timer, then) in launches {
    if let Some(event_loop) = &event_loop {
      event_loop.remove(timer);
    }
    then(None);
  }
}

/// Gives `command` the token where programs look for it: Wayland's
/// `XDG_ACTIVATION_TOKEN`, and `DESKTOP_STARTUP_ID`, which X11 toolkits
/// and older ones read.
pub(crate) fn set_env(command: &mut Command, token: &str) {
  command
    .env("XDG_ACTIVATION_TOKEN", token)
    .env("DESKTOP_STARTUP_ID", token);
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::rc::Rc;
  use std::time::{Duration, Instant};

  use super::with_token;
  use crate::Shared;
  use crate::testing::{Harness, offline};

  /// What a test's token request was asked with, and its callback.
  type Asked = Rc<RefCell<Vec<(Option<String>, Box<dyn FnOnce(String)>)>>>;

  /// A shell's shared services that ask for tokens, keeping each request
  /// for the test to answer, or none if `tokens` is false.
  fn shared(shell: &Harness, tokens: bool) -> (Rc<Shared>, Asked) {
    let shared = Rc::new(Shared::new(
      shell.cx.owner(),
      &shell.event_loop.handle(),
      offline(),
    ));
    let asked = Asked::default();
    if tokens {
      let into = Rc::clone(&asked);
      let request = move |app_id: Option<&str>, done: Box<dyn FnOnce(String)>| {
        into.borrow_mut().push((app_id.map(str::to_owned), done));
      };
      let _ = shared.activation.set(Box::new(request));
    }
    (shared, asked)
  }

  fn run(shell: &mut Harness, ms: u64) -> usize {
    shell.run_until(Instant::now() + Duration::from_millis(ms))
  }

  #[test]
  fn without_a_way_to_ask_it_goes_on_at_once() {
    let shell = Harness::new(offline());
    let (shared, _) = shared(&shell, false);
    let got = Rc::new(RefCell::new(None));
    let into = Rc::clone(&got);
    with_token(&shared, Some("app"), move |token| {
      *into.borrow_mut() = Some(token)
    });
    assert_eq!(*got.borrow(), Some(None));
  }

  #[test]
  fn a_token_that_comes_goes_on_with_it_and_leaves_nothing_in_the_loop() {
    let mut shell = Harness::new(offline());
    let (shared, asked) = shared(&shell, true);
    let got = Rc::new(RefCell::new(None));
    let into = Rc::clone(&got);
    with_token(&shared, Some("org.example.App"), move |token| {
      *into.borrow_mut() = Some(token);
    });
    assert_eq!(*got.borrow(), None, "it waits");
    let (app_id, done) = asked.borrow_mut().pop().expect("a request");
    assert_eq!(app_id.as_deref(), Some("org.example.App"));
    done(String::from("token-1"));
    assert_eq!(*got.borrow(), Some(Some(String::from("token-1"))));
    // The wait's timer went with it: only the end of the run wakes.
    assert_eq!(run(&mut shell, 300), 1);
  }

  #[test]
  fn a_token_that_doesnt_come_goes_on_without_it_after_the_wait() {
    let mut shell = Harness::new(offline());
    let (shared, asked) = shared(&shell, true);
    let got = Rc::new(RefCell::new(Vec::new()));
    let into = Rc::clone(&got);
    let asked_at = Instant::now();
    with_token(&shared, None, move |token| into.borrow_mut().push(token));
    shell.retry_until("the wait's end", || !got.borrow().is_empty(), |_| {});
    assert!(asked_at.elapsed() >= super::WAIT);
    assert_eq!(*got.borrow(), [None]);
    // Too late: dropped.
    let (_, done) = asked.borrow_mut().pop().expect("a request");
    done(String::from("late"));
    assert_eq!(*got.borrow(), [None]);
    assert_eq!(run(&mut shell, 300), 1, "the timer went");
  }

  #[test]
  fn a_launch_waiting_for_its_token_starts_without_it_when_the_shell_stops() {
    use crate::ServicesExt;
    let mut shell = Harness::new(offline());
    let asked: Asked = Asked::default();
    let into = Rc::clone(&asked);
    crate::provide_activation_tokens(&shell.cx, move |app_id, done| {
      into.borrow_mut().push((app_id.map(str::to_owned), done));
    });
    let dir = crate::testing::TempDir::new();
    let out = dir.path().join("started");
    let script = format!("echo \"x$XDG_ACTIVATION_TOKEN\" >> {}", out.display());
    shell.cx.exec_detached(["sh", "-c", script.as_str()]);
    // As `cx.exit` does, before the token comes.
    crate::stop_programs(&shell.cx);
    // The loop doesn't run again, as after an exit.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !out.exists() {
      assert!(Instant::now() < deadline, "the program never started");
      std::thread::sleep(Duration::from_millis(20));
    }
    // Neither the token nor the wait's end starts it again.
    let (_, done) = asked.borrow_mut().pop().expect("a request");
    done(String::from("late"));
    // Its exit may wake the loop once; the timer went.
    assert!(run(&mut shell, 300) <= 2);
    assert_eq!(run(&mut shell, 300), 1, "the timer went");
    assert_eq!(std::fs::read_to_string(&out).expect("its output"), "x\n");
  }

  #[test]
  fn exec_detached_gives_the_program_its_token() {
    use crate::ServicesExt;
    let mut shell = Harness::new(offline());
    let asked: Asked = Asked::default();
    let into = Rc::clone(&asked);
    crate::provide_activation_tokens(&shell.cx, move |app_id, done| {
      into.borrow_mut().push((app_id.map(str::to_owned), done));
    });
    let dir = crate::testing::TempDir::new();
    let out = dir.path().join("token");
    let script = format!(
      "echo \"$XDG_ACTIVATION_TOKEN\" > {0}.tmp && mv {0}.tmp {0}",
      out.display()
    );
    shell.cx.exec_detached(["sh", "-c", script.as_str()]);
    let (app_id, done) = asked.borrow_mut().pop().expect("a request");
    assert_eq!(app_id, None);
    done(String::from("token-3"));
    shell.retry_until("the program's output", || out.exists(), |_| {});
    assert_eq!(
      std::fs::read_to_string(&out).expect("its output"),
      "token-3\n"
    );
  }
}
