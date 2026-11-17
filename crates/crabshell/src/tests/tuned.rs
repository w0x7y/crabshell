//! Telling secure mode from the auxiliary vector, before the shell starts
//! itself again with glibc's `mmap` threshold fixed.

use crate::tuned::secure;

/// An auxiliary vector of `entries`, as `/proc/self/auxv` gives it.
fn auxv(entries: &[(usize, usize)]) -> Vec<u8> {
  entries
    .iter()
    .flat_map(|&(key, value)| [key.to_ne_bytes(), value.to_ne_bytes()])
    .flatten()
    .collect()
}

#[test]
fn secure_mode_is_read_from_at_secure() {
  assert!(!secure(&auxv(&[(6, 4096), (23, 0), (0, 0)])));
  assert!(secure(&auxv(&[(6, 4096), (23, 1), (0, 0)])));
  assert!(
    secure(&auxv(&[(6, 4096), (0, 0)])),
    "no entry counts as secure"
  );
  assert!(secure(&[]), "nothing read counts as secure");
}

#[test]
fn this_test_isnt_in_secure_mode() {
  let auxv = std::fs::read("/proc/self/auxv").expect("/proc/self/auxv");
  assert!(!secure(&auxv));
}
