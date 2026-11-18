//! What the `crabshell` command's tests share: a home of their own, a
//! running supervisor, and a private session bus.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A fresh directory for one test's config and cache, removed on drop.
pub(crate) struct Home(pub(crate) PathBuf);

impl Home {
  pub(crate) fn new(name: &str) -> Home {
    let dir = std::env::temp_dir().join(format!("crabshell-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a test directory");
    Home(dir)
  }

  /// Runs `crabshell` with `args`, its config and cache here.
  pub(crate) fn run(&self, args: &[&str]) -> Output {
    let config = self.0.join("config");
    self.run_with(args, config.to_str().expect("UTF-8"))
  }

  /// [`run`](Self::run) with `XDG_CONFIG_HOME` set to `config_home`.
  pub(crate) fn run_with(&self, args: &[&str], config_home: &str) -> Output {
    self
      .command(args, config_home)
      .output()
      .expect("the crabshell binary runs")
  }

  /// `crabshell` with `args`, set up to run here.
  pub(crate) fn command(&self, args: &[&str], config_home: &str) -> Command {
    // The real cargo home, so building a config needs no download.
    let cargo_home = std::env::var_os("CARGO_HOME")
      .map(PathBuf::from)
      .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    let mut command = Command::new(env!("CARGO_BIN_EXE_crabshell"));
    if let Some(cargo_home) = cargo_home {
      command.env("CARGO_HOME", cargo_home);
    }
    command
      // Here, so a relative path can't land in the source tree.
      .current_dir(&self.0)
      .args(args)
      .env("HOME", &self.0)
      .env("XDG_CONFIG_HOME", config_home)
      .env("XDG_CACHE_HOME", self.0.join("cache"))
      .env("XDG_RUNTIME_DIR", self.0.join("run"))
      // Never the desktop's bus: a test's notification must not pop up.
      .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
      // Nor its compositors: libpulse alone would read X's root window,
      // which starts the desktop's Xwayland.
      .env_remove("DISPLAY")
      .env_remove("XAUTHORITY")
      .env_remove("WAYLAND_DISPLAY")
      .env_remove("SWAYSOCK");
    command
  }

  pub(crate) fn config(&self) -> PathBuf {
    self.0.join("config").join("crabshell")
  }
}

impl Drop for Home {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

/// Writes `script` at `path` as a program. `cp` writes it, as a write
/// handle open here would be copied into another test's fork until that
/// fork's exec, and running the program meanwhile fails with ETXTBSY.
pub(crate) fn write_program(path: &Path, script: &str) {
  use std::os::unix::fs::PermissionsExt;
  let draft = path.with_extension("draft");
  std::fs::write(&draft, script).expect("a program's draft");
  std::fs::set_permissions(&draft, std::fs::Permissions::from_mode(0o755)).expect("its mode");
  let copied = Command::new("cp").arg(&draft).arg(path).status();
  assert!(copied.expect("cp runs").success(), "cp copied {path:?}");
  std::fs::remove_file(&draft).expect("the draft removed");
}

pub(crate) fn text(bytes: &[u8]) -> String {
  String::from_utf8_lossy(bytes).into_owned()
}

/// The repo's root, two levels above this crate.
pub(crate) fn checkout() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

impl Home {
  /// `crabshell run` in its own process group, ended with the group.
  pub(crate) fn supervise(&self) -> Supervised {
    self.supervise_on(None)
  }

  /// [`supervise`](Self::supervise) with `bus` as the session bus.
  pub(crate) fn supervise_on(&self, bus: Option<&PrivateBus>) -> Supervised {
    let config_home = self.0.join("config");
    let mut command = self.command(&["run"], config_home.to_str().expect("UTF-8"));
    if let Some(bus) = bus {
      command.env("DBUS_SESSION_BUS_ADDRESS", &bus.address);
    }
    self.spawn_supervisor(command)
  }

  /// Starts `command`, a `crabshell run`, in its own process group, its
  /// stderr in `run.log` here.
  pub(crate) fn spawn_supervisor(&self, mut command: Command) -> Supervised {
    use std::os::unix::process::CommandExt;
    let log = self.0.join("run.log");
    let child = command
      .process_group(0)
      .stderr(std::fs::File::create(&log).expect("the log"))
      .spawn()
      .expect("crabshell run starts");
    Supervised {
      child: Some(child),
      log,
    }
  }
}

/// A private session bus that starts no services, so nothing on it
/// reaches the desktop, and a `dbus-monitor` logging the notifications
/// sent on it. Both stop on drop.
pub(crate) struct PrivateBus {
  daemon: Child,
  monitor: Child,
  pub(crate) address: String,
  log: PathBuf,
}

impl PrivateBus {
  pub(crate) fn start(home: &Home) -> PrivateBus {
    use std::io::BufRead;
    let config = home.0.join("bus.conf");
    std::fs::write(
      &config,
      "<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>\
       <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
       <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
    )
    .expect("written");
    let mut daemon = Command::new("dbus-daemon")
      .arg(format!("--config-file={}", config.display()))
      .args(["--nofork", "--print-address"])
      .stdout(Stdio::piped())
      .spawn()
      .expect("dbus-daemon starts (it comes with dbus)");
    let mut address = String::new();
    std::io::BufReader::new(daemon.stdout.take().expect("its stdout"))
      .read_line(&mut address)
      .expect("its address");
    let address = address.trim().to_owned();
    let log = home.0.join("monitor.log");
    let monitor = Command::new("dbus-monitor")
      .args([
        "--address",
        &address,
        "interface='org.freedesktop.Notifications'",
      ])
      .stdout(std::fs::File::create(&log).expect("the monitor's log"))
      .stderr(Stdio::null())
      .spawn();
    let monitor = monitor.unwrap_or_else(|error| {
      let _ = daemon.kill();
      let _ = daemon.wait();
      panic!("dbus-monitor didn't start: {error}");
    });
    // Built at once, so a failed wait below stops both.
    let bus = PrivateBus {
      daemon,
      monitor,
      address,
      log,
    };
    // It prints the name it got once it listens.
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(&bus.log)
      .unwrap_or_default()
      .is_empty()
    {
      assert!(Instant::now() < deadline, "dbus-monitor didn't start");
      std::thread::sleep(Duration::from_millis(20));
    }
    bus
  }

  /// Each notification sent so far, as its summary and body.
  pub(crate) fn notifications(&self) -> Vec<(String, String)> {
    let log = std::fs::read_to_string(&self.log).unwrap_or_default();
    let mut found = Vec::new();
    for call in log.split("member=Notify").skip(1) {
      // The strings in order: the app's name, its icon, the summary and
      // the body.
      let strings: Vec<String> = call
        .lines()
        .take_while(|line| !line.starts_with("method call") && !line.starts_with("signal"))
        .filter_map(|line| line.trim().strip_prefix("string \""))
        .map(|rest| rest.trim_end_matches('"').to_owned())
        .collect();
      if strings.len() >= 4 {
        found.push((strings[2].clone(), strings[3].clone()));
      }
    }
    found
  }

  /// Waits up to `limit` for `count` notifications.
  pub(crate) fn wait_for(&self, count: usize, limit: Duration) -> Vec<(String, String)> {
    let deadline = Instant::now() + limit;
    loop {
      let found = self.notifications();
      if found.len() >= count || Instant::now() > deadline {
        return found;
      }
      std::thread::sleep(Duration::from_millis(50));
    }
  }
}

impl Drop for PrivateBus {
  fn drop(&mut self) {
    for child in [&mut self.monitor, &mut self.daemon] {
      let _ = child.kill();
      let _ = child.wait();
    }
    // Killed, dbus-daemon can't remove its socket in /tmp, so it is removed here.
    if let Some(socket) = self
      .address
      .strip_prefix("unix:path=")
      .and_then(|rest| rest.split(',').next())
    {
      let _ = std::fs::remove_file(socket);
    }
  }
}

/// A running `crabshell run`. Dropping it kills its whole group, so a
/// failed test leaves no shell behind.
pub(crate) struct Supervised {
  child: Option<Child>,
  /// Where its stderr goes: a file, not a pipe, which a shell it left
  /// running would hold open.
  log: PathBuf,
}

impl Supervised {
  pub(crate) fn pid(&self) -> u32 {
    self.child.as_ref().expect("running").id()
  }

  pub(crate) fn signal(&self, signal: &str) {
    let sent = Command::new("kill")
      .args([signal, &self.pid().to_string()])
      .status()
      .expect("kill runs");
    assert!(sent.success());
  }

  /// Waits up to `limit` for the supervisor to end, and returns its exit
  /// code and what it logged.
  pub(crate) fn wait(mut self, limit: Duration) -> (Option<i32>, String) {
    let mut child = self.child.take().expect("running");
    let deadline = Instant::now() + limit;
    while child.try_wait().expect("waits").is_none() {
      if Instant::now() > deadline {
        self.child = Some(child);
        panic!("crabshell run didn't end within {limit:?}");
      }
      std::thread::sleep(Duration::from_millis(50));
    }
    // Anything it left running goes too.
    let group = format!("-{}", child.id());
    let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
    (child.wait().expect("waits").code(), self.log())
  }

  /// What it logged so far.
  pub(crate) fn log(&self) -> String {
    std::fs::read_to_string(&self.log).unwrap_or_default()
  }
}

impl Drop for Supervised {
  fn drop(&mut self) {
    // The home, and the log in it, goes next.
    if std::thread::panicking() {
      eprintln!("crabshell run's log:\n{}", self.log());
    }
    if let Some(mut child) = self.child.take() {
      let group = format!("-{}", child.id());
      let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
      let _ = child.wait();
    }
  }
}

/// Whether `message` is a compiler error in `log`, as rustc printed it, so
/// a test doesn't depend on rustc's wording.
pub(crate) fn is_compiler_error(log: &str, message: &str) -> bool {
  !message.is_empty()
    && log
      .lines()
      .any(|line| line.starts_with("error") && line.ends_with(&format!(": {message}")))
}

/// Whether process `pid` still runs.
pub(crate) fn alive(pid: u32) -> bool {
  std::fs::read_to_string(format!("/proc/{pid}/stat"))
    .is_ok_and(|stat| !stat.rsplit(") ").next().unwrap_or("").starts_with('Z'))
}

/// The context switches of every thread of `pid` so far, voluntary and
/// not.
pub(crate) fn switches(pid: u32) -> u64 {
  let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).expect("its threads");
  tasks
    .flatten()
    .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
    .flat_map(|status| {
      status
        .lines()
        .filter(|line| line.contains("ctxt_switches"))
        .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        .collect::<Vec<_>>()
    })
    .sum()
}
