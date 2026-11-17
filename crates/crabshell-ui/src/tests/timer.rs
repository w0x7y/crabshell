//! Timers: one-shots and intervals, driven by fake times through
//! `next_timer` and `fire_timers` as the platform drives them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;

use super::{Counter, capture_logs};
use crate::{Cx, Timer, fire_timers, next_timer};

const MS: Duration = Duration::from_millis(1);

fn root() -> Cx {
  Cx::new(Runtime::current().new_owner())
}

fn hit(counter: &Counter) -> impl FnMut() + 'static {
  let counter = counter.clone();
  move || counter.hit()
}

#[test]
fn a_one_shot_fires_once_at_its_deadline() {
  let cx = root();
  let count = Counter::default();
  let before = Instant::now();
  let timer = cx.after(1500 * MS, hit(&count));
  let deadline = next_timer().expect("armed");
  assert!(deadline >= before + 1500 * MS && deadline <= Instant::now() + 1500 * MS);
  assert!(timer.running());

  fire_timers(deadline - Duration::from_nanos(1));
  assert_eq!(count.get(), 0, "not due yet");
  fire_timers(deadline);
  assert_eq!(count.get(), 1);
  assert!(!timer.running());
  assert_eq!(next_timer(), None, "nothing left to wake for");
  fire_timers(deadline + 10_000 * MS);
  assert_eq!(count.get(), 1, "a one-shot fires once");
}

#[test]
fn an_interval_keeps_its_cadence_and_never_bursts_after_a_stall() {
  let cx = root();
  let count = Counter::default();
  let timer = cx.interval(100 * MS, hit(&count));
  let first = next_timer().expect("armed");
  fire_timers(first);
  assert_eq!(count.get(), 1);
  assert_eq!(next_timer(), Some(first + 100 * MS));

  // Woken 30 ms late: fires once, and the next stays on the schedule.
  fire_timers(first + 130 * MS);
  assert_eq!(count.get(), 2);
  assert_eq!(next_timer(), Some(first + 200 * MS));

  // A stall of a second: one fire, not ten, and the next from now.
  let late = first + 1200 * MS;
  fire_timers(late);
  assert_eq!(count.get(), 3);
  assert_eq!(next_timer(), Some(late + 100 * MS));
  assert!(timer.running());
}

#[test]
fn stop_and_restart_count_again_from_now_and_rearm_a_fired_one_shot() {
  let cx = root();
  let count = Counter::default();
  let timer = cx.after(1000 * MS, hit(&count));
  timer.stop();
  assert!(!timer.running());
  assert_eq!(next_timer(), None);
  fire_timers(Instant::now() + 5000 * MS);
  assert_eq!(count.get(), 0, "a stopped timer never fires");

  let before = Instant::now();
  timer.restart();
  assert!(timer.running());
  let deadline = next_timer().expect("armed again");
  assert!(deadline >= before + 1000 * MS);
  fire_timers(deadline);
  assert_eq!(count.get(), 1);

  timer.restart();
  assert!(timer.running(), "a fired one-shot restarts");
  fire_timers(next_timer().expect("armed again"));
  assert_eq!(count.get(), 2);

  let interval = cx.interval(100 * MS, || {});
  let scheduled = next_timer().expect("armed");
  let before = Instant::now();
  interval.restart();
  let restarted = next_timer().expect("armed");
  assert!(restarted >= before + 100 * MS && restarted >= scheduled);
}

#[test]
fn a_stopped_timer_arms_nothing_and_waits_for_its_first_restart() {
  let cx = root();
  let count = Counter::default();
  let once = cx.after(100 * MS, hit(&count)).stopped();
  let every = cx.interval(100 * MS, hit(&count)).stopped();
  assert!(!once.running() && !every.running());
  assert_eq!(next_timer(), None, "nothing to wake for");
  fire_timers(Instant::now() + 10_000 * MS);
  assert_eq!(count.get(), 0, "neither ever fired");

  once.restart();
  fire_timers(next_timer().expect("the restart armed it"));
  assert_eq!(count.get(), 1);
  every.restart();
  let first = next_timer().expect("the restart armed it");
  fire_timers(first);
  assert_eq!(count.get(), 2);
  assert_eq!(next_timer(), Some(first + 100 * MS), "and it repeats");
}

#[test]
fn running_is_a_signal_bindings_follow() {
  let rt = Runtime::current();
  let cx = root();
  let timer = cx.after(100 * MS, || {});
  let seen = Rc::new(Cell::new(None));
  let record = Rc::clone(&seen);
  cx.effect(move || record.set(Some(timer.running())));
  rt.flush();
  assert_eq!(seen.get(), Some(true));
  fire_timers(next_timer().expect("armed"));
  rt.flush();
  assert_eq!(seen.get(), Some(false), "the binding reran when it fired");
  timer.restart();
  rt.flush();
  assert_eq!(seen.get(), Some(true));
  timer.stop();
  rt.flush();
  assert_eq!(seen.get(), Some(false));
}

#[test]
fn a_callback_may_stop_or_restart_itself_or_start_another_timer() {
  let cx = root();
  // An interval that stops itself on its second fire.
  let fires = Rc::new(Cell::new(0));
  let slot: Rc<Cell<Option<Timer>>> = Rc::default();
  let (counted, own) = (Rc::clone(&fires), Rc::clone(&slot));
  let interval = cx.interval(100 * MS, move || {
    counted.set(counted.get() + 1);
    if counted.get() == 2 {
      own.get().expect("set below").stop();
    }
  });
  slot.set(Some(interval));
  fire_timers(next_timer().expect("armed"));
  fire_timers(next_timer().expect("armed"));
  assert_eq!(fires.get(), 2);
  assert!(!interval.running());
  assert_eq!(next_timer(), None);

  // A one-shot that restarts itself stays running.
  let again = Rc::new(Cell::new(0));
  let slot: Rc<Cell<Option<Timer>>> = Rc::default();
  let (counted, own) = (Rc::clone(&again), Rc::clone(&slot));
  let once = cx.after(100 * MS, move || {
    counted.set(counted.get() + 1);
    own.get().expect("set below").restart();
  });
  slot.set(Some(once));
  fire_timers(next_timer().expect("armed"));
  assert!(once.running());
  fire_timers(next_timer().expect("armed again"));
  assert_eq!(again.get(), 2);
  once.stop();

  // One that starts another: the new one fires at a later call, not in
  // the call that made it.
  let inner = Counter::default();
  let (made, inner_hit) = (cx.clone(), inner.clone());
  cx.after(100 * MS, move || {
    made.after(Duration::ZERO, hit(&inner_hit));
  });
  let far = Instant::now() + 10_000 * MS;
  fire_timers(far);
  assert_eq!(inner.get(), 0, "made during this call");
  fire_timers(far);
  assert_eq!(inner.get(), 1);
}

#[test]
fn due_timers_fire_in_deadline_order() {
  let cx = root();
  let order = Rc::new(RefCell::new(Vec::new()));
  for delay in [300, 100, 200] {
    let order = Rc::clone(&order);
    cx.after(delay * MS, move || order.borrow_mut().push(delay));
  }
  fire_timers(Instant::now() + 1000 * MS);
  assert_eq!(*order.borrow(), [100, 200, 300]);
}

#[test]
fn a_panicking_callback_is_logged_and_stops_and_the_others_still_fire() {
  let cx = Cx::new(Runtime::current().new_owner()).named("PanelWindow[DP-1]/Row");
  let count = Counter::default();
  let broken = cx.interval(100 * MS, || panic!("boom"));
  let fine = cx.interval(100 * MS, hit(&count));
  let logs = capture_logs(|| fire_timers(Instant::now() + 1000 * MS));
  assert!(logs.contains("timer cx.interval at "), "{logs}");
  assert!(logs.contains("tests/timer.rs"), "{logs}");
  assert!(logs.contains("in PanelWindow[DP-1]/Row"), "{logs}");
  assert!(logs.contains("panicked and stops: boom"), "{logs}");
  assert_eq!(count.get(), 1);
  assert!(!broken.running() && fine.running());
  fine.stop();
  assert_eq!(next_timer(), None, "the broken one stopped too");
}

/// Counts its drops, and panics in the drop when asked.
struct Guard(Rc<Cell<u32>>, bool);

impl Drop for Guard {
  fn drop(&mut self) {
    self.0.set(self.0.get() + 1);
    if self.1 {
      panic!("drop failure");
    }
  }
}

#[test]
fn a_gone_owner_frees_its_timers_and_wakes_nothing() {
  let rt = Runtime::current();
  let cx = root();
  let nodes = rt.node_count();
  let drops = Rc::new(Cell::new(0));
  let child = cx.child();
  let guard = Guard(Rc::clone(&drops), false);
  let gone = child.after(100 * MS, move || {
    let _ = &guard;
  });
  let stopped = cx.after(200 * MS, || {});
  stopped.stop();
  let before = Instant::now();
  let live = cx.after(300 * MS, || {});
  assert!(
    next_timer() < Some(before + 300 * MS),
    "the gone one's, still live"
  );

  child.owner().dispose();
  let live_deadline = next_timer().expect("armed");
  assert!(live_deadline >= before + 300 * MS, "the live one's");
  assert_eq!(
    rt.node_count(),
    nodes + 2,
    "only the two timers under the root"
  );
  assert_eq!(
    next_timer(),
    Some(live_deadline),
    "skips stopped and gone ones"
  );
  assert_eq!(drops.get(), 1, "the closure is freed");
  assert!(!gone.running());
  gone.restart();
  assert_eq!(
    next_timer(),
    Some(live_deadline),
    "a gone timer can't restart"
  );
  live.stop();
  assert_eq!(next_timer(), None);

  // A closure whose drop panics is freed isolated.
  let child = cx.child();
  let guard = Guard(Rc::clone(&drops), true);
  child.after(100 * MS, move || {
    let _ = &guard;
  });
  child.owner().dispose();
  let logs = capture_logs(|| assert_eq!(next_timer(), None));
  assert!(logs.contains("dropping timer cx.after at "), "{logs}");
  assert!(logs.contains("drop failure"), "{logs}");
  assert_eq!(drops.get(), 2);
}
