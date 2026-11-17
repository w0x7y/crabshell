use std::path::Path;
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;

use super::{Sampler, Temperature};
use crate::testing::{Harness, TempDir, capture_logs, offline};
use crate::{Config, ServicesExt};

fn write(root: &Path, path: &str, text: &str) {
  let path = root.join(path);
  std::fs::create_dir_all(path.parent().unwrap()).unwrap();
  std::fs::write(path, text).unwrap();
}

/// A `/proc/stat` with every CPU's and two CPUs' counters: user, nice,
/// system, idle, iowait, irq, softirq, steal, guest, guest_nice. Guest
/// time is counted in user time too, as the kernel does.
fn stat(all: [u64; 4], cpu0: [u64; 4], cpu1: [u64; 4]) -> String {
  let line = |label: &str, [user, system, idle, iowait]: [u64; 4]| {
    format!("{label} {user} 0 {system} {idle} {iowait} 0 0 0 {user} 0\n")
  };
  line("cpu ", all) + &line("cpu0", cpu0) + &line("cpu1", cpu1) + "intr 1 2 3\nctxt 99\n"
}

fn sampler(dir: &TempDir) -> Sampler {
  Sampler::new(&dir.path().join("proc"), &dir.path().join("sys"))
}

#[test]
fn cpu_use_is_the_busy_share_since_the_last_sample() {
  let dir = TempDir::new();
  let proc = dir.path().join("proc");
  write(
    &proc,
    "stat",
    &stat([100, 0, 800, 100], [50, 0, 400, 50], [50, 0, 400, 50]),
  );
  let mut sampler = sampler(&dir);
  let first = sampler.sample();
  assert_eq!(
    (first.cpu, first.cpus.as_slice()),
    (0.0, [0.0, 0.0].as_slice())
  );
  // 100 busy ticks of 400 overall; cpu0 all busy, cpu1 all idle or
  // waiting, which counts as idle.
  write(
    &proc,
    "stat",
    &stat([150, 50, 1000, 200], [150, 50, 400, 50], [50, 0, 600, 150]),
  );
  let second = sampler.sample();
  assert_eq!(second.cpu, 0.25);
  assert_eq!(second.cpus, [1.0, 0.0]);
  // No time passed: no share, not a division by zero.
  assert_eq!(sampler.sample().cpu, 0.0);
}

#[test]
fn memory_is_in_bytes() {
  let dir = TempDir::new();
  let meminfo = "MemTotal:       16000 kB\nMemFree:  1 kB\nMemAvailable:    8000 kB\n\
    SwapTotal:  4000 kB\nSwapFree:  3000 kB\nHugePages_Total:       0\n";
  write(&dir.path().join("proc"), "meminfo", meminfo);
  let stats = sampler(&dir).sample();
  assert_eq!(
    (
      stats.memory_total,
      stats.memory_available,
      stats.swap_total,
      stats.swap_free
    ),
    (16000 * 1024, 8000 * 1024, 4000 * 1024, 3000 * 1024)
  );
}

#[test]
fn temperatures_are_named_by_chip_and_label_or_number() {
  let dir = TempDir::new();
  let sys = dir.path().join("sys/class/hwmon");
  write(&sys, "hwmon10/name", "late\n");
  write(&sys, "hwmon10/temp1_input", "20000\n");
  write(&sys, "hwmon2/name", "coretemp\n");
  write(&sys, "hwmon2/temp10_input", "51500\n");
  write(&sys, "hwmon2/temp10_label", "Core 8\n");
  write(&sys, "hwmon2/temp2_input", "-1000\n");
  write(&sys, "hwmon2/temp2_label", "Core 0\n");
  write(&sys, "hwmon2/fan1_input", "1200\n");
  write(&sys, "hwmon3/name", "acpitz\n");
  write(&sys, "hwmon3/temp1_input", "45000\n");
  write(&sys, "hwmon4/temp1_input", "30000\n");
  write(&sys, "not-hwmon/temp1_input", "1\n");
  let temperature = |name: &str, celsius: f32| Temperature {
    name: name.to_owned(),
    celsius,
  };
  assert_eq!(
    sampler(&dir).sample().temperatures,
    [
      temperature("coretemp Core 0", -1.0),
      temperature("coretemp Core 8", 51.5),
      temperature("acpitz temp1", 45.0),
      temperature("hwmon4 temp1", 30.0),
      temperature("late temp1", 20.0),
    ]
  );
}

#[test]
fn bad_lines_are_skipped_and_logged_once() {
  let dir = TempDir::new();
  let proc = dir.path().join("proc");
  let stat = "cpu  10 0 10 80\ncpu0 x 0 10 80\ncpu1 1 2\ncpuX 1 2 3 4\ncpu2 10 0 10 80\n";
  write(&proc, "stat", stat);
  write(
    &proc,
    "meminfo",
    "MemTotal: lots kB\nMemAvailable: 5 MB\nSwapTotal: 7 kB\n",
  );
  write(
    &dir.path().join("sys/class/hwmon"),
    "hwmon0/temp1_input",
    "hot\n",
  );
  let mut sampler = sampler(&dir);
  let logs = capture_logs(|| {
    for _ in 0..2 {
      let stats = sampler.sample();
      assert_eq!(stats.cpus.len(), 1);
      assert_eq!((stats.memory_total, stats.swap_total), (0, 7 * 1024));
      assert!(stats.temperatures.is_empty());
    }
  });
  for problem in ["CPU line", "meminfo line", "doesn't hold a temperature"] {
    assert_eq!(logs.matches(problem).count(), 1, "{problem}: {logs}");
  }
}

#[test]
fn cpus_and_temperatures_are_capped() {
  let dir = TempDir::new();
  let mut stat = String::from("cpu 1 0 1 1\n");
  for n in 0..300 {
    stat += &format!("cpu{n} 1 0 1 1\n");
  }
  write(&dir.path().join("proc"), "stat", &stat);
  let sys = dir.path().join("sys/class/hwmon");
  for n in 0..70 {
    write(&sys, &format!("hwmon0/temp{n}_input"), "1000");
  }
  let mut sampler = sampler(&dir);
  let logs = capture_logs(|| {
    for _ in 0..2 {
      let stats = sampler.sample();
      assert_eq!((stats.cpus.len(), stats.temperatures.len()), (256, 64));
    }
  });
  assert_eq!(logs.matches("over 256 CPUs").count(), 1, "{logs}");
  assert_eq!(logs.matches("over 64 temperatures").count(), 1, "{logs}");
}

#[test]
fn a_file_is_read_up_to_64_kib() {
  let dir = TempDir::new();
  let filler = "x".repeat(70 * 1024);
  write(
    &dir.path().join("proc"),
    "meminfo",
    &format!("{filler}\nMemTotal: 1 kB\n"),
  );
  assert_eq!(sampler(&dir).sample().memory_total, 0);
}

fn fake(dir: &TempDir) -> Config {
  let proc = dir.path().join("proc");
  write(
    &proc,
    "stat",
    &stat([1, 0, 1, 0], [1, 0, 1, 0], [0, 0, 0, 0]),
  );
  write(&proc, "meminfo", "MemTotal: 2 kB\n");
  Config {
    proc,
    sys: dir.path().join("sys"),
    ..offline()
  }
}

fn run(shell: &mut Harness, ms: u64) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(ms))
}

#[test]
fn stats_are_sampled_at_once_then_every_period_until_the_context_goes() {
  let dir = TempDir::new();
  let mut shell = Harness::new(fake(&dir));
  let nodes = Runtime::current().node_count();
  let child = shell.cx.child();
  let stats = child.system_stats(Duration::from_millis(300));
  assert_eq!(stats.get_untracked().memory_total, 2048);
  write(&dir.path().join("proc"), "meminfo", "MemTotal: 3 kB\n");
  assert_eq!(run(&mut shell, 400), 2);
  assert_eq!(stats.get_untracked().memory_total, 3072);

  child.owner().dispose();
  Runtime::current().flush();
  assert_eq!(Runtime::current().node_count(), nodes);
  assert_eq!(run(&mut shell, 700), 1);
}

#[test]
fn a_period_under_250_ms_is_250_ms() {
  let dir = TempDir::new();
  let mut shell = Harness::new(fake(&dir));
  let _stats = shell.cx.system_stats(Duration::from_millis(1));
  // Ticks at 250, 500 and 750 ms, and the deadline.
  let wakeups = run(&mut shell, 900);
  assert!((3..=4).contains(&wakeups), "{wakeups} wakeups");
}

#[test]
fn a_huge_period_is_the_longest_a_timer_takes() {
  let dir = TempDir::new();
  let mut shell = Harness::new(fake(&dir));
  let stats = shell.cx.system_stats(Duration::MAX);
  assert_eq!(stats.get_untracked().memory_total, 2048);
  assert_eq!(run(&mut shell, 100), 1);
}

#[test]
fn cpus_are_matched_by_number_when_one_goes_offline() {
  let dir = TempDir::new();
  let proc = dir.path().join("proc");
  let line = |label: &str, busy: u64, idle: u64| format!("{label} {busy} 0 0 {idle}\n");
  write(
    &proc,
    "stat",
    &(line("cpu ", 0, 0) + &line("cpu0", 0, 100) + &line("cpu1", 0, 100) + &line("cpu2", 100, 0)),
  );
  let mut sampler = sampler(&dir);
  sampler.sample();
  // cpu1 went offline: cpu2 is compared with cpu2, not cpu1.
  write(
    &proc,
    "stat",
    &(line("cpu ", 0, 0) + &line("cpu0", 0, 200) + &line("cpu2", 200, 0)),
  );
  assert_eq!(sampler.sample().cpus, [0.0, 1.0]);
  // Back online, it starts from 0.
  write(
    &proc,
    "stat",
    &(line("cpu ", 0, 0) + &line("cpu0", 0, 300) + &line("cpu1", 50, 150) + &line("cpu2", 300, 0)),
  );
  assert_eq!(sampler.sample().cpus, [0.0, 0.0, 1.0]);
}

#[test]
fn sensors_are_listed_once_and_again_when_one_is_gone() {
  let dir = TempDir::new();
  let sys = dir.path().join("sys/class/hwmon");
  write(&sys, "hwmon0/name", "first\n");
  write(&sys, "hwmon0/temp1_input", "1000\n");
  let mut sampler = sampler(&dir);
  let named = |sampler: &mut Sampler| -> Vec<String> {
    let stats = sampler.sample();
    stats.temperatures.into_iter().map(|t| t.name).collect()
  };
  assert_eq!(named(&mut sampler), ["first temp1"]);
  write(&sys, "hwmon0/temp1_label", "renamed\n");
  write(&sys, "hwmon1/temp1_input", "2000\n");
  assert_eq!(named(&mut sampler), ["first temp1"]);
  // A driver reloaded: the old sensor is gone.
  std::fs::remove_dir_all(sys.join("hwmon0")).unwrap();
  assert_eq!(named(&mut sampler), Vec::<String>::new());
  assert_eq!(named(&mut sampler), ["hwmon1 temp1"]);
}

#[test]
fn a_sleeping_chip_isnt_read() {
  let dir = TempDir::new();
  let sys = dir.path().join("sys/class/hwmon");
  write(&sys, "hwmon0/name", "amdgpu\n");
  write(&sys, "hwmon0/device/power/runtime_status", "suspended\n");
  // Not a reading: it would be logged if read.
  write(&sys, "hwmon0/temp1_input", "asleep\n");
  write(&sys, "hwmon1/name", "coretemp\n");
  write(&sys, "hwmon1/temp1_input", "40000\n");
  let mut sampler = sampler(&dir);
  let logs = capture_logs(|| {
    let names: Vec<_> = sampler
      .sample()
      .temperatures
      .into_iter()
      .map(|t| t.name)
      .collect();
    assert_eq!(names, ["coretemp temp1"]);
  });
  assert!(!logs.contains("temp1_input"), "{logs}");
  write(&sys, "hwmon0/device/power/runtime_status", "active\n");
  write(&sys, "hwmon0/temp1_input", "50000\n");
  assert_eq!(sampler.sample().temperatures.len(), 2);
}

#[test]
fn each_unreadable_file_is_logged_once() {
  let dir = TempDir::new();
  std::fs::create_dir_all(dir.path().join("sys/class/hwmon")).unwrap();
  let mut sampler = sampler(&dir);
  let logs = capture_logs(|| {
    sampler.sample();
    sampler.sample();
  });
  assert_eq!(logs.matches("can't read").count(), 2, "{logs}");
  assert!(
    logs.contains("proc/stat") && logs.contains("proc/meminfo"),
    "{logs}"
  );
}
