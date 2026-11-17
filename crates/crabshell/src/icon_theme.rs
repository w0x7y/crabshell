//! The icon theme a config that names none gets: the one GTK apps use.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest settings file read; GTK's are a few hundred bytes.
const MAX_SETTINGS: u64 = 64 * 1024;

/// The XDG config directory: `$XDG_CONFIG_HOME` when it is an absolute
/// path, else `$HOME/.config`. `None` without either.
pub(crate) fn config_home(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
  let xdg = xdg.map(PathBuf::from).filter(|dir| dir.is_absolute());
  let home = || {
    let home = home.filter(|home| !home.is_empty())?;
    Some(PathBuf::from(home).join(".config"))
  };
  xdg.or_else(home)
}

/// `gtk-icon-theme-name` from `gtk-3.0/settings.ini` under `config_home`,
/// else from `gtk-4.0/settings.ini`. A file that is missing or names none
/// leaves it to the next; one that isn't a small UTF-8 file, or names a
/// theme with a `/`, is logged and skipped.
pub(crate) fn gtk_icon_theme(config_home: &Path) -> Option<String> {
  ["gtk-3.0", "gtk-4.0"].into_iter().find_map(|gtk| {
    let path = config_home.join(gtk).join("settings.ini");
    let theme = read(&path).and_then(|bytes| theme_in(&bytes));
    theme.unwrap_or_else(|why| {
      let path = path.display();
      tracing::warn!(%path, "GTK's settings are left out: {why}");
      None
    })
  })
}

/// The file's bytes, `Ok(empty)` when it isn't there, or why it can't be
/// read. A pipe or a directory is never opened.
fn read(path: &Path) -> Result<Vec<u8>, String> {
  match std::fs::metadata(path) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(error) => return Err(error.to_string()),
    Ok(meta) if !meta.is_file() => return Err(String::from("it isn't a file")),
    Ok(_) => {}
  }
  let mut bytes = Vec::new();
  let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
  file
    .take(MAX_SETTINGS + 1)
    .read_to_end(&mut bytes)
    .map_err(|error| error.to_string())?;
  if bytes.len() as u64 > MAX_SETTINGS {
    return Err(String::from("it is over 64 KiB"));
  }
  Ok(bytes)
}

/// The last `gtk-icon-theme-name` in the `[Settings]` group of a GTK
/// settings file, `None` for none or an empty one.
fn theme_in(bytes: &[u8]) -> Result<Option<String>, String> {
  let text = std::str::from_utf8(bytes).map_err(|_| String::from("it isn't UTF-8"))?;
  let mut settings = false;
  let mut theme = None;
  for line in text.lines().map(str::trim) {
    if line.starts_with('[') {
      settings = line == "[Settings]";
    } else if settings
      && !line.starts_with('#')
      && let Some((key, value)) = line.split_once('=')
      && key.trim_end() == "gtk-icon-theme-name"
    {
      theme = Some(value.trim_start());
    }
  }
  match theme.filter(|theme| !theme.is_empty()) {
    // A theme is a directory's name under `icons/`.
    Some(theme) if theme.contains('/') => Err(format!("its icon theme {theme:?} holds a /")),
    theme => Ok(theme.map(str::to_owned)),
  }
}
