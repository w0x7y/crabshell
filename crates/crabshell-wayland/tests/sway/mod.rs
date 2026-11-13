//! A private headless Sway for one test, and helpers to drive the loop.
//! `crabshell`'s headless tests include this file too.

use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crabshell_wayland::{EventLoop, Platform};
use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{wl_pointer, wl_registry, wl_seat};
use smithay_client_toolkit::reexports::client::{
  Connection, Dispatch, EventQueue, QueueHandle, delegate_noop,
};
use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::{
  zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
  zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::{
  zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
  zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

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
  /// Starts Sway with one output, HEADLESS-1. Refuses when this process
  /// has `SWAYSOCK` set, since an in-process shell would hand it to its
  /// services, and it may name the desktop's Sway.
  pub fn start() -> Sway {
    if let Some(socket) = std::env::var_os("SWAYSOCK") {
      // The test target's name: cargo names its binary `<name>-<hash>`.
      let exe = std::env::current_exe().unwrap_or_default();
      let stem = exe.file_stem().unwrap_or_default().to_string_lossy();
      let target = stem.rsplit_once('-').map_or(&*stem, |(name, _)| name);
      panic!(
        "this test process has SWAYSOCK={socket:?}, maybe the desktop's Sway, so no \
                 private Sway starts. Run the suite as\n  env -u SWAYSOCK cargo test -p {} \
                 --test {target} -- --ignored --test-threads=1",
        env!("CARGO_PKG_NAME")
      );
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
    // A copy drops SwayFX's `cap_sys_nice`, so it never goes realtime
    // and a realtime CPU limit of 0 (as under T3 Code) can't kill it.
    let installed = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
      .map(|bin| bin.join("sway"))
      .find(|sway| sway.is_file())
      .expect("sway is installed");
    let sway = dir.join("sway");
    std::fs::copy(&installed, &sway)
      .unwrap_or_else(|error| panic!("copy {installed:?} to {sway:?}: {error}"));
    let child = Command::new(&sway)
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

  /// Starts a headless labwc instead, to check the platform on another
  /// wlroots compositor. It gets its own runtime directory, an empty
  /// config and one output, HEADLESS-1. `msg` doesn't apply to it.
  #[allow(dead_code, reason = "crabshell's tests don't use it")]
  pub fn start_labwc() -> Sway {
    let run = RUNS.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(format!("/tmp/csl-{}-{run}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).expect("create the runtime directory");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
      .expect("make the runtime directory private");
    let log = std::fs::File::create(dir.join("sway.log")).expect("create the log");
    let child = Command::new("labwc")
      .env_clear()
      .env("PATH", std::env::var_os("PATH").unwrap_or_default())
      .env("HOME", &dir)
      .env("XDG_RUNTIME_DIR", &dir)
      .env("XDG_CONFIG_HOME", dir.join("config"))
      .env("WLR_BACKENDS", "headless")
      .env("WLR_LIBINPUT_NO_DEVICES", "1")
      .env("WLR_RENDERER", "pixman")
      .stdin(Stdio::null())
      .stdout(log.try_clone().expect("share the log file"))
      .stderr(log)
      .spawn()
      .expect("labwc is installed");
    let mut labwc = Sway {
      dir,
      child,
      display: String::new(),
    };
    let deadline = Instant::now() + TIMEOUT;
    loop {
      if let Ok(Some(status)) = labwc.child.try_wait() {
        panic!("labwc exited with {status}:\n{}", labwc.log());
      }
      if let Some(display) = wayland_socket(&labwc.dir)
        && UnixStream::connect(labwc.dir.join(&display)).is_ok()
      {
        labwc.display = display;
        break;
      }
      assert!(
        Instant::now() < deadline,
        "labwc didn't start:\n{}",
        labwc.log()
      );
      std::thread::sleep(Duration::from_millis(20));
    }
    labwc
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

  /// The runtime directory, which a client's `XDG_RUNTIME_DIR` names.
  #[allow(dead_code, reason = "crabshell's tests use it; these don't")]
  pub fn runtime_dir(&self) -> &Path {
    &self.dir
  }

  /// The Wayland socket's name, which a client's `WAYLAND_DISPLAY` names.
  #[allow(dead_code, reason = "crabshell's tests use it; these don't")]
  pub fn display(&self) -> &str {
    &self.display
  }

  /// Sway's IPC socket, which a client's `SWAYSOCK` names.
  #[allow(dead_code, reason = "crabshell's tests use it; these don't")]
  pub fn socket(&self) -> PathBuf {
    self.dir.join("sway.sock")
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

/// A second client with a virtual pointer, so the seat has a pointer: a
/// headless Sway with no input devices has none.
pub struct VirtualPointer {
  queue: EventQueue<Helper>,
  pointer: ZwlrVirtualPointerV1,
}

pub struct Helper;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Helper {
  fn event(
    _: &mut Helper,
    _: &wl_registry::WlRegistry,
    _: wl_registry::Event,
    _: &GlobalListContents,
    _: &Connection,
    _: &QueueHandle<Helper>,
  ) {
  }
}

delegate_noop!(Helper: ZwlrVirtualPointerManagerV1);
delegate_noop!(Helper: ZwpVirtualKeyboardManagerV1);
delegate_noop!(Helper: ZwpVirtualKeyboardV1);
delegate_noop!(Helper: ignore wl_seat::WlSeat);
delegate_noop!(Helper: ZwlrVirtualPointerV1);

impl VirtualPointer {
  pub fn new(sway: &Sway) -> VirtualPointer {
    let connection = sway.connection();
    let (globals, mut queue) =
      registry_queue_init::<Helper>(&connection).expect("the helper's registry");
    let manager: ZwlrVirtualPointerManagerV1 = globals
      .bind(&queue.handle(), 1..=1, ())
      .expect("sway offers zwlr_virtual_pointer_manager_v1");
    let pointer = manager.create_virtual_pointer(None, &queue.handle(), ());
    queue
      .roundtrip(&mut Helper)
      .expect("the helper's roundtrip");
    VirtualPointer { queue, pointer }
  }

  /// Moves to `x`, `y` on a layout `width` by `height` logical pixels:
  /// the outputs' combined logical size.
  pub fn move_to(&mut self, x: u32, y: u32, (width, height): (u32, u32)) {
    self.pointer.motion_absolute(0, x, y, width, height);
    self.pointer.frame();
    self
      .queue
      .roundtrip(&mut Helper)
      .expect("the helper's roundtrip");
  }

  /// Turns a mouse wheel by `notches`, down when positive as Wayland
  /// counts. wlroots turns each notch into 120 `axis_value120` units.
  #[allow(dead_code, reason = "crabshell's tests use it; these don't")]
  pub fn scroll(&mut self, notches: i32) {
    self.pointer.axis_source(wl_pointer::AxisSource::Wheel);
    let axis = wl_pointer::Axis::VerticalScroll;
    // 15 is libinput's length of one notch.
    self
      .pointer
      .axis_discrete(0, axis, 15.0 * f64::from(notches), notches);
    self.pointer.frame();
    self
      .queue
      .roundtrip(&mut Helper)
      .expect("the helper's roundtrip");
  }

  /// Removes the virtual pointer, so the seat has none.
  #[allow(dead_code, reason = "crabshell's tests don't use it")]
  pub fn unplug(&mut self) {
    self.pointer.destroy();
    self
      .queue
      .roundtrip(&mut Helper)
      .expect("the helper's roundtrip");
  }

  /// Presses and releases the left button.
  pub fn click(&mut self) {
    const BTN_LEFT: u32 = 0x110;
    for state in [
      wl_pointer::ButtonState::Pressed,
      wl_pointer::ButtonState::Released,
    ] {
      self.pointer.button(0, BTN_LEFT, state);
      self.pointer.frame();
    }
    self
      .queue
      .roundtrip(&mut Helper)
      .expect("the helper's roundtrip");
  }
}

/// The US layout, by the names Sway's own XKB data resolves.
const KEYMAP: &str = "xkb_keymap {
  xkb_keycodes { include \"evdev+aliases(qwerty)\" };
  xkb_types { include \"complete\" };
  xkb_compat { include \"complete\" };
  xkb_symbols { include \"pc+us+inet(evdev)\" };
};";

/// Linux key codes, as `zwp_virtual_keyboard_v1.key` takes them.
#[allow(dead_code, reason = "each test binary uses some of them")]
pub mod keys {
  pub const ESC: u32 = 1;
  pub const TAB: u32 = 15;
  pub const ENTER: u32 = 28;
  pub const A: u32 = 30;
  pub const LEFT_SHIFT: u32 = 42;
  pub const SPACE: u32 = 57;
}

/// A second client with a virtual keyboard, so the seat has a keyboard:
/// a headless Sway with no input devices has none.
#[allow(dead_code, reason = "crabshell's tests don't use it yet")]
pub struct VirtualKeyboard {
  queue: EventQueue<Helper>,
  keyboard: ZwpVirtualKeyboardV1,
  time: u32,
}

#[allow(dead_code, reason = "crabshell's tests don't use it yet")]
impl VirtualKeyboard {
  pub fn new(sway: &Sway) -> VirtualKeyboard {
    let connection = sway.connection();
    let (globals, mut queue) =
      registry_queue_init::<Helper>(&connection).expect("the helper's registry");
    let seat: wl_seat::WlSeat = globals
      .bind(&queue.handle(), 1..=1, ())
      .expect("sway offers a seat");
    let manager: ZwpVirtualKeyboardManagerV1 = globals
      .bind(&queue.handle(), 1..=1, ())
      .expect("sway offers zwp_virtual_keyboard_manager_v1");
    let keyboard = manager.create_virtual_keyboard(&seat, &queue.handle(), ());
    let path = sway.runtime_dir().join("keymap.xkb");
    // The size sent counts the NUL that ends the keymap.
    let keymap = format!("{KEYMAP}\0");
    std::fs::write(&path, &keymap).expect("write the keymap");
    let file = File::open(&path).expect("open the keymap");
    const XKB_V1: u32 = 1;
    keyboard.keymap(XKB_V1, file.as_fd(), keymap.len() as u32);
    queue
      .roundtrip(&mut Helper)
      .expect("the helper's roundtrip");
    VirtualKeyboard {
      queue,
      keyboard,
      time: 0,
    }
  }

  /// Presses `key`, a code from [`keys`], and holds it.
  pub fn press(&mut self, key: u32) {
    self.send(key, 1);
  }

  /// Releases `key`.
  pub fn release(&mut self, key: u32) {
    self.send(key, 0);
  }

  /// Removes the virtual keyboard, so the seat has none.
  pub fn unplug(&mut self) {
    self.keyboard.destroy();
    self
      .queue
      .roundtrip(&mut Helper)
      .expect("the helper's roundtrip");
  }

  /// Presses and releases `key`.
  pub fn tap(&mut self, key: u32) {
    self.press(key);
    self.release(key);
  }

  fn send(&mut self, key: u32, state: u32) {
    self.time += 10;
    self.keyboard.key(self.time, key, state);
    self
      .queue
      .roundtrip(&mut Helper)
      .expect("the helper's roundtrip");
  }
}
