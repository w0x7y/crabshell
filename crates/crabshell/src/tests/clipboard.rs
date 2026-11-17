//! The clipboard history: its order, caps and image files, without a
//! compositor.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crabshell_reactive::Runtime;
use crabshell_ui::Cx;
use crabshell_wayland::Selection;

use super::capture_logs;
use crate::clipboard::{History, MAX_BYTES, MAX_ENTRIES};
use crate::{ClipboardContent, ShellExt};

fn text(text: &str) -> Selection {
  Selection {
    text: Some(text.to_owned()),
    ..Selection::default()
  }
}

fn image(bytes: &[u8]) -> Selection {
  Selection {
    image: Some((String::from("image/png"), bytes.to_vec())),
    ..Selection::default()
  }
}

fn texts(history: &History) -> Vec<String> {
  let entries = history.entries();
  let text = |content: &ClipboardContent| match content {
    ClipboardContent::Text(text) => text.clone(),
    ClipboardContent::Image(_) => String::from("<image>"),
  };
  entries.iter().map(|entry| text(&entry.content)).collect()
}

fn recording(dir: Option<PathBuf>) -> History {
  let mut history = History::new(dir);
  history.recording = true;
  history
}

/// A new, empty directory for this test's images.
fn scratch(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("crabshell-clip-{}-{name}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  dir
}

#[test]
fn nothing_is_kept_until_the_history_records() {
  let mut history = History::new(None);
  assert!(!history.record(&text("early")));
  assert!(history.entries().is_empty());
}

#[test]
fn the_history_is_newest_first_and_a_text_again_moves_to_the_top() {
  let mut history = recording(None);
  for copied in ["one", "two", "three"] {
    history.record(&text(copied));
  }
  assert_eq!(texts(&history), ["three", "two", "one"]);
  let id = history.entries()[2].id;
  history.record(&text("one"));
  assert_eq!(texts(&history), ["one", "three", "two"]);
  assert_eq!(history.entries()[0].id, id, "the entry keeps its id");
  assert!(!history.record(&Selection::default()), "nothing to keep");
  assert!(!history.record(&text("")));
}

#[test]
fn a_secret_is_never_kept() {
  let mut history = recording(None);
  let secret = Selection {
    secret: true,
    ..text("hunter2")
  };
  assert!(!history.record(&secret));
  assert!(history.entries().is_empty());
}

#[test]
fn the_oldest_entries_go_past_the_caps() {
  let mut history = recording(None);
  for n in 0..=MAX_ENTRIES {
    history.record(&text(&n.to_string()));
  }
  let kept = texts(&history);
  assert_eq!(kept.len(), MAX_ENTRIES);
  assert_eq!(kept.last().map(String::as_str), Some("1"), "0 went");

  let mut history = recording(None);
  let big = MAX_BYTES / 3;
  for letter in ["a", "b", "c", "d"] {
    history.record(&text(&letter.repeat(big)));
  }
  let firsts: Vec<_> = texts(&history)
    .iter()
    .map(|text| text[..1].to_owned())
    .collect();
  assert_eq!(firsts, ["d", "c", "b"], "a went for the bytes");
}

#[test]
fn an_image_is_kept_in_a_private_file_removed_with_its_entry() {
  let dir = scratch("image");
  let mut history = recording(Some(dir.clone()));
  history.record(&image(b"first"));
  history.record(&text("between"));
  history.record(&image(b"first"));
  let entries = history.entries();
  assert_eq!(
    texts(&history),
    ["<image>", "between"],
    "the same image moved"
  );
  let ClipboardContent::Image(path) = &entries[0].content else {
    panic!("an image");
  };
  assert_eq!(std::fs::read(path).expect("its file"), b"first");
  let mode = |path: &PathBuf| std::fs::metadata(path).expect("there").permissions().mode() & 0o777;
  assert_eq!(mode(path), 0o600);
  assert_eq!(mode(&dir), 0o700);

  history.record(&image(b"second"));
  let ClipboardContent::Image(second) = &history.entries()[0].content else {
    panic!("an image");
  };
  assert!(history.remove(entries[0].id));
  assert!(!path.exists(), "removed with its entry");
  assert!(second.exists());
  drop(history);
  assert!(!second.exists(), "removed with the history");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clearing_removes_every_image() {
  let dir = scratch("clear");
  let mut history = recording(Some(dir.clone()));
  history.record(&image(b"one"));
  history.record(&image(b"two"));
  assert!(history.clear());
  assert!(history.entries().is_empty());
  assert_eq!(std::fs::read_dir(&dir).expect("the dir").count(), 0);
  assert!(!history.clear(), "nothing left");
  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_a_directory_images_are_skipped() {
  let mut history = recording(None);
  capture_logs(|| assert!(!history.record(&image(b"lost"))));
  assert!(history.entries().is_empty());
}

#[test]
fn a_handle_without_a_shell_stays_empty() {
  let cx = Cx::new(Runtime::current().new_owner());
  let mut clipboard = None;
  capture_logs(|| clipboard = Some(cx.clipboard()));
  let clipboard = clipboard.expect("a handle");
  assert_eq!(clipboard.text().get(), None);
  assert!(clipboard.history().get().is_empty());
  clipboard.set_text("nowhere");
  clipboard.copy(0);
  cx.owner().dispose();
  clipboard.clear_history();
  assert_eq!(clipboard.history().try_get(), None, "gone, not a panic");
}

#[test]
fn a_binding_that_asks_for_the_history_doesnt_rerun_at_every_copy() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let mut clipboard = None;
  capture_logs(|| clipboard = Some(cx.clipboard()));
  let clipboard = clipboard.expect("a handle");
  let runs = cx.signal(0);
  cx.effect(move || {
    clipboard.history();
    runs.update(|runs| *runs += 1);
  });
  runtime.flush();
  clipboard.heard(&text("copied"));
  runtime.flush();
  assert_eq!(runs.get_untracked(), 1);
  assert_eq!(clipboard.history().get_untracked().len(), 1, "it records");
  cx.owner().dispose();
}
