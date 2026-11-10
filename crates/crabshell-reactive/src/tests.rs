//! Tests for the reactive core, in two parts: signals, effects, flush and
//! owners; then memos and untrack. The tests the design doc requires for
//! Phase 1 are marked.
//!
//! Each test runs on its own thread, so each starts with an empty runtime.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::runtime::is_unborrowed;
use crate::{MAX_RUNS_PER_FLUSH, Runtime, Signal, untrack};

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
            self.0
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
