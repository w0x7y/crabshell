//! The `crabshell` binary, run against a config directory of its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A fresh directory for one test's config and cache, removed on drop.
struct Home(PathBuf);

impl Home {
  fn new(name: &str) -> Home {
    let dir = std::env::temp_dir().join(format!("crabshell-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a test directory");
    Home(dir)
  }

  /// Runs `crabshell` with `args`, its config and cache here.
  fn run(&self, args: &[&str]) -> Output {
    let config = self.0.join("config");
    self.run_with(args, config.to_str().expect("UTF-8"))
  }

  /// [`run`](Self::run) with `XDG_CONFIG_HOME` set to `config_home`.
  fn run_with(&self, args: &[&str], config_home: &str) -> Output {
    // The real cargo home, so building a config needs no download.
    let cargo_home = std::env::var_os("CARGO_HOME")
      .map(PathBuf::from)
      .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    let mut command = Command::new(env!("CARGO_BIN_EXE_crabshell"));
    if let Some(cargo_home) = cargo_home {
      command.env("CARGO_HOME", cargo_home);
    }
    command
      // Here, so a relative path can't land in the source tree.
      .current_dir(&self.0)
      .args(args)
      .env("HOME", &self.0)
      .env("XDG_CONFIG_HOME", config_home)
      .env("XDG_CACHE_HOME", self.0.join("cache"))
      .output()
      .expect("the crabshell binary runs")
  }

  fn config(&self) -> PathBuf {
    self.0.join("config").join("crabshell")
  }
}

impl Drop for Home {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn text(bytes: &[u8]) -> String {
  String::from_utf8_lossy(bytes).into_owned()
}

/// The repo's root, two levels above this crate.
fn checkout() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The fenced block of `language` after `heading` in the design doc.
fn doc_block(heading: &str, language: &str) -> String {
  let doc = std::fs::read_to_string(checkout().join("docs/design.md")).expect("the design doc");
  let after = &doc[doc.find(heading).expect("the heading")..];
  let fence = format!("```{language}\n");
  let start = after.find(&fence).expect("the block") + fence.len();
  let end = start + after[start..].find("```").expect("the block's end");
  after[start..end].to_owned()
}

#[test]
fn the_template_is_the_design_docs_config() {
  let manifest = std::fs::read_to_string(
    Path::new(env!("CARGO_MANIFEST_DIR")).join("template/Cargo.toml.template"),
  )
  .expect("the template's manifest");
  let main =
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("template/src/main.rs"))
      .expect("the template's bar");
  assert_eq!(manifest, doc_block("## Writing a config", "toml"));
  assert_eq!(
    main,
    doc_block("The whole v0.1 bar as a user writes it:", "rust")
  );
}

#[test]
fn init_writes_the_template_once_and_never_overwrites() {
  let home = Home::new("init");
  let done = home.run(&["init"]);
  assert!(done.status.success(), "{}", text(&done.stderr));
  assert!(text(&done.stdout).contains("created a config in"));
  assert!(text(&done.stderr).contains("isn't on crates.io yet"));
  let manifest = std::fs::read_to_string(home.config().join("Cargo.toml")).expect("written");
  assert!(manifest.contains("crabshell = \"0.1\""), "{manifest}");
  let main = std::fs::read_to_string(home.config().join("src/main.rs")).expect("written");
  assert!(main.contains("crabshell::run(|cx| cx.per_output(bar));"));

  std::fs::write(home.config().join("src/main.rs"), "// mine").expect("edited");
  let again = home.run(&["init"]);
  assert_eq!(again.status.code(), Some(1));
  assert!(
    text(&again.stderr).contains("already exists"),
    "{}",
    text(&again.stderr)
  );
  let main = std::fs::read_to_string(home.config().join("src/main.rs")).expect("still there");
  assert_eq!(main, "// mine", "nothing overwritten");
}

#[test]
fn init_with_a_path_builds_against_that_checkout() {
  let home = Home::new("path");
  let checkout = checkout().canonicalize().expect("the checkout");
  let done = home.run(&["init", "--path", checkout.to_str().expect("UTF-8")]);
  assert!(done.status.success(), "{}", text(&done.stderr));
  let manifest = std::fs::read_to_string(home.config().join("Cargo.toml")).expect("written");
  let line = format!(
    "crabshell = {{ path = \"{}\" }}",
    checkout.join("crates/crabshell").display()
  );
  assert!(manifest.contains(&line), "{manifest}");

  // A directory with a crates/crabshell that has no Cargo.toml in it.
  let elsewhere = Home::new("not-a-checkout");
  let fake = elsewhere.0.join("fake");
  std::fs::create_dir_all(fake.join("crates/crabshell")).expect("a fake checkout");
  let refused = elsewhere.run(&["init", "--path", fake.to_str().expect("UTF-8")]);
  assert_eq!(refused.status.code(), Some(1));
  assert!(text(&refused.stderr).contains("isn't a Crabshell checkout"));
  assert!(!elsewhere.config().exists(), "nothing created");
}

#[test]
fn a_relative_xdg_config_home_is_ignored_for_home() {
  let home = Home::new("relative");
  let done = home.run_with(&["init"], "relative/config");
  assert!(done.status.success(), "{}", text(&done.stderr));
  assert!(home.0.join(".config/crabshell/Cargo.toml").is_file());
}

#[test]
fn check_without_a_config_says_how_to_make_one() {
  let home = Home::new("no-config");
  let done = home.run(&["check"]);
  assert_eq!(done.status.code(), Some(1));
  assert!(text(&done.stderr).contains("create one with `crabshell init`"));
}

#[test]
fn a_bad_command_line_shows_the_usage() {
  let home = Home::new("usage");
  for line in [&[][..], &["build"][..], &["init", "--force"][..]] {
    let done = home.run(line);
    assert_eq!(done.status.code(), Some(2), "{line:?}");
    assert!(text(&done.stderr).contains("usage: crabshell"), "{line:?}");
  }
  // An argument that isn't UTF-8 gets the usage too, not a panic.
  use std::os::unix::ffi::OsStrExt;
  let odd = Command::new(env!("CARGO_BIN_EXE_crabshell"))
    .current_dir(&home.0)
    .args(["init", "--path"])
    .arg(std::ffi::OsStr::from_bytes(b"/src/\xff"))
    .output()
    .expect("runs");
  assert_eq!(odd.status.code(), Some(2));
  assert!(text(&odd.stderr).contains("usage: crabshell"));
  let help = home.run(&["help"]);
  assert!(help.status.success());
  assert!(text(&help.stdout).contains("usage: crabshell"));
}

/// Builds a new config end to end, which compiles every dependency once,
/// so it takes minutes and is ignored by default.
#[test]
#[ignore = "compiles the whole template: minutes"]
fn a_new_config_checks_clean_and_a_mistake_fails_the_check() {
  let home = Home::new("check");
  let checkout = checkout().canonicalize().expect("the checkout");
  let done = home.run(&["init", "--path", checkout.to_str().expect("UTF-8")]);
  assert!(done.status.success(), "{}", text(&done.stderr));
  let checked = home.run(&["check"]);
  assert!(checked.status.success(), "{}", text(&checked.stderr));
  assert!(
    home.0.join("cache/crabshell/target").is_dir(),
    "built in the cache"
  );

  let main = home.config().join("src/main.rs");
  let code = std::fs::read_to_string(&main).expect("the bar");
  std::fs::write(&main, code.replace(".height(30)", ".height(\"tall\")")).expect("broken");
  let failed = home.run(&["check"]);
  assert!(!failed.status.success());
  assert!(
    text(&failed.stderr).contains("error"),
    "{}",
    text(&failed.stderr)
  );
}
