//! Finding an icon by name, following the freedesktop Icon Theme
//! Specification.
//!
//! Base directories are searched in order: `~/.icons`,
//! `$XDG_DATA_HOME/icons`, each `$XDG_DATA_DIRS/icons`, then
//! `/usr/share/pixmaps`. A theme is a directory of that name in any of them;
//! the first `index.theme` found describes it. A name is looked up in the
//! theme, then depth-first in the themes it inherits, then in `hicolor`,
//! then as a plain file in a base directory. Within a theme, a directory
//! whose size and scale match wins; otherwise the closest by the spec's
//! distance. PNG comes before SVG, as the spec orders them; XPM can't be
//! drawn.
//!
//! Each directory is listed once, so a lookup checks names in memory
//! instead of asking the filesystem about every candidate.

use std::cell::{RefCell, RefMut};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::icons::read_capped;

/// The file types we draw, in the spec's order.
const EXTENSIONS: [&str; 2] = ["png", "svg"];

/// How many lookups are remembered. Past that, the least recently used is
/// forgotten.
const REMEMBERED: usize = 1024;

/// The largest `index.theme` read, in bytes.
const MAX_INDEX_BYTES: u64 = 1024 * 1024;

/// The icon theme in use and the lookups in it, shared by the renderer
/// and `cx.icon_path`, so both find the same file and read each directory
/// once.
#[derive(Clone)]
pub struct IconLookup(Rc<RefCell<Lookup>>);

pub(crate) struct Lookup {
  pub(crate) theme: String,
  pub(crate) themes: Themes,
}

impl IconLookup {
  pub(crate) fn new(themes: Themes) -> IconLookup {
    IconLookup(Rc::new(RefCell::new(Lookup {
      theme: crate::icons::DEFAULT_THEME.to_owned(),
      themes,
    })))
  }

  /// The file the current theme resolves icon `name` to at `size`
  /// logical pixels and scale 1, through the themes it inherits and
  /// `hicolor`, as an `Icon` finds it.
  pub fn find(&self, name: &str, size: u32) -> Option<PathBuf> {
    let mut lookup = self.borrow();
    let Lookup { theme, themes } = &mut *lookup;
    themes.find(theme, name, size, 1)
  }

  /// Looks icon names up in `theme` from now on.
  pub fn set_theme(&self, theme: String) {
    self.borrow().theme = theme;
  }

  /// No user code runs while this is held.
  pub(crate) fn borrow(&self) -> RefMut<'_, Lookup> {
    self.0.borrow_mut()
  }
}

/// Icon themes and the icons found in them, cached.
pub(crate) struct Themes {
  bases: Vec<PathBuf>,
  /// Each theme's parsed `index.theme`; `None` if none was found.
  themes: HashMap<String, Option<Rc<Theme>>>,
  /// Lookups by theme, name, size and scale, misses included.
  found: HashMap<(String, String, u32, u32), Found>,
  /// Counts lookups, to tell which was used least recently.
  tick: u64,
  /// The names of the files in each directory read, hashed.
  listings: HashMap<PathBuf, HashSet<u64>>,
  /// Names already logged as only available as XPM.
  xpm: HashSet<String>,
  /// How many directories were read, for tests.
  read: u64,
  /// How many times a directory was checked for a file, for tests.
  checked: u64,
}

struct Found {
  path: Option<PathBuf>,
  /// The lookup that last asked for it.
  used: u64,
}

struct Theme {
  /// The theme's directory in each base directory that has one.
  roots: Vec<PathBuf>,
  inherits: Vec<String>,
  directories: Vec<Directory>,
}

struct Directory {
  path: String,
  size: u32,
  scale: u32,
  kind: Kind,
  min: u32,
  max: u32,
  threshold: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
  Fixed,
  Scalable,
  Threshold,
}

impl Themes {
  /// Searches `bases`, in order, each once.
  pub(crate) fn new(mut bases: Vec<PathBuf>) -> Themes {
    let mut seen = HashSet::new();
    bases.retain(|base| seen.insert(base.clone()));
    Themes {
      bases,
      themes: HashMap::new(),
      found: HashMap::new(),
      tick: 0,
      listings: HashMap::new(),
      xpm: HashSet::new(),
      read: 0,
      checked: 0,
    }
  }

  /// Searches the base directories the environment names.
  pub(crate) fn from_env() -> Themes {
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    Themes::new(bases(
      var("HOME").map(PathBuf::from),
      var("XDG_DATA_HOME"),
      var("XDG_DATA_DIRS"),
    ))
  }

  /// How many directories were read, how many times one was checked for
  /// a file, and how many lookups are remembered, for tests.
  #[cfg(test)]
  pub(crate) fn work(&self) -> (u64, u64, usize) {
    (self.read, self.checked, self.found.len())
  }

  /// The file for icon `name` at `size` logical pixels and `scale`,
  /// looked up in `theme`.
  pub(crate) fn find(&mut self, theme: &str, name: &str, size: u32, scale: u32) -> Option<PathBuf> {
    self.tick += 1;
    let key = (theme.to_owned(), name.to_owned(), size, scale);
    if let Some(found) = self.found.get_mut(&key) {
      found.used = self.tick;
      return found.path.clone();
    }
    let path = if safe(name) {
      let mut visited = HashSet::new();
      self
        .in_theme(theme, name, size, scale, &mut visited)
        .or_else(|| self.in_theme("hicolor", name, size, scale, &mut visited))
        .or_else(|| self.unthemed(name, &EXTENSIONS))
    } else {
      None
    };
    if path.is_none() && safe(name) && self.only_xpm(name) && self.xpm.insert(name.to_owned()) {
      tracing::warn!(name, "icon is only available as XPM, which can't be drawn");
    }
    if self.found.len() >= REMEMBERED {
      let oldest = self
        .found
        .iter()
        .min_by_key(|(_, found)| found.used)
        .map(|(key, _)| key.clone());
      if let Some(oldest) = oldest {
        self.found.remove(&oldest);
      }
    }
    let used = self.tick;
    self.found.insert(
      key,
      Found {
        path: path.clone(),
        used,
      },
    );
    path
  }

  /// Looks in `theme`, then depth-first in the themes it inherits,
  /// skipping any already visited.
  fn in_theme(
    &mut self,
    theme: &str,
    name: &str,
    size: u32,
    scale: u32,
    visited: &mut HashSet<String>,
  ) -> Option<PathBuf> {
    if !visited.insert(theme.to_owned()) {
      return None;
    }
    let parsed = self.theme(theme)?;
    self.lookup(&parsed, name, size, scale).or_else(|| {
      parsed
        .inherits
        .iter()
        .find_map(|parent| self.in_theme(parent, name, size, scale, visited))
    })
  }

  /// The spec's `LookupIcon`: a directory matching the size and scale
  /// first, then the closest by the spec's distance, keeping the theme's
  /// order on ties.
  fn lookup(&mut self, parsed: &Theme, name: &str, size: u32, scale: u32) -> Option<PathBuf> {
    let mut ranked: Vec<(u64, &Directory)> = parsed
      .directories
      .iter()
      .map(|directory| {
        let rank = if directory.matches(size, scale) {
          0
        } else {
          directory.distance(size, scale).saturating_add(1)
        };
        (rank, directory)
      })
      .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked
      .iter()
      .find_map(|(_, directory)| self.file_in(&parsed.roots, &directory.path, name, &EXTENSIONS))
  }

  /// The first file for icon `name` in `subdirectory` of any of `roots`,
  /// trying `extensions` in order.
  fn file_in(
    &mut self,
    roots: &[PathBuf],
    subdirectory: &str,
    name: &str,
    extensions: &[&str],
  ) -> Option<PathBuf> {
    roots.iter().find_map(|root| {
      let directory = root.join(subdirectory);
      extensions
        .iter()
        .map(|extension| format!("{name}.{extension}"))
        .find(|file| self.has(&directory, file))
        .map(|file| directory.join(file))
    })
  }

  /// Whether `directory` holds a regular file named `file`. The directory
  /// is listed the first time; a name in the list is confirmed on disk,
  /// so a hash collision can't give a wrong answer.
  fn has(&mut self, directory: &Path, file: &str) -> bool {
    self.checked += 1;
    if !self.listings.contains_key(directory) {
      self.read += 1;
      let names = std::fs::read_dir(directory)
        .map(|entries| {
          entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str().map(hashed))
            .collect()
        })
        .unwrap_or_default();
      self.listings.insert(directory.to_owned(), names);
    }
    self
      .listings
      .get(directory)
      .is_some_and(|names| names.contains(&hashed(file)))
      && directory.join(file).is_file()
  }

  /// A file named for the icon directly in a base directory.
  fn unthemed(&mut self, name: &str, extensions: &[&str]) -> Option<PathBuf> {
    let bases = self.bases.clone();
    self.file_in(&bases, "", name, extensions)
  }

  /// Whether an XPM, which can't be drawn, was all there was: unthemed, or
  /// in a directory of a theme already read.
  fn only_xpm(&mut self, name: &str) -> bool {
    let directories: Vec<(Rc<Theme>, usize)> = self
      .themes
      .values()
      .flatten()
      .flat_map(|theme| (0..theme.directories.len()).map(|index| (Rc::clone(theme), index)))
      .collect();
    self.unthemed(name, &["xpm"]).is_some()
      || directories.into_iter().any(|(theme, index)| {
        let subdirectory = &theme.directories[index].path;
        self
          .file_in(&theme.roots, subdirectory, name, &["xpm"])
          .is_some()
      })
  }

  /// The parsed theme, read once from the first `index.theme` among the
  /// base directories that have the theme.
  fn theme(&mut self, theme: &str) -> Option<Rc<Theme>> {
    if let Some(parsed) = self.themes.get(theme) {
      return parsed.clone();
    }
    let roots: Vec<PathBuf> = if safe(theme) {
      self
        .bases
        .iter()
        .map(|base| base.join(theme))
        .filter(|root| root.is_dir())
        .collect()
    } else {
      Vec::new()
    };
    let parsed = roots
      .iter()
      .find_map(|root| {
        let path = root.join("index.theme");
        read_capped(&path, MAX_INDEX_BYTES)
          .ok()
          .map(|bytes| parse(&String::from_utf8_lossy(&bytes), &path))
      })
      .map(|(inherits, directories)| {
        Rc::new(Theme {
          roots: roots.clone(),
          inherits,
          directories,
        })
      });
    if parsed.is_none() && theme != "hicolor" {
      tracing::warn!(theme, "icon theme not found");
    }
    self.themes.insert(theme.to_owned(), parsed.clone());
    parsed
  }
}

/// A file name's hash, for a directory listing.
fn hashed(name: &str) -> u64 {
  let mut hasher = DefaultHasher::new();
  name.hash(&mut hasher);
  hasher.finish()
}

/// The base directories, in the spec's order: `~/.icons`, then
/// `$XDG_DATA_HOME/icons` (default `~/.local/share/icons`), which the Base
/// Directory spec ranks above each `$XDG_DATA_DIRS/icons` (default
/// `/usr/local/share` and `/usr/share`), then `/usr/share/pixmaps`. Relative
/// paths in the variables are invalid and ignored.
pub(crate) fn bases(
  home: Option<PathBuf>,
  data_home: Option<OsString>,
  data_dirs: Option<OsString>,
) -> Vec<PathBuf> {
  let mut bases = Vec::new();
  bases.extend(home.as_ref().map(|home| home.join(".icons")));
  let data_home = data_home
    .map(PathBuf::from)
    .filter(|path| path.is_absolute())
    .or_else(|| home.map(|home| home.join(".local/share")));
  bases.extend(data_home.map(|path| path.join("icons")));
  let data_dirs = data_dirs.unwrap_or_else(|| "/usr/local/share:/usr/share".into());
  bases.extend(
    std::env::split_paths(&data_dirs)
      .filter(|path| path.is_absolute())
      .map(|path| path.join("icons")),
  );
  bases.push(PathBuf::from("/usr/share/pixmaps"));
  bases
}

/// Whether a name from outside can be used as a path component.
fn safe(name: &str) -> bool {
  !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// Parses an `index.theme` into the themes it inherits and its icon
/// directories. Lines and directories it can't use are logged and skipped.
fn parse(text: &str, path: &Path) -> (Vec<String>, Vec<Directory>) {
  let mut sections: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
  let mut section = None;
  for line in text.lines().map(str::trim) {
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    if let Some(name) = line
      .strip_prefix('[')
      .and_then(|line| line.strip_suffix(']'))
    {
      section = Some(name);
    } else if let (Some(section), Some((key, value))) = (section, line.split_once('=')) {
      sections
        .entry(section)
        .or_default()
        .insert(key.trim(), value.trim());
    } else {
      tracing::warn!(path = %path.display(), line, "skipping a line in an icon theme");
    }
  }
  let header = sections.get("Icon Theme");
  let list = |key| {
    header
      .and_then(|header| header.get(key))
      .into_iter()
      .flat_map(|value| value.split(','))
      .map(str::trim)
      .filter(|item| !item.is_empty())
  };
  let mut seen = HashSet::new();
  let directories = list("Directories")
        .chain(list("ScaledDirectories"))
        .filter(|name| seen.insert(*name))
        .filter_map(|name| {
            // A directory must stay inside the theme.
            let inside = !name.starts_with('/')
                && !name.contains('\0')
                && name.split('/').all(|part| part != "..");
            if !inside {
                tracing::warn!(path = %path.display(), directory = name, "skipping an icon directory outside the theme");
                return None;
            }
            let directory = sections.get(name).and_then(|keys| Directory::parse(name, keys));
            if directory.is_none() {
                tracing::warn!(path = %path.display(), directory = name, "skipping an icon directory with no usable Size");
            }
            directory
        })
        .collect();
  (list("Inherits").map(str::to_owned).collect(), directories)
}

impl Directory {
  fn parse(path: &str, keys: &HashMap<&str, &str>) -> Option<Directory> {
    let number = |key| keys.get(key).and_then(|value| value.parse::<u32>().ok());
    let size = number("Size")?;
    Some(Directory {
      path: path.to_owned(),
      size,
      scale: number("Scale").unwrap_or(1).max(1),
      kind: match keys.get("Type").copied() {
        Some("Fixed") => Kind::Fixed,
        Some("Scalable") => Kind::Scalable,
        _ => Kind::Threshold,
      },
      min: number("MinSize").unwrap_or(size),
      max: number("MaxSize").unwrap_or(size),
      threshold: number("Threshold").unwrap_or(2),
    })
  }

  /// The spec's `DirectoryMatchesSize`.
  fn matches(&self, size: u32, scale: u32) -> bool {
    self.scale == scale
      && match self.kind {
        Kind::Fixed => self.size == size,
        Kind::Scalable => (self.min..=self.max).contains(&size),
        Kind::Threshold => {
          self.size.saturating_sub(self.threshold) <= size
            && size <= self.size.saturating_add(self.threshold)
        }
      }
  }

  /// The spec's `DirectorySizeDistance`, in physical pixels.
  fn distance(&self, size: u32, scale: u32) -> u64 {
    let wanted = u64::from(size) * u64::from(scale);
    let at = |value: u32| u64::from(value) * u64::from(self.scale);
    let (low, high) = match self.kind {
      Kind::Fixed => return at(self.size).abs_diff(wanted),
      Kind::Scalable => (at(self.min), at(self.max)),
      Kind::Threshold => (
        at(self.size.saturating_sub(self.threshold)),
        at(self.size.saturating_add(self.threshold)),
      ),
    };
    // Outside the range, the spec measures from MinSize and MaxSize.
    if wanted < low {
      at(self.min).saturating_sub(wanted)
    } else if wanted > high {
      wanted.saturating_sub(at(self.max))
    } else {
      0
    }
  }
}
