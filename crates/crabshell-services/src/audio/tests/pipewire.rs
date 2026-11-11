//! The audio service against a real pipewire-pulse of its own. Ignored,
//! since they need `pipewire` and `pactl`; run them by hand:
//!
//! ```sh
//! cargo test -p crabshell-services --lib audio::tests::pipewire -- --ignored --test-threads=1
//! ```
//!
//! Each test starts a PipeWire daemon and pipewire-pulse from the configs
//! in the crate's `tests/pipewire`, in a private runtime directory with no
//! D-Bus, and talks to them only through that directory's socket. It
//! stops only the two processes it started.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{Harness, libpulse_switches, logs_of, one_at_a_time};
use crate::ServicesExt;

/// A PipeWire daemon and pipewire-pulse, both stopped on drop.
struct PrivateServer {
    runtime: PathBuf,
    daemon: Child,
    pulse: Option<Child>,
}

impl PrivateServer {
    fn start(name: &str) -> PrivateServer {
        // Short, since socket paths are capped at 108 bytes.
        let runtime =
            std::env::temp_dir().join(format!("crabshell-pw-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime);
        std::fs::create_dir_all(runtime.join("pulse")).expect("a runtime directory");
        std::fs::create_dir_all(runtime.join("config")).expect("a config directory");
        let private = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(&runtime, private).expect("a private runtime directory");
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

    fn address(&self) -> String {
        format!("unix:{}", self.socket().display())
    }

    fn start_pulse(&mut self) {
        self.pulse = Some(pipewire(&self.runtime, "pulse.conf"));
        wait_for(&self.socket());
    }

    /// Kills pipewire-pulse at once, as a crash would.
    fn kill_pulse(&mut self) {
        let mut pulse = self.pulse.take().expect("pipewire-pulse runs");
        pulse.kill().expect("pipewire-pulse killed");
        pulse.wait().expect("pipewire-pulse reaped");
        // So the next start is seen by its new socket.
        std::fs::remove_file(self.socket()).expect("the old socket");
    }

    /// Runs `pactl` against this server only, and returns what it printed.
    fn pactl(&self, args: &[&str]) -> String {
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

    fn volume(&self) -> String {
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

/// Starts `pipewire` with `config` from `tests/pipewire`, seeing only the
/// private runtime directory.
fn pipewire(runtime: &Path, config: &str) -> Child {
    let configs = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pipewire");
    Command::new("pipewire")
        .arg("-c")
        .arg(configs.join(config))
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

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_shown_sink_wakes_nothing_while_it_stays() {
    let _one = one_at_a_time();
    let server = PrivateServer::start("idle");
    let mut shell = Harness::new(&server.address());
    let sink = shell.cx.audio().default_sink();
    shell.run_until(|_| sink.get_untracked().is_some());
    let shown = sink.get_untracked().expect("a sink");
    assert_eq!(shown.name, "test-sink");
    // Let anything the connect started finish.
    shell.run_for(Duration::from_millis(500));
    let switches = libpulse_switches();
    // One wakeup: the one that ends the run.
    assert_eq!(shell.run_for(Duration::from_secs(3)), 1);
    assert_eq!(libpulse_switches(), switches, "libpulse's thread slept");
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn volume_and_mute_reach_the_server() {
    let _one = one_at_a_time();
    let server = PrivateServer::start("commands");
    server.pactl(&["set-sink-volume", "test-sink", "50%"]);
    let mut shell = Harness::new(&server.address());
    let audio = shell.cx.audio();
    let sink = audio.default_sink();
    let percent = move || sink.get_untracked().map(|sink| sink.volume_percent);
    shell.run_until(|_| percent() == Some(50));
    audio.change_volume(5);
    shell.run_until(|_| percent() == Some(55));
    assert!(server.volume().contains(" 55% "), "{}", server.volume());
    audio.toggle_mute();
    shell.run_until(|_| sink.get_untracked().is_some_and(|sink| sink.muted));
    let muted = server.pactl(&["get-sink-mute", "test-sink"]);
    assert_eq!(muted.trim(), "Mute: yes");
    audio.toggle_mute();
    shell.run_until(|_| sink.get_untracked().is_some_and(|sink| !sink.muted));
    let muted = server.pactl(&["get-sink-mute", "test-sink"]);
    assert_eq!(muted.trim(), "Mute: no");
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_fast_scroll_lands_every_step() {
    let _one = one_at_a_time();
    let server = PrivateServer::start("scroll");
    let mut shell = Harness::new(&server.address());
    let audio = shell.cx.audio();
    let sink = audio.default_sink();
    let percent = move || sink.get_untracked().map(|sink| sink.volume_percent);
    for burst in 0..8 {
        server.pactl(&["set-sink-volume", "test-sink", "50%"]);
        shell.run_until(|_| percent() == Some(50));
        // One or two turns of the loop per wheel click, so the server's
        // replies to one step come back before the next.
        for _ in 0..8 {
            audio.change_volume(5);
            for _ in 0..=burst % 2 {
                shell.turn();
            }
        }
        shell.run_for(Duration::from_millis(500));
        assert!(
            server.volume().contains(" 90% "),
            "burst {burst}: {}",
            server.volume()
        );
        assert_eq!(percent(), Some(90), "burst {burst}");
    }
}

#[test]
#[ignore = "needs pipewire; run by hand"]
fn a_killed_server_gives_none_then_the_sink_again() {
    let _one = one_at_a_time();
    let mut server = PrivateServer::start("killed");
    let logs = logs_of(|| {
        let mut shell = Harness::new(&server.address());
        let sink = shell.cx.audio().default_sink();
        shell.run_until(|_| sink.get_untracked().is_some());
        server.kill_pulse();
        shell.run_until(|_| sink.get_untracked().is_none());
        let lost = shell.lost();
        // Tries every 50 ms while the server is down.
        shell.run_for(Duration::from_millis(300));
        assert!(shell.lost() >= lost + 2, "{} tries", shell.lost() - lost);
        server.start_pulse();
        shell.run_until(|_| sink.get_untracked().is_some());
    });
    assert_eq!(logs.matches("no audio server").count(), 1, "{logs}");
    assert_eq!(
        logs.matches("the audio server is back").count(),
        1,
        "{logs}"
    );
}
