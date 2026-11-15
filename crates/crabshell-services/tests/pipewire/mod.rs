//! A private PipeWire daemon and pipewire-pulse for one test, from the
//! configs beside this file, in a private runtime directory with no D-Bus.
//! Talk to it only through [`PrivateServer::address`]. It stops only the
//! two processes it started. `crabshell`'s headless tests include this
//! file too.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

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

  /// Plays silence to the default sink with `pacat`, as an app would,
  /// until the returned child is killed.
  pub fn play(&self) -> Child {
    let silence = std::fs::File::open("/dev/zero").expect("/dev/zero");
    Command::new("pacat")
      .arg(format!("--server={}", self.address()))
      .arg("--playback")
      .env_clear()
      .env("PATH", std::env::var_os("PATH").unwrap_or_default())
      .env("XDG_RUNTIME_DIR", &self.runtime)
      .stdin(silence)
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
      .expect("pacat starts")
  }

  /// Plays a 1 kHz square wave as loud as `left` and `right`, from 0 to
  /// 1, to the default sink with `pacat`, until the returned child is
  /// killed. Its peaks are the levels, though its average is 0.
  pub fn play_square(&self, left: f32, right: f32) -> Child {
    let mut player = Command::new("pacat")
      .arg(format!("--server={}", self.address()))
      .args(["--playback", "--rate=48000", "--format=float32le"])
      .args(["--channels=2", "--channel-map=front-left,front-right"])
      .env_clear()
      .env("PATH", std::env::var_os("PATH").unwrap_or_default())
      .env("XDG_RUNTIME_DIR", &self.runtime)
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
      .expect("pacat starts");
    let mut input = player.stdin.take().expect("pacat's input");
    // A second of it: 24 frames high, then 24 low.
    let frame = |sign: f32| [left * sign, right * sign].map(f32::to_le_bytes).concat();
    let half = |sign| frame(sign).repeat(24);
    let second = [half(1.0), half(-1.0)].concat().repeat(1000);
    // Ends when pacat is killed and the write fails.
    std::thread::spawn(move || while input.write_all(&second).is_ok() {});
    player
  }

  /// Links every stream to what it plays to or records, as a session
  /// manager would: a player to its target or the default sink, and a
  /// recording to the node it names, a sink's monitor or a stream. This
  /// server has none, so nothing plays or records until a test calls this.
  pub fn link(&self) {
    let dump = self.pipewire_tool("pw-dump", &[]);
    let objects: Vec<Value> = serde_json::from_str(&dump).expect("pw-dump's JSON");
    let of_type = |kind: &str| {
      let kind = format!("PipeWire:Interface:{kind}");
      objects
        .iter()
        .filter(move |object| object["type"] == kind.as_str())
    };
    let prop = |object: &Value, key: &str| object["info"]["props"][key].clone();
    let linked: Vec<(Value, Value)> = of_type("Link")
      .map(|link| {
        (
          link["info"]["output-port-id"].clone(),
          link["info"]["input-port-id"].clone(),
        )
      })
      .collect();
    let ports = |node: &Value, direction: &str| -> Vec<(Value, Value)> {
      let ports = of_type("Port").filter(|port| {
        prop(port, "node.id") == node["id"] && prop(port, "port.direction") == direction
      });
      ports
        .map(|port| (prop(port, "audio.channel"), port["id"].clone()))
        .collect()
    };
    for node in of_type("Node") {
      let (records, default) = match prop(node, "media.class").as_str() {
        Some("Stream/Output/Audio") => (false, "test-sink"),
        Some("Stream/Input/Audio") => (true, "test-source"),
        _ => continue,
      };
      let target = Some(prop(node, "target.object")).filter(|target| !target.is_null());
      let target = target.unwrap_or_else(|| default.into());
      let named = |other: &&Value| {
        prop(other, "node.name") == target || prop(other, "object.serial") == target
      };
      let Some(other) = of_type("Node").find(named) else {
        continue;
      };
      let (outputs, inputs) = match records {
        true => (ports(other, "out"), ports(node, "in")),
        false => (ports(node, "out"), ports(other, "in")),
      };
      for (channel, output) in outputs {
        let input = inputs.iter().find(|(other, _)| *other == channel);
        if let Some((_, input)) = input
          && !linked.contains(&(output.clone(), input.clone()))
        {
          self.pipewire_tool("pw-link", &[&output.to_string(), &input.to_string()]);
        }
      }
    }
  }

  /// Runs a PipeWire tool, such as `pw-dump`, against this daemon only,
  /// and returns what it printed.
  fn pipewire_tool(&self, tool: &str, args: &[&str]) -> String {
    let output = Command::new(tool)
      .args(args)
      .env_clear()
      .env("PATH", std::env::var_os("PATH").unwrap_or_default())
      .env("XDG_RUNTIME_DIR", &self.runtime)
      .env("PIPEWIRE_RUNTIME_DIR", &self.runtime)
      .env("PIPEWIRE_REMOTE", "pipewire-0")
      .output()
      .expect("a PipeWire tool runs");
    assert!(output.status.success(), "{tool} {args:?}: {output:?}");
    String::from_utf8(output.stdout).expect("UTF-8 from a PipeWire tool")
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
