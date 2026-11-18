//! Phase 5's exit check: `crabshell run` on the template's bar, made with
//! `crabshell init --path`, in a private headless Sway, with Phase 19's
//! screenshots through a reload that never miss the bar. The first run
//! builds the whole toolkit into `target/tmp/exit-check`, which later runs
//! reuse, so it is ignored by default. Run it as
//!
//! ```sh
//! env -u SWAYSOCK cargo test -p crabshell-cli --test exit_check -- --ignored --nocapture
//! ```

#[allow(dead_code, reason = "the exit check uses part of the helpers")]
mod common;

#[allow(
  dead_code,
  reason = "the exit check uses part of the helper crabshell-wayland's tests share"
)]
#[path = "../../crabshell-wayland/tests/sway/mod.rs"]
mod sway;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::{Home, PrivateBus, Supervised, alive, checkout, is_compiler_error};
use sway::Sway;

/// Waits up to `limit` for the `count`th line of the log holding `needle`,
/// and says when it was seen, to within 5 ms.
fn wait_for_line(supervised: &Supervised, needle: &str, count: usize, limit: Duration) -> Instant {
  let deadline = Instant::now() + limit;
  loop {
    if supervised.log().matches(needle).count() >= count {
      return Instant::now();
    }
    assert!(
      Instant::now() < deadline,
      "no {count} × {needle:?} within {limit:?}:\n{}",
      supervised.log()
    );
    std::thread::sleep(Duration::from_millis(5));
  }
}

/// The number in the `count`th line holding `needle`, the first word
/// after it.
fn number_after(log: &str, needle: &str, count: usize) -> f64 {
  let line = log
    .lines()
    .filter(|line| line.contains(needle))
    .nth(count - 1)
    .expect("the line");
  let after = &line[line.find(needle).expect("the needle") + needle.len()..];
  after
    .split_whitespace()
    .next()
    .and_then(|word| word.parse().ok())
    .expect("a number")
}

/// The supervisor's children: the shells running.
fn shell_pids(supervised: &Supervised) -> Vec<u32> {
  let pid = supervised.pid();
  let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
    .expect("the supervisor's children");
  children
    .split_whitespace()
    .map(|child| child.parse().expect("a pid"))
    .collect()
}

/// The supervisor's one child: the running shell.
fn shell_pid(supervised: &Supervised) -> u32 {
  let pids = shell_pids(supervised);
  assert_eq!(pids.len(), 1, "one shell runs: {pids:?}");
  pids[0]
}

/// Kills the running shell with SIGKILL, and says when.
fn kill_shell(supervised: &Supervised) -> Instant {
  let killed = Instant::now();
  let sent = std::process::Command::new("kill")
    .args(["-KILL", &shell_pid(supervised).to_string()])
    .status()
    .expect("kill runs");
  assert!(sent.success());
  killed
}

/// The template bar's background, `#1e1e2e`.
const BAR: [u8; 3] = [0x1e, 0x1e, 0x2e];

/// Takes screenshots of the top strip of the headless output back to back
/// until `stop`, and returns when each was taken and whether the bar
/// showed in it: its background across the strip's row 1, above the
/// widgets.
fn screenshots(dir: PathBuf, display: String, stop: Arc<AtomicBool>) -> Vec<(Instant, bool)> {
  let mut shots = Vec::new();
  while !stop.load(Ordering::Relaxed) {
    let shot = std::process::Command::new("grim")
      .args(["-g", "0,0 400x4", "-"])
      .env_clear()
      .env("PATH", std::env::var_os("PATH").unwrap_or_default())
      .env("XDG_RUNTIME_DIR", &dir)
      .env("WAYLAND_DISPLAY", &display)
      .output()
      .expect("grim is installed");
    assert!(shot.status.success(), "grim failed");
    let image = sway::Image::decode(&shot.stdout);
    let row = &image.rgba[image.width as usize * 4..image.width as usize * 8];
    let bar = row.chunks(4).all(|pixel| {
      pixel[..3]
        .iter()
        .zip(BAR)
        .all(|(&got, want)| got.abs_diff(want) <= 2)
    });
    shots.push((Instant::now(), bar));
  }
  shots
}

/// The resident memory of `pid`, in KiB, while it runs.
fn rss_kib(pid: u32) -> Option<u64> {
  let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
  status
    .lines()
    .find_map(|line| line.strip_prefix("VmRSS:"))
    .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
}

#[test]
#[ignore = "builds the toolkit and runs a headless Sway: minutes the first time"]
fn phase_5_exit_check() {
  let sway = Sway::start();
  let home = Home::new("exit-check");
  let config_home = home.0.join("config");
  let checkout = checkout().canonicalize().expect("the checkout");
  let made = home.run(&["init", "--path", checkout.to_str().expect("UTF-8")]);
  assert!(made.status.success(), "{}", common::text(&made.stderr));
  let main = home.config().join("src/main.rs");
  let edit = |change: &dyn Fn(String) -> String| {
    let code = std::fs::read_to_string(&main).expect("the bar");
    std::fs::write(&main, change(code)).expect("edited");
  };

  // A cache that outlives the test, so a later run builds only the config.
  let cache = checkout.join("target/tmp/exit-check");
  let _ = std::fs::remove_file(cache.join("crabshell/last-good"));
  let bus = PrivateBus::start(&home);
  let mut command = home.command(&["run"], config_home.to_str().expect("UTF-8"));
  command
    .env("XDG_CACHE_HOME", &cache)
    .env("XDG_RUNTIME_DIR", sway.runtime_dir())
    .env("WAYLAND_DISPLAY", sway.display())
    .env("SWAYSOCK", sway.socket())
    .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
    // A server that names nothing, so libpulse never looks for the
    // desktop's.
    .env(
      "PULSE_SERVER",
      format!(
        "unix:{}",
        sway.runtime_dir().join("no-pulse-server").display()
      ),
    );
  let supervised = home.spawn_supervisor(command);
  wait_for_line(
    &supervised,
    "the shell is ready",
    1,
    Duration::from_secs(1200),
  );
  wait_for_line(
    &supervised,
    "its build is kept as",
    1,
    Duration::from_secs(90),
  );
  let supervisor_rss = rss_kib(supervised.pid()).expect("the supervisor's VmRSS");

  // 1. A syntax error leaves the old shell running and sends a notification.
  let first = shell_pid(&supervised);
  let environment = std::fs::read(format!("/proc/{first}/environ")).expect("its environment");
  assert!(
    !environment
      .split(|&byte| byte == 0)
      .any(|entry| entry.starts_with(b"DISPLAY=")),
    "the shell could reach the desktop's X server"
  );
  edit(&|code| code + "fn broken() {\n");
  let notes = bus.wait_for(1, Duration::from_secs(300));
  assert_eq!(notes.len(), 1, "{notes:?}");
  assert_eq!(notes[0].0, "Your Crabshell config didn't build");
  assert!(
    is_compiler_error(&supervised.log(), &notes[0].1),
    "{notes:?}\n{}",
    supervised.log()
  );
  assert!(alive(first), "the old shell runs on");
  assert_eq!(supervised.log().matches("the shell is ready").count(), 1);

  // 2. Fixing it brings up the new shell within build time plus 200 ms,
  // started beside the old one, so every screenshot through the swap
  // shows the bar.
  let stop = Arc::new(AtomicBool::new(false));
  let shooting = std::thread::spawn({
    let (dir, display) = (sway.runtime_dir().to_owned(), sway.display().to_owned());
    let stop = Arc::clone(&stop);
    move || screenshots(dir, display, stop)
  });
  edit(&|code| code.replace("fn broken() {\n", ""));
  let built = wait_for_line(&supervised, "the build took", 3, Duration::from_secs(300));
  let saved = wait_for_line(
    &supervised,
    "the old shell saved",
    1,
    Duration::from_secs(30),
  );
  // Both shells' memory, the most seen together, until the new one is
  // ready and the old one told to exit.
  let mut both = Vec::new();
  while !supervised.log().contains("reloaded with no gap") {
    assert!(saved.elapsed() < Duration::from_secs(30), "no reload");
    // Only those still running as their status is read.
    let pids = shell_pids(&supervised).into_iter();
    let rss: Vec<u64> = pids.filter_map(rss_kib).collect();
    if rss.iter().sum::<u64>() > both.iter().sum() {
      both = rss;
    }
    std::thread::sleep(Duration::from_millis(5));
  }
  let ready = Instant::now();
  let ended = wait_for_line(
    &supervised,
    "the old shell ended",
    1,
    Duration::from_secs(10),
  );
  std::thread::sleep(Duration::from_millis(500));
  stop.store(true, Ordering::Relaxed);
  let shots = shooting.join().expect("the screenshots");
  let after_build = ready - built;
  let log = supervised.log();
  let rebuild = number_after(&log, "the build took", 3);
  let overlap = number_after(&log, "the new shell was ready", 1);
  assert!(
    after_build <= Duration::from_millis(200),
    "the new shell was ready {after_build:?} after the build"
  );
  assert!(!alive(first), "the old shell is gone");
  let missing = shots.iter().filter(|(_, bar)| !bar).count();
  let during = |from: Instant, to: Instant| {
    let shots = shots.iter().filter(|(at, _)| (from..to).contains(at));
    shots.count()
  };
  let (swap, after_exit) = (
    during(saved, ended),
    during(ended, ended + Duration::from_millis(500)),
  );
  let spacing = shots
    .windows(2)
    .map(|pair| pair[1].0 - pair[0].0)
    .max()
    .unwrap_or_default();
  assert_eq!(
    missing,
    0,
    "{missing} of {} screenshots had no bar",
    shots.len()
  );
  assert!(
    swap >= 1 && after_exit >= 5,
    "{swap} during the swap, {after_exit} after"
  );

  // 3. After `kill -9` the shell is back within 2 s.
  let killed = Instant::now();
  let second = shell_pid(&supervised);
  let sent = std::process::Command::new("kill")
    .args(["-KILL", &second.to_string()])
    .status()
    .expect("kill runs");
  assert!(sent.success());
  let back = wait_for_line(
    &supervised,
    "the shell is ready",
    3,
    Duration::from_secs(10),
  ) - killed;
  assert!(back <= Duration::from_secs(2), "back after {back:?}");

  // 4. A config that panics at startup leaves the old shell running.
  // Once that one crashes, the new build's three crashes fall back to
  // last-good.
  let third = shell_pid(&supervised);
  edit(&|code| {
    code.replacen(
      "fn main() {",
      "fn main() {\n  panic!(\"the exit check's startup panic\");",
      1,
    )
  });
  let notes = bus.wait_for(2, Duration::from_secs(300));
  assert_eq!(notes.len(), 2, "{notes:?}");
  assert_eq!(notes[1].0, "Your Crabshell config didn't start");
  assert!(alive(third), "the old shell runs on");
  let panicking = kill_shell(&supervised);
  let notes = bus.wait_for(3, Duration::from_secs(60));
  assert_eq!(notes[2].0, "Your Crabshell config keeps crashing");
  wait_for_line(
    &supervised,
    "the shell is ready",
    4,
    Duration::from_secs(30),
  );
  let fallback = panicking.elapsed();
  let log = supervised.log();
  assert!(log.contains("last-good starts"), "{log}");
  assert_eq!(log.matches("panicked at src/main.rs").count(), 4, "{log}");

  // 5. So does a config whose bars all panic at startup: the shell has
  // nothing to show and exits with 1.
  edit(&|code| {
    code
      .replace("\n  panic!(\"the exit check's startup panic\");", "")
      .replacen(
        "-> PanelWindow {",
        "-> PanelWindow {\n    panic!(\"the exit check's bar panic\");",
        1,
      )
  });
  let notes = bus.wait_for(4, Duration::from_secs(300));
  assert_eq!(notes.len(), 4, "{notes:?}");
  assert_eq!(notes[3].0, "Your Crabshell config didn't start");
  let bars_panicking = kill_shell(&supervised);
  let notes = bus.wait_for(5, Duration::from_secs(60));
  assert_eq!(notes[4].0, "Your Crabshell config keeps crashing");
  wait_for_line(
    &supervised,
    "the shell is ready",
    5,
    Duration::from_secs(30),
  );
  let bars_fallback = bars_panicking.elapsed();
  let log = supervised.log();
  assert_eq!(log.matches("has nothing to show").count(), 4, "{log}");
  assert_eq!(
    log
      .matches("per_output[HEADLESS-1] (called at src/main.rs:")
      .count(),
    4,
    "{log}"
  );

  supervised.signal("-TERM");
  let (code, log) = supervised.wait(Duration::from_secs(10));
  assert_eq!(code, Some(0), "{log}");

  let results = format!(
    "Phase 5 exit check, template bar on one headless Sway output:\n\
     rebuild after a one-line fix: {rebuild:.1} s\n\
     the new shell ready after the build ended: {} ms\n\
     the new shell ready after the old one saved: {overlap} ms\n\
     the old shell gone after the new one was ready: {} ms\n\
     RSS of the two shells while both ran: {both:?} KiB\n\
     screenshots through the reload: {}, none without the bar, at most {} ms apart, \
     {swap} while both shells ran and {after_exit} in the 500 ms after the old one ended\n\
     back after kill -9: {} ms\n\
     crash of the old shell to last-good ready after three startup panics: {:.1} s\n\
     crash of the old shell to last-good ready after three starts whose bars all panic: {:.1} s\n\
     supervisor RSS with the shell up: {supervisor_rss} KiB\n",
    after_build.as_millis(),
    (ended - ready).as_millis(),
    shots.len(),
    spacing.as_millis(),
    back.as_millis(),
    fallback.as_secs_f64(),
    bars_fallback.as_secs_f64(),
  );
  std::fs::write(cache.join("results.txt"), &results).expect("the results");
  println!("{results}");
}
