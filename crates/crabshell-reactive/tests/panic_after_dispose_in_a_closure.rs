//! A `with()` or `update()` closure that disposes its own signal's owner and
//! then panics. The runtime then holds the value's last reference while the
//! panic unwinds, and the value's `Drop` panics too. That second panic must
//! be caught and logged; escaping a `Drop` during unwinding would abort the
//! process. This is a plain program (`harness = false` in Cargo.toml), and
//! cargo fails the test unless it exits with status 0.

mod common;

use std::panic::{self, AssertUnwindSafe};

use common::{DELIBERATE_PANIC, PanicsOnDrop};
use crabshell_reactive::Runtime;

fn main() {
    common::fail_on_unexpected_panics();
    let rt = Runtime::current();
    for write in [false, true] {
        let owner = rt.new_owner();
        let held = owner.signal(PanicsOnDrop);
        let caught = panic::catch_unwind(AssertUnwindSafe(|| {
            if write {
                held.update(|_| {
                    owner.dispose();
                    panic!("{DELIBERATE_PANIC}");
                });
            } else {
                held.with(|_| {
                    owner.dispose();
                    panic!("{DELIBERATE_PANIC}");
                });
            }
        }));
        common::must_hold(caught.is_err());
    }
    common::expect_drops(2);
    println!("panic after dispose in a closure: ok");
}
