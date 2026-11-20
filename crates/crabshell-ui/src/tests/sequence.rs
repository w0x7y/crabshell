//! Keyframe sequences: steps played one after another, holds that sleep
//! off the clock, and loops.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::{Harness, capture_logs};
use crate::{Easing, Step, Transition, advance_animations, animating, fire_timers, next_timer};

fn close(a: f32, b: f32) -> bool {
  (a - b).abs() < 0.01
}

fn at(start: Instant, ms: u64) -> Instant {
  start + Duration::from_millis(ms)
}

/// Wakes what is due at `ms` past `start`, then ticks the clock there,
/// as the platform does, returning whether the clock still runs.
fn tick(start: Instant, ms: u64) -> bool {
  fire_timers(at(start, ms));
  advance_animations(at(start, ms))
}

/// Rime's lock screen shake.
const SHAKE: [Step<f32>; 5] = [
  Step::to(-14.0, Transition::new(70).easing(Easing::OutQuad)),
  Step::to(12.0, Transition::new(90).easing(Easing::InOutQuad)),
  Step::to(-8.0, Transition::new(85).easing(Easing::InOutQuad)),
  Step::to(5.0, Transition::new(80).easing(Easing::InOutQuad)),
  Step::to(0.0, Transition::new(95).easing(Easing::OutQuad)),
];

#[test]
fn a_shake_moves_through_its_steps_then_rests_and_the_clock_stops() {
  let h = Harness::new();
  let shakes = h.cx.signal(0_u32);
  let shake = h.cx.sequence(0.0, move || shakes.get(), SHAKE);
  h.runtime.flush();
  assert!(!animating() && next_timer().is_none(), "nothing plays yet");
  assert!(!shake.running().get());

  shakes.set(1);
  h.runtime.flush();
  assert!(shake.running().get(), "running from the trigger");
  let start = Instant::now();
  assert!(tick(start, 0));
  assert_eq!(shake.value().get(), 0.0);
  tick(start, 35);
  // OutQuad halfway is 0.75.
  assert!(close(shake.value().get(), -10.5), "{}", shake.value().get());
  tick(start, 70);
  assert!(close(shake.value().get(), -14.0), "{}", shake.value().get());
  tick(start, 115);
  // InOutQuad halfway is 0.5, from -14 to 12.
  assert!(close(shake.value().get(), -1.0), "{}", shake.value().get());
  tick(start, 300);
  // 55 ms into the 80 ms step from -8 to 5.
  let eased = Easing::InOutQuad.value_at(55.0 / 80.0);
  assert!(close(shake.value().get(), -8.0 + 13.0 * eased));
  assert!(!tick(start, 420), "the clock stops at the end");
  h.runtime.flush();
  assert_eq!(shake.value().get(), 0.0);
  assert!(!shake.running().get());
  assert!(!animating() && next_timer().is_none(), "settled, it sleeps");

  // The same value again plays nothing.
  shakes.set(1);
  h.runtime.flush();
  assert!(!animating() && !shake.running().get());
}

#[test]
fn a_new_trigger_mid_sequence_starts_over_from_where_it_is() {
  let h = Harness::new();
  let shakes = h.cx.signal(0_u32);
  let shake = h.cx.sequence(0.0, move || shakes.get(), SHAKE);
  h.runtime.flush();
  shakes.set(1);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 115);
  assert!(close(shake.value().get(), -1.0));

  shakes.set(2);
  h.runtime.flush();
  // The new play counts from the clock's last tick, 115, and its first
  // step goes from -1 to -14.
  tick(start, 150);
  assert!(
    close(shake.value().get(), -1.0 - 13.0 * 0.75),
    "{}",
    shake.value().get()
  );
  tick(start, 185);
  assert!(close(shake.value().get(), -14.0), "the old play stopped");
  assert!(tick(start, 534));
  assert!(!tick(start, 535));
  assert_eq!(shake.value().get(), 0.0);
}

#[test]
fn a_hold_sleeps_off_the_clock_and_running_ends_with_the_last_step() {
  let h = Harness::new();
  let unlocking = h.cx.signal(false);
  let exit = h.cx.sequence(
    0.0,
    move || unlocking.get(),
    vec![
      Step::hold(260),
      Step::to(1.0, Transition::new(220).easing(Easing::Linear)),
    ],
  );
  let seen = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&seen);
  let running = exit.running();
  h.cx.effect(move || log.borrow_mut().push(running.get()));
  h.runtime.flush();

  unlocking.set(true);
  h.runtime.flush();
  let start = Instant::now();
  assert!(!tick(start, 0), "the hold takes it off the clock");
  assert!(!animating());
  assert_eq!(
    next_timer(),
    Some(at(start, 260)),
    "it wakes when the hold ends"
  );
  assert!(exit.running().get() && exit.value().get() == 0.0);

  fire_timers(at(start, 259));
  assert!(!animating(), "not before");
  assert!(tick(start, 260));
  assert!(next_timer().is_none());
  assert_eq!(exit.value().get(), 0.0);
  tick(start, 370);
  assert!(close(exit.value().get(), 0.5));
  assert!(!tick(start, 480));
  h.runtime.flush();
  assert_eq!(exit.value().get(), 1.0);
  assert_eq!(*seen.borrow(), [false, true, false], "one edge each way");
  assert!(!animating() && next_timer().is_none());
}

#[test]
fn a_hold_between_moves_sleeps_and_a_late_wake_lands_on_the_timeline() {
  let h = Harness::new();
  let go = h.cx.signal(0_u32);
  let linear = Transition::new(100).easing(Easing::Linear);
  let seq = h.cx.sequence(
    0.0,
    move || go.get(),
    [
      Step::to(10.0, linear),
      Step::hold(300),
      Step::hold(200),
      Step::to(20.0, linear.delay(50)),
    ],
  );
  h.runtime.flush();
  go.set(1);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 50);
  assert!(close(seq.value().get(), 5.0));
  // A tick past the move's end lands on its target and sleeps through the
  // holds and the next step's delay together.
  assert!(!tick(start, 120));
  assert_eq!(seq.value().get(), 10.0);
  assert_eq!(next_timer(), Some(at(start, 650)));
  // A wake 50 ms late moves to where the timeline says.
  assert!(tick(start, 700));
  assert!(close(seq.value().get(), 15.0), "{}", seq.value().get());
  assert!(!tick(start, 750));
  assert_eq!(seq.value().get(), 20.0);
}

#[test]
fn a_sequence_of_no_time_jumps_and_an_empty_one_never_runs() {
  let h = Harness::new();
  let on = h.cx.signal(false);
  let jump = h.cx.sequence(
    0.0,
    move || on.get(),
    [
      Step::to(-40.0, Transition::new(0)),
      Step::to(3.0, Transition::new(0)),
    ],
  );
  let empty = h.cx.sequence(0.0, move || on.get(), Vec::new());
  h.runtime.flush();
  on.set(true);
  h.runtime.flush();
  assert!(!empty.running().get(), "nothing to play");
  let start = Instant::now();
  assert!(!tick(start, 0), "it ends at its first tick");
  h.runtime.flush();
  assert_eq!(jump.value().get(), 3.0);
  assert!(!jump.running().get() && !animating());
}

#[test]
fn steps_can_be_a_binding_read_as_each_play_starts() {
  let h = Harness::new();
  let go = h.cx.signal(0_u32);
  let far = h.cx.signal(10.0_f32);
  let linear = Transition::new(100).easing(Easing::Linear);
  let seq = h.cx.sequence(
    0.0,
    move || go.get(),
    move || vec![Step::to(far.get(), linear)],
  );
  h.runtime.flush();
  go.set(1);
  h.runtime.flush();
  // Untracked: changing it mid-play changes nothing.
  far.set(20.0);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  assert!(!tick(start, 100));
  assert_eq!(seq.value().get(), 10.0);
  go.set(2);
  h.runtime.flush();
  tick(start, 200);
  assert!(!tick(start, 300));
  assert_eq!(seq.value().get(), 20.0);
}

#[test]
fn a_panicking_trigger_is_logged_with_its_call_site_and_only_that_sequence_stops() {
  let h = Harness::new();
  let go = h.cx.signal(0_u32);
  let linear = Transition::new(100).easing(Easing::Linear);
  let broken = h.cx.sequence(
    0.0,
    move || {
      let n = go.get();
      assert!(n < 2, "trigger failure");
      n
    },
    [Step::to(1.0, linear)],
  );
  let fine = h.cx.sequence(
    0.0,
    move || go.get(),
    [Step::to(1.0, linear), Step::to(0.0, linear)],
  );
  h.runtime.flush();
  go.set(1);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 50);
  let logs = capture_logs(|| {
    go.set(2);
    h.runtime.flush();
  });
  assert!(logs.contains("animation cx.sequence at "), "{logs}");
  assert!(logs.contains("tests/sequence.rs"), "{logs}");
  assert!(
    logs.contains("panicked and stops: trigger failure"),
    "{logs}"
  );
  assert!(!broken.running().get());
  tick(start, 60);
  assert!(close(broken.value().get(), 0.5), "it stopped where it was");
  // The other started over at 50, from 0.5 toward 1.
  assert!(close(fine.value().get(), 0.55), "{}", fine.value().get());
  go.set(1);
  h.runtime.flush();
  assert!(!broken.running().get(), "and it stays stopped");
}

#[test]
fn a_panicking_steps_binding_is_logged_and_plays_nothing() {
  let h = Harness::new();
  let go = h.cx.signal(0_u32);
  let steps = h.cx.sequence(
    0.0,
    move || go.get(),
    || -> Vec<Step<f32>> { panic!("steps failure") },
  );
  h.runtime.flush();
  let logs = capture_logs(|| {
    go.set(1);
    h.runtime.flush();
  });
  assert!(logs.contains("animation cx.sequence at "), "{logs}");
  assert!(logs.contains("steps failure"), "{logs}");
  assert!(!steps.running().get() && !animating());
}

#[test]
fn everything_goes_with_its_owner_asleep_or_moving() {
  let h = Harness::new();
  h.runtime.flush();
  let before = h.runtime.node_count();
  let child = h.cx.child();
  let go = h.cx.signal(0_u32);
  let moving = child.sequence(0.0, move || go.get(), SHAKE);
  let asleep = child.sequence(
    0.0,
    move || go.get(),
    [Step::hold(1000), Step::to(1.0, Transition::new(10))],
  );
  h.runtime.flush();
  go.set(1);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  assert!(next_timer().is_some(), "one sleeps in its hold");
  child.owner().dispose();
  assert_eq!(h.runtime.node_count(), before + 1, "only `go` is left");
  assert!(
    next_timer().is_none(),
    "the sleeper is freed, so it never wakes"
  );
  assert!(!tick(start, 10), "nothing runs");
  assert!(moving.value().try_get().is_none() && asleep.value().try_get().is_none());
}
