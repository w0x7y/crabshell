//! `cx.clipboard()`: the clipboard's text, setting it, and a history of
//! what was copied, with images kept in private files.

use std::collections::HashMap;
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

/// The most files a sweep of the image directory looks at.
const MAX_SWEPT: usize = 4096;

/// The clipboard, from [`cx.clipboard()`](crate::ShellExt::clipboard). It
/// is `Copy`, so closures capture it as they do a signal.
#[derive(Clone, Copy)]
pub struct Clipboard {
  pub(crate) text: Signal<Option<String>>,
  /// The entries, which hold the history's texts, each once.
  pub(crate) history: Signal<Vec<ClipboardEntry>>,
  /// What the history knows of each entry besides its content.
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
  /// otherwise `None`. A selection a password manager marks as secret
  /// gives `None` too, and isn't read.
  pub fn text(&self) -> ReadSignal<Option<String>> {
    self.text.read_only()
  }

  /// What was copied since the first call, newest first: each new
  /// selection's text, or its image when it has no text but a PNG or JPEG
  /// of at most 8 MiB. It keeps at most 100 entries and 32 MiB, dropping
  /// the oldest. A text or image already there moves to the top, and a
  /// selection a password manager marks as secret is never kept. The
  /// first call also removes the images a shell that crashed left behind.
  pub fn history(&self) -> ReadSignal<Vec<ClipboardEntry>> {
    // Untracked, so a binding that asks doesn't rerun at every copy.
    if untrack(|| self.store.try_with(|store| store.recording)) == Some(false) {
      self.store.try_update(|store| {
        store.recording = true;
        store.sweep();
      });
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
    let content = |entries: &Vec<ClipboardEntry>| {
      let entry = entries.iter().find(|entry| entry.id == id)?;
      Some(entry.content.clone())
    };
    match untrack(|| self.history.try_with(content)).flatten() {
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
    let known = |entries: &Vec<ClipboardEntry>| entries.iter().any(|entry| entry.id == id);
    if untrack(|| self.history.try_with(known)) == Some(true) {
      self.change(|store, entries| store.remove(entries, id));
    }
  }

  /// Empties the history, removing its image files.
  pub fn clear_history(&self) {
    if untrack(|| self.history.try_with(Vec::is_empty)) == Some(false) {
      self.change(History::clear);
    }
  }

  /// Records `selection` from the platform: the text, and the history if
  /// it is kept.
  pub(crate) fn heard(&self, selection: &Selection) {
    let text = selection.text.as_ref().filter(|_| !selection.secret);
    if self.text.try_with(|now| now.as_ref() != text) == Some(true) {
      self.text.try_set(text.cloned());
    }
    if untrack(|| self.store.try_with(|store| store.keeps(selection))) == Some(true) {
      self.change(|store, entries| store.record(entries, selection));
    }
  }

  /// Removes the history's image files, as the shell exits.
  pub(crate) fn remove_files(&self) {
    self.store.try_update(History::forget_all);
  }

  /// Changes the entries in place, with the store, so no text is copied.
  fn change(&self, f: impl FnOnce(&mut History, &mut Vec<ClipboardEntry>)) {
    let store = self.store;
    self.history.try_update(|entries| {
      store.try_update(|store| f(store, entries));
    });
  }
}

/// What the history knows of its entries besides their content, which
/// lives once, in the entries themselves.
pub(crate) struct History {
  /// Whether `history()` was asked for, so selections are kept.
  pub(crate) recording: bool,
  /// Where images go; `None` without `XDG_RUNTIME_DIR`.
  dir: Option<PathBuf>,
  /// Each entry's size and, for an image, its hash and file, by id.
  kept: HashMap<u64, Kept>,
  next: u64,
  warned: bool,
}

struct Kept {
  bytes: usize,
  /// An image's bytes hashed, to find it again, and its file.
  image: Option<(u64, PathBuf)>,
}

impl History {
  /// A history whose images go in `dir`.
  pub(crate) fn new(dir: Option<PathBuf>) -> History {
    History {
      recording: false,
      dir,
      kept: HashMap::new(),
      next: 0,
      warned: false,
    }
  }

  /// `$XDG_RUNTIME_DIR/crabshell/clipboard`.
  pub(crate) fn default_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())?;
    Some(Path::new(&dir).join("crabshell").join("clipboard"))
  }

  /// Whether `selection` would be kept: the history records and it holds
  /// text or an image that isn't secret.
  pub(crate) fn keeps(&self, selection: &Selection) -> bool {
    let text = selection.text.as_ref().is_some_and(|text| !text.is_empty());
    self.recording && !selection.secret && (text || selection.image.is_some())
  }

  /// Keeps `selection` at the top of `entries`, if [`keeps`](Self::keeps)
  /// says so.
  pub(crate) fn record(&mut self, entries: &mut Vec<ClipboardEntry>, selection: &Selection) {
    if !self.keeps(selection) {
      return;
    }
    if let Some(text) = selection.text.as_ref().filter(|text| !text.is_empty()) {
      let same = |entry: &ClipboardEntry| matches!(&entry.content, ClipboardContent::Text(kept) if kept == text);
      if !raise(entries, same) {
        let content = ClipboardContent::Text(text.clone());
        self.push(entries, content, text.len(), None);
      }
    } else if let Some((kind, bytes)) = &selection.image {
      let hash = hash(bytes);
      let kept = &self.kept;
      let same = |entry: &ClipboardEntry| {
        kept.get(&entry.id).is_some_and(|kept| {
          kept.bytes == bytes.len() && kept.image.as_ref().is_some_and(|(ours, _)| *ours == hash)
        })
      };
      if !raise(entries, same) {
        let Some(path) = self.write_image(kind, bytes) else {
          return;
        };
        let content = ClipboardContent::Image(path.clone());
        self.push(entries, content, bytes.len(), Some((hash, path)));
      }
    }
    self.trim(entries);
  }

  fn push(
    &mut self,
    entries: &mut Vec<ClipboardEntry>,
    content: ClipboardContent,
    bytes: usize,
    image: Option<(u64, PathBuf)>,
  ) {
    let id = self.next;
    self.next += 1;
    entries.insert(0, ClipboardEntry { id, content });
    self.kept.insert(id, Kept { bytes, image });
  }

  /// Drops the oldest entries past the caps.
  fn trim(&mut self, entries: &mut Vec<ClipboardEntry>) {
    let mut total: usize = self.kept.values().map(|kept| kept.bytes).sum();
    while entries.len() > MAX_ENTRIES || total > MAX_BYTES {
      let Some(oldest) = entries.pop() else {
        break;
      };
      if let Some(kept) = self.kept.remove(&oldest.id) {
        total -= kept.bytes;
        forget(&kept);
      }
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

  /// Removes the images a shell that is no longer running left in the
  /// directory: files named after a process id that isn't this one and
  /// isn't alive.
  pub(crate) fn sweep(&self) {
    let Some(entries) = self
      .dir
      .as_ref()
      .and_then(|dir| std::fs::read_dir(dir).ok())
    else {
      return;
    };
    for entry in entries.filter_map(Result::ok).take(MAX_SWEPT) {
      let name = entry.file_name();
      let Some(pid) = name
        .to_str()
        .and_then(|name| name.split_once('-'))
        .and_then(|(pid, _)| pid.parse::<u32>().ok())
      else {
        continue;
      };
      let gone = pid != std::process::id() && !Path::new("/proc").join(pid.to_string()).exists();
      if gone && entry.file_type().is_ok_and(|kind| kind.is_file()) {
        let path = entry.path();
        if let Err(error) = std::fs::remove_file(&path) {
          tracing::warn!(path = %path.display(), %error, "can't remove an image a crashed shell left");
        }
      }
    }
  }

  /// Removes entry `id` from `entries`, and its image file.
  pub(crate) fn remove(&mut self, entries: &mut Vec<ClipboardEntry>, id: u64) {
    entries.retain(|entry| entry.id != id);
    if let Some(kept) = self.kept.remove(&id) {
      forget(&kept);
    }
  }

  /// Empties `entries`, removing their image files.
  pub(crate) fn clear(&mut self, entries: &mut Vec<ClipboardEntry>) {
    entries.clear();
    self.forget_all();
  }

  /// Removes every image file and forgets every entry.
  pub(crate) fn forget_all(&mut self) {
    self.kept.drain().for_each(|(_, kept)| forget(&kept));
  }
}

impl Drop for History {
  fn drop(&mut self) {
    self.forget_all();
  }
}

/// Moves the first entry `same` picks to the top. Returns whether there
/// was one.
fn raise(entries: &mut Vec<ClipboardEntry>, same: impl Fn(&ClipboardEntry) -> bool) -> bool {
  let Some(index) = entries.iter().position(same) else {
    return false;
  };
  let entry = entries.remove(index);
  entries.insert(0, entry);
  true
}

/// Removes an entry's image file, if it has one.
fn forget(kept: &Kept) {
  if let Some((_, path)) = &kept.image
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
