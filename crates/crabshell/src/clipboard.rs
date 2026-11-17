//! `cx.clipboard()`: the clipboard's text, setting it, and a history of
//! what was copied, with images kept in private files.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_wayland::Selection;

use crate::shell::{Requests, request};

/// The most entries the history keeps; past that the oldest go.
pub(crate) const MAX_ENTRIES: usize = 100;

/// The most bytes the history's texts and images take together; past
/// that the oldest go.
pub(crate) const MAX_BYTES: usize = 32 * 1024 * 1024;

/// The largest image read back from the history to copy it again, as
/// large as the clipboard reads.
const MAX_IMAGE: u64 = 8 * 1024 * 1024;

/// The clipboard, from [`cx.clipboard()`](crate::ShellExt::clipboard). It
/// is `Copy`, so closures capture it as they do a signal.
#[derive(Clone, Copy)]
pub struct Clipboard {
  pub(crate) text: Signal<Option<String>>,
  pub(crate) history: Signal<Vec<ClipboardEntry>>,
  pub(crate) store: Signal<History>,
  /// `None` on a context the shell didn't set up.
  pub(crate) requests: Option<ReadSignal<Requests>>,
}

/// What the clipboard held once, newest first in
/// [`Clipboard::history`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardEntry {
  /// Names the entry while it is kept. Ids aren't reused.
  pub id: u64,
  /// What was copied.
  pub content: ClipboardContent,
}

/// A history entry's text, or its image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardContent {
  /// Text, up to 4 MiB.
  Text(String),
  /// A PNG or JPEG file only this user can read, which an `Image` can
  /// show. It is removed when its entry goes.
  Image(PathBuf),
}

impl Clipboard {
  /// The clipboard's text, whenever it offers some of at most 4 MiB, and
  /// otherwise `None`.
  pub fn text(&self) -> ReadSignal<Option<String>> {
    self.text.read_only()
  }

  /// What was copied since the first call, newest first: each new
  /// selection's text, or its image when it has no text but a PNG or JPEG
  /// of at most 8 MiB. It keeps at most 100 entries and 32 MiB, dropping
  /// the oldest. A text or image already there moves to the top, and a
  /// selection a password manager marks as secret is never kept.
  pub fn history(&self) -> ReadSignal<Vec<ClipboardEntry>> {
    // Untracked, so a binding that asks doesn't rerun at every copy.
    if untrack(|| self.store.try_with(|store| store.recording)) == Some(false) {
      self.store.try_update(|store| store.recording = true);
      request(self.requests, "clipboard.history", |platform| {
        platform.read_clipboard_images(true);
      });
    }
    self.history.read_only()
  }

  /// Sets the clipboard to `text`.
  pub fn set_text(&self, text: impl Into<String>) {
    let text = text.into();
    request(self.requests, "clipboard.set_text", move |platform| {
      platform.set_clipboard_text(text);
    });
  }

  /// Puts history entry `id` back on the clipboard. An unknown id is
  /// ignored.
  pub fn copy(&self, id: u64) {
    let found = untrack(|| self.store.try_with(|store| store.content(id).cloned())).flatten();
    match found {
      Some(ClipboardContent::Text(text)) => self.set_text(text),
      Some(ClipboardContent::Image(path)) => {
        let Some(bytes) = read_image(&path) else {
          return;
        };
        let kind = image_type(&path);
        request(self.requests, "clipboard.copy", move |platform| {
          platform.set_clipboard(&[kind], bytes);
        });
      }
      None => {}
    }
  }

  /// Removes history entry `id`, and its image file. An unknown id is
  /// ignored.
  pub fn remove(&self, id: u64) {
    self.change(|store| store.remove(id));
  }

  /// Empties the history, removing its image files.
  pub fn clear_history(&self) {
    self.change(History::clear);
  }

  /// Records `selection` from the platform: the text, and the history if
  /// it is kept.
  pub(crate) fn heard(&self, selection: &Selection) {
    if self.text.try_with(|now| *now != selection.text) == Some(true) {
      self.text.try_set(selection.text.clone());
    }
    self.change(|store| store.record(selection));
  }

  /// Removes the history's image files, as the shell exits.
  pub(crate) fn remove_files(&self) {
    self.store.try_update(|store| {
      store.clear();
    });
  }

  fn change(&self, f: impl FnOnce(&mut History) -> bool) {
    let mut changed = false;
    self.store.try_update(|store| changed = f(store));
    if changed && let Some(entries) = self.store.try_with(History::entries) {
      self.history.try_set(entries);
    }
  }
}

/// The history as kept, with each entry's size.
pub(crate) struct History {
  /// Whether `history()` was asked for, so selections are kept.
  pub(crate) recording: bool,
  /// Where images go; `None` without `XDG_RUNTIME_DIR`.
  dir: Option<PathBuf>,
  /// Newest first.
  kept: Vec<Kept>,
  next: u64,
  warned: bool,
}

struct Kept {
  entry: ClipboardEntry,
  bytes: usize,
  /// An image's bytes hashed, to find it again.
  hash: u64,
}

impl History {
  /// A history whose images go in `dir`.
  pub(crate) fn new(dir: Option<PathBuf>) -> History {
    History {
      recording: false,
      dir,
      kept: Vec::new(),
      next: 0,
      warned: false,
    }
  }

  /// `$XDG_RUNTIME_DIR/crabshell/clipboard`.
  pub(crate) fn default_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())?;
    Some(Path::new(&dir).join("crabshell").join("clipboard"))
  }

  pub(crate) fn entries(&self) -> Vec<ClipboardEntry> {
    self.kept.iter().map(|kept| kept.entry.clone()).collect()
  }

  fn content(&self, id: u64) -> Option<&ClipboardContent> {
    let kept = self.kept.iter().find(|kept| kept.entry.id == id)?;
    Some(&kept.entry.content)
  }

  /// Keeps `selection`, if the history is recording and it holds text or
  /// an image that isn't secret. Returns whether the entries changed.
  pub(crate) fn record(&mut self, selection: &Selection) -> bool {
    if !self.recording || selection.secret {
      return false;
    }
    if let Some(text) = selection.text.as_ref().filter(|text| !text.is_empty()) {
      let same = |kept: &Kept| kept.entry.content == ClipboardContent::Text(text.clone());
      if !self.raise(same) {
        let content = ClipboardContent::Text(text.clone());
        self.push(content, text.len(), 0);
      }
    } else if let Some((kind, bytes)) = &selection.image {
      let hash = hash(bytes);
      let same = |kept: &Kept| {
        kept.hash == hash
          && kept.bytes == bytes.len()
          && matches!(kept.entry.content, ClipboardContent::Image(_))
      };
      if !self.raise(same) {
        let Some(path) = self.write_image(kind, bytes) else {
          return false;
        };
        self.push(ClipboardContent::Image(path), bytes.len(), hash);
      }
    } else {
      return false;
    }
    self.trim();
    true
  }

  /// Moves the first entry `same` picks to the top. Returns whether there
  /// was one.
  fn raise(&mut self, same: impl Fn(&Kept) -> bool) -> bool {
    let Some(index) = self.kept.iter().position(same) else {
      return false;
    };
    let kept = self.kept.remove(index);
    self.kept.insert(0, kept);
    true
  }

  fn push(&mut self, content: ClipboardContent, bytes: usize, hash: u64) {
    let id = self.next;
    self.next += 1;
    let entry = ClipboardEntry { id, content };
    self.kept.insert(0, Kept { entry, bytes, hash });
  }

  /// Drops the oldest entries past the caps.
  fn trim(&mut self) {
    let mut total: usize = self.kept.iter().map(|kept| kept.bytes).sum();
    while self.kept.len() > MAX_ENTRIES || total > MAX_BYTES {
      let Some(oldest) = self.kept.pop() else {
        break;
      };
      total -= oldest.bytes;
      forget(&oldest);
    }
  }

  /// Writes an image of type `kind` to a new file only this user can read,
  /// in a directory only this user can enter.
  fn write_image(&mut self, kind: &str, bytes: &[u8]) -> Option<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let Some(dir) = &self.dir else {
      if !std::mem::replace(&mut self.warned, true) {
        tracing::warn!("no XDG_RUNTIME_DIR, so the clipboard history keeps no images");
      }
      return None;
    };
    let extension = if kind == "image/jpeg" { "jpg" } else { "png" };
    let path = dir.join(format!("{}-{}.{extension}", std::process::id(), self.next));
    let written = crate::persist::private_dir(dir).and_then(|()| {
      // New, so it never writes through a link left in its place.
      let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
      file.write_all(bytes)
    });
    match written {
      Ok(()) => Some(path),
      Err(error) => {
        tracing::warn!(path = %path.display(), %error, "can't keep a copied image");
        let _ = std::fs::remove_file(&path);
        None
      }
    }
  }

  /// Removes entry `id`. Returns whether there was one.
  pub(crate) fn remove(&mut self, id: u64) -> bool {
    let Some(index) = self.kept.iter().position(|kept| kept.entry.id == id) else {
      return false;
    };
    forget(&self.kept.remove(index));
    true
  }

  /// Removes every entry. Returns whether there was one.
  pub(crate) fn clear(&mut self) -> bool {
    let kept = std::mem::take(&mut self.kept);
    kept.iter().for_each(forget);
    !kept.is_empty()
  }
}

impl Drop for History {
  fn drop(&mut self) {
    self.clear();
  }
}

/// Removes an entry's image file, if it has one.
fn forget(kept: &Kept) {
  if let ClipboardContent::Image(path) = &kept.entry.content
    && let Err(error) = std::fs::remove_file(path)
    && error.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!(path = %path.display(), %error, "can't remove a copied image");
  }
}

fn hash(bytes: &[u8]) -> u64 {
  let mut hasher = DefaultHasher::new();
  bytes.hash(&mut hasher);
  hasher.finish()
}

fn image_type(path: &Path) -> &'static str {
  match path.extension().and_then(|extension| extension.to_str()) {
    Some("jpg") => "image/jpeg",
    _ => "image/png",
  }
}

/// A history image's bytes, read back to copy it again.
fn read_image(path: &Path) -> Option<Vec<u8>> {
  use std::io::Read;
  let mut bytes = Vec::new();
  let read =
    std::fs::File::open(path).and_then(|file| file.take(MAX_IMAGE).read_to_end(&mut bytes));
  match read {
    Ok(_) => Some(bytes),
    Err(error) => {
      tracing::warn!(path = %path.display(), %error, "can't read a copied image back");
      None
    }
  }
}
