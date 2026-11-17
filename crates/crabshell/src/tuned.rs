//! Starting the shell again with glibc's `mmap` threshold fixed, as the
//! supervisor starts it. Left to move, the threshold rises to the size of
//! each large block freed, so the next decoded picture lands on the heap
//! and a replaced wallpaper keeps the RSS at its high-water mark.

use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant, SystemTime};

use crabshell_services::FIRST_START;

/// How long a restart may take before the time it carries is taken for
/// one left from another shell or a clock that moved.
const MAX_RESTART: Duration = Duration::from_secs(10);

/// Replaces this process with the shell started again with
/// `GLIBC_TUNABLES` fixing glibc's `mmap` threshold, unless it fixes one
/// already, and with [`FIRST_START`] holding `started`, when this process
/// started, so its first frame counts the restart. Everything `main` did
/// before `run` happens again. It runs before logging starts, so nothing
/// is logged twice: if it can't, it returns why, for `run` to log, and the
/// shell runs on as it is.
pub(crate) fn start_again_tuned(started: Instant) -> Option<String> {
  let tunables = crabshell_services::shell_tunables(std::env::var_os("GLIBC_TUNABLES"))?;
  // In secure mode glibc drops `GLIBC_TUNABLES`, so this would loop.
  if std::fs::read("/proc/self/auxv").map_or(true, |auxv| secure(&auxv)) {
    return Some(String::from(
      "running in secure mode, or without /proc, so glibc's mmap threshold stays unfixed and \
       memory a large picture frees may stay in use",
    ));
  }
  let error = match std::env::current_exe() {
    Ok(program) => {
      let first = SystemTime::now() - started.elapsed();
      let nanos = first
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
      let mut args = std::env::args_os();
      let mut command = std::process::Command::new(program);
      if let Some(name) = args.next() {
        command.arg0(name);
      }
      command
        .args(args)
        .env("GLIBC_TUNABLES", tunables)
        .env(FIRST_START, nanos.to_string())
        .exec()
    }
    Err(error) => error,
  };
  Some(format!(
    "couldn't start the shell again with glibc's mmap threshold fixed, so memory a large \
     picture frees may stay in use: {error}"
  ))
}

/// When the shell first started, if `run` started it again: from
/// [`FIRST_START`], read now.
pub(crate) fn first_start() -> Option<Instant> {
  let since = since_first_start(std::env::var_os(FIRST_START).as_deref(), SystemTime::now())?;
  Instant::now().checked_sub(since)
}

/// How long before `now` the shell first started, from `value`,
/// [`FIRST_START`]'s nanoseconds since the Unix epoch. `None` for anything
/// unreadable, later than `now` or more than [`MAX_RESTART`] back, as from a
/// clock that moved or a variable another shell left.
pub(crate) fn since_first_start(value: Option<&OsStr>, now: SystemTime) -> Option<Duration> {
  let nanos: u64 = value?.to_str()?.parse().ok()?;
  let first = SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos);
  let since = now.duration_since(first).ok()?;
  (since <= MAX_RESTART).then_some(since)
}

/// Whether `auxv`, the kernel's auxiliary vector as `/proc/self/auxv`
/// gives it, says the process runs in secure mode, as a setuid program or
/// one with file capabilities does. Without the entry, it counts as
/// secure.
pub(crate) fn secure(auxv: &[u8]) -> bool {
  const AT_SECURE: usize = 23;
  let (words, _) = auxv.as_chunks::<{ size_of::<usize>() }>();
  words
    .as_chunks::<2>()
    .0
    .iter()
    .map(|&[key, value]| (usize::from_ne_bytes(key), usize::from_ne_bytes(value)))
    .find(|&(key, _)| key == AT_SECURE)
    .is_none_or(|(_, value)| value != 0)
}
