//! `crabshell`, the command that creates, checks and runs a Crabshell
//! config: a Cargo project in `$XDG_CONFIG_HOME/crabshell` (by default
//! `~/.config/crabshell`), built into `$XDG_CACHE_HOME/crabshell/target`
//! (by default `~/.cache/crabshell/target`) so the config directory stays
//! small.

#![forbid(unsafe_code)]

mod build;
mod policy;
mod supervise;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: crabshell <command>

commands:
  init [--path DIR]  create a config in ~/.config/crabshell from the template.
                     With --path, it builds against the Crabshell checkout at
                     DIR instead of crates.io
  check              run cargo check on the config and print its errors
  run                build the config and run it, starting it again if it
                     crashes
  help               show this
";

/// The template's manifest and the bar it starts with: the design doc's
/// "Writing a config", word for word.
const TEMPLATE_MANIFEST: &str = include_str!("../template/Cargo.toml.template");
const TEMPLATE_MAIN: &str = include_str!("../template/src/main.rs");

/// The template's dependency line, which `--path` replaces.
const FROM_CRATES_IO: &str = "crabshell = \"0.1\"";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Task {
  Init { path: Option<PathBuf> },
  Check,
  Run,
  Help,
}

fn main() -> ExitCode {
  let args: Option<Vec<String>> = std::env::args_os()
    .skip(1)
    .map(|arg| arg.into_string().ok())
    .collect();
  let task = match args.ok_or_else(|| String::from("an argument isn't UTF-8")) {
    Ok(args) => parse(&args),
    Err(problem) => Err(problem),
  };
  let task = match task {
    Ok(task) => task,
    Err(problem) => {
      eprintln!("crabshell: {problem}\n\n{USAGE}");
      return ExitCode::from(2);
    }
  };
  let done = match task {
    Task::Init { path } => init(path.as_deref()).map(|()| ExitCode::SUCCESS),
    Task::Check => check(),
    Task::Run => run(),
    Task::Help => {
      print!("{USAGE}");
      Ok(ExitCode::SUCCESS)
    }
  };
  done.unwrap_or_else(|problem| {
    eprintln!("crabshell: {problem}");
    ExitCode::FAILURE
  })
}

fn parse(args: &[String]) -> Result<Task, String> {
  let words: Vec<&str> = args.iter().map(String::as_str).collect();
  match words.as_slice() {
    ["init"] => Ok(Task::Init { path: None }),
    ["init", "--path", dir] => Ok(Task::Init {
      path: Some(PathBuf::from(dir)),
    }),
    ["check"] => Ok(Task::Check),
    ["run"] => Ok(Task::Run),
    ["help" | "--help" | "-h"] => Ok(Task::Help),
    [] => Err(String::from("no command given")),
    ["init" | "check" | "run" | "help" | "--help" | "-h", ..] => {
      Err(format!("can't make sense of `{}`", args.join(" ")))
    }
    [command, ..] => Err(format!("unknown command `{command}`")),
  }
}

/// `$XDG_CONFIG_HOME/crabshell`, or `~/.config/crabshell`.
fn config_dir() -> Result<PathBuf, String> {
  base_dir("XDG_CONFIG_HOME", ".config").map(|dir| dir.join("crabshell"))
}

/// Where configs build: `$XDG_CACHE_HOME/crabshell/target`, or
/// `~/.cache/crabshell/target`.
fn target_dir() -> Result<PathBuf, String> {
  base_dir("XDG_CACHE_HOME", ".cache").map(|dir| dir.join("crabshell").join("target"))
}

/// The XDG base directory `variable` names, or `fallback` under `HOME`.
/// The spec says a relative path in the variable is ignored.
fn base_dir(variable: &str, fallback: &str) -> Result<PathBuf, String> {
  let set = |name: &str| {
    std::env::var_os(name)
      .map(PathBuf::from)
      .filter(|path| path.is_absolute())
  };
  set(variable)
    .or_else(|| set("HOME").map(|home| home.join(fallback)))
    .ok_or_else(|| format!("neither ${variable} nor $HOME is set"))
}

/// Creates the config from the template. Refuses if the directory is
/// there already, so nothing is ever overwritten.
fn init(path: Option<&Path>) -> Result<(), String> {
  let dir = config_dir()?;
  if dir.symlink_metadata().is_ok() {
    return Err(format!(
      "{} already exists; init only creates a new config",
      dir.display()
    ));
  }
  let manifest = match path {
    None => TEMPLATE_MANIFEST.to_owned(),
    Some(checkout) => {
      let crate_dir = checkout.join("crates").join("crabshell");
      let crate_dir = crate_dir
        .canonicalize()
        .ok()
        .filter(|dir| dir.join("Cargo.toml").is_file())
        .ok_or_else(|| format!("{} isn't a Crabshell checkout", checkout.display()))?;
      let path = toml_string(&crate_dir)
        .ok_or_else(|| format!("{} isn't a UTF-8 path", crate_dir.display()))?;
      let line = format!("crabshell = {{ path = {path} }}");
      TEMPLATE_MANIFEST.replacen(FROM_CRATES_IO, &line, 1)
    }
  };
  let parent = dir.parent().unwrap_or(&dir);
  std::fs::create_dir_all(parent)
    .map_err(|error| format!("can't create {}: {error}", parent.display()))?;
  // `create_dir` refuses one that appeared since the check above.
  std::fs::create_dir(&dir).map_err(|error| format!("can't create {}: {error}", dir.display()))?;
  let written = std::fs::create_dir(dir.join("src"))
    .and_then(|()| std::fs::write(dir.join("Cargo.toml"), &manifest))
    .and_then(|()| std::fs::write(dir.join("src").join("main.rs"), TEMPLATE_MAIN));
  if let Err(error) = written {
    // A half-made config would block the next try.
    let _ = std::fs::remove_dir_all(&dir);
    return Err(format!(
      "can't write the config in {}: {error}",
      dir.display()
    ));
  }
  println!(
    "created a config in {}\nedit src/main.rs there, and check it with `crabshell check`",
    dir.display()
  );
  if path.is_none() {
    eprintln!(
      "crabshell isn't on crates.io yet, so this config builds only once it is; \
       `crabshell init --path <checkout>` builds against a local checkout instead"
    );
  }
  Ok(())
}

/// A TOML basic string holding `path`; `None` if it isn't UTF-8.
fn toml_string(path: &Path) -> Option<String> {
  let mut quoted = String::from("\"");
  for character in path.to_str()?.chars() {
    match character {
      '"' => quoted.push_str("\\\""),
      '\\' => quoted.push_str("\\\\"),
      '\t' => quoted.push('\t'),
      control if control.is_control() => {
        quoted.push_str(&format!("\\u{:04X}", u32::from(control)));
      }
      other => quoted.push(other),
    }
  }
  quoted.push('"');
  Some(quoted)
}

/// The config directory, if it has a `Cargo.toml`.
fn config_with_manifest() -> Result<PathBuf, String> {
  let dir = config_dir()?;
  if !dir.join("Cargo.toml").is_file() {
    return Err(format!(
      "there is no config in {}; create one with `crabshell init`",
      dir.display()
    ));
  }
  Ok(dir)
}

/// Builds the config and supervises the shell it makes.
fn run() -> Result<ExitCode, String> {
  supervise::run(&config_with_manifest()?, &target_dir()?).map(|()| ExitCode::SUCCESS)
}

/// Runs `cargo check` on the config, its output passed straight through.
/// The exit status is cargo's.
fn check() -> Result<ExitCode, String> {
  let dir = config_with_manifest()?;
  // From the config's directory, so another project's .cargo/config.toml
  // or rust-toolchain.toml doesn't apply.
  let status = std::process::Command::new(build::cargo())
    .current_dir(&dir)
    .args(["check", "--manifest-path"])
    .arg(dir.join("Cargo.toml"))
    .env("CARGO_TARGET_DIR", target_dir()?)
    .status()
    .map_err(|error| format!("can't run cargo ({error}); is Rust installed?"))?;
  Ok(match status.code() {
    Some(0) => ExitCode::SUCCESS,
    Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
    None => ExitCode::FAILURE,
  })
}

#[cfg(test)]
mod tests;
