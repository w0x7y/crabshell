//! `cx.persist`'s values: kept by key under the root owner, saved as JSON,
//! and read back by the next shell.

use crabshell_reactive::Runtime;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::capture_logs;
use crate::persist::{self, Persisted};

fn saved(pairs: &[(&str, Value)]) -> Map<String, Value> {
  pairs
    .iter()
    .map(|(key, value)| ((*key).to_owned(), value.clone()))
    .collect()
}

#[test]
fn a_value_saved_by_one_shell_is_where_the_next_one_starts() {
  let runtime = Runtime::current();
  let first = Persisted::new(runtime.new_owner(), Map::new());
  let clicks = first.signal("clicks", 0_u32).expect("kept");
  let name = first.signal("name", String::from("bar")).expect("kept");
  clicks.set(3);
  name.set(String::from("top"));
  let values = first.save();
  assert_eq!(
    values,
    saved(&[("clicks", json!(3)), ("name", json!("top"))])
  );

  let next = Persisted::new(runtime.new_owner(), values);
  assert_eq!(
    next.signal("clicks", 0_u32).expect("kept").get_untracked(),
    3
  );
  assert_eq!(
    next
      .signal("name", String::new())
      .expect("kept")
      .get_untracked(),
    "top"
  );
  assert_eq!(
    next.signal("other", 7_u32).expect("kept").get_untracked(),
    7,
    "unsaved: the default"
  );
}

#[test]
fn the_same_key_gives_the_same_signal_under_the_root_owner() {
  let runtime = Runtime::current();
  let root = runtime.new_owner();
  let persisted = Persisted::new(root, Map::new());
  let inner = root.child();
  let first = inner.signal(()); // just something under the child
  let a = persisted.signal("clicks", 0_u32).expect("kept");
  let b = persisted.signal("clicks", 9_u32).expect("kept");
  a.set(5);
  assert_eq!(b.get_untracked(), 5, "one signal");
  // It belongs to the root, so a context that goes leaves it.
  inner.dispose();
  assert!(first.try_get().is_none());
  assert_eq!(a.try_get(), Some(5));
  // And it goes with the root.
  root.dispose();
  assert_eq!(a.try_get(), None);
}

#[test]
fn a_saved_value_that_no_longer_fits_falls_back_to_the_default_and_is_logged() {
  let runtime = Runtime::current();
  let persisted = Persisted::new(runtime.new_owner(), saved(&[("clicks", json!("three"))]));
  let logs = capture_logs(|| {
    assert_eq!(
      persisted
        .signal("clicks", 1_u32)
        .expect("kept")
        .get_untracked(),
      1
    );
  });
  assert!(logs.contains("the saved value no longer fits"), "{logs}");
  // Asked again, it is the same signal, with no second log line.
  let logs = capture_logs(|| {
    persisted.signal("clicks", 1_u32).expect("kept");
  });
  assert_eq!(logs, "");
}

#[test]
fn another_type_for_a_key_is_logged_and_not_kept() {
  let runtime = Runtime::current();
  let persisted = Persisted::new(runtime.new_owner(), Map::new());
  persisted.signal("clicks", 0_u32).expect("kept").set(2);
  let logs = capture_logs(|| {
    let other = persisted.signal("clicks", String::from("x"));
    assert_eq!(other, Err(String::from("x")), "the default comes back");
  });
  assert!(
    logs.contains("asked for before with another type"),
    "{logs}"
  );
  assert_eq!(persisted.save(), saved(&[("clicks", json!(2))]));
}

/// A type whose own serde code panics, as a config's might.
#[derive(Clone, Debug, PartialEq)]
struct Touchy(u32);

impl Serialize for Touchy {
  fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
    panic!("no saving")
  }
}

impl<'de> Deserialize<'de> for Touchy {
  fn deserialize<D: serde::Deserializer<'de>>(_: D) -> Result<Touchy, D::Error> {
    panic!("no reading")
  }
}

#[test]
fn panicking_serde_code_is_logged_and_the_rest_is_saved() {
  let runtime = Runtime::current();
  let persisted = Persisted::new(runtime.new_owner(), saved(&[("touchy", json!(1))]));
  let logs = capture_logs(|| {
    assert_eq!(
      persisted
        .signal("touchy", Touchy(5))
        .expect("kept")
        .get_untracked(),
      Touchy(5)
    );
    persisted.signal("clicks", 4_u32).expect("kept");
    assert_eq!(persisted.save(), saved(&[("clicks", json!(4))]));
  });
  assert!(logs.contains("its Deserialize panicked"), "{logs}");
  assert!(logs.contains("Serialize panicked"), "{logs}");
}

#[test]
fn the_file_is_written_whole_read_once_and_private() {
  use std::os::unix::fs::PermissionsExt;

  let dir = std::env::temp_dir().join(format!("crabshell-persist-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  let file = dir.join("crabshell").join("state.json");
  persist::write(&file, &saved(&[("clicks", json!(3))])).expect("written");
  let mode = std::fs::metadata(file.parent().expect("a directory"))
    .expect("the directory")
    .permissions()
    .mode();
  assert_eq!(mode & 0o777, 0o700);
  assert_eq!(persist::load(&file), saved(&[("clicks", json!(3))]));
  assert!(!file.exists(), "read once");
  assert_eq!(persist::load(&file), Map::new());

  std::fs::write(&file, "not json").expect("written");
  let logs = capture_logs(|| assert_eq!(persist::load(&file), Map::new()));
  assert!(logs.contains("don't parse"), "{logs}");

  // A directory others can read is made private, and a link in the
  // partial file's place is never written through.
  let crabshell = file.parent().expect("a directory");
  std::fs::set_permissions(crabshell, std::fs::Permissions::from_mode(0o755)).expect("loosened");
  let elsewhere = dir.join("elsewhere");
  std::os::unix::fs::symlink(&elsewhere, file.with_extension("json.partial")).expect("a link");
  persist::write(&file, &saved(&[("clicks", json!(4))])).expect("written");
  let mode = std::fs::metadata(crabshell)
    .expect("the directory")
    .permissions()
    .mode();
  assert_eq!(mode & 0o777, 0o700);
  assert!(!elsewhere.exists(), "nothing written through the link");
  assert_eq!(persist::load(&file), saved(&[("clicks", json!(4))]));

  // A directory that is a link is refused.
  std::fs::remove_dir_all(crabshell).expect("removed");
  std::fs::create_dir(dir.join("real")).expect("a directory");
  std::os::unix::fs::symlink(dir.join("real"), crabshell).expect("a link");
  assert!(persist::write(&file, &saved(&[])).is_err());
  let _ = std::fs::remove_dir_all(&dir);
}
