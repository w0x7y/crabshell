//! Phase 2's exit criteria on headless Sway, one Sway per test: the
//! counter example matches its goldens at scales 1, 1.5 and 2, before and
//! after a click; an idle bar wakes nothing, by its own counters and from
//! outside; unplugging an output drops its bar and frees its nodes.
//!
//! They need `sway`, `swaymsg` and `grim`, so they are ignored by default.
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

use std::io::{self, BufRead, BufReader};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crabshell::Shell;
use crabshell::prelude::*;
use crabshell_reactive::Runtime;
use crabshell_ui::{Content, NodeView, Rect};
use crabshell_wayland::{Platform, WindowId};
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

/// The counter button: the bar's row's first child.
fn button<T>(platform: &Platform, id: WindowId, read: impl FnOnce(NodeView<'_>) -> T) -> T {
    let (_, window) = platform
        .windows()
        .find(|(held, _)| *held == id)
        .expect("the bar is held");
    let view = window.view();
    let row = view.root().children().next().expect("the bar has a row");
    read(
        row.children()
            .next()
            .expect("the row starts with the button"),
    )
}

/// The counter button's label.
fn label(platform: &Platform, id: WindowId) -> String {
    button(platform, id, |button| {
        let text = button
            .children()
            .next()
            .expect("the button holds its label");
        let Content::Text(text) = text.content() else {
            panic!("the label is text");
        };
        text.runs().map(|run| run.text).collect()
    })
}

/// The middle of the counter button, in logical pixels.
fn button_center(platform: &Platform, id: WindowId) -> (u32, u32) {
    let Rect {
        x,
        y,
        width,
        height,
    } = button(platform, id, |button| button.rect());
    ((x + width / 2.0) as u32, (y + height / 2.0) as u32)
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
        text.runs()
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
                shot.iter()
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

/// The counter example as its own program, the way `run` runs it, with
/// its log lines read as they come.
struct ExampleBar {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl ExampleBar {
    /// Builds the example (in release if `release`) and starts it on `sway`.
    fn start(sway: &Sway, release: bool) -> ExampleBar {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut build = Command::new(cargo);
        build
            .args(["build", "-q", "-p", "crabshell", "--example", "counter"])
            .current_dir(&workspace);
        if release {
            build.arg("--release");
        }
        assert!(
            build.status().is_ok_and(|status| status.success()),
            "the counter example builds"
        );
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.join("target"));
        let program = target
            .join(if release { "release" } else { "debug" })
            .join("examples/counter");
        ExampleBar::spawn(sway, Command::new(program), &[])
    }

    /// Starts `command` as a shell on `sway`, with `extra` in its otherwise
    /// cleared environment, and its log read as it comes.
    fn spawn(sway: &Sway, mut command: Command, extra: &[(&str, &str)]) -> ExampleBar {
        command.env_clear();
        for (name, value) in extra {
            command.env(name, value);
        }
        let mut child = command
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .env("XDG_RUNTIME_DIR", sway.runtime_dir())
            .env("WAYLAND_DISPLAY", sway.display())
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
                eprintln!("counter: {line}");
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
                Err(_) => panic!("the counter example never logged {needle:?}"),
            }
        }
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
    drop(shell);

    // From outside, for a minute: the example program, sleeping in the
    // loop `run` runs, gets no context switches at all. A timer as slow as
    // one a minute would show.
    let bar = ExampleBar::start(&sway, false);
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
        platform.windows().count() == 1
            && platform.outputs().all(|output| output.name != "HEADLESS-2")
    });
    runtime.flush();
    assert_eq!(
        runtime.node_count(),
        nodes,
        "per_output's owner for HEADLESS-2 was disposed"
    );
}

#[test]
#[ignore = "needs headless Sway; measures only with CRABSHELL_MEASURE set"]
fn measure_the_counter_bar() {
    if std::env::var_os("CRABSHELL_MEASURE").is_none() {
        eprintln!("not measuring: set CRABSHELL_MEASURE=1 to, which takes a minute");
        return;
    }
    let sway = Sway::start();
    sway.msg(&["output", "HEADLESS-1", "mode", "1920x1080", "scale", "1"]);
    let bar = ExampleBar::start(&sway, true);
    let line = bar.wait_for("first frame committed");
    let first_frame_ms: f64 = line
        .split("first frame committed ")
        .nth(1)
        .and_then(|rest| rest.split(" ms").next())
        .and_then(|ms| ms.parse().ok())
        .expect("the log line has the time");
    let started = Instant::now();
    bar.wait_until_quiet();
    let quiet = bar.context_switches();
    std::thread::sleep(Duration::from_secs(60).saturating_sub(started.elapsed()));
    let switches = bar.context_switches() - quiet;
    let rss = bar.rss_kib();
    let report = format!(
        "counter example, release build, one 1920x1080 headless output at scale 1: \
         first frame {first_frame_ms:.1} ms after run(), RSS {rss} KiB ({:.1} MiB) after 60 s, \
         {quiet} context switches by the time it was quiet, {switches} more while idle\n",
        rss as f64 / 1024.0
    );
    eprint!("{report}");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("headless");
    std::fs::create_dir_all(&dir).expect("create the output directory");
    std::fs::write(dir.join("measurements.txt"), report).expect("write the measurements");
}

/// Log lines captured from this thread, while a guard from
/// [`install`](Logs::install) lives.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl io::Write for Logs {
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
        _ => {}
    }
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
