use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use crabshell_reactive::Runtime;
use serde::{Deserialize, Deserializer, Serialize};

use super::MAX_FILE;
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
