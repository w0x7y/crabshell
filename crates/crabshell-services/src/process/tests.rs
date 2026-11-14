use std::cell::RefCell;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;

use super::{MAX_OUTPUT, Process, decode};
use crate::ServicesExt;
use crate::testing::{Harness, capture_logs, offline};

fn sh(script: &str) -> Command {
  let mut command = Command::new("/bin/sh");
  command.arg("-c").arg(script);
  command
}

/// Runs the loop until `done`, for at most 5 s.
fn run_until(shell: &mut Harness, what: &str, done: impl Fn() -> bool) {
  let give_up = Instant::now() + Duration::from_secs(5);
  while !done() {
    assert!(Instant::now() < give_up, "no {what} after 5 s");
    shell.run_until(Instant::now() + Duration::from_millis(10));
  }
}

fn finish(shell: &mut Harness, process: &Process) {
  let status = process.exit_status();
  run_until(shell, "exit", move || status.get_untracked().is_some());
}

/// Whether `pid` is gone, reaped and all.
fn gone(pid: u32) -> bool {
  !Path::new(&format!("/proc/{pid}")).exists()
}

/// A temporary directory, gone on drop.
struct Dir(PathBuf);

impl Dir {
  fn new() -> Dir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-process-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    Dir(dir)
  }
}

impl Drop for Dir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn it_keeps_what_the_program_printed_and_how_it_ended() {
  let mut shell = Harness::new(offline());
  let process = shell
    .cx
    .process(|| sh("printf 'h\\303'; printf '\\251llo\\n'; echo oops >&2; exit 3"));
  assert!(process.running().get_untracked(), "running from the start");
  finish(&mut shell, &process);
  assert_eq!(process.stdout().get_untracked(), "h\u{e9}llo\n");
  assert_eq!(process.stderr().get_untracked(), "oops\n");
  assert_eq!(
    process.exit_status().get_untracked().unwrap().code(),
    Some(3)
  );
  assert!(!process.running().get_untracked());
}

#[test]
fn on_line_gets_each_line_and_the_rest_at_the_end() {
  let mut shell = Harness::new(offline());
  let lines = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&lines);
  let process = shell
    .cx
    .process(|| sh("printf 'a\\n\\nb\\nlast'"))
    .on_line(move |line| kept.borrow_mut().push(line.to_owned()));
  finish(&mut shell, &process);
  assert_eq!(*lines.borrow(), ["a", "", "b", "last"]);
}

#[test]
fn a_signal_the_command_read_starts_it_again() {
  let mut shell = Harness::new(offline());
  let word = shell.cx.signal("one");
  let process = shell
    .cx
    .process(move || sh(&format!("echo {}", word.get())));
  finish(&mut shell, &process);
  assert_eq!(process.stdout().get_untracked(), "one\n");
  word.set("two");
  Runtime::current().flush();
  assert_eq!(process.exit_status().get_untracked(), None, "running again");
  finish(&mut shell, &process);
  assert_eq!(process.stdout().get_untracked(), "two\n");
}

#[test]
fn restart_runs_it_again_and_stops_the_run_before() {
  let mut shell = Harness::new(offline());
  let pids = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&pids);
  let process = shell
    .cx
    .process(|| sh("echo $$; exec sleep 30"))
    .on_line(move |line| kept.borrow_mut().push(line.parse::<u32>().unwrap()));
  run_until(&mut shell, "a pid", || pids.borrow().len() == 1);
  process.restart();
  Runtime::current().flush();
  run_until(&mut shell, "a second pid", || pids.borrow().len() == 2);
  let first = pids.borrow()[0];
  run_until(&mut shell, "the first run reaped", || gone(first));
  assert!(process.running().get_untracked());
  process.kill();
  finish(&mut shell, &process);
  let status = process.exit_status().get_untracked().unwrap();
  assert_eq!(status.signal(), Some(15), "{status:?}");
}

#[test]
fn with_stdin_takes_what_is_written_even_before_it_starts() {
  let mut shell = Harness::new(offline());
  let process = shell.cx.process(|| Command::new("cat")).with_stdin();
  Runtime::current().flush();
  process.write("before ");
  shell.run_until(Instant::now() + Duration::from_millis(20));
  process.write(vec![b'x'; 300 * 1024]);
  process.write("\n");
  process.close_stdin();
  finish(&mut shell, &process);
  let out = process.stdout().get_untracked();
  assert!(out.starts_with("before xxx"), "{}", &out[..20]);
  assert_eq!(out.len(), 7 + 300 * 1024 + 1);
}

#[test]
fn a_write_without_stdin_is_dropped_and_logged() {
  let mut shell = Harness::new(offline());
  let process = shell.cx.process(|| Command::new("cat"));
  Runtime::current().flush();
  shell.run_until(Instant::now() + Duration::from_millis(20));
  let logs = capture_logs(|| process.write("lost"));
  assert!(logs.contains("takes no input"), "{logs}");
  finish(&mut shell, &process);
  assert_eq!(process.stdout().get_untracked(), "");
}

#[test]
fn its_context_going_stops_and_reaps_it() {
  let mut shell = Harness::new(offline());
  let child = shell.cx.child();
  let pid = Rc::new(RefCell::new(None));
  let kept = Rc::clone(&pid);
  let _process = child
    .process(|| sh("echo $$; exec sleep 30"))
    .on_line(move |line| *kept.borrow_mut() = line.parse::<u32>().ok());
  run_until(&mut shell, "a pid", || pid.borrow().is_some());
  child.owner().dispose();
  let pid = pid.borrow().unwrap();
  run_until(&mut shell, "the program reaped", || gone(pid));
}

#[test]
fn a_detached_program_is_reaped_when_it_ends() {
  let dir = Dir::new();
  let file = dir.0.join("pid");
  let mut shell = Harness::new(offline());
  shell
    .cx
    .exec_detached(sh(&format!("echo $$ > {}", file.display())));
  run_until(&mut shell, "the pid file", || {
    std::fs::read_to_string(&file).is_ok_and(|text| text.ends_with('\n'))
  });
  let pid: u32 = std::fs::read_to_string(&file)
    .unwrap()
    .trim()
    .parse()
    .unwrap();
  run_until(&mut shell, "the program reaped", || gone(pid));
  assert!(
    shell
      .cx
      .service::<super::Processes>()
      .unwrap()
      .reaping
      .borrow()
      .is_empty()
  );
}

#[test]
fn a_panicking_on_line_is_logged_and_disabled() {
  let mut shell = Harness::new(offline());
  let calls = Rc::new(RefCell::new(0));
  let counted = Rc::clone(&calls);
  let process = shell
    .cx
    // Two reads, so the second line comes after the panic.
    .process(|| sh("echo a; sleep 0.2; echo b"))
    .on_line(move |line| {
      *counted.borrow_mut() += 1;
      panic!("bad line {line}");
    });
  let logs = capture_logs(|| finish(&mut shell, &process));
  assert!(
    logs.contains(
      "process /bin/sh on_line (called at crates/crabshell-services/src/process/tests.rs:"
    ),
    "{logs}"
  );
  assert!(logs.contains("panicked: bad line a"), "{logs}");
  assert_eq!(*calls.borrow(), 1);
  assert_eq!(process.stdout().get_untracked(), "a\nb\n", "still kept");
}

#[test]
fn a_program_that_cant_start_is_logged_and_not_running() {
  let mut shell = Harness::new(offline());
  let mut process = None;
  let logs = capture_logs(|| {
    process = Some(
      shell
        .cx
        .process(|| Command::new("/nonexistent/crabshell-test")),
    );
    Runtime::current().flush();
    shell.run_until(Instant::now() + Duration::from_millis(20));
  });
  let process = process.unwrap();
  assert!(logs.contains("couldn't start the program"), "{logs}");
  assert!(!process.running().get_untracked());
  assert_eq!(process.exit_status().get_untracked(), None);
}

#[test]
fn an_end_shows_while_a_program_it_started_holds_the_output() {
  let mut shell = Harness::new(offline());
  let process = shell.cx.process(|| sh("sleep 2 & echo done"));
  let started = Instant::now();
  finish(&mut shell, &process);
  assert!(
    started.elapsed() < Duration::from_secs(1),
    "{:?}",
    started.elapsed()
  );
  assert_eq!(process.stdout().get_untracked(), "done\n");
}

#[test]
fn output_past_the_cap_isnt_kept() {
  let mut shell = Harness::new(offline());
  let process = shell
    .cx
    .process(|| sh("head -c 1100000 /dev/zero | tr '\\0' x"));
  let logs = capture_logs(|| finish(&mut shell, &process));
  assert_eq!(process.stdout().get_untracked().len(), MAX_OUTPUT);
  assert!(logs.contains("over 1 MiB"), "{logs}");
}

#[test]
fn a_quiet_running_program_wakes_nothing() {
  let mut shell = Harness::new(offline());
  let process = shell.cx.process(|| ["sleep", "30"]);
  Runtime::current().flush();
  shell.run_until(Instant::now() + Duration::from_millis(50));
  assert_eq!(
    shell.run_until(Instant::now() + Duration::from_secs(1)),
    1,
    "only the run's own deadline"
  );
  process.kill();
  finish(&mut shell, &process);
}

#[test]
fn a_panicking_command_is_logged_and_runs_nothing() {
  let mut shell = Harness::new(offline());
  let mut process = None;
  let logs = capture_logs(|| {
    process = Some(shell.cx.process(|| -> Command { panic!("no command") }));
    Runtime::current().flush();
    shell.run_until(Instant::now() + Duration::from_millis(20));
  });
  assert!(logs.contains("no command"), "{logs}");
  assert!(!process.unwrap().running().get_untracked());
}

#[test]
fn a_character_cut_by_a_read_waits_for_its_end() {
  let mut bytes = b"caf\xc3".to_vec();
  assert_eq!(decode(&mut bytes), "caf");
  assert_eq!(bytes, b"\xc3");
  bytes.extend_from_slice(b"\xa9 \xff!");
  assert_eq!(decode(&mut bytes), "\u{e9} \u{fffd}!");
  assert!(bytes.is_empty());
}

#[test]
fn a_command_can_be_a_list_and_an_empty_one_runs_nothing() {
  let mut shell = Harness::new(offline());
  let word = shell.cx.signal(String::from("hi"));
  let listed = shell
    .cx
    .process(move || vec![String::from("echo"), word.get()]);
  finish(&mut shell, &listed);
  assert_eq!(listed.stdout().get_untracked(), "hi\n");
  let mut empty = None;
  let logs = capture_logs(|| {
    empty = Some(shell.cx.process(Vec::<String>::new));
    Runtime::current().flush();
  });
  assert!(logs.contains("empty command"), "{logs}");
  assert!(!empty.unwrap().running().get_untracked());
}

#[test]
fn closing_the_input_first_sends_everything_written() {
  // Over the socket's buffer, so some of it waits when the input closes.
  for size in [64 * 1024, 4 * 1024 * 1024] {
    let mut shell = Harness::new(offline());
    let process = shell.cx.process(|| ["wc", "-c"]).with_stdin();
    process.write(vec![b'x'; size]);
    process.close_stdin();
    process.write("dropped");
    finish(&mut shell, &process);
    assert_eq!(process.stdout().get_untracked().trim(), size.to_string());
  }
}

#[test]
fn a_program_can_write_to_dev_stderr() {
  let mut shell = Harness::new(offline());
  let process = shell
    .cx
    .process(|| sh("echo x > /dev/stderr; echo y > /dev/stdout"));
  finish(&mut shell, &process);
  assert_eq!(process.stderr().get_untracked(), "x\n");
  assert_eq!(process.stdout().get_untracked(), "y\n");
}

#[test]
fn a_program_reaped_elsewhere_ends_its_run_without_spinning() {
  use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};
  let mut shell = Harness::new(offline());
  let pid = Rc::new(RefCell::new(None));
  let kept = Rc::clone(&pid);
  let process = shell
    .cx
    .process(|| sh("echo $$; exec sleep 30"))
    .on_line(move |line| *kept.borrow_mut() = line.parse::<i32>().ok());
  run_until(&mut shell, "a pid", || pid.borrow().is_some());
  let pid = Pid::from_raw(pid.borrow().unwrap()).unwrap();
  kill_process(pid, Signal::KILL).unwrap();
  waitpid(Some(pid), WaitOptions::empty()).unwrap();
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(200));
  assert!(wakeups < 10, "{wakeups} wakeups");
  assert!(!process.running().get_untracked());
}

#[test]
fn a_command_that_panics_later_stops_the_run_before_and_tries_again_on_a_change() {
  let mut shell = Harness::new(offline());
  let mode = shell.cx.signal(0);
  let pids = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&pids);
  let process = shell
    .cx
    .process(move || {
      assert!(mode.get() != 1, "mode 1");
      sh("echo $$; exec sleep 30")
    })
    .on_line(move |line| kept.borrow_mut().push(line.parse::<u32>().unwrap()));
  run_until(&mut shell, "a pid", || pids.borrow().len() == 1);
  let logs = capture_logs(|| {
    mode.set(1);
    Runtime::current().flush();
  });
  assert!(
    logs.contains("cx.process (called at crates/crabshell-services/src/process/tests.rs:"),
    "{logs}"
  );
  assert!(logs.contains("mode 1"), "{logs}");
  assert!(!process.running().get_untracked());
  let first = pids.borrow()[0];
  run_until(&mut shell, "the first run reaped", || gone(first));
  mode.set(2);
  run_until(&mut shell, "a run after the change", || {
    pids.borrow().len() == 2
  });
  process.kill();
  finish(&mut shell, &process);
}

#[test]
fn kill_before_the_start_means_it_never_starts() {
  let mut shell = Harness::new(offline());
  let process = shell.cx.process(|| ["echo", "started"]);
  Runtime::current().flush();
  process.kill();
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert!(!process.running().get_untracked());
  assert_eq!(process.stdout().get_untracked(), "");
}

#[test]
fn nothing_after_a_character_that_didnt_fit_is_kept() {
  let mut shell = Harness::new(offline());
  let script = format!(
    "head -c {} /dev/zero | tr '\\0' x; printf '\\303\\251'; sleep 0.1; printf abc",
    MAX_OUTPUT - 1
  );
  let process = shell.cx.process(move || sh(&script));
  capture_logs(|| finish(&mut shell, &process));
  let out = process.stdout().get_untracked();
  assert_eq!(out.len(), MAX_OUTPUT - 1);
  assert!(out.ends_with('x'));
}

#[test]
fn closing_the_input_after_a_run_ended_leaves_the_next_runs_input_open() {
  let mut shell = Harness::new(offline());
  let process = shell.cx.process(|| ["cat"]).with_stdin();
  process.write("one");
  process.close_stdin();
  finish(&mut shell, &process);
  assert_eq!(process.stdout().get_untracked(), "one");
  process.close_stdin();
  process.restart();
  Runtime::current().flush();
  process.write("two");
  process.close_stdin();
  finish(&mut shell, &process);
  assert_eq!(process.stdout().get_untracked(), "two");
}

#[test]
fn an_ended_program_wakes_nothing() {
  let mut shell = Harness::new(offline());
  let process = shell.cx.process(|| ["true"]);
  finish(&mut shell, &process);
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(300));
  assert_eq!(wakeups, 1, "only the run's own deadline");
}

#[test]
fn stop_programs_stops_every_program_before_the_shell_exits() {
  let mut shell = Harness::new(offline());
  let pid = Rc::new(RefCell::new(None));
  let kept = Rc::clone(&pid);
  let _process = shell
    .cx
    .process(|| sh("echo $$; exec sleep 30"))
    .on_line(move |line| *kept.borrow_mut() = line.parse::<u32>().ok());
  run_until(&mut shell, "a pid", || pid.borrow().is_some());
  crate::stop_programs(&shell.cx);
  let pid = pid.borrow().unwrap();
  run_until(&mut shell, "the program reaped", || gone(pid));
}

#[test]
fn the_end_shows_while_a_program_it_started_keeps_printing() {
  use rustix::process::{Pid, Signal, kill_process_group};
  let mut shell = Harness::new(offline());
  let pid = Rc::new(RefCell::new(None));
  let kept = Rc::clone(&pid);
  let process = shell
    .cx
    .process(|| sh("echo $$; yes & sleep 0.3; exit 0"))
    .on_line(move |line| {
      if kept.borrow().is_none() {
        *kept.borrow_mut() = line.parse::<i32>().ok();
      }
    });
  let started = Instant::now();
  capture_logs(|| finish(&mut shell, &process));
  let group = Pid::from_raw(pid.borrow().unwrap()).unwrap();
  let _ = kill_process_group(group, Signal::TERM);
  assert!(
    started.elapsed() < Duration::from_secs(3),
    "{:?}",
    started.elapsed()
  );
}
