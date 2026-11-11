//! A private PipeWire daemon and pipewire-pulse for one test, from the
//! configs beside this file, in a private runtime directory with no D-Bus.
//! Talk to it only through [`PrivateServer::address`]. It stops only the
//! two processes it started. `crabshell`'s headless tests include this
//! file too.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A PipeWire daemon and pipewire-pulse, both stopped on drop.
pub struct PrivateServer {
  runtime: PathBuf,
  daemon: Child,
  pulse: Option<Child>,
}

impl PrivateServer {
  pub fn start(name: &str) -> PrivateServer {
    // Short, since socket paths are capped at 108 bytes.
    let runtime = std::env::temp_dir().join(format!("crabshell-pw-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime);
    std::fs::create_dir_all(runtime.join("pulse")).expect("a runtime directory");
    std::fs::create_dir_all(runtime.join("config")).expect("a config directory");
    let private = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(&runtime, private).expect("a private runtime directory");
    // Embedded, so the configs travel with this file into other crates.
    for (config, text) in [
      ("daemon.conf", include_str!("daemon.conf")),
      ("pulse.conf", include_str!("pulse.conf")),
    ] {
      std::fs::write(runtime.join(config), text).expect("write a PipeWire config");
    }
    let daemon = pipewire(&runtime, "daemon.conf");
    wait_for(&runtime.join("pipewire-0"));
    let mut server = PrivateServer {
      runtime,
      daemon,
      pulse: None,
    };
    server.start_pulse();
    server
  }

  fn socket(&self) -> PathBuf {
    self.runtime.join("pulse/native")
  }

  /// In libpulse's server syntax, for `PULSE_SERVER`.
  pub fn address(&self) -> String {
    format!("unix:{}", self.socket().display())
  }

  pub fn start_pulse(&mut self) {
    self.pulse = Some(pipewire(&self.runtime, "pulse.conf"));
    wait_for(&self.socket());
  }

  /// Kills pipewire-pulse at once, as a crash would.
  pub fn kill_pulse(&mut self) {
    let mut pulse = self.pulse.take().expect("pipewire-pulse runs");
    pulse.kill().expect("pipewire-pulse killed");
    pulse.wait().expect("pipewire-pulse reaped");
    // So the next start is seen by its new socket.
    std::fs::remove_file(self.socket()).expect("the old socket");
  }

  /// Runs `pactl` against this server only, and returns what it printed.
  pub fn pactl(&self, args: &[&str]) -> String {
    let output = Command::new("pactl")
      .arg(format!("--server={}", self.address()))
      .args(args)
      .env_clear()
      .env("PATH", std::env::var_os("PATH").unwrap_or_default())
      .env("XDG_RUNTIME_DIR", &self.runtime)
      .output()
      .expect("pactl runs");
    assert!(output.status.success(), "pactl {args:?}: {output:?}");
    String::from_utf8(output.stdout).expect("UTF-8 from pactl")
  }

  pub fn volume(&self) -> String {
    self.pactl(&["get-sink-volume", "test-sink"])
  }
}

impl Drop for PrivateServer {
  fn drop(&mut self) {
    for child in self.pulse.iter_mut().chain([&mut self.daemon]) {
      let _ = child.kill();
      let _ = child.wait();
    }
    let _ = std::fs::remove_dir_all(&self.runtime);
  }
}

/// Starts `pipewire` with `config` from the runtime directory, seeing only
/// that directory.
fn pipewire(runtime: &Path, config: &str) -> Child {
  Command::new("pipewire")
    .arg("-c")
    .arg(runtime.join(config))
    .env_clear()
    .env("PATH", std::env::var_os("PATH").unwrap_or_default())
    .env("XDG_RUNTIME_DIR", runtime)
    .env("PIPEWIRE_RUNTIME_DIR", runtime)
    .env("PULSE_RUNTIME_PATH", runtime.join("pulse"))
    .env("XDG_CONFIG_HOME", runtime.join("config"))
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .expect("pipewire starts")
}

fn wait_for(socket: &Path) {
  let deadline = Instant::now() + Duration::from_secs(5);
  while !socket.exists() {
    assert!(Instant::now() < deadline, "no {}", socket.display());
    std::thread::sleep(Duration::from_millis(10));
  }
}
