//! The icon theme a config gets when it names none: GTK's.

use std::path::{Path, PathBuf};

use super::capture_logs;
use crate::icon_theme::{config_home, gtk_icon_theme};

/// A config directory of the test's own, gone on drop.
struct ConfigHome(PathBuf);

impl ConfigHome {
  fn new(name: &str) -> ConfigHome {
    let dir = std::env::temp_dir().join(format!("crabshell-gtk-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    ConfigHome(dir)
  }

  fn write(&self, gtk: &str, text: impl AsRef<[u8]>) {
    let dir = self.0.join(gtk);
    std::fs::create_dir_all(&dir).expect("GTK's directory");
    std::fs::write(dir.join("settings.ini"), text).expect("GTK's settings");
  }
}

impl Drop for ConfigHome {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn gtk_3s_icon_theme_wins_then_gtk_4s() {
  let home = ConfigHome::new("order");
  assert_eq!(gtk_icon_theme(&home.0), None, "no settings at all");
  home.write("gtk-4.0", "[Settings]\ngtk-icon-theme-name=Adwaita\n");
  assert_eq!(gtk_icon_theme(&home.0).as_deref(), Some("Adwaita"));
  home.write(
    "gtk-3.0",
    "# a comment\n[Settings]\ngtk-theme-name=Rime\ngtk-icon-theme-name = Papirus-Dark \n",
  );
  assert_eq!(gtk_icon_theme(&home.0).as_deref(), Some("Papirus-Dark"));
  // GTK 3's file without the key leaves it to GTK 4's.
  home.write("gtk-3.0", "[Settings]\ngtk-theme-name=Rime\n");
  assert_eq!(gtk_icon_theme(&home.0).as_deref(), Some("Adwaita"));
}

#[test]
fn only_the_settings_group_counts_and_the_last_key_wins() {
  let home = ConfigHome::new("groups");
  home.write(
    "gtk-3.0",
    "gtk-icon-theme-name=Outside\n[Settings]\ngtk-icon-theme-name=First\ngtk-icon-theme-name=Last\n[Other]\ngtk-icon-theme-name=Elsewhere\n",
  );
  assert_eq!(gtk_icon_theme(&home.0).as_deref(), Some("Last"));
  home.write("gtk-3.0", "[Settings]\ngtk-icon-theme-name=\n");
  home.write("gtk-4.0", "[Settings]\ngtk-icon-theme-name=Adwaita\n");
  assert_eq!(
    gtk_icon_theme(&home.0).as_deref(),
    Some("Adwaita"),
    "an empty name names nothing"
  );
}

#[test]
fn a_malformed_file_is_logged_and_skipped() {
  let home = ConfigHome::new("malformed");
  home.write("gtk-4.0", "[Settings]\ngtk-icon-theme-name=Adwaita\n");
  for (bad, why) in [
    (
      b"[Settings]\ngtk-icon-theme-name=\xff\n".to_vec(),
      "isn't UTF-8",
    ),
    (
      b"[Settings]\ngtk-icon-theme-name=../../x\n".to_vec(),
      "holds a /",
    ),
    (vec![b'#'; 64 * 1024 + 1], "over 64 KiB"),
  ] {
    home.write("gtk-3.0", bad);
    let mut theme = None;
    let logs = capture_logs(|| theme = gtk_icon_theme(&home.0));
    assert_eq!(theme.as_deref(), Some("Adwaita"), "{why}");
    assert!(logs.contains(why), "{why}: {logs}");
  }
  // A directory where the file should be is skipped too.
  std::fs::remove_file(home.0.join("gtk-3.0/settings.ini")).expect("the file");
  std::fs::create_dir(home.0.join("gtk-3.0/settings.ini")).expect("a directory");
  let mut theme = None;
  let logs = capture_logs(|| theme = gtk_icon_theme(&home.0));
  assert_eq!(theme.as_deref(), Some("Adwaita"));
  assert!(logs.contains("isn't a file"), "{logs}");
}

#[test]
fn the_config_home_is_xdg_config_home_else_home_dot_config() {
  let path = |text: &str| Some(std::ffi::OsString::from(text));
  assert_eq!(
    config_home(path("/xdg/config"), path("/home/me")),
    Some(PathBuf::from("/xdg/config"))
  );
  // The XDG spec ignores a relative or empty path.
  assert_eq!(
    config_home(path("relative"), path("/home/me")),
    Some(Path::new("/home/me/.config").to_owned())
  );
  assert_eq!(
    config_home(path(""), path("/home/me")),
    Some(Path::new("/home/me/.config").to_owned())
  );
  assert_eq!(config_home(None, None), None);
  assert_eq!(config_home(None, path("")), None);
}
