//! What the supervisor does when the shell exits: when to start it again,
//! and when to fall back to the last build that worked. Plain state, so
//! the tests can drive it with made-up times.

use std::time::{Duration, Instant};

/// The first wait before a crashed shell starts again.
pub(crate) const FIRST_DELAY: Duration = Duration::from_secs(1);

/// The longest wait: each crash doubles the last one up to this.
pub(crate) const MAX_DELAY: Duration = Duration::from_secs(30);

/// How long a shell runs after it said `ready` before it counts as
/// working: then its build is kept as `last-good`, and the wait goes back
/// to [`FIRST_DELAY`].
pub(crate) const HEALTHY_AFTER: Duration = Duration::from_secs(60);

/// How many crashes within [`HEALTHY_AFTER`] of a new build's first start
/// make the supervisor start `last-good` instead.
pub(crate) const CRASHES_BEFORE_FALLBACK: usize = 3;

/// The exit status the shell uses when there is no compositor, or the
/// connection to it is lost, as at logout: `crabshell::EXIT_NO_COMPOSITOR`.
pub(crate) const EXIT_NO_COMPOSITOR: i32 = 2;

/// How the shell ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Exit {
  /// Status 0: it stopped on purpose.
  Clean,
  /// [`EXIT_NO_COMPOSITOR`]: there is no session to draw in.
  NoCompositor,
  /// Any other status, a signal, or failing to start.
  Crashed,
}

impl Exit {
  /// How a status `code` reads; `None` for death by a signal.
  pub(crate) fn from_code(code: Option<i32>) -> Exit {
    match code {
      Some(0) => Exit::Clean,
      Some(EXIT_NO_COMPOSITOR) => Exit::NoCompositor,
      _ => Exit::Crashed,
    }
  }
}

/// Which binary to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Binary {
  /// The newest build.
  Build,
  /// The copy of the last build that ran for [`HEALTHY_AFTER`] after it
  /// said `ready`.
  LastGood,
}

/// What to do after the shell exited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
  /// Stop supervising.
  Stop,
  /// Start `binary` after `after`.
  Restart { after: Duration, binary: Binary },
}

/// The supervisor's memory of crashes and waits.
#[derive(Debug)]
pub(crate) struct Policy {
  /// The binary that runs, or ran last.
  pub(crate) binary: Binary,
  /// When the newest build first started, for counting its crashes.
  build_started: Option<Instant>,
  /// Its crashes since then.
  crashes: usize,
  delay: Duration,
  /// The running shell said `ready`.
  said_ready: bool,
}

impl Policy {
  /// Starting with `binary`.
  pub(crate) fn new(binary: Binary) -> Policy {
    Policy {
      binary,
      build_started: None,
      crashes: 0,
      delay: FIRST_DELAY,
      said_ready: false,
    }
  }

  /// A new build replaces the old one: it starts with no crashes, and
  /// leaves `last-good`.
  pub(crate) fn new_build(&mut self) {
    *self = Policy::new(Binary::Build);
  }

  /// A shell is starting now.
  pub(crate) fn started(&mut self, now: Instant) {
    self.said_ready = false;
    if self.binary == Binary::Build && self.build_started.is_none() {
      self.build_started = Some(now);
    }
  }

  /// The shell said `ready` at `now`. Returns when it counts as healthy,
  /// [`HEALTHY_AFTER`] later, or `None` if it said so already since it
  /// started. A shell that never says it never counts.
  pub(crate) fn ready(&mut self, now: Instant) -> Option<Instant> {
    if std::mem::replace(&mut self.said_ready, true) {
      return None;
    }
    Some(now + HEALTHY_AFTER)
  }

  /// The shell has run for [`HEALTHY_AFTER`] since `ready`: the next crash waits
  /// [`FIRST_DELAY`] again.
  pub(crate) fn healthy(&mut self) {
    self.delay = FIRST_DELAY;
  }

  /// The shell ended as `exit` at `now`. `has_last_good` says whether a
  /// last good build exists to fall back to.
  pub(crate) fn exited(&mut self, now: Instant, exit: Exit, has_last_good: bool) -> Next {
    if exit != Exit::Crashed {
      return Next::Stop;
    }
    let after = self.delay;
    self.delay = (self.delay * 2).min(MAX_DELAY);
    if self.binary == Binary::Build
      && self
        .build_started
        .is_some_and(|started| now.duration_since(started) <= HEALTHY_AFTER)
    {
      self.crashes += 1;
      if self.crashes >= CRASHES_BEFORE_FALLBACK && has_last_good {
        self.binary = Binary::LastGood;
        self.delay = FIRST_DELAY;
        return Next::Restart {
          after: FIRST_DELAY,
          binary: Binary::LastGood,
        };
      }
    }
    Next::Restart {
      after,
      binary: self.binary,
    }
  }
}
