//! Cover art for an `Image`: a `file://` URL's file, or a remote picture
//! downloaded once into a cache.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fs::{DirBuilder, File, OpenOptions, Permissions};
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, SystemTime};

use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::{Cx, IntoProp};

use super::{Fetch, HttpError, Service, Sink, TIMEOUT, allowed, chosen, fetch, set};

/// The largest picture downloaded.
pub(crate) const MAX_ART: u64 = 16 * 1024 * 1024;

/// What the cache keeps after each download, newest first.
pub(crate) const CACHE_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const CACHE_FILES: usize = 200;

/// The most directory entries looked at when trimming.
const MAX_ENTRIES: usize = 4096;

/// How old a download's leftover part, from a shell that ended mid-way,
/// is before a trim removes it.
const STALE_PART: Duration = Duration::from_secs(10 * 60);

/// The formats `Image` reads by extension, by their first bytes.
/// Whether a file's first bytes are a format's.
type Magic = fn(&[u8]) -> bool;

const FORMATS: [(&str, Magic); 4] = [
  ("png", |head| head.starts_with(b"\x89PNG\r\n\x1a\n")),
  ("jpg", |head| head.starts_with(b"\xff\xd8\xff")),
  ("gif", |head| {
    head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a")
  }),
  ("webp", |head| {
    head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP")
  }),
];

/// `$XDG_CACHE_HOME/crabshell/art`, or `$HOME/.cache/crabshell/art` when
/// that is unset or relative.
pub(crate) fn cache_dir() -> Option<PathBuf> {
  let absolute = |name| {
    std::env::var_os(name)
      .map(PathBuf::from)
      .filter(|path| path.is_absolute())
  };
  let base = absolute("XDG_CACHE_HOME").or_else(|| Some(absolute("HOME")?.join(".cache")))?;
  Some(base.join("crabshell").join("art"))
}

struct Art {
  service: Option<Rc<Service>>,
  path: Signal<Option<PathBuf>>,
  download: RefCell<Option<Download>>,
}

/// A picture on its way into the cache. Dropped before it arrived whole,
/// curl stops and the part is removed.
struct Download {
  #[allow(dead_code, reason = "held so curl stops when this drops")]
  fetch: Fetch,
  part: PathBuf,
}

impl Drop for Download {
  fn drop(&mut self) {
    // Gone already once it was renamed into place.
    let _ = std::fs::remove_file(&self.part);
  }
}

/// `cx.cover_art(url)`.
#[track_caller]
pub(crate) fn cover_art(cx: &Cx, url: impl IntoProp<String>) -> ReadSignal<Option<PathBuf>> {
  let at = Location::caller();
  let art = Rc::new(Art {
    service: cx.service::<Service>(),
    path: cx.signal(None),
    download: RefCell::new(None),
  });
  let url = chosen(cx, url, at, "cx.cover_art");
  let weak = Rc::downgrade(&art);
  cx.effect(move || {
    let url = url.get().unwrap_or_default();
    if let Some(art) = weak.upgrade() {
      untrack(|| art.show(&url));
    }
  });
  let path = art.path.read_only();
  // Dropped with the context, which stops a download.
  cx.signal(art);
  path
}

impl Art {
  /// Shows the art at `url`, stopping a download for another.
  fn show(self: &Rc<Self>, url: &str) {
    let old = self.download.borrow_mut().take();
    drop(old);
    let found = self.find(url);
    set(self.path, found);
  }

  /// The art's file, if it is here now. One to download starts.
  fn find(self: &Rc<Self>, url: &str) -> Option<PathBuf> {
    if url.is_empty() {
      return None;
    }
    if url.starts_with("file://") {
      let path = file_path(url).filter(|path| path.exists());
      return path.or_else(|| failed(url, "no such file"));
    }
    if !allowed(url) {
      return failed(url, "only file:, http: and https: art is shown");
    }
    let Some(service) = self.service.clone() else {
      return failed(url, "cx.cover_art() needs the services the shell provides");
    };
    let Some(dir) = service.shared.config.art_cache.clone() else {
      return failed(
        url,
        "no cache directory, as neither XDG_CACHE_HOME nor HOME is set",
      );
    };
    if let Err(why) = ready(&dir) {
      thread_local!(static LOGGED: Cell<bool> = const { Cell::new(false) });
      if !LOGGED.replace(true) {
        let dir = dir.display();
        tracing::warn!(%dir, why, "remote cover art isn't shown");
      }
      return None;
    }
    let key = key(url);
    if let Some(cached) = cached(&dir, &key) {
      // So a trim keeps what is shown.
      let _ = File::open(&cached).and_then(|file| file.set_modified(SystemTime::now()));
      return Some(cached);
    }
    if let Err(error) = self.download(&service, &dir, key, url) {
      failed(url, &error.message);
    }
    None
  }

  fn download(
    self: &Rc<Self>,
    service: &Rc<Service>,
    dir: &Path,
    key: String,
    url: &str,
  ) -> Result<(), HttpError> {
    let network = |error: std::io::Error| {
      let message = format!("couldn't write the cache: {error}");
      HttpError::new(super::HttpErrorKind::Network, message)
    };
    thread_local!(static NEXT: Cell<u64> = const { Cell::new(0) });
    let n = NEXT.replace(NEXT.get() + 1);
    let part = dir.join(format!(".{key}-{}-{n}.part", std::process::id()));
    // A new file, never one a link names.
    let file = OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o600)
      .open(&part)
      .map_err(network)?;
    let weak = Rc::downgrade(self);
    let (owned, dir) = (url.to_owned(), dir.to_owned());
    let done = move |result| {
      if let Some(art) = Weak::upgrade(&weak) {
        art.downloaded(&owned, &dir, &key, result);
      }
    };
    match fetch(service, url, MAX_ART, TIMEOUT, Sink::File(file), done) {
      Ok(fetch) => {
        self.download.replace(Some(Download { fetch, part }));
        Ok(())
      }
      Err(error) => {
        let _ = std::fs::remove_file(&part);
        Err(error)
      }
    }
  }

  /// A download ended: the picture is named for its format and shown, and
  /// the cache trimmed.
  fn downloaded(&self, url: &str, dir: &Path, key: &str, result: Result<Sink, HttpError>) {
    let Some(download) = self.download.borrow_mut().take() else {
      return;
    };
    let found = result.and_then(|_| {
      let ext = format_of(&download.part).ok_or_else(|| {
        let message = "not a PNG, JPEG, GIF or WebP picture";
        HttpError::new(super::HttpErrorKind::Refused, message)
      })?;
      let path = dir.join(format!("{key}.{ext}"));
      std::fs::rename(&download.part, &path).map_err(|error| {
        let message = format!("couldn't keep the download: {error}");
        HttpError::new(super::HttpErrorKind::Network, message)
      })?;
      Ok(path)
    });
    drop(download);
    match found {
      Ok(path) => {
        trim(dir);
        set(self.path, Some(path));
      }
      Err(error) => {
        failed(url, &error.message);
      }
    }
  }
}

/// Makes the cache directory if it is missing, and checks that it is a
/// directory of the user's own, not a link, tightening a wider one to
/// `0700`.
fn ready(dir: &Path) -> Result<(), String> {
  DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(dir)
    .map_err(|error| format!("couldn't make the cache directory: {error}"))?;
  let meta = dir
    .symlink_metadata()
    .map_err(|error| format!("couldn't read the cache directory: {error}"))?;
  if !meta.is_dir() || meta.uid() != rustix::process::geteuid().as_raw() {
    return Err(String::from(
      "the cache directory isn't a directory of the user's own",
    ));
  }
  if meta.mode() & 0o077 != 0 {
    std::fs::set_permissions(dir, Permissions::from_mode(0o700))
      .map_err(|error| format!("couldn't make the cache directory private: {error}"))?;
  }
  Ok(())
}

/// Logs why the art at `url` isn't shown, once per URL.
fn failed(url: &str, why: &str) -> Option<PathBuf> {
  thread_local!(static LOGGED: RefCell<HashSet<String>> = RefCell::default());
  LOGGED.with_borrow_mut(|logged| {
    // A shell that plays many tracks doesn't keep every URL.
    if logged.len() >= 256 {
      logged.clear();
    }
    if logged.insert(url.to_owned()) {
      tracing::info!(url, why, "no cover art");
    }
  });
  None
}

/// The file a `file://` URL names, percent-decoded. `None` for one with a
/// host or a relative path.
pub(crate) fn file_path(url: &str) -> Option<PathBuf> {
  let mut rest = url.strip_prefix("file://")?.as_bytes();
  let mut bytes = Vec::with_capacity(rest.len());
  while let Some((&byte, after)) = rest.split_first() {
    let hex = |at: usize| {
      after
        .get(at)
        .and_then(|digit| (*digit as char).to_digit(16))
    };
    match (byte, hex(0), hex(1)) {
      (b'%', Some(high), Some(low)) => {
        bytes.push((high * 16 + low) as u8);
        rest = &after[2..];
      }
      _ => {
        bytes.push(byte);
        rest = after;
      }
    }
  }
  bytes
    .starts_with(b"/")
    .then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// The cache's name for `url`: FNV-1a, which stays the same across Rust
/// releases, unlike std's hasher.
fn key(url: &str) -> String {
  let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
  for byte in url.bytes() {
    hash ^= u64::from(byte);
    hash = hash.wrapping_mul(0x0100_0000_01b3);
  }
  format!("{hash:016x}")
}

/// The picture already cached under `key`, in any format.
fn cached(dir: &Path, key: &str) -> Option<PathBuf> {
  FORMATS
    .iter()
    .map(|(ext, _)| dir.join(format!("{key}.{ext}")))
    .find(|path| path.symlink_metadata().is_ok_and(|meta| meta.is_file()))
}

/// The extension for the picture in `path`, from its first bytes.
fn format_of(path: &Path) -> Option<&'static str> {
  let mut head = Vec::with_capacity(12);
  File::open(path)
    .ok()?
    .take(12)
    .read_to_end(&mut head)
    .ok()?;
  FORMATS
    .iter()
    .find(|(_, matches)| matches(&head))
    .map(|(ext, _)| *ext)
}

/// Removes the oldest pictures past [`CACHE_FILES`] or [`CACHE_BYTES`],
/// and parts a shell left behind.
pub(crate) fn trim(dir: &Path) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  let now = SystemTime::now();
  let mut files = Vec::new();
  for entry in entries.flatten().take(MAX_ENTRIES) {
    // Not followed: a link counts as nothing to keep.
    let Ok(meta) = entry.metadata() else {
      continue;
    };
    let Ok(modified) = meta.modified() else {
      continue;
    };
    let path = entry.path();
    if entry.file_name().as_encoded_bytes().starts_with(b".") {
      let stale = now
        .duration_since(modified)
        .is_ok_and(|age| age > STALE_PART);
      if stale && meta.is_file() {
        let _ = std::fs::remove_file(&path);
      }
    } else if meta.is_file() {
      files.push((modified, meta.len(), path));
    }
  }
  files.sort_by_key(|(modified, ..)| std::cmp::Reverse(*modified));
  let mut bytes = 0;
  for (kept, (_, len, path)) in files.into_iter().enumerate() {
    bytes += len;
    if kept >= CACHE_FILES || bytes > CACHE_BYTES {
      let _ = std::fs::remove_file(&path);
    }
  }
}
