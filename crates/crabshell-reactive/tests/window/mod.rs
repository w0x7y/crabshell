//! The scenario both thread-exit tests run: a window whose owner is never
//! disposed, holding probe values whose `Drop` calls back into the runtime.

use std::cell::Cell;

use crabshell_reactive::{Owner, Runtime, Signal};

use crate::common::{PanicsOnDrop, count_drop, expect_drops, must_hold};

/// How many probe values `build_window_and_leave` creates.
pub const PROBES_PER_WINDOW: usize = 3;

/// Checks, after this thread's runtime has been torn down, that `expected`
/// probe values have been dropped by then. Call it before the thread first
/// uses the runtime: on Linux, thread-locals are destroyed in the reverse
/// order of their first use, so this check runs after the runtime is gone.
/// If that order ever changed, the check would fail, not pass by mistake.
pub fn expect_drops_at_thread_exit(expected: usize) {
  struct Check(Cell<usize>);

  impl Drop for Check {
    fn drop(&mut self) {
      expect_drops(self.0.get());
    }
  }

  thread_local! {
      static CHECK: Check = const { Check(Cell::new(0)) };
  }
  CHECK.with(|check| check.0.set(expected));
}

/// Disposes its child owner when dropped, like a widget does.
struct ChildWidget(Owner);

impl Drop for ChildWidget {
  fn drop(&mut self) {
    count_drop();
    self.0.dispose();
  }
}

/// Uses a signal when dropped. Once the runtime is gone, writes must do
/// nothing and reads must find no value.
struct SignalProbe(Signal<i32>);

impl Drop for SignalProbe {
  fn drop(&mut self) {
    count_drop();
    let signal = self.0;
    signal.set(2);
    signal.update(|n| *n += 1);
    must_hold(signal.try_get().is_none() && !signal.try_set(4) && !signal.try_update(|n| *n += 1));
  }
}

/// Builds a window and leaves without disposing its owner. The runtime is
/// torn down, values and all, when the thread exits.
pub fn build_window_and_leave() {
  let rt = Runtime::current();
  let window = rt.new_owner();
  let count = window.signal(1);
  let child = ChildWidget(window.child());
  let probe = SignalProbe(count);
  window.signal(PanicsOnDrop);
  window.effect(move || {
    let _keep = (&child, &probe);
    count.get();
  });
  rt.flush();
}
