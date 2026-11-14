//! The installed applications, from their desktop entries: what a
//! launcher lists, and where a window's app id finds its icon.

mod parse;

use std::path::PathBuf;

/// An installed application, from its desktop entry.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopEntry {
  /// Its desktop file ID: the file's path under `applications/`, with
  /// `/` as `-` and without `.desktop`, such as `org.gnome.Nautilus`.
  pub id: String,
  /// Its name, in the user's language when the file has it.
  pub name: String,
  /// What kind of program it is, such as `Web Browser`, or `""`.
  pub generic_name: String,
  /// A line about it, for a tooltip, or `""`.
  pub comment: String,
  /// Its icon, a name in the icon theme or a file's path as the entry
  /// writes it, or `""`.
  pub icon: String,
  /// What it runs, as its `Exec` key writes it, field codes such as `%U`
  /// and quotes included. `""` for one only D-Bus starts.
  pub exec: String,
  /// Whether it runs in a terminal.
  pub terminal: bool,
  /// Its categories, such as `Network` and `WebBrowser`.
  pub categories: Vec<String>,
  /// More words a search may match, in the user's language when the
  /// file has them.
  pub keywords: Vec<String>,
  /// Whether a launcher leaves it out: for `NoDisplay`, or because
  /// `OnlyShowIn` or `NotShowIn` leave out this desktop. A window's icon
  /// may still come from it.
  pub no_display: bool,
  /// The window class its windows have, if the entry says, or `""`.
  pub startup_wm_class: String,
  /// The directory it runs in, if not the shell's.
  pub working_dir: Option<PathBuf>,
  /// What else it can do, such as open a private window, in the
  /// entry's order.
  pub actions: Vec<DesktopAction>,
  /// The desktop file it was read from.
  pub path: PathBuf,
}

/// Something more an application can do, as a launcher's menu offers it.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopAction {
  /// Its name in the desktop file, such as `new-window`.
  pub id: String,
  /// What a menu shows, in the user's language when the file has it.
  pub name: String,
  /// Its icon, as [`DesktopEntry::icon`], or `""`.
  pub icon: String,
  /// What it runs, as [`DesktopEntry::exec`].
  pub exec: String,
}

/// Where the desktop entries are, and what picks among them. Tests point
/// it at their own directories.
#[derive(Default)]
pub(crate) struct Sources {
  /// Each `applications` directory, the user's first.
  pub(crate) dirs: Vec<PathBuf>,
  /// The desktops `XDG_CURRENT_DESKTOP` names, such as `sway`.
  pub(crate) desktops: Vec<String>,
  /// The locale for messages, from `LC_ALL`, `LC_MESSAGES` or `LANG`.
  pub(crate) locale: String,
  /// `PATH`'s directories, where `TryExec` and `xdg-terminal-exec` are
  /// looked for.
  pub(crate) path: Vec<PathBuf>,
}
