//! A private headless Sway for one test, and helpers to drive the loop.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crabshell_wayland::{EventLoop, Platform};
use smithay_client_toolkit::reexports::client::Connection;

/// No wallpaper program (an `output bg` line would start swaybg), no
/// Xwayland, no borders.
const CONFIG: &str = "swaybg_command -\nxwayland disable\ndefault_border none\n";

/// How long any wait may take before the test fails.
const TIMEOUT: Duration = Duration::from_secs(10);

static RUNS: AtomicU32 = AtomicU32::new(0);

/// A headless Sway with its own runtime directory, so tests running at
/// once don't meet. It is stopped and its directory removed on drop; if
/// the test failed, its log is printed first.
pub struct Sway {
    dir: PathBuf,
    child: Child,
    /// The Wayland socket's name in `dir`.
    display: String,
}

impl Sway {
    /// Starts Sway with one output, HEADLESS-1.
    pub fn start() -> Sway {
        let limits = std::fs::read_to_string("/proc/self/limits").unwrap_or_default();
        if let Some(problem) = realtime_problem(&limits) {
            panic!("{problem}");
        }
        // Unix socket paths may be only 108 bytes long, so the directory
        // is short and under /tmp rather than in the target directory.
        let run = RUNS.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/csw-{}-{run}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the runtime directory");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .expect("make the runtime directory private");
        std::fs::write(dir.join("config"), CONFIG).expect("write the Sway config");
        let log = std::fs::File::create(dir.join("sway.log")).expect("create the Sway log");
        let child = Command::new("sway")
            .arg("-c")
            .arg(dir.join("config"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &dir)
            .env("XDG_RUNTIME_DIR", &dir)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env("WLR_RENDERER", "pixman")
            .env("SWAYSOCK", dir.join("sway.sock"))
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("share the log file"))
            .stderr(log)
            .spawn()
            .expect("sway is installed");
        let mut sway = Sway {
            dir,
            child,
            display: String::new(),
        };
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Ok(Some(status)) = sway.child.try_wait() {
                panic!("sway exited with {status}:\n{}", sway.log());
            }
            // Ready once both sockets exist and Sway answers on its IPC one.
            if sway.dir.join("sway.sock").exists()
                && let Some(display) = wayland_socket(&sway.dir)
                && sway.answers()
            {
                sway.display = display;
                break;
            }
            assert!(
                Instant::now() < deadline,
                "sway didn't start:\n{}",
                sway.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        sway
    }

    /// Whether Sway answers `swaymsg -t get_version`.
    fn answers(&self) -> bool {
        Command::new("swaymsg")
            .args(["-t", "get_version"])
            .env("SWAYSOCK", self.dir.join("sway.sock"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Sway's log so far.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("sway.log")).unwrap_or_default()
    }

    /// Runs `swaymsg` with `args` and returns what it printed.
    pub fn msg(&self, args: &[&str]) -> String {
        let output = Command::new("swaymsg")
            .args(args)
            .env("SWAYSOCK", self.dir.join("sway.sock"))
            .output()
            .expect("swaymsg is installed");
        let printed = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "swaymsg {args:?} failed: {printed}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        printed
    }

    /// A new connection to this Sway.
    pub fn connection(&self) -> Connection {
        let stream =
            UnixStream::connect(self.dir.join(&self.display)).expect("connect to sway's socket");
        Connection::from_socket(stream).expect("a Wayland connection")
    }

    /// An event loop connected to this Sway.
    pub fn connect(&self) -> EventLoop {
        EventLoop::with_connection(self.connection()).expect("sway offers what Crabshell needs")
    }

    /// A screenshot of `output` from grim, as RGBA.
    pub fn screenshot(&self, output: &str) -> Image {
        let shot = Command::new("grim")
            .args(["-o", output, "-"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_RUNTIME_DIR", &self.dir)
            .env("WAYLAND_DISPLAY", &self.display)
            .output()
            .expect("grim is installed");
        assert!(
            shot.status.success(),
            "grim failed: {}",
            String::from_utf8_lossy(&shot.stderr)
        );
        Image::decode(&shot.stdout)
    }
}

impl Drop for Sway {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // Sway may have died first; how is the first clue.
            let status = match self.child.try_wait() {
                Ok(Some(status)) => format!("sway had exited with {status}"),
                _ => String::from("sway was still running"),
            };
            eprintln!(
                "{status}; its log, from {}:\n{}",
                self.dir.display(),
                self.log()
            );
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Why Sway would be killed as it starts under these `/proc/self/limits`,
/// if it would. SwayFX has `cap_sys_nice` and makes itself realtime at
/// startup; with a realtime timeout (`RLIMIT_RTTIME`) of 0, as everything
/// started from T3 Code has, the kernel kills it with SIGKILL as soon as
/// it uses the CPU.
pub fn realtime_problem(limits: &str) -> Option<String> {
    let line = limits
        .lines()
        .find_map(|line| line.strip_prefix("Max realtime timeout"))?;
    let mut fields = line.split_whitespace();
    let (soft, hard) = (fields.next()?, fields.next()?);
    if soft != "0" && hard != "0" {
        return None;
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace);
    Some(format!(
        "Sway would be killed as it starts: this process's realtime timeout \
         (RLIMIT_RTTIME) is {soft} µs (hard limit {hard}), as under T3 Code, and Sway makes \
         itself realtime. Run the suite through the desktop Sway, which has no such \
         limit:\n\n  SWAYSOCK=/run/user/$(id -u)/sway-ipc.$(id -u).$(pgrep -xo sway).sock \
         swaymsg exec 'cd {} && ~/.cargo/bin/cargo test -p crabshell-wayland --test headless \
         -- --ignored --test-threads=1 > target/headless.log 2>&1 && echo passed > \
         target/headless.status || echo failed > target/headless.status'\n\n(No `;` in it: \
         Sway would take what follows one as a second command.)",
        workspace.display()
    ))
}

/// The `wayland-N` socket Sway made in `dir`.
fn wayland_socket(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("wayland-") && !name.ends_with(".lock"))
}

/// An RGBA image.
pub struct Image {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Image {
    /// Decodes an 8-bit RGB or RGBA PNG.
    pub fn decode(png_bytes: &[u8]) -> Image {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().expect("grim writes a PNG");
        let mut pixels = vec![0; reader.output_buffer_size().expect("a screenshot's size")];
        let info = reader.next_frame(&mut pixels).expect("the PNG decodes");
        pixels.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => pixels,
            png::ColorType::Rgb => pixels
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|&[red, green, blue]| [red, green, blue, 255])
                .collect(),
            other => panic!("unexpected screenshot color type {other:?}"),
        };
        Image {
            rgba,
            width: info.width,
            height: info.height,
        }
    }

    /// Writes the image as a PNG, for looking at after a failure.
    pub fn save(&self, path: &Path) {
        let file = std::fs::File::create(path).expect("create the image file");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write the PNG header");
        writer
            .write_image_data(&self.rgba)
            .expect("write the PNG pixels");
    }
}

/// Dispatches until `done` holds, failing the test after ten seconds.
pub fn run_until(
    event_loop: &mut EventLoop,
    what: &str,
    mut done: impl FnMut(&mut Platform) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    while !done(event_loop.platform()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        event_loop
            .dispatch(Some(Duration::from_millis(20)))
            .expect("the connection works");
    }
}

/// Dispatches until no frame has been committed for half a second.
pub fn settle(event_loop: &mut EventLoop) {
    let deadline = Instant::now() + TIMEOUT;
    let mut commits = event_loop.platform().stats().commits;
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < Duration::from_millis(500) {
        assert!(
            Instant::now() < deadline,
            "the windows never stopped drawing"
        );
        event_loop
            .dispatch(Some(Duration::from_millis(20)))
            .expect("the connection works");
        let now = event_loop.platform().stats().commits;
        if now != commits {
            commits = now;
            quiet_since = Instant::now();
        }
    }
}
