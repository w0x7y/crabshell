//! Tests for the reactive core, in three parts: signals, effects, flush and
//! owners; memos and untrack; then scoped effects, panics and drops. The
//! tests the design doc requires for Phase 1 are marked.
//!
//! Each test runs on its own thread, so each starts with an empty runtime.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::runtime::is_unborrowed;
use crate::{MAX_RUNS_PER_FLUSH, Memo, Owner, Runtime, Signal, untrack};

/// Counts how many times a closure ran.
#[derive(Clone, Default)]
struct Counter(Rc<Cell<u32>>);

impl Counter {
  fn hit(&self) {
    self.0.set(self.0.get() + 1);
  }

  fn get(&self) -> u32 {
    self.0.get()
  }
}

/// Runs `f` and returns everything it logged through `tracing`.
fn capture_logs(f: impl FnOnce()) -> String {
  #[derive(Clone, Default)]
  struct Buffer(Arc<Mutex<Vec<u8>>>);

  impl io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      self
        .0
        .lock()
        .expect("log buffer lock")
        .extend_from_slice(bytes);
      Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  let buffer = Buffer::default();
  let writer = buffer.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_writer(move || writer.clone())
    .finish();
  tracing::subscriber::with_default(subscriber, f);
  let bytes = buffer.0.lock().expect("log buffer lock").clone();
  String::from_utf8(bytes).expect("log output is UTF-8")
}

// ---- Signals, effects, flush and owners ----

// Required by the design doc for Phase 1.
#[test]
fn after_a_branch_switch_the_old_branch_no_longer_triggers() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let use_left = owner.signal(true);
  let left = owner.signal(0);
  let right = owner.signal(0);
  let runs = Counter::default();
  let counter = runs.clone();
  owner.effect(move || {
    counter.hit();
    if use_left.get() {
      left.get();
    } else {
      right.get();
    }
  });
  rt.flush();
  assert_eq!(runs.get(), 1);

  use_left.set(false);
  rt.flush();
  assert_eq!(runs.get(), 2);

  left.set(1);
  rt.flush();
  assert_eq!(runs.get(), 2, "the effect stopped reading `left`");

  right.set(1);
  rt.flush();
  assert_eq!(runs.get(), 3);
}

// Required by the design doc for Phase 1.
#[test]
fn self_feeding_effect_stops_after_100_rounds_and_others_still_run() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let trigger = owner.signal(0);
  let feedback = owner.signal(0);
  let before_runs = Counter::default();
  let loop_runs = Counter::default();
  let after_runs = Counter::default();

  let counter = before_runs.clone();
  owner.effect(move || {
    counter.hit();
    trigger.get();
  });
  let counter = loop_runs.clone();
  owner
    .effect(move || {
      counter.hit();
      trigger.get();
      feedback.set(feedback.get() + 1);
    })
    .named("bar[eDP-1]/Row/Text.text");
  let counter = after_runs.clone();
  owner.effect(move || {
    counter.hit();
    trigger.get();
  });

  let logs = capture_logs(|| rt.flush());
  assert_eq!(loop_runs.get(), MAX_RUNS_PER_FLUSH);
  assert_eq!(feedback.get_untracked(), 100);
  assert_eq!((before_runs.get(), after_runs.get()), (1, 1));
  assert!(
    logs.contains("effect bar[eDP-1]/Row/Text.text is in a dependency cycle"),
    "{logs}"
  );

  // The looping effect stays disabled; the others keep working.
  trigger.set(1);
  rt.flush();
  assert_eq!(loop_runs.get(), MAX_RUNS_PER_FLUSH);
  assert_eq!((before_runs.get(), after_runs.get()), (2, 2));
}

#[test]
fn effects_first_run_at_flush_and_set_runs_nothing() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let runs = Counter::default();
  let counter = runs.clone();
  owner.effect(move || {
    counter.hit();
    n.get();
  });
  assert_eq!(runs.get(), 0, "creating an effect only queues it");
  rt.flush();
  assert_eq!(runs.get(), 1);

  n.set(1);
  assert_eq!(runs.get(), 1, "set never runs effects on the spot");
  rt.flush();
  assert_eq!(runs.get(), 2, "set notifies even when the value is equal");

  n.update(|_| {});
  rt.flush();
  assert_eq!(runs.get(), 3);

  rt.flush();
  assert_eq!(runs.get(), 3, "nothing was queued");
}

#[test]
fn effects_run_in_creation_order_even_in_reused_slots() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let scratch = rt.new_owner();
  scratch.signal(());
  scratch.signal(());
  let tick = owner.signal(());
  let order = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&order);
  owner.effect(move || {
    tick.get();
    log.borrow_mut().push("first");
  });
  // The next two effects reuse the slots `scratch` frees, which come
  // before the first effect's slot.
  scratch.dispose();
  for name in ["second", "third"] {
    let log = Rc::clone(&order);
    owner.effect(move || {
      tick.get();
      log.borrow_mut().push(name);
    });
  }
  rt.flush();
  // A `set` queues the three in whatever order the signal's subscriber
  // list gives, and the flush still runs them in creation order.
  tick.set(());
  rt.flush();
  assert_eq!(
    *order.borrow(),
    ["first", "second", "third", "first", "second", "third"]
  );
}

#[test]
fn a_write_during_flush_reruns_an_earlier_effect_in_the_same_flush() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let source = owner.signal(1);
  let mirror = owner.signal(0);
  let seen = Counter::default();
  let shown = Rc::clone(&seen.0);
  // Created first, so it runs before the effect that writes `mirror`.
  owner.effect(move || shown.set(mirror.get()));
  owner.effect(move || mirror.set(source.get() * 10));

  rt.flush();
  assert_eq!(seen.get(), 10);
  source.set(2);
  rt.flush();
  assert_eq!(seen.get(), 20, "a flush leaves nothing stale");
}

#[test]
fn an_effect_created_during_flush_runs_in_the_same_flush() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let inner_runs = Counter::default();
  let counter = inner_runs.clone();
  let created = Cell::new(false);
  owner.effect(move || {
    if !created.replace(true) {
      let counter = counter.clone();
      owner.effect(move || counter.hit());
    }
  });
  rt.flush();
  assert_eq!(inner_runs.get(), 1);
}

#[test]
fn flush_inside_an_effect_runs_nothing() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let trigger = owner.signal(0);
  let n = owner.signal(0);
  let reader_runs = Counter::default();
  let seen_inside = Rc::new(Cell::new(u32::MAX));
  let probe = reader_runs.clone();
  let seen = Rc::clone(&seen_inside);
  owner.effect(move || {
    if trigger.get() == 1 {
      n.set(1); // queues the reader below
      Runtime::current().flush(); // must not run it here
      seen.set(probe.get());
    }
  });
  let counter = reader_runs.clone();
  owner.effect(move || {
    counter.hit();
    n.get();
  });
  rt.flush(); // the reader runs once and starts depending on `n`
  trigger.set(1);
  rt.flush();
  assert_eq!(seen_inside.get(), 1, "the nested flush ran the reader");
  assert_eq!(reader_runs.get(), 2, "the outer flush ran it");
  assert!(is_unborrowed());
}

#[test]
fn cycle_between_two_effects_terminates_and_is_logged() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let a = owner.signal(0);
  let b = owner.signal(0);
  owner.effect(move || b.set(a.get() + 1)).named("one");
  owner.effect(move || a.set(b.get() + 1)).named("two");
  let logs = capture_logs(|| rt.flush());
  assert!(logs.contains("is in a dependency cycle"), "{logs}");
  let fresh_runs = Counter::default();
  let counter = fresh_runs.clone();
  owner.effect(move || counter.hit());
  rt.flush();
  assert_eq!(fresh_runs.get(), 1);
  assert!(is_unborrowed());
}

#[test]
fn read_signal_tracks_like_its_signal() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let view = n.read_only();
  let seen = Counter::default();
  let shown = Rc::clone(&seen.0);
  owner.effect(move || shown.set(view.get()));
  rt.flush();
  n.set(7);
  rt.flush();
  assert_eq!(seen.get(), 7);
  assert_eq!(view.with(|n| n + 1), 8);
}

#[test]
#[should_panic(expected = "Signal<i32> was written while a with() call on it was still running")]
fn writing_a_signal_inside_its_own_with_panics() {
  let owner = Runtime::current().new_owner();
  let n = owner.signal(1);
  n.with(|_| n.set(2));
}

#[test]
#[should_panic(expected = "Signal<i32> was read inside its own update()")]
fn reading_a_signal_inside_its_own_update_panics() {
  let owner = Runtime::current().new_owner();
  let n = owner.signal(1);
  n.update(|_| {
    n.get();
  });
}

#[test]
#[should_panic(expected = "Signal<i32> was used after its owner was disposed")]
fn get_on_a_disposed_signal_panics_with_a_clear_message() {
  let owner = Runtime::current().new_owner();
  let n = owner.signal(1);
  owner.dispose();
  n.get();
}

#[test]
#[should_panic(expected = "Signal<i32> was used after its owner was disposed")]
fn set_on_a_disposed_signal_panics() {
  let owner = Runtime::current().new_owner();
  let n = owner.signal(1);
  owner.dispose();
  n.set(2);
}

#[test]
fn try_set_and_try_update_report_a_disposed_signal() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let runs = Counter::default();
  let counter = runs.clone();
  owner.effect(move || {
    counter.hit();
    n.get();
  });
  rt.flush();
  assert!(n.try_set(2));
  assert!(n.try_update(|n| *n += 1));
  rt.flush();
  assert_eq!((n.get(), runs.get()), (3, 2));

  let token = Rc::new(());
  let held = owner.signal(Rc::clone(&token));
  owner.dispose();
  assert!(!n.try_set(4));
  assert!(!n.try_update(|n| *n += 1));
  assert!(!held.try_set(Rc::clone(&token)));
  assert_eq!(
    Rc::strong_count(&token),
    1,
    "the rejected value was dropped"
  );
}

#[test]
#[should_panic(expected = "tried to create a new signal under an owner that was already disposed")]
fn creating_under_a_disposed_owner_panics() {
  let owner = Runtime::current().new_owner();
  owner.dispose();
  owner.signal(1);
}

#[test]
fn an_owner_says_whether_it_was_disposed() {
  let root = Runtime::current().new_owner();
  let child = root.child();
  assert!(!root.is_disposed() && !child.is_disposed());
  root.dispose();
  assert!(root.is_disposed(), "disposed");
  assert!(child.is_disposed(), "disposed with its parent");
}

#[test]
fn an_owner_is_named_by_itself_or_its_nearest_named_ancestor() {
  let rt = Runtime::current();
  let bar = rt.new_owner().named("bar[DP-1]");
  let row = bar.child();
  assert_eq!(rt.new_owner().name(), None, "nothing named");
  assert_eq!(row.name().as_deref(), Some("bar[DP-1]"));
  let clock = row.child().named("bar[DP-1]/Clock");
  assert_eq!(clock.name().as_deref(), Some("bar[DP-1]/Clock"));
  bar.dispose();
  assert_eq!(clock.name(), None, "disposed");
}

#[test]
fn disposing_a_child_leaves_the_parent_alone() {
  let rt = Runtime::current();
  let before = rt.node_count();
  let root = rt.new_owner();
  let kept = root.signal(1);
  let child = root.child();
  let gone = child.signal(2);

  child.dispose();
  child.dispose(); // a second dispose does nothing
  assert_eq!(gone.try_get(), None);
  assert_eq!(kept.get(), 1);
  assert_eq!(rt.node_count(), before + 2);

  root.dispose();
  assert_eq!(rt.node_count(), before);
}

#[test]
fn effect_can_dispose_its_own_owner() {
  let rt = Runtime::current();
  let before = rt.node_count();
  let owner = rt.new_owner();
  let close = owner.signal(false);
  owner.effect(move || {
    if close.get() {
      owner.dispose();
    }
  });
  rt.flush();
  close.set(true);
  rt.flush();
  assert_eq!(rt.node_count(), before);
  assert!(is_unborrowed());
}

#[test]
fn disposed_effect_still_in_the_queue_does_not_run() {
  let rt = Runtime::current();
  let root = rt.new_owner();
  let n = root.signal(0);
  let widget = root.child();
  let runs = Counter::default();
  let counter = runs.clone();
  widget.effect(move || {
    counter.hit();
    n.get();
  });
  rt.flush();
  n.set(1);
  widget.dispose();
  rt.flush();
  assert_eq!(runs.get(), 1);
}

#[test]
fn effect_can_dispose_a_later_queued_effect_and_create_one_in_its_slot() {
  let rt = Runtime::current();
  let root = rt.new_owner();
  let n = root.signal(0);
  let victim_owner = root.child();
  let victim_runs = Counter::default();
  let new_runs = Counter::default();
  let counter = new_runs.clone();
  // Created first, so it runs first in the round.
  root.effect(move || {
    if n.get() == 1 {
      victim_owner.dispose();
      let counter = counter.clone();
      root.effect(move || counter.hit());
    }
  });
  let counter = victim_runs.clone();
  victim_owner.effect(move || {
    counter.hit();
    n.get();
  });
  rt.flush();
  n.set(1);
  rt.flush();
  assert_eq!((victim_runs.get(), new_runs.get()), (1, 1));
  assert!(is_unborrowed());
}

#[test]
fn handles_into_reused_slots_do_not_alias() {
  let rt = Runtime::current();
  let keep = rt.new_owner();
  let doomed = rt.new_owner();
  let old = doomed.signal(1);
  doomed.dispose();
  // The last slot freed is the first one reused, so this signal lands in
  // the slot `old` points to. Only the key's version tells them apart.
  let fresh = keep.signal(2);
  let (old_slot, old_version) = old.slot();
  let (fresh_slot, fresh_version) = fresh.slot();
  assert_eq!(old_slot, fresh_slot, "the new signal reuses the old slot");
  assert_ne!(old_version, fresh_version);
  assert_eq!((old.try_get(), fresh.get()), (None, 2));
}

/// Bumps a counter signal when dropped, like a user type whose `Drop` uses
/// the runtime.
struct BumpOnDrop(Signal<u32>);

impl Drop for BumpOnDrop {
  fn drop(&mut self) {
    self.0.update(|n| *n += 1);
  }
}

#[test]
fn user_values_and_closures_are_dropped_with_no_runtime_borrow() {
  let rt = Runtime::current();
  let drops = rt.new_owner().signal(0_u32);
  let owner = rt.new_owner();
  let held = owner.signal(BumpOnDrop(drops));
  held.set(BumpOnDrop(drops)); // drops the old value
  assert_eq!(drops.get_untracked(), 1);

  let captured = BumpOnDrop(drops);
  owner.effect(move || {
    let _keep = &captured;
  });
  owner.dispose(); // drops the signal's value and the effect's closure
  assert_eq!(drops.get_untracked(), 3);
}

// ---- Memos and untrack ----

// Required by the design doc for Phase 1.
#[test]
fn diamond_runs_the_effect_once_per_set() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let a = owner.signal(1);
  let b = owner.memo(move || a.get() + 1);
  let c = owner.memo(move || a.get() * 10);
  let seen = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&seen);
  owner.effect(move || log.borrow_mut().push((b.get(), c.get())));

  rt.flush();
  a.set(2);
  rt.flush();

  // One entry per flush: the effect ran once for the single `set`, not
  // once for each of the two memos it reads.
  assert_eq!(*seen.borrow(), [(2, 10), (3, 20)]);
}

// Required by the design doc for Phase 1.
#[test]
fn memo_with_an_equal_value_does_not_rerun_its_dependents() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(2);
  let parity_runs = Counter::default();
  let label_runs = Counter::default();
  let effect_runs = Counter::default();

  let counter = parity_runs.clone();
  let is_even = owner.memo(move || {
    counter.hit();
    n.get() % 2 == 0
  });
  let counter = label_runs.clone();
  let label = owner.memo(move || {
    counter.hit();
    if is_even.get() { "even" } else { "odd" }
  });
  let counter = effect_runs.clone();
  owner.effect(move || {
    counter.hit();
    label.get();
  });
  rt.flush();
  assert_eq!(
    (parity_runs.get(), label_runs.get(), effect_runs.get()),
    (1, 1, 1)
  );

  // 2 -> 4: `is_even` recomputes to the same `true`, so neither the memo
  // that reads it nor the effect further down runs.
  n.set(4);
  rt.flush();
  assert_eq!(
    (parity_runs.get(), label_runs.get(), effect_runs.get()),
    (2, 1, 1)
  );

  // 4 -> 5: the value changes, and everything downstream runs.
  n.set(5);
  rt.flush();
  assert_eq!(
    (parity_runs.get(), label_runs.get(), effect_runs.get()),
    (3, 2, 2)
  );
}

// Required by the design doc for Phase 1.
#[test]
fn disposing_an_owner_frees_every_slot_it_created() {
  let rt = Runtime::current();
  let before = rt.node_count();
  // A value that counts its clones: once it's back to one, every signal
  // value and closure holding a clone has been dropped.
  let token = Rc::new(());

  let root = rt.new_owner();
  let held = root.signal(Rc::clone(&token));
  let child = root.child();
  let strong_count = child.memo(move || held.with(Rc::strong_count));
  let captured = Rc::clone(&token);
  child.effect(move || {
    let _keep = &captured;
    strong_count.get();
  });
  let grandchild = child.child();
  grandchild.signal(Rc::clone(&token));
  rt.flush();

  // root, held, child, strong_count, the effect, grandchild, its signal
  assert_eq!(rt.node_count(), before + 7);
  root.dispose();
  assert_eq!(rt.node_count(), before);
  assert_eq!(Rc::strong_count(&token), 1);
}

// Required by the design doc for Phase 1.
#[test]
fn try_get_on_disposed_handles_returns_none() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let read_only = n.read_only();
  let doubled = owner.memo(move || n.get() * 2);
  assert_eq!(
    (n.try_get(), read_only.try_get(), doubled.try_get()),
    (Some(1), Some(1), Some(2))
  );

  owner.dispose();
  assert_eq!(n.try_get(), None);
  assert_eq!(n.try_with(|n| *n), None);
  assert_eq!(read_only.try_get(), None);
  assert_eq!(doubled.try_get(), None);
}

#[test]
fn memo_computes_only_when_read_while_stale() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let computes = Counter::default();
  let counter = computes.clone();
  let doubled = owner.memo(move || {
    counter.hit();
    n.get() * 2
  });
  rt.flush();
  assert_eq!(computes.get(), 0, "nothing has read it yet");

  assert_eq!(doubled.get(), 2);
  assert_eq!(doubled.get(), 2);
  assert_eq!(computes.get(), 1);

  n.set(2);
  n.set(3);
  rt.flush();
  assert_eq!(computes.get(), 1, "stale, but nothing read it");
  assert_eq!(doubled.get_untracked(), 6);
  assert_eq!(computes.get(), 2);
}

#[test]
fn deep_chain_stops_at_an_equal_middle_value() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let runs: [Counter; 4] = Default::default();
  let [k1, k2, k3, ke] = runs.clone();
  let m1 = owner.memo(move || {
    k1.hit();
    n.get()
  });
  let m2 = owner.memo(move || {
    k2.hit();
    m1.get() > 0
  });
  let m3 = owner.memo(move || {
    k3.hit();
    u8::from(m2.get())
  });
  owner.effect(move || {
    ke.hit();
    m3.get();
  });
  let counts = || runs.each_ref().map(Counter::get);
  rt.flush();
  n.set(2); // m2 stays true: m3 and the effect don't run
  rt.flush();
  assert_eq!(counts(), [2, 2, 1, 1]);
  n.set(-1);
  rt.flush();
  assert_eq!(counts(), [3, 3, 2, 2]);
}

#[test]
fn untracked_reads_record_no_dependency() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let tracked = owner.signal(0);
  let quiet = owner.signal(0);
  let quiet_view = quiet.read_only();
  let quiet_memo = owner.memo(move || quiet.get());
  let runs = Counter::default();
  let counter = runs.clone();
  owner.effect(move || {
    counter.hit();
    tracked.get();
    quiet.get_untracked();
    quiet_view.with_untracked(|_| ());
    quiet_memo.get_untracked();
  });
  rt.flush();
  quiet.set(1);
  rt.flush();
  assert_eq!(runs.get(), 1);
  tracked.set(1);
  rt.flush();
  assert_eq!(runs.get(), 2);
}

#[test]
fn untrack_resumes_tracking_after_a_panic() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let quiet = owner.signal(0);
  let loud = owner.signal(0);
  let runs = Counter::default();
  let counter = runs.clone();
  owner.effect(move || {
    counter.hit();
    let caught = panic::catch_unwind(AssertUnwindSafe(|| {
      untrack(|| {
        quiet.get();
        panic!("inside untrack");
      })
    }));
    assert!(caught.is_err());
    loud.get(); // tracked again
  });
  rt.flush();
  quiet.set(1);
  rt.flush();
  assert_eq!(runs.get(), 1, "the read inside untrack wasn't recorded");
  loud.set(1);
  rt.flush();
  assert_eq!(runs.get(), 2, "the read after it was");
  assert_eq!(untrack(|| 5), 5, "outside any effect it just runs f");
}

#[test]
fn disposing_owners_removes_their_edges_from_long_lived_signals() {
  let rt = Runtime::current();
  let service = rt.new_owner();
  let clock = service.signal(0);
  let doubled = service.memo(move || clock.get() * 2);
  doubled.get();
  let (start_subscribers, start_nodes) = (clock.subscriber_count(), rt.node_count());
  for _ in 0..50 {
    let window = rt.new_owner().child();
    window.effect(move || {
      clock.get();
      doubled.get();
    });
    let local = window.memo(move || clock.get() + doubled.get());
    window.effect(move || {
      local.get();
    });
    rt.flush();
    window.dispose();
  }
  assert_eq!(clock.subscriber_count(), start_subscribers);
  assert_eq!(doubled.subscriber_count(), 0);
  // The 50 root owners above were never disposed; only they remain.
  assert_eq!(rt.node_count(), start_nodes + 50);
}

#[test]
fn handles_compare_hash_and_print_by_key_alone() {
  /// Implements none of the traits, to show the handles don't need them.
  struct Plain;

  let owner = Runtime::current().new_owner();
  let a = owner.signal(Plain);
  let b = owner.signal(Plain);
  let a_again = a;
  assert_eq!(a, a_again);
  assert_ne!(a, b);
  assert_eq!(a.read_only(), a_again.read_only());
  assert_ne!(a.read_only(), b.read_only());
  let unique: HashSet<Signal<Plain>> = [a, b, a_again].into_iter().collect();
  assert_eq!(unique.len(), 2);

  let memo = owner.memo(|| 1);
  let effect = owner.effect(|| ());
  let child = owner.child();
  assert_ne!(owner, child);
  let printed = [
    format!("{a:?}"),
    format!("{:?}", a.read_only()),
    format!("{memo:?}"),
    format!("{effect:?}"),
    format!("{owner:?}"),
  ];
  for (text, name) in printed
    .iter()
    .zip(["Signal", "ReadSignal", "Memo", "Effect", "Owner"])
  {
    assert!(text.starts_with(&format!("{name}(NodeId(")), "{text}");
  }
  assert_eq!(
    printed[0].strip_prefix("Signal"),
    printed[1].strip_prefix("ReadSignal"),
    "a view prints its signal's key"
  );
}

// ---- Scoped effects, panics and drops ----

// Required by the design doc for Phase 1.
#[test]
fn panicking_effect_is_disabled_and_the_runtime_stays_usable() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(0);
  let bad_runs = Counter::default();
  let good_runs = Counter::default();

  let counter = bad_runs.clone();
  owner
    .effect(move || {
      // Panics while `with` holds a borrow of the signal's value.
      n.with(|n| assert!(*n != 1, "boom at {n}"));
      counter.hit();
    })
    .named("bar/Clock.text");
  let counter = good_runs.clone();
  owner.effect(move || {
    counter.hit();
    n.get();
  });
  rt.flush();
  assert_eq!((bad_runs.get(), good_runs.get()), (1, 1));

  n.set(1);
  let logs = capture_logs(|| rt.flush());
  assert!(
    logs.contains("effect bar/Clock.text panicked and is disabled: boom at 1"),
    "{logs}"
  );
  assert_eq!((bad_runs.get(), good_runs.get()), (1, 2));
  assert!(is_unborrowed(), "the runtime is left borrowed");

  // The next set and flush work. `update` needs a mutable borrow of the
  // value, so it also shows the panic released the borrow `with` held.
  n.set(2);
  n.update(|n| *n += 1);
  rt.flush();
  assert_eq!((bad_runs.get(), good_runs.get()), (1, 3));
  assert_eq!(n.get_untracked(), 3);
  assert!(is_unborrowed());
}

#[test]
fn unnamed_nodes_are_described_by_number_and_owner() {
  let rt = Runtime::current();
  let owner = rt.new_owner().named("bar[eDP-1]/Clock");
  owner.effect(|| panic!("unnamed"));
  let logs = capture_logs(|| rt.flush());
  assert!(
    logs.contains("unnamed effect #1 in bar[eDP-1]/Clock"),
    "{logs}"
  );
}

#[test]
fn log_lines_name_the_nearest_named_ancestor() {
  let rt = Runtime::current();
  let tray = rt.new_owner().named("bar[eDP-1]/Tray");
  tray.effect_scoped(|scope| {
    let row = scope.child();
    row.effect(|| panic!("row boom"));
  });
  let logs = capture_logs(|| rt.flush());
  assert!(
    logs.contains("in bar[eDP-1]/Tray panicked and is disabled: row boom"),
    "{logs}"
  );
}

#[test]
fn panicking_memo_keeps_its_last_value_and_its_dependents_stay_put() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let tens = owner
    .memo(move || {
      let n = n.get();
      assert!(n != 2, "memo boom");
      n * 10
    })
    .named("bar/Volume.percent");
  let runs = Counter::default();
  let counter = runs.clone();
  owner.effect(move || {
    counter.hit();
    tens.get();
  });
  rt.flush();

  n.set(2);
  let logs = capture_logs(|| rt.flush());
  assert!(
    logs.contains("memo bar/Volume.percent panicked and is disabled: memo boom"),
    "{logs}"
  );
  assert_eq!(runs.get(), 1, "the effect doesn't rerun");
  assert_eq!(tens.get(), 10, "the memo keeps its last value");

  n.set(3);
  rt.flush();
  assert_eq!((runs.get(), tens.get()), (1, 10), "the memo stays disabled");
  assert!(is_unborrowed());
}

#[test]
fn memo_that_panics_before_its_first_value_has_none() {
  let owner = Runtime::current().new_owner();
  let broken: Memo<i32> = owner.memo(|| panic!("no value yet"));
  assert_eq!(broken.try_get(), None);
}

#[test]
#[should_panic(expected = "Memo<i32> has no value")]
fn get_on_a_memo_without_a_value_panics() {
  let owner = Runtime::current().new_owner();
  let broken: Memo<i32> = owner.memo(|| panic!("no value yet"));
  broken.get();
}

#[test]
fn memo_that_reads_itself_is_disabled_as_a_cycle() {
  let owner = Runtime::current().new_owner();
  let itself: Signal<Option<Memo<i32>>> = owner.signal(None);
  let count = owner.memo(move || itself.get().map_or(0, |memo| memo.get() + 1));
  assert_eq!(count.get(), 0);

  itself.set(Some(count));
  let logs = capture_logs(|| assert_eq!(count.get(), 0, "it keeps its last value"));
  assert!(logs.contains("a dependency cycle"), "{logs}");
  assert!(is_unborrowed());
}

#[test]
fn memo_panicking_while_a_dirty_effect_reads_it_keeps_its_value() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let m = owner.memo(move || {
    let v = n.get();
    assert!(v != 2, "memo boom");
    v
  });
  let seen = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&seen);
  // Also reads `n` directly, so it is dirty (not just "check") and reads
  // the memo during its own run.
  owner.effect(move || log.borrow_mut().push((n.get(), m.get())));
  rt.flush();
  n.set(2);
  capture_logs(|| rt.flush());
  n.set(3);
  rt.flush();
  assert_eq!(*seen.borrow(), [(1, 1), (2, 1), (3, 1)]);
  assert!(is_unborrowed());
}

#[test]
fn memo_that_writes_a_signal_is_disabled() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let page = owner.signal(0_i32);
  // Clamps the page and writes the clamped value back. Allowing that would
  // leave the memo stale and its readers would miss every later change.
  let clamped = owner
    .memo(move || {
      let p = page.get().clamp(0, 9);
      page.set(p);
      p
    })
    .named("pager.clamped");
  let logs = capture_logs(|| assert_eq!(clamped.try_get(), None));
  assert!(
    logs.contains(
      "memo pager.clamped panicked and is disabled: Signal<i32> was written inside a \
             memo: memos must not write signals; use an effect"
    ),
    "{logs}"
  );
  page.set(5);
  rt.flush();
  assert_eq!(page.get(), 5, "the signal itself still works");
  assert!(is_unborrowed());
}

#[test]
fn memo_writing_inside_untrack_is_still_disabled() {
  let owner = Runtime::current().new_owner();
  let n = owner.signal(0);
  let writes = owner.memo(move || untrack(|| n.set(1)));
  let logs = capture_logs(|| assert_eq!(writes.try_get(), None));
  assert!(logs.contains("memos must not write signals"), "{logs}");
  assert_eq!(n.get(), 0);
}

#[test]
fn flush_inside_a_memo_panics_and_disables_the_memo() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let effect_runs = Counter::default();
  let counter = effect_runs.clone();
  owner.effect(move || {
    counter.hit();
    n.get();
  });
  let flusher = owner
    .memo(move || {
      Runtime::current().flush();
      n.get()
    })
    .named("flusher");
  let logs = capture_logs(|| assert_eq!(flusher.try_get(), None));
  assert!(
    logs
      .contains("memo flusher panicked and is disabled: Runtime::flush() was called inside a memo"),
    "{logs}"
  );
  assert_eq!(
    effect_runs.get(),
    0,
    "no effect ran in the middle of the memo"
  );
  rt.flush();
  assert_eq!(effect_runs.get(), 1);
}

/// Returns the message of a caught panic.
fn panic_text<R>(caught: std::thread::Result<R>) -> String {
  let Err(payload) = caught else {
    panic!("expected a panic");
  };
  match (
    payload.downcast_ref::<String>(),
    payload.downcast_ref::<&str>(),
  ) {
    (Some(message), _) => message.clone(),
    (None, Some(message)) => (*message).to_owned(),
    (None, None) => String::from("(not a string)"),
  }
}

#[test]
fn memos_caught_in_a_borrow_conflict_are_not_disabled() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let items = owner.signal(vec![1]);
  let len = owner.memo(move || items.with(Vec::len));
  let len_plus_one = owner.memo(move || len.get() + 1);
  assert_eq!(len_plus_one.get(), 2);

  // Both memos are stale now. Reading one inside items' own update()
  // recomputes them, and `len` can't read `items` while it is updated.
  items.set(vec![1, 2]);
  let caught = panic::catch_unwind(AssertUnwindSafe(|| {
    items.update(|v| v.push(len_plus_one.get()));
  }));
  let message = panic_text(caught);
  assert!(
    message.contains("was read inside its own update()"),
    "{message}"
  );
  // Neither memo was at fault, and both still follow `items`.
  assert_eq!((len.get(), len_plus_one.get()), (2, 3));
  items.set(Vec::new());
  assert_eq!((len.get(), len_plus_one.get()), (0, 1));

  // Recomputing a memo while a with() on it is still running conflicts too.
  let n = owner.signal(1);
  let tens = owner.memo(move || n.get() * 10);
  assert_eq!(tens.get(), 10);
  let caught = panic::catch_unwind(AssertUnwindSafe(|| {
    tens.with(|_| {
      n.set(5);
      tens.get()
    })
  }));
  let message = panic_text(caught);
  assert!(
    message.contains("was recomputed while a with() call on it was still running"),
    "{message}"
  );
  assert_eq!(tens.get(), 50);
  n.set(6);
  assert_eq!(tens.get(), 60);
  assert!(is_unborrowed());
}

#[test]
fn a_borrow_conflict_inside_flush_leaves_the_effect_queued() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let items = owner.signal(vec![1]);
  let len = owner.memo(move || items.with(Vec::len));
  let shown = Counter::default();
  let sink = Rc::clone(&shown.0);
  owner.effect(move || sink.set(u32::try_from(len.get()).unwrap_or(u32::MAX)));
  rt.flush();

  items.set(vec![1, 2]); // queues the effect
  // A flush inside items' update() makes `len` read `items`: a conflict.
  let caught = panic::catch_unwind(AssertUnwindSafe(|| items.update(|_| rt.flush())));
  let message = panic_text(caught);
  assert!(
    message.contains("was read inside its own update()"),
    "{message}"
  );
  rt.flush();
  assert_eq!(
    shown.get(),
    2,
    "the effect stayed queued and `len` still works"
  );
  assert!(is_unborrowed());
}

#[test]
fn a_panic_inside_update_still_notifies_readers() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(0);
  let shown = Rc::new(Cell::new(-1));
  let sink = Rc::clone(&shown);
  owner.effect(move || sink.set(n.get()));
  rt.flush();

  let caught = panic::catch_unwind(AssertUnwindSafe(|| {
    n.update(|v| {
      *v = 5;
      panic!("update boom");
    });
  }));
  assert_eq!(panic_text(caught), "update boom");
  rt.flush();
  assert_eq!(shown.get(), 5, "the reader saw the value update() left");

  // Here the panic comes from dropping a value the closure captured.
  let guard = PanicOnDrop("update closure");
  let caught = panic::catch_unwind(AssertUnwindSafe(|| {
    n.update(move |v| {
      let _keep = &guard;
      *v = 6;
    });
  }));
  assert_eq!(panic_text(caught), "update closure dropped");
  rt.flush();
  assert_eq!(shown.get(), 6);
  assert!(is_unborrowed());
}

/// A user value whose `Drop` panics, unless it is already being dropped
/// while unwinding (a second panic would abort the test binary).
struct PanicOnDrop(&'static str);

impl Drop for PanicOnDrop {
  fn drop(&mut self) {
    if !std::thread::panicking() {
      panic!("{} dropped", self.0);
    }
  }
}

#[test]
fn panic_in_the_drop_of_a_panicked_effect_is_contained() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(0);
  let guard = PanicOnDrop("bad effect's closure");
  owner
    .effect(move || {
      let _keep = &guard;
      assert!(n.get() != 1, "effect boom");
    })
    .named("bar/Bad");
  let later_runs = Counter::default();
  let counter = later_runs.clone();
  owner.effect(move || {
    counter.hit();
    n.get();
  });
  rt.flush();

  n.set(1);
  let logs = capture_logs(|| rt.flush());
  assert!(
    logs.contains("effect bar/Bad panicked and is disabled: effect boom"),
    "{logs}"
  );
  assert!(logs.contains("bad effect's closure dropped"), "{logs}");
  assert_eq!(
    later_runs.get(),
    2,
    "the next effect in the round still ran"
  );

  // flush() isn't stuck: a new effect and the old one both run.
  let fresh_runs = Counter::default();
  let counter = fresh_runs.clone();
  rt.new_owner().effect(move || counter.hit());
  n.set(2);
  rt.flush();
  assert_eq!((later_runs.get(), fresh_runs.get()), (3, 1));
  assert!(is_unborrowed());
}

#[test]
fn panic_in_the_drop_of_an_effect_that_disposed_its_owner_is_contained() {
  let rt = Runtime::current();
  let before = rt.node_count();
  let owner = rt.new_owner();
  let close = owner.signal(false);
  let guard = PanicOnDrop("self-disposing closure");
  owner.effect(move || {
    let _keep = &guard;
    if close.get() {
      owner.dispose(); // run() drops this closure afterward
    }
  });
  rt.flush();
  close.set(true);
  let logs = capture_logs(|| rt.flush());
  assert!(logs.contains("self-disposing closure dropped"), "{logs}");
  assert_eq!(rt.node_count(), before);

  let fresh_runs = Counter::default();
  let counter = fresh_runs.clone();
  rt.new_owner().effect(move || counter.hit());
  rt.flush();
  assert_eq!(fresh_runs.get(), 1, "flush() isn't stuck");
}

#[test]
fn panic_in_the_drop_of_a_cycling_effect_is_contained() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let feedback = owner.signal(0);
  let other_runs = Counter::default();
  let guard = PanicOnDrop("cycling closure");
  owner.effect(move || {
    let _keep = &guard;
    feedback.set(feedback.get() + 1);
  });
  let counter = other_runs.clone();
  owner.effect(move || {
    counter.hit();
    feedback.get();
  });
  let logs = capture_logs(|| rt.flush());
  assert!(logs.contains("is in a dependency cycle"), "{logs}");
  assert!(logs.contains("cycling closure dropped"), "{logs}");

  let fresh_runs = Counter::default();
  let counter = fresh_runs.clone();
  rt.new_owner().effect(move || counter.hit());
  feedback.set(0);
  rt.flush();
  assert_eq!(fresh_runs.get(), 1, "flush() isn't stuck");
  assert!(other_runs.get() > 1, "the other effect still reacts");
  assert!(is_unborrowed());
}

#[test]
fn panic_in_the_drop_of_a_disposed_value_is_contained() {
  let rt = Runtime::current();
  let before = rt.node_count();
  let token = Rc::new(());
  let owner = rt.new_owner();
  owner.signal(PanicOnDrop("disposed value"));
  owner.signal(Rc::clone(&token));
  let logs = capture_logs(|| owner.dispose());
  assert!(logs.contains("disposed value dropped"), "{logs}");
  assert_eq!(rt.node_count(), before);
  assert_eq!(
    Rc::strong_count(&token),
    1,
    "the other value was dropped too"
  );
  assert!(is_unborrowed());
}

#[test]
fn panic_in_the_drop_of_a_replaced_value_is_contained() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let held = owner.signal(PanicOnDrop("replaced value"));
  let logs = capture_logs(|| held.set(PanicOnDrop("current value")));
  assert!(logs.contains("replaced value dropped"), "{logs}");
  let logs = capture_logs(|| owner.dispose());
  assert!(logs.contains("current value dropped"), "{logs}");
}

/// A memo value whose `Drop` panics while it holds 1.
#[derive(PartialEq)]
struct PanicsWhenOneDrops(i32);

impl Drop for PanicsWhenOneDrops {
  fn drop(&mut self) {
    if self.0 == 1 && !std::thread::panicking() {
      panic!("old memo value dropped");
    }
  }
}

#[test]
fn a_panic_dropping_a_memos_old_value_still_notifies_readers() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let n = owner.signal(1);
  let m = owner.memo(move || PanicsWhenOneDrops(n.get()));
  let shown = Rc::new(Cell::new(0));
  let sink = Rc::clone(&shown);
  owner.effect(move || sink.set(m.with(|v| v.0)));
  rt.flush();

  n.set(2);
  let logs = capture_logs(|| rt.flush());
  assert!(logs.contains("old memo value dropped"), "{logs}");
  assert_eq!(shown.get(), 2, "the reader saw the new value");
  n.set(3);
  rt.flush();
  assert_eq!(shown.get(), 3, "the memo is not disabled");
}

/// Reads a memo when dropped and records what it saw.
struct ReadOnDrop {
  memo: Signal<Option<Memo<i32>>>,
  seen: Rc<Cell<Option<Option<i32>>>>,
}

impl Drop for ReadOnDrop {
  fn drop(&mut self) {
    if let Some(memo) = self.memo.get_untracked() {
      self.seen.set(Some(memo.try_get()));
    }
  }
}

#[test]
fn panicking_memo_is_disabled_before_its_closure_is_dropped() {
  let owner = Runtime::current().new_owner();
  let n = owner.signal(1);
  let slot: Signal<Option<Memo<i32>>> = owner.signal(None);
  let seen = Rc::new(Cell::new(None));
  let probe = ReadOnDrop {
    memo: slot,
    seen: Rc::clone(&seen),
  };
  let m = owner.memo(move || {
    let _keep = &probe;
    let v = n.get();
    assert!(v != 2, "memo boom");
    v
  });
  slot.set(Some(m));
  assert_eq!(m.get(), 1);
  n.set(2);
  let logs = capture_logs(|| assert_eq!(m.get(), 1));
  // Disabled first, the memo just shows its last value to Drop code that
  // reads it. Otherwise the read would look like a dependency cycle.
  assert_eq!(seen.get(), Some(Some(1)), "{logs}");
}

/// Reads a signal with `try_get()` when dropped, like a row that saves its
/// state on the way out.
struct ReadsOnDrop(Signal<u32>);

impl Drop for ReadsOnDrop {
  fn drop(&mut self) {
    self.0.try_get();
  }
}

impl PartialEq for ReadsOnDrop {
  // Never equal, so a memo of these replaces and drops its old value on
  // every run.
  fn eq(&self, _: &Self) -> bool {
    false
  }
}

#[test]
fn drop_code_run_by_the_runtime_records_no_dependency() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let theme = owner.signal(0_u32);
  let items = owner.signal(1_usize);
  let counts: [Counter; 3] = Default::default();
  let [rebuilds, writes, recomputes] = counts.clone();

  // A list whose rows read the theme when they are dropped.
  owner.effect_scoped(move |scope| {
    rebuilds.hit();
    for _ in 0..items.get() {
      scope.signal(ReadsOnDrop(theme));
    }
  });
  // An effect that replaces a value which reads the theme when dropped.
  let slot = owner.signal(ReadsOnDrop(theme));
  owner.effect(move || {
    writes.hit();
    items.get();
    slot.set(ReadsOnDrop(theme));
  });
  // A memo whose old values read the theme when dropped, and its reader.
  let latest = owner.memo(move || {
    recomputes.hit();
    items.get();
    ReadsOnDrop(theme)
  });
  owner.effect(move || latest.with(|_| ()));

  rt.flush();
  items.set(2); // all three run again and drop their old values
  rt.flush();
  theme.set(1); // none of them read the theme themselves
  rt.flush();
  assert_eq!(counts.each_ref().map(Counter::get), [2, 2, 2]);
}

#[test]
fn effect_scoped_frees_the_previous_runs_children() {
  let rt = Runtime::current();
  let before = rt.node_count();
  let owner = rt.new_owner();
  let items = owner.signal(vec![1, 2, 3]);
  let tick = owner.signal(());
  let row_runs = Counter::default();
  let counter = row_runs.clone();
  owner.effect_scoped(move |scope| {
    // One child owner, signal and effect per item, rebuilt on every run.
    for item in items.get() {
      let row = scope.child();
      let text = row.signal(item);
      let counter = counter.clone();
      row.effect(move || {
        counter.hit();
        text.get();
        tick.get();
      });
    }
  });
  rt.flush();
  let after_first = rt.node_count();
  for i in 0..10 {
    items.set(vec![i, i + 1, i + 2]);
    rt.flush();
  }
  assert_eq!(rt.node_count(), after_first);
  // Every row reads `tick`. If old rows were still alive, all 33 would
  // run; only the 3 current ones should.
  let runs_before = row_runs.get();
  tick.set(());
  rt.flush();
  assert_eq!(
    row_runs.get() - runs_before,
    3,
    "only the current rows react"
  );

  owner.dispose();
  assert_eq!(
    rt.node_count(),
    before,
    "disposing the owner frees the last scope"
  );
}

#[test]
fn effect_scoped_frees_the_scope_of_a_run_that_panics() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  owner
    .effect_scoped(|scope| {
      scope.signal(1);
      // Its Drop panics too, while the failed run's scope is disposed.
      scope.child().signal(PanicOnDrop("scoped value"));
      panic!("scoped boom");
    })
    .named("bar/List");
  let before_run = rt.node_count();
  let logs = capture_logs(|| rt.flush());
  assert_eq!(
    rt.node_count(),
    before_run,
    "the failed run's scope is freed"
  );
  assert!(
    logs.contains("effect bar/List panicked and is disabled: scoped boom"),
    "{logs}"
  );
  assert!(logs.contains("scoped value dropped"), "{logs}");
  assert!(is_unborrowed());

  let fresh_runs = Counter::default();
  let counter = fresh_runs.clone();
  owner.effect(move || counter.hit());
  rt.flush();
  assert_eq!(fresh_runs.get(), 1, "the runtime stays usable");
}

#[test]
fn a_scoped_effect_stopped_by_the_cycle_cap_frees_its_scope() {
  let rt = Runtime::current();
  let owner = rt.new_owner();
  let feedback = owner.signal(0);
  let row_runs = Counter::default();
  let counter = row_runs.clone();
  owner.effect_scoped(move |scope| {
    let counter = counter.clone();
    scope.effect(move || {
      counter.hit();
      feedback.get();
    });
    feedback.set(feedback.get() + 1); // the list feeds itself
  });
  let before = rt.node_count();
  let logs = capture_logs(|| rt.flush());
  assert!(logs.contains("is in a dependency cycle"), "{logs}");
  assert_eq!(rt.node_count(), before, "its last scope went with it");

  let runs = row_runs.get();
  feedback.set(-1);
  rt.flush();
  assert_eq!(row_runs.get(), runs, "no row binding is left reacting");
}

/// A label widget: shows `text`, starting from its current value.
fn label(owner: Owner, text: Signal<String>) -> Rc<RefCell<String>> {
  // Reading the initial value must not make the caller's effect depend on
  // the text, or every text change would rebuild the caller.
  let shown = Rc::new(RefCell::new(untrack(|| text.get())));
  let sink = Rc::clone(&shown);
  owner.effect(move || *sink.borrow_mut() = text.get());
  shown
}

#[test]
fn untrack_keeps_a_childs_reads_out_of_its_parents_dependencies() {
  let rt = Runtime::current();
  let root = rt.new_owner();
  let rows = root.signal(2_usize);
  let text = root.signal(String::from("hello"));
  let rebuilds = Counter::default();
  let labels = Rc::new(RefCell::new(Vec::new()));
  let counter = rebuilds.clone();
  let built = Rc::clone(&labels);
  root.effect_scoped(move |scope| {
    counter.hit();
    *built.borrow_mut() = (0..rows.get())
      .map(|_| label(scope.child(), text))
      .collect();
  });
  rt.flush();

  text.set(String::from("world"));
  rt.flush();
  assert_eq!(rebuilds.get(), 1, "only the labels update");
  assert!(
    labels
      .borrow()
      .iter()
      .all(|shown| *shown.borrow() == "world")
  );

  rows.set(3);
  rt.flush();
  assert_eq!((rebuilds.get(), labels.borrow().len()), (2, 3));
}
