use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;

use super::{FolderView, READS};
use crate::ServicesExt;
use crate::file::{FileErrorKind, Files};
use crate::testing::{Harness, TempDir, capture_logs, offline};

fn run(shell: &mut Harness, ms: u64) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(ms))
}

fn reads() -> usize {
  READS.with(std::cell::Cell::get)
}

fn names(view: FolderView) -> Vec<String> {
  view
    .entries()
    .get_untracked()
    .into_iter()
    .map(|entry| entry.name)
    .collect()
}

fn kind(view: FolderView) -> Option<FileErrorKind> {
  view.error().get_untracked().map(|error| error.kind)
}

fn touch(path: &Path) {
  std::fs::write(path, "x").unwrap();
}

#[test]
fn a_folder_view_lists_every_entry_sorted_by_name() {
  let dir = TempDir::new();
  for name in ["b", "a", "B", "A", ".hidden", "Ä"] {
    touch(&dir.path().join(name));
  }
  std::fs::write(dir.path().join("c"), "four").unwrap();
  std::fs::create_dir(dir.path().join("D")).unwrap();
  std::os::unix::fs::symlink(dir.path().join("D"), dir.path().join("e")).unwrap();
  let shell = Harness::new(offline());
  let view = shell.cx.folder_view(dir.path());
  Runtime::current().flush();
  assert_eq!(
    names(view),
    [".hidden", "A", "a", "B", "b", "c", "D", "e", "Ä"]
  );
  let entries = view.entries().get_untracked();
  let c = &entries[5];
  assert_eq!(
    (c.path.as_path(), c.size, c.is_dir),
    (dir.path().join("c").as_path(), 4, false)
  );
  assert!(c.modified.is_some());
  assert!(entries[6].is_dir, "a directory");
  assert!(entries[7].is_dir, "a link to one");
  assert_eq!(kind(view), None);
}

#[test]
fn a_burst_of_changes_reads_the_folder_once() {
  let dir = TempDir::new();
  touch(&dir.path().join("old"));
  let mut shell = Harness::new(offline());
  let view = shell.cx.folder_view(dir.path());
  Runtime::current().flush();
  let before = reads();
  for n in 0..20 {
    touch(&dir.path().join(format!("new{n:02}")));
  }
  std::fs::rename(dir.path().join("old"), dir.path().join("renamed")).unwrap();
  run(&mut shell, 50);
  assert_eq!(reads() - before, 1);
  let listed = names(view);
  assert_eq!(listed.len(), 21);
  assert!(listed.contains(&"renamed".to_owned()) && !listed.contains(&"old".to_owned()));
  std::fs::remove_file(dir.path().join("new00")).unwrap();
  run(&mut shell, 50);
  assert_eq!(names(view).len(), 20);
}

#[test]
fn a_name_that_isnt_utf8_is_left_out_and_logged_once() {
  let dir = TempDir::new();
  touch(&dir.path().join("fine"));
  touch(&dir.path().join(std::ffi::OsStr::from_bytes(b"bad\xff")));
  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let view = shell.cx.folder_view(dir.path());
    Runtime::current().flush();
    touch(&dir.path().join("more"));
    run(&mut shell, 50);
    assert_eq!(names(view), ["fine", "more"]);
  });
  assert_eq!(logs.matches("isn't UTF-8").count(), 1, "{logs}");
}

#[test]
fn a_folder_view_lists_at_most_10000_entries() {
  let dir = TempDir::new();
  for n in 0..10_001 {
    touch(&dir.path().join(n.to_string()));
  }
  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let view = shell.cx.folder_view(dir.path());
    Runtime::current().flush();
    assert_eq!(view.entries().get_untracked().len(), 10_000);
    touch(&dir.path().join("more"));
    run(&mut shell, 100);
    assert_eq!(view.entries().get_untracked().len(), 10_000);
  });
  assert_eq!(logs.matches("over 10000 entries").count(), 1, "{logs}");
}

#[test]
fn a_folder_view_follows_its_folder_made_later_removed_and_replaced() {
  let dir = TempDir::new();
  let path = dir.path().join("later/walls");
  let mut shell = Harness::new(offline());
  let view = shell.cx.folder_view(path.as_path());
  Runtime::current().flush();
  assert_eq!(kind(view), Some(FileErrorKind::NotFound));

  // Read when it is made, not after the wait a new file gets.
  std::fs::create_dir(dir.path().join("later")).unwrap();
  run(&mut shell, 20);
  std::fs::create_dir(&path).unwrap();
  run(&mut shell, 20);
  assert_eq!(kind(view), None);
  touch(&path.join("one"));
  run(&mut shell, 50);
  assert_eq!(names(view), ["one"]);
  assert_eq!(kind(view), None);

  std::fs::remove_dir_all(dir.path().join("later")).unwrap();
  run(&mut shell, 50);
  assert_eq!(names(view), Vec::<String>::new());
  assert_eq!(kind(view), Some(FileErrorKind::NotFound));

  std::fs::create_dir_all(&path).unwrap();
  touch(&path.join("two"));
  run(&mut shell, 50);
  assert_eq!(names(view), ["two"]);

  // Another folder renamed over it is listed and then followed.
  let other = dir.path().join("later/other");
  std::fs::create_dir(&other).unwrap();
  touch(&other.join("three"));
  std::fs::remove_dir_all(&path).unwrap();
  std::fs::rename(&other, &path).unwrap();
  run(&mut shell, 50);
  assert_eq!(names(view), ["three"]);
  touch(&path.join("four"));
  run(&mut shell, 50);
  assert_eq!(names(view), ["four", "three"]);

  // A file in its place isn't a folder.
  std::fs::remove_dir_all(&path).unwrap();
  touch(&path);
  run(&mut shell, 50);
  assert_eq!(kind(view), Some(FileErrorKind::NotAFile));
}

#[test]
fn a_new_path_follows_the_new_folder_and_drops_the_old() {
  let (first, second) = (TempDir::new(), TempDir::new());
  touch(&first.path().join("a"));
  touch(&second.path().join("b"));
  let mut shell = Harness::new(offline());
  let path = shell.cx.signal(first.path().to_owned());
  let view = shell.cx.folder_view(move || path.get());
  Runtime::current().flush();
  assert_eq!(names(view), ["a"]);
  path.set(second.path().to_owned());
  Runtime::current().flush();
  assert_eq!(names(view), ["b"]);
  let before = reads();
  touch(&first.path().join("ignored"));
  run(&mut shell, 50);
  assert_eq!(reads(), before);
  touch(&second.path().join("c"));
  run(&mut shell, 50);
  assert_eq!(names(view), ["b", "c"]);
}

#[test]
fn a_panic_choosing_the_path_is_logged_and_the_shell_keeps_running() {
  let dir = TempDir::new();
  touch(&dir.path().join("a"));
  let mut shell = Harness::new(offline());
  let broken = shell.cx.signal(false);
  let path = dir.path().to_owned();
  let logs = capture_logs(|| {
    let view = shell.cx.folder_view(move || {
      assert!(!broken.get(), "a broken path");
      path.clone()
    });
    Runtime::current().flush();
    assert_eq!(names(view), ["a"]);
    broken.set(true);
    Runtime::current().flush();
    assert_eq!(names(view), Vec::<String>::new());
    broken.set(false);
    Runtime::current().flush();
    assert_eq!(names(view), ["a"]);
  });
  assert!(
    logs.contains("cx.folder_view (called at") && logs.contains("a broken path"),
    "{logs}"
  );
  run(&mut shell, 10);
}

#[test]
fn an_unchanged_folder_wakes_nothing_and_goes_with_its_context() {
  // Not the temporary directory itself, whose parent other tests change.
  let dir = TempDir::new();
  let path = dir.path().join("walls");
  std::fs::create_dir(&path).unwrap();
  touch(&path.join("a"));
  let mut shell = Harness::new(offline());
  let nodes = Runtime::current().node_count();
  let child = shell.cx.child();
  let view = child.folder_view(path.as_path());
  Runtime::current().flush();
  assert_eq!(names(view), ["a"]);
  assert_eq!(run(&mut shell, 300), 1);

  child.owner().dispose();
  Runtime::current().flush();
  assert_eq!(Runtime::current().node_count(), nodes);
  let files = shell.cx.service::<Files>().expect("the files service");
  assert!(!files.0.watcher.is_open());
}
