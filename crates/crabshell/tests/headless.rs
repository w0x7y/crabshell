//! Phase 2's exit criteria on headless Sway, one Sway per test: the
//! counter example matches its goldens at scales 1, 1.5 and 2, before and
//! after a click; an idle bar wakes nothing, by its own counters and from
//! outside; unplugging an output drops its bar and frees its nodes. And
//! Phase 3's: the v1 bar example shows the private Sway's workspaces,
//! switches them on a click, logs each switch's time to the screen, wakes
//! only on the minute with audio connected, and its volume button agrees
//! with a private pipewire-pulse through scrolls, clicks and changes made
//! elsewhere.
//!
//! Anything that asks for `cx.sway()` or `cx.audio()` runs as its own
//! process, given the private Sway's socket and a `PULSE_SERVER` that is
//! private or names nothing: in this process, libpulse would look for the
//! desktop's audio server.
//!
//! They need `sway`, `swaymsg` and `grim`, and the audio ones `pipewire`
//! and `pactl`, so they are ignored by default. Run them without the
//! session's `SWAYSOCK`, which `Sway::start()` refuses:
//!
//! ```text
//! env -u SWAYSOCK cargo test -p crabshell --test headless -- --ignored --test-threads=1
//! ```
//!
//! Goldens are written again, on purpose only, with `CRABSHELL_BLESS=1`.

#[allow(
  dead_code,
  reason = "these tests use part of the helper crabshell-wayland's share"
)]
#[path = "../../crabshell-wayland/tests/sway/mod.rs"]
mod sway;

#[allow(
  dead_code,
  reason = "the example's bar is tested; its main isn't run here"
)]
#[path = "../examples/counter.rs"]
mod counter;

#[allow(
  dead_code,
  reason = "the example's bar is tested; its main isn't run here"
)]
#[path = "../examples/bar.rs"]
mod v1;

#[allow(dead_code, reason = "the services' own tests use the rest")]
#[path = "../../crabshell-services/tests/pipewire/mod.rs"]
mod pipewire;

use std::io::{self, BufRead, BufReader};
use std::ops::Range;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crabshell::Shell;
use crabshell::prelude::*;
use crabshell_reactive::Runtime;
use crabshell_ui::{Content, NodeView, Rect};
use crabshell_wayland::{EventLoop, Platform, WindowId};
use pipewire::PrivateServer;
use sway::{Image, Sway, VirtualPointer};

/// How long any wait may take before the test fails.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A shell running the counter example's bar on every output.
fn start(sway: &Sway) -> Shell {
  Shell::new(sway.connect(), |cx| cx.per_output(counter::bar)).expect("the root closure runs")
}

/// Dispatches until `done` holds, failing the test after ten seconds.
fn run_until(shell: &mut Shell, what: &str, mut done: impl FnMut(&mut Platform) -> bool) {
  let deadline = Instant::now() + TIMEOUT;
  while !done(shell.platform()) {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    shell
      .dispatch(Some(Duration::from_millis(20)))
      .expect("the connection works");
  }
}

/// Dispatches until no frame has been committed for half a second.
fn settle(shell: &mut Shell) {
  let deadline = Instant::now() + TIMEOUT;
  let mut commits = shell.platform().stats().commits;
  let mut quiet_since = Instant::now();
  while quiet_since.elapsed() < Duration::from_millis(500) {
    assert!(Instant::now() < deadline, "the bars never stopped drawing");
    shell
      .dispatch(Some(Duration::from_millis(20)))
      .expect("the connection works");
    let now = shell.platform().stats().commits;
    if now != commits {
      commits = now;
      quiet_since = Instant::now();
    }
  }
}

/// The bar on `output`, if it has committed a frame.
fn shown_on(platform: &Platform, output: &str) -> Option<WindowId> {
  platform
    .windows()
    .filter(|(_, window)| window.output().name() == output)
    .map(|(id, _)| id)
    .find(|id| {
      platform
        .window_info(*id)
        .is_some_and(|info| info.commits > 0)
    })
}

/// The `n`th child of the bar's row.
fn in_row<T>(
  platform: &Platform,
  id: WindowId,
  n: usize,
  read: impl FnOnce(NodeView<'_>) -> T,
) -> T {
  let (_, window) = platform
    .windows()
    .find(|(held, _)| *held == id)
    .expect("the bar is held");
  let view = window.view();
  let row = view.root().children().next().expect("the bar has a row");
  read(row.children().nth(n).expect("the row holds that child"))
}

/// The counter button: the bar's row's first child.
fn button<T>(platform: &Platform, id: WindowId, read: impl FnOnce(NodeView<'_>) -> T) -> T {
  in_row(platform, id, 0, read)
}

/// A button's label, as laid out.
fn text_of(button: NodeView<'_>) -> String {
  let text = button
    .children()
    .next()
    .expect("the button holds its label");
  let Content::Text(text) = text.content() else {
    panic!("the label is text");
  };
  text.runs().map(|run| run.text).collect()
}

/// The counter button's label.
fn label(platform: &Platform, id: WindowId) -> String {
  button(platform, id, text_of)
}

/// The middle of `rect`, in whole logical pixels.
fn middle(
  Rect {
    x,
    y,
    width,
    height,
  }: Rect,
) -> (u32, u32) {
  ((x + width / 2.0) as u32, (y + height / 2.0) as u32)
}

/// The middle of the counter button, in logical pixels.
fn button_center(platform: &Platform, id: WindowId) -> (u32, u32) {
  button(platform, id, |button| middle(button.rect()))
}

/// Fails unless the label's font is Noto Sans, which `sans-serif` was when
/// the goldens were drawn: other fonts draw other pixels.
fn check_fonts(shell: &mut Shell, id: WindowId) {
  let font = button(shell.platform(), id, |button| {
    let text = button
      .children()
      .next()
      .expect("the button holds its label");
    let Content::Text(text) = text.content() else {
      panic!("the label is text");
    };
    text
      .runs()
      .flat_map(|run| run.glyphs.iter())
      .next()
      .map(|glyph| glyph.font_id)
  })
  .expect("the label has glyphs");
  let family = shell
    .platform()
    .text_system()
    .font_system()
    .db()
    .face(font)
    .and_then(|face| face.families.first())
    .map(|(name, _)| name.clone())
    .unwrap_or_default();
  assert_eq!(
    family, "Noto Sans",
    "the goldens were drawn with Noto Sans for `sans-serif`, but fontconfig picks \
         {family:?} here (see `fc-match sans-serif`); they can't match"
  );
}

/// Checks the top `rows` of `shot` against `tests/golden/<name>.png`,
/// within 2/255 per channel, or writes the golden when `CRABSHELL_BLESS`
/// is set. On a mismatch the screenshot is saved under the target
/// directory.
fn check_golden(name: &str, shot: &Image, rows: u32) {
  assert!(
    shot.height >= rows,
    "{name}: the screenshot is shorter than the bar"
  );
  let top = Image {
    rgba: shot.rgba[..shot.width as usize * rows as usize * 4].to_vec(),
    width: shot.width,
    height: rows,
  };
  let path = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/golden")
    .join(format!("{name}.png"));
  if std::env::var_os("CRABSHELL_BLESS").is_some() {
    std::fs::create_dir_all(path.parent().expect("a directory")).expect("create tests/golden");
    top.save(&path);
    return;
  }
  let bytes = std::fs::read(&path).unwrap_or_else(|_| {
    panic!(
      "no golden {}; run with CRABSHELL_BLESS=1 to write it",
      path.display()
    )
  });
  let golden = Image::decode(&bytes);
  let failure = if (golden.width, golden.height) != (top.width, top.height) {
    Some(format!(
      "size {}x{}, golden {}x{}",
      top.width, top.height, golden.width, golden.height
    ))
  } else {
    let differing = top
      .rgba
      .as_chunks::<4>()
      .0
      .iter()
      .zip(golden.rgba.as_chunks::<4>().0)
      .filter(|(shot, golden)| {
        shot
          .iter()
          .zip(golden.iter())
          .any(|(a, b)| a.abs_diff(*b) > 2)
      })
      .count();
    (differing > 0).then(|| format!("{differing} pixels differ by more than 2/255"))
  };
  if let Some(failure) = failure {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("headless");
    std::fs::create_dir_all(&dir).expect("create the failure directory");
    let saved = dir.join(format!("{name}.png"));
    top.save(&saved);
    panic!(
      "{name} doesn't match its golden: {failure}; the screenshot is in {}",
      saved.display()
    );
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn the_counter_bar_matches_its_goldens_before_and_after_a_click() {
  for (scale, factor, suffix) in [("1", 1.0, "1x"), ("1.5", 1.5, "1.5x"), ("2", 2.0, "2x")] {
    let sway = Sway::start();
    // 1920 by 1080 is a whole number of logical pixels at every scale.
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", scale]);
    let mut shell = start(&sway);
    run_until(&mut shell, "the bar's first frame", |platform| {
      shown_on(platform, "HEADLESS-1").is_some()
    });
    settle(&mut shell);
    let id = shown_on(shell.platform(), "HEADLESS-1").expect("the bar is shown");
    check_fonts(&mut shell, id);
    assert_eq!(label(shell.platform(), id), "clicked 0 times");
    let rows = (30.0 * factor) as u32;
    check_golden(
      &format!("counter@{suffix}"),
      &sway.screenshot("HEADLESS-1"),
      rows,
    );

    // A virtual pointer gives the seat a pointer; the platform takes it
    // while the loop runs.
    let mut pointer = VirtualPointer::new(&sway);
    settle(&mut shell);
    let (x, y) = button_center(shell.platform(), id);
    let layout = ((1920.0 / factor) as u32, (1080.0 / factor) as u32);
    pointer.move_to(x, y, layout);
    pointer.click();
    run_until(&mut shell, "the click to count", |platform| {
      label(platform, id) == "clicked once"
    });
    settle(&mut shell);
    // The pointer is still over the button, so it shows its hover style.
    check_golden(
      &format!("counter-clicked@{suffix}"),
      &sway.screenshot("HEADLESS-1"),
      rows,
    );
  }
}

/// An example as its own program, the way `run` runs it, with its log
/// lines read as they come.
struct ExampleBar {
  child: Child,
  lines: mpsc::Receiver<String>,
}

impl ExampleBar {
  /// Builds the example `name` (in release if `release`) and starts it on
  /// `sway`, with `extra` in its environment.
  fn start(sway: &Sway, name: &str, release: bool, extra: &[(&str, &str)]) -> ExampleBar {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = Command::new(cargo);
    build
      .args(["build", "-q", "-p", "crabshell", "--example", name])
      .current_dir(&workspace);
    if release {
      build.arg("--release");
    }
    assert!(
      build.status().is_ok_and(|status| status.success()),
      "the {name} example builds"
    );
    let target = std::env::var_os("CARGO_TARGET_DIR")
      .map(PathBuf::from)
      .unwrap_or_else(|| workspace.join("target"));
    let program = target
      .join(if release { "release" } else { "debug" })
      .join("examples")
      .join(name);
    ExampleBar::spawn(sway, Command::new(program), extra)
  }

  /// Starts `command` as a shell on `sway`, with `extra` in its otherwise
  /// cleared environment, and its log read as it comes. Its `SWAYSOCK`
  /// is the private Sway's, never this process's. Its `PULSE_SERVER` is
  /// a socket that doesn't exist, unless `extra` names a private server:
  /// unset, libpulse could find the desktop's through HOME's client.conf.
  fn spawn(sway: &Sway, mut command: Command, extra: &[(&str, &str)]) -> ExampleBar {
    command.env_clear();
    let nowhere = sway.runtime_dir().join("no-pulse-server");
    command.env("PULSE_SERVER", format!("unix:{}", nowhere.display()));
    for (name, value) in extra {
      command.env(name, value);
    }
    let name = Path::new(command.get_program())
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
    let mut child = command
      .env("PATH", std::env::var_os("PATH").unwrap_or_default())
      .env("HOME", std::env::var_os("HOME").unwrap_or_default())
      .env("XDG_RUNTIME_DIR", sway.runtime_dir())
      .env("WAYLAND_DISPLAY", sway.display())
      .env("SWAYSOCK", sway.socket())
      .env("CRABSHELL_LOG", "info")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::piped())
      .spawn()
      .unwrap_or_else(|error| panic!("start {command:?}: {error}"));
    let stderr = child.stderr.take().expect("stderr is piped");
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || {
      for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        eprintln!("{name}: {line}");
        if sender.send(line).is_err() {
          break;
        }
      }
    });
    ExampleBar { child, lines }
  }

  /// The first log line containing `needle`.
  fn wait_for(&self, needle: &str) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
      let left = deadline.saturating_duration_since(Instant::now());
      match self.lines.recv_timeout(left) {
        Ok(line) if line.contains(needle) => return line,
        Ok(_) => {}
        Err(_) => panic!("the shell never logged {needle:?}"),
      }
    }
  }

  /// The first frame's time after `run()`, from its log line, in ms.
  fn first_frame_ms(&self) -> f64 {
    ms_after(&self.wait_for(FIRST_FRAME), FIRST_FRAME)
  }

  /// The log lines that came and weren't read yet.
  fn drain(&self) -> Vec<String> {
    self.lines.try_iter().collect()
  }

  /// Its exit status, waiting up to ten seconds; `None` if it is still
  /// running then.
  fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
      if let Ok(Some(status)) = self.child.try_wait() {
        return Some(status);
      }
      std::thread::sleep(Duration::from_millis(50));
    }
    None
  }

  /// Context switches of all its threads so far: each is a wakeup.
  fn context_switches(&self) -> u64 {
    let tasks = format!("/proc/{}/task", self.child.id());
    std::fs::read_dir(&tasks)
      .expect("the example is running")
      .filter_map(Result::ok)
      .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
      .flat_map(|status| {
        status
          .lines()
          .filter(|line| line.contains("ctxt_switches"))
          .filter_map(|line| line.split_whitespace().last()?.parse::<u64>().ok())
          .collect::<Vec<u64>>()
      })
      .sum()
  }

  /// Waits until its context switches stop changing for a second.
  fn wait_until_quiet(&self) {
    let deadline = Instant::now() + TIMEOUT;
    let mut last = self.context_switches();
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < Duration::from_secs(1) {
      assert!(Instant::now() < deadline, "the example never went quiet");
      std::thread::sleep(Duration::from_millis(100));
      let now = self.context_switches();
      if now != last {
        last = now;
        quiet_since = Instant::now();
      }
    }
  }

  /// Watches its context switches for `time`, every 50 ms. Returns each
  /// change seen: when, in seconds past the wall clock's minute, and how
  /// many switches it was.
  fn wakeups_for(&self, time: Duration) -> Vec<(f64, u64)> {
    let end = Instant::now() + time;
    let mut last = self.context_switches();
    let mut seen = Vec::new();
    while Instant::now() < end {
      std::thread::sleep(Duration::from_millis(50));
      let now = self.context_switches();
      if now != last {
        let wall = SystemTime::now()
          .duration_since(SystemTime::UNIX_EPOCH)
          .expect("the clock is past 1970");
        seen.push((wall.as_secs_f64() % 60.0, now - last));
        last = now;
      }
    }
    seen
  }

  /// Waits until `server` lists it as a client.
  fn wait_until_connected(&self, server: &PrivateServer) {
    let deadline = Instant::now() + TIMEOUT;
    let me = format!("application.process.id = \"{}\"", self.child.id());
    loop {
      let clients = server.pactl(&["list", "clients"]);
      if clients.contains(&me) {
        return;
      }
      assert!(
        Instant::now() < deadline,
        "it never connected to the private audio server: {clients}"
      );
      std::thread::sleep(Duration::from_millis(50));
    }
  }

  /// Waits for the v1 bar run by [`run_v1_bar_logging_its_volume`] to log
  /// its volume button saying `label`, and returns the button's middle.
  fn volume_says(&self, label: &str) -> (u32, u32) {
    let line = self.wait_for(&format!(" says {label}"));
    line
      .split(VOLUME)
      .nth(1)
      .and_then(|rest| rest.split(" says").next()?.split_once(','))
      .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))
      .unwrap_or_else(|| panic!("no volume button's middle in {line:?}"))
  }

  /// Its resident memory in KiB, from `ps -o rss=`.
  fn rss_kib(&self) -> u64 {
    let output = Command::new("ps")
      .args(["-o", "rss=", "-p", &self.child.id().to_string()])
      .output()
      .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
      .trim()
      .parse()
      .expect("ps prints the RSS")
  }
}

impl Drop for ExampleBar {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

#[test]
#[ignore = "needs headless Sway"]
fn a_shell_never_looks_for_the_desktops_audio_server() {
  let sway = Sway::start();
  let mut sleep = Command::new("sleep");
  sleep.arg("10");
  let shell = ExampleBar::spawn(&sway, sleep, &[]);
  let environ =
    std::fs::read(format!("/proc/{}/environ", shell.child.id())).expect("the shell's environment");
  let server = environ
    .split(|byte| *byte == 0)
    .find_map(|entry| entry.strip_prefix(b"PULSE_SERVER="))
    .map(|server| String::from_utf8_lossy(server).into_owned())
    .expect("PULSE_SERVER is set, so libpulse never looks elsewhere");
  let socket = server.strip_prefix("unix:").expect("a unix socket");
  assert!(
    Path::new(socket).starts_with(sway.runtime_dir()) && !Path::new(socket).exists(),
    "{server} is a socket in the private Sway's directory that doesn't exist"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn an_idle_counter_bar_wakes_nothing() {
  let sway = Sway::start();
  // In this process: with nothing to do, one dispatch waits out its whole
  // timeout, and the platform's counters stand still.
  let mut shell = start(&sway);
  run_until(&mut shell, "the bar's first frame", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  settle(&mut shell);
  let before = shell.platform().stats();
  let waited = Instant::now();
  shell
    .dispatch(Some(Duration::from_secs(5)))
    .expect("the connection works");
  let waited = waited.elapsed();
  assert!(
    waited >= Duration::from_millis(4950),
    "something woke the loop after {waited:?}"
  );
  let after = shell.platform().stats();
  assert_eq!(
    after.wayland_wakeups, before.wayland_wakeups,
    "Wayland events"
  );
  assert_eq!(after.commits, before.commits, "frames");
  // Nor did the switch log start the Sway service to read it.
  assert!(crabshell_services::Sway::started(shell.cx()).is_none());
  drop(shell);

  // From outside, for a minute: the example program, sleeping in the
  // loop `run` runs, gets no context switches at all. A timer as slow as
  // one a minute would show.
  let bar = ExampleBar::start(&sway, "counter", false, &[]);
  bar.wait_for("first frame committed");
  bar.wait_until_quiet();
  let before = bar.context_switches();
  std::thread::sleep(Duration::from_secs(60));
  assert_eq!(
    bar.context_switches(),
    before,
    "context switches in 60 idle seconds"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn unplugging_an_output_drops_its_bar_and_frees_its_nodes() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  let mut shell = start(&sway);
  run_until(&mut shell, "the first bar", |platform| {
    shown_on(platform, "HEADLESS-1").is_some()
  });
  settle(&mut shell);
  runtime.flush();
  let nodes = runtime.node_count();

  sway.msg(&["create_output"]);
  run_until(&mut shell, "a bar on the new output", |platform| {
    shown_on(platform, "HEADLESS-2").is_some()
  });
  settle(&mut shell);
  assert!(runtime.node_count() > nodes, "the second bar has nodes");

  // Sway closes the bar's surface before it removes the output, so this
  // takes per_output's closed-window path; its unit tests guard the
  // removal path.
  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(&mut shell, "the second bar to go", |platform| {
    platform.windows().count() == 1 && platform.outputs().all(|output| output.name != "HEADLESS-2")
  });
  runtime.flush();
  // All but HEADLESS-2's persisted click count, which the shell keeps for
  // when it comes back.
  assert_eq!(
    runtime.node_count(),
    nodes + 1,
    "per_output's owner for HEADLESS-2 was disposed"
  );
}

#[test]
#[ignore = "needs headless Sway; measures only with CRABSHELL_MEASURE set"]
fn measure_the_counter_bar() {
  if std::env::var_os("CRABSHELL_MEASURE").is_none() {
    eprintln!("not measuring: set CRABSHELL_MEASURE=1 to, which takes two minutes");
    return;
  }
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let counter = ExampleBar::start(&sway, "counter", true, &[]);
  let mut report = format!("counter example, {}\n", measure(&counter));
  drop(counter);
  // Then the v1 bar, on workspace 1, then switching between 1 and 2. Its
  // tray runs the watcher on a private session bus.
  let server = PrivateServer::start("measure");
  let bus = PrivateBus::start();
  let bar = ExampleBar::start(
    &sway,
    "bar",
    true,
    &[
      ("PULSE_SERVER", &server.address()),
      ("DBUS_SESSION_BUS_ADDRESS", &bus.address),
    ],
  );
  bar.wait_until_connected(&server);
  let idle = measure(&bar);
  let times = switch_workspaces(&sway, &bar, 10);
  let slowest = times.iter().copied().fold(0.0, f64::max);
  report += &format!(
    "bar example with audio on a private pipewire-pulse and the tray on a private bus, \
         {idle}; workspace switch to \
         screen: median {:.1} ms, slowest {slowest:.1} ms over {} switches, each {times:.1?} ms\n",
    median(&times),
    times.len()
  );
  eprint!("{report}");
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("headless");
  std::fs::create_dir_all(&dir).expect("create the output directory");
  std::fs::write(dir.join("measurements.txt"), report).expect("write the measurements");
}

/// A release example's numbers on one 1920x1080 output: its first frame,
/// its RSS 60 s after it, and its wakeups while idle until then.
fn measure(bar: &ExampleBar) -> String {
  let first_frame_ms = bar.first_frame_ms();
  let started = Instant::now();
  bar.wait_until_quiet();
  let quiet = bar.context_switches();
  let wakeups = bar.wakeups_for(Duration::from_secs(60).saturating_sub(started.elapsed()));
  let idle: u64 = wakeups.iter().map(|(_, switches)| switches).sum();
  let seen: Vec<f64> = wakeups.iter().map(|(past, _)| *past).collect();
  let rss = bar.rss_kib();
  format!(
    "release build, one 1920x1080 headless output at scale 1: \
         first frame {first_frame_ms:.1} ms after run(), RSS {rss} KiB ({:.1} MiB) after 60 s, \
         {quiet} context switches by the time it was quiet, {idle} more while idle, \
         seen at {seen:.2?} s past the minute",
    rss as f64 / 1024.0
  )
}

/// What the shell logs once a workspace switch is on screen, before its
/// time.
const SWITCHED: &str = "workspace switch on screen in ";

/// Switches the private Sway between workspaces 2 and 1 `n` times, a
/// quarter second apart so the bar is idle each time, and returns each
/// switch's time to the screen from `bar`'s log, in ms. Each switch must
/// be logged exactly once.
fn switch_workspaces(sway: &Sway, bar: &ExampleBar, n: usize) -> Vec<f64> {
  let mut times = Vec::new();
  for i in 0..=n {
    std::thread::sleep(Duration::from_millis(250));
    let extra = bar.drain();
    assert!(
      !extra.iter().any(|line| line.contains(SWITCHED)),
      "a switch was logged twice: {extra:?}"
    );
    if i < n {
      sway.msg(&["workspace", if i % 2 == 0 { "2" } else { "1" }]);
      times.push(ms_after(&bar.wait_for(SWITCHED), SWITCHED));
    }
  }
  times
}

/// The middle of `times`, or the upper of the two middle ones.
fn median(times: &[f64]) -> f64 {
  let mut sorted = times.to_vec();
  sorted.sort_by(f64::total_cmp);
  sorted[sorted.len() / 2]
}

/// What `run` logs once the first frame is committed, before its time.
const FIRST_FRAME: &str = "first frame committed ";

/// The number of ms in `line` right after `prefix`, as in `... 4.2 ms`.
fn ms_after(line: &str, prefix: &str) -> f64 {
  line
    .split(prefix)
    .nth(1)
    .and_then(|rest| rest.split(" ms").next())
    .and_then(|ms| ms.parse().ok())
    .unwrap_or_else(|| panic!("no time after {prefix:?} in {line:?}"))
}

/// The bar's background and the focused workspace's, from examples/bar.rs.
const BAR_BACKGROUND: [u8; 3] = [0x1e, 0x1e, 0x2e];
const FOCUSED: [u8; 3] = [0x45, 0x47, 0x5a];

/// The pixel at `x`, `y` of `shot`, without its alpha.
fn rgb(shot: &Image, x: u32, y: u32) -> [u8; 3] {
  let at = (y as usize * shot.width as usize + x as usize) * 4;
  [shot.rgba[at], shot.rgba[at + 1], shot.rgba[at + 2]]
}

/// The columns of the focused workspace's button in the bar along the top
/// of `shot`: the longest run of its background on the rows between the
/// button's top edge and its text. A text edge can blend to that color
/// too, but never 16 pixels in a row.
fn focused_button(shot: &Image) -> Option<Range<u32>> {
  let close = |pixel: [u8; 3]| pixel.iter().zip(FOCUSED).all(|(a, b)| a.abs_diff(b) <= 2);
  let mut longest = 0..0;
  for y in 4..9 {
    let mut start = 0;
    for x in 0..=shot.width {
      if x < shot.width && close(rgb(shot, x, y)) {
        continue;
      }
      if x - start > longest.len() as u32 {
        longest = start..x;
      }
      start = x + 1;
    }
  }
  (longest.len() >= 16).then_some(longest)
}

/// Whether the columns `button` of the bar in `shot` hold a label on the
/// bar's background: an unfocused workspace's button.
fn labelled(shot: &Image, button: &Range<u32>) -> bool {
  let area = button.clone().flat_map(|x| (4..26).map(move |y| (x, y)));
  let pixels: Vec<[u8; 3]> = area.map(|(x, y)| rgb(shot, x, y)).collect();
  pixels.contains(&BAR_BACKGROUND) && pixels.iter().any(|pixel| *pixel != BAR_BACKGROUND)
}

/// Screenshots `output` until `ready` holds for it, failing after ten
/// seconds.
fn wait_for_shot(sway: &Sway, output: &str, what: &str, ready: impl Fn(&Image) -> bool) {
  let deadline = Instant::now() + TIMEOUT;
  while !ready(&sway.screenshot(output)) {
    assert!(
      Instant::now() < deadline,
      "timed out waiting for {what} on {output}"
    );
    std::thread::sleep(Duration::from_millis(50));
  }
}

/// The private Sway's focused workspace, as `swaymsg -t get_workspaces`
/// says.
fn focused_in(sway: &Sway) -> Option<String> {
  let json = sway.msg(&["-t", "get_workspaces", "-r"]);
  let list: Vec<Workspace> = serde_json::from_str(&json).expect("swaymsg prints the list");
  list.into_iter().find(|ws| ws.focused).map(|ws| ws.name)
}

/// The private Sway with two 1920 by 1080 outputs side by side, focused
/// workspace 1 on HEADLESS-1 and workspace 2 on HEADLESS-2, and the bar
/// example showing them. Sway removes an empty workspace once it isn't
/// shown, so one output would never show two. Returns where workspace 1's
/// button is.
fn start_bar(release: bool) -> (Sway, ExampleBar, Range<u32>) {
  let sway = Sway::start();
  sway.msg(&[
    "output",
    "HEADLESS-1",
    "mode",
    "1920x1080",
    "position",
    "0",
    "0",
  ]);
  sway.msg(&["create_output"]);
  sway.msg(&[
    "output",
    "HEADLESS-2",
    "mode",
    "1920x1080",
    "position",
    "1920",
    "0",
  ]);
  let bar = ExampleBar::start(&sway, "bar", release, &[]);
  let deadline = Instant::now() + TIMEOUT;
  let one = loop {
    if let Some(one) = focused_button(&sway.screenshot("HEADLESS-1")) {
      break one;
    }
    assert!(
      Instant::now() < deadline,
      "workspace 1 never showed focused"
    );
    std::thread::sleep(Duration::from_millis(50));
  };
  wait_for_shot(&sway, "HEADLESS-2", "workspace 2, not focused", |shot| {
    focused_button(shot).is_none() && labelled(shot, &one)
  });
  assert_eq!(focused_in(&sway).as_deref(), Some("1"));
  (sway, bar, one)
}

#[test]
#[ignore = "needs headless Sway"]
fn the_bar_shows_each_outputs_workspaces_as_sway_switches() {
  let (sway, _bar, one) = start_bar(false);
  sway.msg(&["workspace", "2"]);
  // Each output's first button starts at the same place.
  wait_for_shot(&sway, "HEADLESS-2", "workspace 2, focused", |shot| {
    focused_button(shot).is_some_and(|two| two.start == one.start)
  });
  wait_for_shot(&sway, "HEADLESS-1", "workspace 1, not focused", |shot| {
    focused_button(shot).is_none() && labelled(shot, &one)
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn clicking_a_workspace_focuses_it_in_the_private_sway() {
  let (sway, _bar, one) = start_bar(false);
  sway.msg(&["workspace", "2"]);
  wait_for_shot(&sway, "HEADLESS-1", "workspace 1, not focused", |shot| {
    focused_button(shot).is_none()
  });
  let mut pointer = VirtualPointer::new(&sway);
  let x = (one.start + one.end) / 2;
  // The shell takes the new pointer while its loop runs, in its own
  // process; a click before that misses, so it clicks until Sway agrees.
  let deadline = Instant::now() + TIMEOUT;
  while focused_in(&sway).as_deref() != Some("1") {
    assert!(
      Instant::now() < deadline,
      "the click never focused workspace 1"
    );
    pointer.move_to(x, 15, (3840, 1080));
    pointer.click();
    std::thread::sleep(Duration::from_millis(200));
  }
  wait_for_shot(&sway, "HEADLESS-1", "workspace 1, focused again", |shot| {
    focused_button(shot) == Some(one.clone())
  });
}

#[test]
#[ignore = "needs headless Sway"]
fn each_workspace_switch_logs_its_time_to_the_screen() {
  // Release, as a config's template builds its dependencies optimized.
  // Each switch repaints both outputs' bars.
  let (sway, bar, _) = start_bar(true);
  let mut times = switch_workspaces(&sway, &bar, 10);
  // A bar that never finishes its frame, on an output that is off, doesn't
  // keep the other output's commit from showing the switch.
  sway.msg(&["output", "HEADLESS-2", "power", "off"]);
  times.extend(switch_workspaces(&sway, &bar, 4));
  let median = median(&times);
  let slowest = times.iter().copied().fold(0.0, f64::max);
  eprintln!("workspace switch to screen: median {median:.1} ms, each {times:.1?} ms");
  let frame = 1000.0 / 60.0;
  assert!(
    slowest <= frame,
    "a switch took {slowest:.1} ms, more than a frame at 60 Hz: {times:?}"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn an_idle_bar_wakes_only_on_the_minute() {
  let sway = Sway::start();
  // Connected, so libpulse's thread and socket are watched too.
  let server = PrivateServer::start("idle");
  let bar = ExampleBar::start(&sway, "bar", false, &[("PULSE_SERVER", &server.address())]);
  bar.wait_for(FIRST_FRAME);
  bar.wait_until_connected(&server);
  bar.wait_until_quiet();
  // 65 s holds a minute boundary or two, and the minute clock's tick on
  // each, early by up to 50 ms or late by the sampling.
  let wakeups = bar.wakeups_for(Duration::from_secs(65));
  eprintln!("idle wakeups, as (seconds past the minute, context switches): {wakeups:.2?}");
  assert!(!wakeups.is_empty(), "the clock never ticked in 65 s");
  for (past, _) in &wakeups {
    assert!(
      *past < 1.0 || *past > 59.9,
      "a wakeup {past:.2} s past the minute, away from the clock's tick: {wakeups:.2?}"
    );
  }
}

/// Log lines captured from this thread, while a guard from
/// [`install`](Logs::install) lives.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl io::Write for Logs {
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

impl Logs {
  fn install(&self) -> tracing::subscriber::DefaultGuard {
    let writer = self.clone();
    tracing::subscriber::set_default(
      tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .finish(),
    )
  }

  fn text(&self) -> String {
    String::from_utf8_lossy(&self.0.lock().expect("log buffer lock")).into_owned()
  }
}

/// How many windows `platform` holds for `output`.
fn on(platform: &Platform, output: &str) -> usize {
  platform
    .windows()
    .filter(|(_, window)| window.output().name() == output)
    .count()
}

#[test]
#[ignore = "needs headless Sway"]
fn a_panic_after_a_nested_per_output_doesnt_end_the_shell() {
  let sway = Sway::start();
  let logs = Logs::default();
  let _guard = logs.install();
  // f asks for more bars from its own context, then fails: that context
  // is gone by the time the nested request runs.
  let mut shell = Shell::new(sway.connect(), |cx| {
    cx.per_output(|cx: &Cx, output: &Output| -> PanelWindow {
      cx.per_output(counter::bar);
      panic!("the bar for {} fails", output.name());
    })
  })
  .expect("the root closure runs");
  let result = panic::catch_unwind(AssertUnwindSafe(|| {
    for _ in 0..50 {
      shell
        .dispatch(Some(Duration::from_millis(20)))
        .expect("the connection works");
    }
  }));
  assert!(
    result.is_ok(),
    "a panic escaped Shell::dispatch, which would end the shell"
  );
  let logs = logs.text();
  assert!(
    logs.contains("per_output[HEADLESS-1] (called at crates/crabshell/tests/headless.rs:"),
    "the panic is logged with the path of the per_output call: {logs}"
  );
  assert_eq!(shell.platform().windows().count(), 0);
}

#[test]
#[ignore = "needs headless Sway"]
fn a_nested_builders_bars_go_with_its_context() {
  let sway = Sway::start();
  let logs = Logs::default();
  let _guard = logs.install();
  let mut shell = Shell::new(sway.connect(), |cx| {
    cx.per_output(|cx: &Cx, output: &Output| {
      // An extra bar on every output, asked for while building
      // HEADLESS-1's: it belongs to HEADLESS-1's context.
      if output.name() == "HEADLESS-1" {
        cx.per_output(counter::bar);
      }
      counter::bar(cx, output)
    })
  })
  .expect("the root closure runs");
  run_until(&mut shell, "two bars on HEADLESS-1", |platform| {
    on(platform, "HEADLESS-1") == 2
  });
  sway.msg(&["create_output"]);
  run_until(&mut shell, "two bars on HEADLESS-2", |platform| {
    on(platform, "HEADLESS-2") == 2
  });
  settle(&mut shell);
  sway.msg(&["output", "HEADLESS-1", "unplug"]);
  run_until(
    &mut shell,
    "HEADLESS-1 and its extra bars to go",
    |platform| {
      platform.outputs().all(|output| output.name != "HEADLESS-1")
        && on(platform, "HEADLESS-2") == 1
    },
  );
  sway.msg(&["create_output"]);
  run_until(&mut shell, "a bar on HEADLESS-3", |platform| {
    on(platform, "HEADLESS-3") == 1
  });
  settle(&mut shell);
  assert_eq!(
    on(shell.platform(), "HEADLESS-3"),
    1,
    "only the root's builder is left"
  );
  let logs = logs.text();
  assert!(!logs.contains("panicked"), "{logs}");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_plain_unplug_logs_no_warning() {
  let sway = Sway::start();
  let logs = Logs::default();
  let _guard = logs.install();
  let mut shell = start(&sway);
  run_until(&mut shell, "a bar", |platform| {
    on(platform, "HEADLESS-1") == 1
  });
  sway.msg(&["create_output"]);
  run_until(&mut shell, "a second bar", |platform| {
    on(platform, "HEADLESS-2") == 1
  });
  settle(&mut shell);
  let mark = logs.text().len();
  sway.msg(&["output", "HEADLESS-2", "unplug"]);
  run_until(&mut shell, "HEADLESS-2 to go", |platform| {
    platform.outputs().all(|output| output.name != "HEADLESS-2")
  });
  settle(&mut shell);
  let during = logs.text()[mark..].to_owned();
  assert!(
    !during.contains("WARN") && !during.contains("ERROR"),
    "a plain unplug logged a warning or an error:\n{during}"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn dropping_a_shell_frees_its_nodes() {
  let sway = Sway::start();
  let runtime = Runtime::current();
  runtime.flush();
  let before = runtime.node_count();
  {
    let mut shell = start(&sway);
    run_until(&mut shell, "a bar", |platform| {
      on(platform, "HEADLESS-1") == 1
    });
    settle(&mut shell);
    assert!(runtime.node_count() > before);
  }
  runtime.flush();
  assert_eq!(
    runtime.node_count(),
    before,
    "nodes left after the Shell was dropped"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn the_root_context_hands_out_running_services() {
  let sway = Sway::start();
  let mut clock = None;
  let mut shell = Shell::new(sway.connect(), |cx| {
    clock = Some(cx.clock(Precision::Second));
  })
  .expect("the root closure runs");
  let clock = clock.expect("the root closure ran");
  // Without the services, cx.clock() hands out a clock that stands still.
  let first = clock.get_untracked();
  run_until(&mut shell, "a second clock to tick", |_| {
    clock.get_untracked() != first
  });
}

/// Set when this test binary runs again as a shell; the value says which.
const CHILD: &str = "CRABSHELL_HEADLESS_CHILD";

/// This test binary, to run again as a shell: the test called `test`,
/// which runs one when [`CHILD`] is set.
fn this_as_shell(test: &str) -> Command {
  let mut command = Command::new(std::env::current_exe().expect("the test binary"));
  command.args(["--exact", test, "--ignored", "--nocapture"]);
  command
}

/// In a child run as a shell: runs the shell `CHILD` names, which never
/// returns.
fn run_as_child() {
  match std::env::var(CHILD).as_deref() {
    Ok("panics") => crabshell::run(|_| panic!("a broken config")),
    Ok("counter") => crabshell::run(|cx| cx.per_output(counter::bar)),
    Ok("v1") => run_v1_bar_logging_its_volume(),
    _ => {}
  }
}

/// What [`run_v1_bar_logging_its_volume`] logs, before the volume button's
/// middle and label.
const VOLUME: &str = "the volume button at ";

/// The v1 bar on every output, on a loop of its own that logs the volume
/// button's middle and label whenever they change: what the bar shows,
/// read from its laid-out tree after each turn. Never returns.
fn run_v1_bar_logging_its_volume() -> ! {
  let event_loop = EventLoop::connect().expect("the private Sway");
  let mut shell =
    Shell::new(event_loop, |cx| cx.per_output(v1::bar)).expect("the root closure runs");
  let mut logged = String::new();
  loop {
    shell.dispatch(None).expect("the connection works");
    let platform = shell.platform();
    let Some(id) = shown_on(platform, "HEADLESS-1") else {
      continue;
    };
    // Between the tray and the clock.
    let ((x, y), label) = in_row(platform, id, 3, |button| {
      (middle(button.rect()), text_of(button))
    });
    let line = format!("{VOLUME}{x},{y} says {label}");
    if line != logged {
      eprintln!("{line}");
      logged = line;
    }
  }
}

/// Phase 3's exit check for audio, plus mute and the way back.
#[test]
#[ignore = "needs headless Sway and pipewire"]
fn the_volume_button_and_the_audio_server_agree() {
  run_as_child();
  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080"]);
  let server = PrivateServer::start("volume");
  server.pactl(&["set-sink-volume", "@DEFAULT_SINK@", "50%"]);
  let volume = || server.pactl(&["get-sink-volume", "@DEFAULT_SINK@"]);
  let mute = || server.pactl(&["get-sink-mute", "@DEFAULT_SINK@"]);
  // Before the shell, so the seat has a pointer when the shell binds it,
  // and the first scroll can't miss.
  let mut pointer = VirtualPointer::new(&sway);
  let bar = ExampleBar::spawn(
    &sway,
    this_as_shell("the_volume_button_and_the_audio_server_agree"),
    &[(CHILD, "v1"), ("PULSE_SERVER", &server.address())],
  );
  let layout = (1920, 1080);
  let (x, y) = bar.volume_says("vol 50%");

  // One notch up.
  pointer.move_to(x, y, layout);
  pointer.scroll(-1);
  bar.volume_says("vol 55%");
  assert!(volume().contains(" 55% "), "{}", volume());

  // Two notches down.
  pointer.scroll(1);
  pointer.scroll(1);
  let (x, y) = bar.volume_says("vol 45%");
  assert!(volume().contains(" 45% "), "{}", volume());

  // A click mutes, and another unmutes.
  pointer.move_to(x, y, layout);
  pointer.click();
  let (x, y) = bar.volume_says("muted");
  assert_eq!(mute().trim(), "Mute: yes");
  pointer.move_to(x, y, layout);
  pointer.click();
  bar.volume_says("vol 45%");
  assert_eq!(mute().trim(), "Mute: no");

  // A change from elsewhere shows.
  server.pactl(&["set-sink-volume", "@DEFAULT_SINK@", "30%"]);
  bar.volume_says("vol 30%");
}

#[test]
#[ignore = "needs headless Sway"]
fn a_root_that_panics_exits_1() {
  run_as_child();
  let sway = Sway::start();
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell("a_root_that_panics_exits_1"),
    &[(CHILD, "panics")],
  );
  shell.wait_for("the root closure panicked");
  let status = shell.exit_status().expect("the shell exits");
  assert_eq!(
    status.code(),
    Some(crabshell::EXIT_ROOT_PANICKED),
    "{status}"
  );
}

#[test]
#[ignore = "needs headless Sway"]
fn losing_the_compositor_exits_2() {
  run_as_child();
  let sway = Sway::start();
  let mut shell = ExampleBar::spawn(
    &sway,
    this_as_shell("losing_the_compositor_exits_2"),
    &[(CHILD, "counter")],
  );
  shell.wait_for("first frame committed");
  // Sway goes, as at logout.
  drop(sway);
  let status = shell.exit_status().expect("the shell exits");
  assert_eq!(
    status.code(),
    Some(crabshell::EXIT_NO_COMPOSITOR),
    "{status}"
  );
}

/// A private session bus, for the v1 bar's tray: `dbus-daemon --session`
/// on an address of its own. It stops on drop.
struct PrivateBus {
  daemon: Child,
  address: String,
}

impl PrivateBus {
  fn start() -> PrivateBus {
    let mut daemon = Command::new("dbus-daemon")
      .args(["--session", "--nofork", "--print-address"])
      .stdout(Stdio::piped())
      .spawn()
      .expect("dbus-daemon starts (it comes with dbus)");
    let stdout = daemon.stdout.take().expect("dbus-daemon's stdout");
    let mut address = String::new();
    BufReader::new(stdout)
      .read_line(&mut address)
      .expect("dbus-daemon prints its address");
    let address = address.trim().to_owned();
    PrivateBus { daemon, address }
  }
}

impl Drop for PrivateBus {
  fn drop(&mut self) {
    let _ = self.daemon.kill();
    let _ = self.daemon.wait();
  }
}

/// Waits for a shell to connect to `listener` and say it is ready; gives
/// the connection back.
fn accept_ready(listener: &std::os::unix::net::UnixListener) -> std::os::unix::net::UnixStream {
  use std::io::Read;

  listener
    .set_nonblocking(true)
    .expect("a non-blocking listener");
  let deadline = Instant::now() + Duration::from_secs(10);
  let mut stream = loop {
    match listener.accept() {
      Ok((stream, _)) => break stream,
      Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
        assert!(Instant::now() < deadline, "the shell never connected");
        std::thread::sleep(Duration::from_millis(20));
      }
      Err(error) => panic!("accept the shell: {error}"),
    }
  };
  stream.set_nonblocking(false).expect("a blocking stream");
  stream
    .set_read_timeout(Some(Duration::from_secs(10)))
    .expect("a read timeout");
  let mut said = [0_u8; 6];
  stream
    .read_exact(&mut said)
    .expect("the shell says something");
  assert_eq!(&said, b"ready\n");
  stream
}

/// The shell's half of a reload: it says `ready` on the supervisor's
/// socket once its bar has drawn, saves its persisted values and exits
/// with 0 on `save`, and the next shell starts from them.
#[test]
#[ignore = "needs headless Sway"]
fn a_reload_saves_the_count_and_the_next_shell_starts_from_it() {
  use std::io::Write;

  let sway = Sway::start();
  sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
  let socket = sway.runtime_dir().join("control.sock");
  let listener = std::os::unix::net::UnixListener::bind(&socket).expect("the control socket");
  let control = socket.to_str().expect("a UTF-8 path");
  let state = sway.runtime_dir().join("crabshell").join("state.json");
  let saved_count = || -> u64 {
    let text = std::fs::read_to_string(&state).expect("the saved values");
    let values: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    values["clicks on HEADLESS-1"].as_u64().expect("a count")
  };
  let mut pointer = VirtualPointer::new(&sway);

  let mut first = ExampleBar::start(&sway, "counter", false, &[("CRABSHELL_CONTROL", control)]);
  let mut supervisor = accept_ready(&listener);
  // The button starts at the bar's left padding.
  pointer.move_to(30, 15, (1920, 1080));
  pointer.click();
  std::thread::sleep(Duration::from_millis(500));
  supervisor.write_all(b"save\n").expect("told to save");
  let status = first.exit_status().expect("the shell exits");
  assert_eq!(status.code(), Some(0));
  assert_eq!(saved_count(), 1);

  let mut second = ExampleBar::start(&sway, "counter", false, &[("CRABSHELL_CONTROL", control)]);
  let mut supervisor = accept_ready(&listener);
  assert!(
    !state.exists(),
    "the next shell read the file and removed it"
  );
  supervisor.write_all(b"save\n").expect("told to save");
  let status = second.exit_status().expect("the shell exits");
  assert_eq!(status.code(), Some(0));
  assert_eq!(saved_count(), 1, "it started from the saved count");
}
