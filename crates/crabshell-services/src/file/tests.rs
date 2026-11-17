use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use serde::{Deserialize, Deserializer, Serialize};

use super::{FileErrorKind, FileView, MAX_FILE};
use crate::ServicesExt;
use crate::testing::{Harness, capture_logs, offline};

/// A temporary directory, gone on drop.
struct Dir(PathBuf);

impl Dir {
  fn new() -> Dir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-file-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    Dir(dir)
  }

  fn join(&self, name: &str) -> PathBuf {
    self.0.join(name)
  }
}

impl Drop for Dir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn a_file_view_reads_the_text_and_writes_it_whole() {
  let dir = Dir::new();
  let path = dir.join("note");
  std::fs::write(&path, "hello\n").unwrap();
  let shell = Harness::new(offline());
  let view = shell.cx.file_view(&path);
  assert_eq!(view.text().get_untracked().as_deref(), Some("hello\n"));
  view.write("bye\n");
  assert_eq!(std::fs::read_to_string(&path).unwrap(), "bye\n");
  assert_eq!(view.text().get_untracked().as_deref(), Some("bye\n"));
  let leftovers: Vec<_> = std::fs::read_dir(&dir.0).unwrap().collect();
  assert_eq!(leftovers.len(), 1, "no partial file left");
}

#[test]
fn a_missing_or_bad_file_reads_as_none_logged_once() {
  let dir = Dir::new();
  let shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let view = shell.cx.file_view(dir.join("missing"));
    assert_eq!(view.text().get_untracked(), None);
    view.inner.reread();
  });
  assert_eq!(logs.matches("can't read the file").count(), 1, "{logs}");

  std::fs::write(dir.join("binary"), b"\xff\xfe").unwrap();
  let binary = shell.cx.file_view(dir.join("binary"));
  assert_eq!(binary.text().get_untracked(), None);

  let big = std::fs::File::create(dir.join("big")).unwrap();
  big.set_len(MAX_FILE + 1).unwrap();
  let logs = capture_logs(|| {
    let view = shell.cx.file_view(dir.join("big"));
    assert_eq!(view.text().get_untracked(), None);
  });
  assert!(logs.contains("over 16 MiB"), "{logs}");
}

/// The kind of read error `view` shows, if any.
fn kind(view: &FileView) -> Option<FileErrorKind> {
  view.error().get_untracked().map(|error| error.kind)
}

#[test]
fn a_read_error_says_why_and_is_cleared_by_a_read_or_a_write() {
  let dir = Dir::new();
  let path = dir.join("note");
  let shell = Harness::new(offline());
  let view = shell.cx.file_view(&path);
  let error = view.error().get_untracked().expect("an error");
  assert_eq!(error.kind, FileErrorKind::NotFound);
  assert!(error.message.contains("No such file"), "{error:?}");

  std::fs::write(&path, "hi").unwrap();
  view.inner.reread();
  assert_eq!(view.error().get_untracked(), None);
  assert_eq!(view.text().get_untracked().as_deref(), Some("hi"));

  std::fs::remove_file(&path).unwrap();
  view.inner.reread();
  assert_eq!(kind(&view), Some(FileErrorKind::NotFound));
  view.write("back");
  assert_eq!(view.error().get_untracked(), None, "a write that worked");
}

/// A pipe at `path` that a writer opens after 2 s, so a read that waits
/// for one fails its test rather than hanging it. The writer doesn't wait
/// itself, so with no reader waiting it finds none and goes.
fn late_pipe(path: &Path) {
  let made = std::process::Command::new("mkfifo")
    .arg(path)
    .status()
    .unwrap();
  assert!(made.success());
  let path = path.to_owned();
  std::thread::spawn(move || {
    std::thread::sleep(Duration::from_secs(2));
    let flags = rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK;
    let _ = rustix::fs::open(&path, flags, rustix::fs::Mode::empty());
  });
}

#[test]
fn opening_a_pipe_never_waits_for_a_writer() {
  let dir = Dir::new();
  let pipe = dir.join("pipe");
  late_pipe(&pipe);
  let started = Instant::now();
  let opened = super::open_file(&pipe);
  assert!(
    started.elapsed() < Duration::from_secs(1),
    "the open waited"
  );
  assert_eq!(
    opened.err().map(|error| error.kind),
    Some(FileErrorKind::NotAFile)
  );
}

#[test]
fn each_kind_of_read_error_is_told_apart_and_a_pipe_never_blocks() {
  let dir = Dir::new();
  let shell = Harness::new(offline());
  std::fs::create_dir(dir.join("sub")).unwrap();
  assert_eq!(
    kind(&shell.cx.file_view(dir.join("sub"))),
    Some(FileErrorKind::NotAFile)
  );
  late_pipe(&dir.join("pipe"));
  let started = Instant::now();
  assert_eq!(
    kind(&shell.cx.file_view(dir.join("pipe"))),
    Some(FileErrorKind::NotAFile)
  );
  assert!(
    started.elapsed() < Duration::from_secs(1),
    "the read waited"
  );
  std::fs::write(dir.join("binary"), b"\xff").unwrap();
  let binary = shell.cx.file_view(dir.join("binary"));
  assert_eq!(kind(&binary), Some(FileErrorKind::Other));
  assert!(
    binary
      .error()
      .get_untracked()
      .unwrap()
      .message
      .contains("UTF-8")
  );
  // Root reads anything.
  if rustix::process::getuid().as_raw() != 0 {
    std::fs::write(dir.join("secret"), "x").unwrap();
    std::fs::set_permissions(dir.join("secret"), std::fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
      kind(&shell.cx.file_view(dir.join("secret"))),
      Some(FileErrorKind::PermissionDenied)
    );
  }
}

#[test]
fn a_write_replaces_what_a_link_names_and_keeps_its_permissions() {
  let dir = Dir::new();
  let (real, link) = (dir.join("real"), dir.join("link"));
  std::fs::write(&real, "old").unwrap();
  std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
  std::os::unix::fs::symlink(&real, &link).unwrap();
  let shell = Harness::new(offline());
  shell.cx.file_view(&link).write("new");
  assert!(
    std::fs::symlink_metadata(&link)
      .unwrap()
      .file_type()
      .is_symlink()
  );
  assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
  let mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
  assert_eq!(mode, 0o600);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Settings {
  volume: u32,
  muted: bool,
}

const DEFAULT: Settings = Settings {
  volume: 50,
  muted: false,
};

#[test]
fn a_json_file_reads_its_value_and_writes_what_is_set() {
  let dir = Dir::new();
  let path = dir.join("settings.json");
  std::fs::write(&path, r#"{"volume": 70, "muted": true}"#).unwrap();
  let shell = Harness::new(offline());
  let settings = shell.cx.json_file(&path, DEFAULT);
  Runtime::current().flush();
  assert_eq!(
    settings.get_untracked(),
    Settings {
      volume: 70,
      muted: true
    }
  );
  let inode = std::fs::metadata(&path).unwrap().ino();
  assert_eq!(
    std::fs::read_to_string(&path).unwrap(),
    r#"{"volume": 70, "muted": true}"#,
    "what was read isn't written back"
  );

  settings.update(|settings| settings.volume = 30);
  Runtime::current().flush();
  let written: Settings = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
  assert_eq!(
    written,
    Settings {
      volume: 30,
      muted: true
    }
  );
  assert_ne!(
    std::fs::metadata(&path).unwrap().ino(),
    inode,
    "replaced whole"
  );
  assert!(std::fs::read_to_string(&path).unwrap().ends_with("}\n"));
}

#[test]
fn a_missing_json_file_is_made_only_when_the_value_is_set() {
  let dir = Dir::new();
  let path = dir.join("settings.json");
  let shell = Harness::new(offline());
  let settings = shell.cx.json_file(&path, DEFAULT);
  Runtime::current().flush();
  assert!(!path.exists());
  settings.update(|settings| settings.muted = true);
  Runtime::current().flush();
  assert!(path.exists());
}

#[test]
fn a_json_file_of_the_wrong_shape_keeps_the_value_and_is_logged() {
  let dir = Dir::new();
  let path = dir.join("settings.json");
  std::fs::write(&path, r#"{"volume": "loud"}"#).unwrap();
  let shell = Harness::new(offline());
  let mut settings = None;
  let logs = capture_logs(|| {
    settings = Some(shell.cx.json_file(&path, DEFAULT));
    Runtime::current().flush();
  });
  assert_eq!(settings.unwrap().get_untracked(), DEFAULT);
  assert!(logs.contains("isn't the JSON expected"), "{logs}");
  assert_eq!(
    std::fs::read_to_string(&path).unwrap(),
    r#"{"volume": "loud"}"#,
    "left alone"
  );
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct Touchy(u32);

impl<'de> Deserialize<'de> for Touchy {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Touchy, D::Error> {
    let value = u32::deserialize(deserializer)?;
    assert!(value < 10, "too big: {value}");
    Ok(Touchy(value))
  }
}

#[test]
fn a_panicking_deserialize_is_logged_and_keeps_the_value() {
  let dir = Dir::new();
  let path = dir.join("touchy.json");
  std::fs::write(&path, "99").unwrap();
  let shell = Harness::new(offline());
  let mut value = None;
  let logs = capture_logs(|| value = Some(shell.cx.json_file(&path, Touchy(1))));
  assert_eq!(value.unwrap().get_untracked(), Touchy(1));
  assert!(logs.contains("reading it panicked: too big: 99"), "{logs}");
}

fn run(shell: &mut Harness, ms: u64) -> usize {
  shell.run_until(std::time::Instant::now() + std::time::Duration::from_millis(ms))
}

#[test]
fn a_file_view_follows_writes_replacements_and_removal() {
  let dir = Dir::new();
  let path = dir.join("note");
  std::fs::write(&path, "one").unwrap();
  let mut shell = Harness::new(offline());
  // Only the signal is kept, as a binding would keep it.
  let text = shell.cx.file_view(&path).text();
  std::fs::write(&path, "two").unwrap();
  run(&mut shell, 50);
  assert_eq!(text.get_untracked().as_deref(), Some("two"));
  std::fs::write(dir.join("new"), "three").unwrap();
  std::fs::rename(dir.join("new"), &path).unwrap();
  run(&mut shell, 50);
  assert_eq!(text.get_untracked().as_deref(), Some("three"));
  std::fs::remove_file(&path).unwrap();
  run(&mut shell, 50);
  assert_eq!(text.get_untracked(), None);
}

#[test]
fn a_json_file_follows_the_file_and_doesnt_write_back_what_it_read() {
  let dir = Dir::new();
  let path = dir.join("settings.json");
  std::fs::write(&path, r#"{"volume": 70, "muted": true}"#).unwrap();
  let mut shell = Harness::new(offline());
  let settings = shell.cx.json_file(&path, DEFAULT);
  Runtime::current().flush();
  std::fs::write(&path, r#"{"volume": 10, "muted": false}"#).unwrap();
  let inode = std::fs::metadata(&path).unwrap().ino();
  run(&mut shell, 50);
  assert_eq!(
    settings.get_untracked(),
    Settings {
      volume: 10,
      muted: false
    }
  );
  assert_eq!(
    std::fs::metadata(&path).unwrap().ino(),
    inode,
    "not written back"
  );
  assert_eq!(
    std::fs::read_to_string(&path).unwrap(),
    r#"{"volume": 10, "muted": false}"#
  );
}

#[test]
fn a_watched_file_wakes_nothing_while_it_stays_the_same_and_its_watch_goes_with_its_context() {
  let dir = Dir::new();
  let path = dir.join("note");
  std::fs::write(&path, "one").unwrap();
  let mut shell = Harness::new(offline());
  let child = shell.cx.child();
  let _text = child.file_view(&path).text();
  run(&mut shell, 20);
  assert_eq!(run(&mut shell, 500), 1, "only the run's own deadline");
  let files = shell.cx.service::<super::Files>().unwrap();
  assert!(files.0.watcher.is_open());
  child.owner().dispose();
  assert!(
    !files.0.watcher.is_open(),
    "the last watch took inotify away"
  );
}

#[test]
fn a_file_still_being_written_is_read_once_its_writes_pause() {
  use std::io::Write;
  let dir = Dir::new();
  let path = dir.join("log");
  std::fs::write(&path, "one").unwrap();
  let mut shell = Harness::new(offline());
  let text = shell.cx.file_view(&path).text();
  let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
  let kept = std::rc::Rc::clone(&seen);
  shell.cx.effect(move || kept.borrow_mut().push(text.get()));
  Runtime::current().flush();
  // A writer that keeps the file open, as a log's does.
  let mut file = std::fs::OpenOptions::new()
    .append(true)
    .open(&path)
    .unwrap();
  file.write_all(b" two").unwrap();
  run(&mut shell, 20);
  file.write_all(b" three").unwrap();
  run(&mut shell, 20);
  assert_eq!(seen.borrow().len(), 1, "not read at each write");
  assert_eq!(run(&mut shell, 60), 2, "one timer, then the run's deadline");
  let shown = |text: &str| Some(String::from(text));
  assert_eq!(*seen.borrow(), [shown("one"), shown("one two three")]);
  assert_eq!(run(&mut shell, 300), 1, "no timer left");

  // Closed before the wait ends: read at the close, once.
  file.write_all(b" four").unwrap();
  drop(file);
  run(&mut shell, 20);
  assert_eq!(seen.borrow().last(), Some(&shown("one two three four")));
  assert_eq!(run(&mut shell, 300), 1, "the timer went at the close");
  assert_eq!(seen.borrow().len(), 3);

  // A view whose context goes while a read waits takes its timer along.
  let other = dir.join("other");
  std::fs::write(&other, "").unwrap();
  let child = shell.cx.child();
  let _other = child.file_view(&other).text();
  let mut file = std::fs::OpenOptions::new()
    .append(true)
    .open(&other)
    .unwrap();
  file.write_all(b"x").unwrap();
  run(&mut shell, 20);
  child.owner().dispose();
  assert_eq!(run(&mut shell, 300), 1, "the timer went with the view");
}

#[test]
fn a_json_file_in_a_directory_not_made_yet_makes_it_and_then_follows_it() {
  let dir = Dir::new();
  let path = dir.join("state/crabshell/settings.json");
  let mut shell = Harness::new(offline());
  let settings = shell.cx.json_file(&path, DEFAULT);
  Runtime::current().flush();
  settings.update(|settings| settings.volume = 20);
  Runtime::current().flush();
  assert!(path.exists(), "made with its directories");
  std::fs::write(&path, r#"{"volume": 90, "muted": false}"#).unwrap();
  run(&mut shell, 50);
  assert_eq!(settings.get_untracked().volume, 90, "followed once made");
}

/// A value whose `Serialize` panics past a limit and whose `PartialEq`
/// always holds the shell up to it, as a config's own may.
#[derive(Clone, Debug, Deserialize)]
struct Fragile(u32);

impl PartialEq for Fragile {
  fn eq(&self, other: &Fragile) -> bool {
    self.0 == other.0
  }
}

impl Serialize for Fragile {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    assert!(self.0 < 100, "too big to write: {}", self.0);
    serializer.serialize_u32(self.0)
  }
}

#[test]
fn a_panicking_serialize_is_logged_and_the_file_goes_on() {
  let dir = Dir::new();
  let path = dir.join("fragile.json");
  std::fs::write(&path, "1").unwrap();
  let mut shell = Harness::new(offline());
  let value = shell.cx.json_file(&path, Fragile(0));
  Runtime::current().flush();
  // From the loop: a file whose value can't be written back.
  let logs = capture_logs(|| {
    std::fs::write(&path, "500").unwrap();
    run(&mut shell, 50);
  });
  assert!(
    logs.contains("reading it panicked: too big to write: 500"),
    "{logs}"
  );
  // Set by the config.
  let logs = capture_logs(|| {
    value.set(Fragile(200));
    Runtime::current().flush();
  });
  assert!(
    logs.contains("turning the value into JSON panicked: too big to write: 200"),
    "{logs}"
  );
  // Both go on: a set is written and an outside edit is read.
  value.set(Fragile(7));
  Runtime::current().flush();
  assert_eq!(std::fs::read_to_string(&path).unwrap(), "7\n");
  std::fs::write(&path, "9").unwrap();
  run(&mut shell, 50);
  assert_eq!(value.get_untracked(), Fragile(9));
}

#[test]
fn a_set_before_the_first_flush_is_written() {
  let dir = Dir::new();
  let path = dir.join("count.json");
  let shell = Harness::new(offline());
  let count = shell.cx.json_file(&path, 0_u32);
  count.update(|count| *count += 1);
  Runtime::current().flush();
  assert_eq!(std::fs::read_to_string(&path).unwrap(), "1\n");
}

#[test]
fn a_file_view_follows_its_directory_made_later_and_made_again() {
  let dir = Dir::new();
  let path = dir.join("later/deeper/note");
  let mut shell = Harness::new(offline());
  let text = shell.cx.file_view(&path).text();
  std::fs::create_dir_all(dir.join("later/deeper")).unwrap();
  run(&mut shell, 50);
  std::fs::write(&path, "one").unwrap();
  run(&mut shell, 50);
  assert_eq!(text.get_untracked().as_deref(), Some("one"));
  std::fs::remove_dir_all(dir.join("later")).unwrap();
  run(&mut shell, 50);
  assert_eq!(text.get_untracked(), None);
  std::fs::create_dir_all(dir.join("later/deeper")).unwrap();
  run(&mut shell, 50);
  std::fs::write(&path, "two").unwrap();
  run(&mut shell, 50);
  assert_eq!(text.get_untracked().as_deref(), Some("two"));
}

#[test]
fn a_file_view_doesnt_show_a_file_made_but_not_written_yet() {
  use std::io::Write;
  let dir = Dir::new();
  let path = dir.join("note");
  let mut shell = Harness::new(offline());
  let text = shell.cx.file_view(&path).text();
  let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
  let kept = std::rc::Rc::clone(&seen);
  shell.cx.effect(move || kept.borrow_mut().push(text.get()));
  Runtime::current().flush();
  let mut file = std::fs::File::create(&path).unwrap();
  run(&mut shell, 50);
  file.write_all(b"full").unwrap();
  drop(file);
  run(&mut shell, 50);
  assert_eq!(*seen.borrow(), [None, Some(String::from("full"))]);
}
