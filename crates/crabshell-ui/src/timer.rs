//! Timers: callbacks that run once after a delay or again every period,
//! from a table the platform wakes for only while a timer runs.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe, Location};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::{Owner, Signal, untrack};

use crate::cx::Cx;
use crate::input::panic_message;

/// The shortest interval, so a period of 0 can't spin the loop.
const MIN_PERIOD: Duration = Duration::from_millis(1);
/// The longest delay or period, so adding it to now can't overflow.
const MAX_PERIOD: Duration = Duration::from_secs(u32::MAX as u64);

/// A timer made by [`Cx::after`] or [`Cx::interval`]. It is a `Copy`
/// handle, like a signal, so closures capture it without cloning. Calls on
/// a timer whose context is gone do nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timer {
  id: u64,
  /// The table is per thread, like the reactive runtime.
  _not_send: PhantomData<*const ()>,
}

struct Entry {
  owner: Owner,
  /// Names the timer in log lines, as `cx.after at src/main.rs:12`.
  label: Rc<str>,
  period: Duration,
  repeats: bool,
  /// When it fires next. `None` while stopped.
  deadline: Option<Instant>,
  /// Whether it is running, for bindings to read.
  running: Signal<bool>,
  /// `None` while the callback runs.
  callback: Option<Box<dyn FnMut()>>,
}

thread_local! {
  /// Ids are never reused, so a stale handle never reaches a newer timer.
  static TIMERS: RefCell<(u64, BTreeMap<u64, Entry>)> = const { RefCell::new((0, BTreeMap::new())) };
}

impl Cx {
  /// Runs `f` once, `delay` from now. The timer belongs to this context
  /// and goes with it; [`Timer::restart`] runs it again.
  ///
  /// ```
  /// use std::time::Duration;
  /// use crabshell_ui::Cx;
  ///
  /// fn osd(cx: &Cx) {
  ///     let hide = cx.after(Duration::from_millis(1500), || {});
  ///     // Show the OSD while `hide.running()`, and call `hide.restart()`
  ///     // on each change.
  /// #   let _ = hide;
  /// }
  /// ```
  #[track_caller]
  pub fn after(&self, delay: Duration, f: impl FnMut() + 'static) -> Timer {
    let label = format!("cx.after at {}", Location::caller());
    add(self, label, delay.min(MAX_PERIOD), false, Box::new(f))
  }

  /// Runs `f` every `period`, the first time `period` from now. A late
  /// wakeup fires once and keeps the schedule; after a stall longer than
  /// a period the next fire is a period from then, never a burst. The
  /// timer belongs to this context and goes with it.
  #[track_caller]
  pub fn interval(&self, period: Duration, f: impl FnMut() + 'static) -> Timer {
    let label = format!("cx.interval at {}", Location::caller());
    let period = period.clamp(MIN_PERIOD, MAX_PERIOD);
    add(self, label, period, true, Box::new(f))
  }
}

fn add(cx: &Cx, label: String, period: Duration, repeats: bool, f: Box<dyn FnMut()>) -> Timer {
  let running = cx.signal(true);
  let entry = Entry {
    owner: cx.owner(),
    label: label.into(),
    period,
    repeats,
    deadline: Some(Instant::now() + period),
    running,
    callback: Some(f),
  };
  let id = TIMERS.with(|timers| {
    let (next, table) = &mut *timers.borrow_mut();
    *next += 1;
    table.insert(*next, entry);
    *next
  });
  Timer {
    id,
    _not_send: PhantomData,
  }
}

impl Timer {
  /// Stops the timer before it fires again.
  pub fn stop(&self) {
    self.set_deadline(|_| None);
  }

  /// Counts the delay or period again from now, so a timer restarted on
  /// every change fires only once the changes stop. It also starts a
  /// one-shot that fired or was stopped.
  pub fn restart(&self) {
    self.set_deadline(|period| Some(Instant::now() + period));
  }

  /// Whether the timer will fire. Read in a binding, it reruns the binding
  /// when that changes.
  pub fn running(&self) -> bool {
    let running = TIMERS.with(|timers| timers.borrow().1.get(&self.id).map(|entry| entry.running));
    running
      .and_then(|running| running.try_get())
      .unwrap_or(false)
  }

  fn set_deadline(&self, deadline: impl FnOnce(Duration) -> Option<Instant>) {
    let changed = TIMERS.with(|timers| {
      let mut timers = timers.borrow_mut();
      let entry = timers.1.get_mut(&self.id)?;
      if entry.owner.is_disposed() {
        return None;
      }
      entry.deadline = deadline(entry.period);
      Some((entry.running, entry.deadline.is_some()))
    });
    if let Some((signal, running)) = changed {
      set_running(signal, running);
    }
  }
}

/// Writes `running` only when it changes, since a signal notifies on
/// every write.
fn set_running(signal: Signal<bool>, running: bool) {
  if untrack(|| signal.try_with(|&was| was != running)) == Some(true) {
    signal.try_set(running);
  }
}

/// When the earliest running timer is due, for the platform to wake then.
/// `None` means no timer runs and nothing needs waking. Timers whose
/// context is gone are freed first, so they never wake the loop.
pub fn next_timer() -> Option<Instant> {
  sweep();
  TIMERS.with(|timers| {
    let timers = timers.borrow();
    timers.1.values().filter_map(|entry| entry.deadline).min()
  })
}

/// Runs every timer due at `now`, earliest first. A timer started while
/// they run first fires at a later call. One whose callback panics is
/// logged and stops; the others still fire.
pub fn fire_timers(now: Instant) {
  sweep();
  let mut due: Vec<(Instant, u64)> = TIMERS.with(|timers| {
    let timers = timers.borrow();
    let due = timers.1.iter().filter_map(|(&id, entry)| {
      let deadline = entry.deadline.filter(|&deadline| deadline <= now)?;
      Some((deadline, id))
    });
    due.collect()
  });
  due.sort_unstable();
  for (_, id) in due {
    fire(id, now);
  }
}

fn fire(id: u64, now: Instant) {
  // The callback runs with the table released, so it may start, stop or
  // restart any timer, itself included.
  let taken = TIMERS.with(|timers| {
    let mut timers = timers.borrow_mut();
    let entry = timers.1.get_mut(&id)?;
    // An earlier callback may have stopped it, restarted it or dropped
    // its context.
    let deadline = entry.deadline.filter(|&deadline| deadline <= now)?;
    if entry.owner.is_disposed() {
      return None;
    }
    let callback = entry.callback.take()?;
    entry.deadline = entry.repeats.then(|| {
      let next = deadline + entry.period;
      if next > now { next } else { now + entry.period }
    });
    let entry = &*entry;
    Some((
      callback,
      Rc::clone(&entry.label),
      entry.owner,
      entry.running,
      entry.deadline.is_some(),
    ))
  });
  let Some((mut callback, label, owner, running, again)) = taken else {
    return;
  };
  if !again {
    set_running(running, false);
  }
  let panicked = panic::catch_unwind(AssertUnwindSafe(&mut callback)).err();
  if let Some(payload) = &panicked {
    let place = owner
      .name()
      .map(|name| format!(" in {name}"))
      .unwrap_or_default();
    tracing::error!(
      "timer {label}{place} panicked and stops: {}",
      panic_message(payload.as_ref())
    );
  }
  let orphan = TIMERS.with(|timers| {
    let mut timers = timers.borrow_mut();
    let Some(entry) = timers.1.get_mut(&id) else {
      return Some(callback);
    };
    if panicked.is_some() {
      entry.deadline = None;
    }
    entry.callback = Some(callback);
    None
  });
  if panicked.is_some() {
    set_running(running, false);
  }
  // Freed while it ran, by a sweep inside it.
  if let Some(callback) = orphan {
    drop_isolated(&label, callback);
  }
}

/// Frees the timers whose context is gone, dropping their callbacks with
/// the table released.
fn sweep() {
  let gone: Vec<Entry> = TIMERS.with(|timers| {
    let mut timers = timers.borrow_mut();
    let ids: Vec<u64> = timers
      .1
      .iter()
      .filter(|(_, entry)| entry.owner.is_disposed())
      .map(|(&id, _)| id)
      .collect();
    ids.iter().filter_map(|id| timers.1.remove(id)).collect()
  });
  for entry in gone {
    if let Some(callback) = entry.callback {
      drop_isolated(&entry.label, callback);
    }
  }
}

/// Drops a callback, and the user's values it holds, isolated.
fn drop_isolated(label: &str, callback: Box<dyn FnMut()>) {
  if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(move || drop(callback))) {
    tracing::error!(
      "dropping timer {label} panicked: {}",
      panic_message(payload.as_ref())
    );
  }
}
