//! `process::exit(0)` with owners still alive must exit with status 0. It
//! tears down the main thread's runtime too, which must drop everything the
//! owners held. This is a plain program (`harness = false` in Cargo.toml),
//! and cargo checks the exit status.

mod common;
mod window;

fn main() {
  common::fail_on_unexpected_panics();
  window::expect_drops_at_thread_exit(window::PROBES_PER_WINDOW);
  window::build_window_and_leave();
  println!("process::exit with live owners: ok");
  std::process::exit(0);
}
