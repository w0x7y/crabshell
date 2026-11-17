//! Starting the shell again with glibc's `mmap` threshold fixed, as the
//! supervisor starts it. Left to move, the threshold rises to the size of
//! each large block freed, so the next decoded picture lands on the heap
//! and a replaced wallpaper keeps the RSS at its high-water mark.

use std::os::unix::process::CommandExt;

/// Replaces this process with the shell started again with
/// `GLIBC_TUNABLES` fixing glibc's `mmap` threshold, unless it fixes one
/// already. Everything `main` did before `run` happens again. If it
/// can't, that is logged and the shell runs on as it is.
pub(crate) fn start_again_tuned() {
  let Some(tunables) = crabshell_services::shell_tunables(std::env::var_os("GLIBC_TUNABLES"))
  else {
    return;
  };
  // In secure mode glibc drops `GLIBC_TUNABLES`, so this would loop.
  if std::fs::read("/proc/self/auxv").map_or(true, |auxv| secure(&auxv)) {
    tracing::warn!(
      "running in secure mode, or without /proc, so glibc's mmap threshold stays unfixed and \
       memory a large picture frees may stay in use"
    );
    return;
  }
  let error = match std::env::current_exe() {
    Ok(program) => {
      let mut args = std::env::args_os();
      let mut command = std::process::Command::new(program);
      if let Some(name) = args.next() {
        command.arg0(name);
      }
      command.args(args).env("GLIBC_TUNABLES", tunables).exec()
    }
    Err(error) => error,
  };
  tracing::warn!(
    %error,
    "couldn't start the shell again with glibc's mmap threshold fixed, so memory a large \
     picture frees may stay in use"
  );
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
