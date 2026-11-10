//! A thread, and then `main`, that end with owners still alive must exit
//! cleanly, and the runtime must drop everything they held. This is a plain
//! program (`harness = false` in Cargo.toml), and cargo fails the test
//! unless it exits with status 0.

mod common;
mod window;

use window::PROBES_PER_WINDOW;

fn main() {
    common::fail_on_unexpected_panics();
    std::thread::spawn(window::build_window_and_leave)
        .join()
        .expect("the thread exits without panicking");
    common::expect_drops(PROBES_PER_WINDOW);

    // This runtime is torn down after `main` returns.
    window::expect_drops_at_thread_exit(2 * PROBES_PER_WINDOW);
    window::build_window_and_leave();
    println!("exit with live owners: ok");
}
