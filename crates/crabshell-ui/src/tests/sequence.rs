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
fn a_loop_plays_its_rounds_with_their_pauses_and_finishes_the_round_when_stopped() {
  let h = Harness::new();
  let loading = h.cx.signal(false);
  let sine = Transition::new(1100).easing(Easing::InOutSine);
  let pulse = h.cx.sequence_loop(
    1.0,
    move || loading.get(),
    [Step::hold(280), Step::to(0.4, sine), Step::to(1.0, sine)],
  );
  h.runtime.flush();
  assert!(!animating() && next_timer().is_none());

  loading.set(true);
  h.runtime.flush();
  let start = Instant::now();
  assert!(!tick(start, 0), "round 0's pause sleeps");
  assert_eq!(next_timer(), Some(at(start, 280)));
  assert!(tick(start, 280));
  tick(start, 830);
  assert!(close(pulse.value().get(), 0.7), "{}", pulse.value().get());
  tick(start, 1380);
  assert!(close(pulse.value().get(), 0.4));
  // Round 1 begins at 2480 with its own pause.
  assert!(!tick(start, 2480));
  assert_eq!(pulse.value().get(), 1.0);
  assert_eq!(next_timer(), Some(at(start, 2760)));
  assert!(pulse.running().get());

  // Stopped mid-round, the round runs to its end and rests at its last
  // step's target.
  loading.set(false);
  h.runtime.flush();
  assert!(tick(start, 2760));
  tick(start, 3310);
  assert!(close(pulse.value().get(), 0.7));
  assert!(!tick(start, 4960), "the clock stops at the round's end");
  h.runtime.flush();
  assert_eq!(pulse.value().get(), 1.0);
  assert!(!pulse.running().get());
  assert!(!animating() && next_timer().is_none(), "stopped, it sleeps");

  // Started again it plays from round 0.
  loading.set(true);
  h.runtime.flush();
  let again = at(start, 6000);
  assert!(!tick(again, 0));
  assert_eq!(next_timer(), Some(at(again, 280)));
}

#[test]
fn a_loop_restarted_before_its_round_ends_never_stops() {
  let h = Harness::new();
  let on = h.cx.signal(true);
  let linear = Transition::new(100).easing(Easing::Linear);
  let pulse = h
    .cx
    .sequence_loop(0.0, move || on.get(), [Step::to(1.0, linear)]);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 50);
  on.set(false);
  h.runtime.flush();
  on.set(true);
  h.runtime.flush();
  // Round 1 starts from round 0's end, 1, and goes to 1.
  assert!(tick(start, 150));
  assert!(tick(start, 250));
  assert_eq!(pulse.value().get(), 1.0);
  assert!(pulse.running().get());
}

#[test]
fn a_sequence_of_no_time_jumps_and_an_empty_one_never_runs() {
  let h = Harness::new();
  let on = h.cx.signal(false);
  let jump = h.cx.sequence_loop(
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
  assert!(!tick(start, 0), "a loop of no time plays once");
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
fn a_panicking_running_closure_or_steps_binding_is_logged_and_stops_it() {
  let h = Harness::new();
  let fail = h.cx.signal(false);
  let pulse = h.cx.sequence_loop(
    0.0,
    move || {
      assert!(!fail.get(), "running failure");
      true
    },
    [Step::to(1.0, Transition::new(100))],
  );
  let go = h.cx.signal(0_u32);
  let steps = h.cx.sequence(
    0.0,
    move || go.get(),
    || -> Vec<Step<f32>> { panic!("steps failure") },
  );
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  let logs = capture_logs(|| {
    fail.set(true);
    go.set(1);
    h.runtime.flush();
  });
  assert!(logs.contains("animation cx.sequence_loop at "), "{logs}");
  assert!(logs.contains("running failure") && logs.contains("steps failure"));
  assert!(!steps.running().get());
  // The loop finishes its round, as one stopped would.
  assert!(!tick(start, 100));
  assert_eq!(pulse.value().get(), 1.0);
  assert!(!pulse.running().get() && !animating());
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
  let pulse = child.sequence_loop(0.0, || true, [Step::to(1.0, Transition::new(100))]);
  let entrance = child.sequence_now(0.0, [Step::hold(1000), Step::to(1.0, Transition::new(10))]);
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
  assert!(pulse.value().try_get().is_none());
  assert!(entrance.value().try_get().is_none());
}

#[test]
fn a_play_replaced_in_its_hold_frees_its_sleeper_so_it_never_wakes() {
  let h = Harness::new();
  let moves = h.cx.signal(0_u32);
  let fade = h.cx.sequence(
    1.0,
    move || moves.get(),
    [Step::hold(5000), Step::to(0.0, Transition::new(100))],
  );
  h.runtime.flush();
  let start = Instant::now();
  // A pointer at 125 Hz restarts the idle fade every 8 ms.
  for n in 1..=125 {
    moves.set(n);
    h.runtime.flush();
    assert!(
      !tick(start, u64::from(n) * 8),
      "each play sleeps in its hold"
    );
  }
  assert_eq!(crate::animation::asleep(), 1, "only the last play sleeps");
  assert_eq!(
    next_timer(),
    Some(at(start, 1000 + 5000)),
    "and only it wakes"
  );
  assert!(fade.running().get() && fade.value().get() == 1.0);
}

/// A value whose `PartialEq`, a config's own, panics near 0.5.
#[derive(Clone, Debug)]
struct Touchy(f32);

impl PartialEq for Touchy {
  fn eq(&self, other: &Touchy) -> bool {
    for side in [self.0, other.0] {
      assert!((side - 0.5).abs() > 0.1, "eq failure");
    }
    self.0 == other.0
  }
}

impl crate::Animate for Touchy {
  fn lerp(&self, to: &Touchy, t: f32) -> Touchy {
    Touchy(self.0 + (to.0 - self.0) * t)
  }
}

#[test]
fn a_panicking_partial_eq_is_logged_and_running_ends() {
  let h = Harness::new();
  let go = h.cx.signal(0_u32);
  let linear = Transition::new(100).easing(Easing::Linear);
  let seq = h.cx.sequence(
    Touchy(0.0),
    move || go.get(),
    [Step::to(Touchy(1.0), linear)],
  );
  h.runtime.flush();
  go.set(1);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  let logs = capture_logs(|| assert!(!tick(start, 50), "the play stops"));
  assert!(logs.contains("animation cx.sequence at "), "{logs}");
  assert!(logs.contains("eq failure"), "{logs}");
  h.runtime.flush();
  assert!(!seq.running().get(), "and running ends");
  assert!(!animating());
}

#[test]
fn a_panicking_partial_eq_at_the_end_of_a_play_ends_running() {
  let h = Harness::new();
  let go = h.cx.signal(0_u32);
  // One step of 0 ms: the play's only write is the one at its end.
  let seq = h.cx.sequence(
    Touchy(0.0),
    move || go.get(),
    [Step::to(Touchy(0.5), Transition::new(0))],
  );
  h.runtime.flush();
  go.set(1);
  h.runtime.flush();
  let start = Instant::now();
  let logs = capture_logs(|| assert!(!tick(start, 0), "the play stops"));
  assert!(logs.contains("eq failure"), "{logs}");
  h.runtime.flush();
  assert!(!seq.running().get(), "and running ends");
  assert!(!animating());
}

#[test]
fn a_sequence_made_now_plays_its_entrance_at_once_then_sleeps() {
  let h = Harness::new();
  let linear = Transition::new(200).easing(Easing::Linear);
  // A card's staggered entrance: wait its turn, then fade in.
  let card = h
    .cx
    .sequence_now(0.0, [Step::hold(120), Step::to(1.0, linear)]);
  assert!(card.running().get(), "running from when it is made");
  assert_eq!(card.value().get(), 0.0);
  let start = Instant::now();
  assert!(!tick(start, 0), "the hold takes it off the clock");
  assert_eq!(next_timer(), Some(at(start, 120)));
  assert!(tick(start, 120));
  tick(start, 220);
  assert!(close(card.value().get(), 0.5), "{}", card.value().get());
  assert!(!tick(start, 320), "the clock stops at the end");
  h.runtime.flush();
  assert_eq!(card.value().get(), 1.0);
  assert!(!card.running().get());
  assert!(!animating() && next_timer().is_none(), "settled, it sleeps");
}

#[test]
fn a_panicking_steps_binding_made_now_is_logged_and_never_runs() {
  let h = Harness::new();
  let logs = capture_logs(|| {
    let broken = h
      .cx
      .sequence_now(0.0, || -> Vec<Step<f32>> { panic!("steps failure") });
    assert!(!broken.running().get());
  });
  assert!(logs.contains("animation cx.sequence_now at "), "{logs}");
  assert!(logs.contains("tests/sequence.rs"), "{logs}");
  assert!(logs.contains("steps failure"), "{logs}");
  assert!(!animating() && next_timer().is_none());
}
