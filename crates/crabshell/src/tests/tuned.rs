//! Telling secure mode from the auxiliary vector, before the shell starts
//! itself again with glibc's `mmap` threshold fixed.

use std::time::{Duration, SystemTime};

use crate::tuned::{secure, since_first_start};

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

#[test]
fn the_first_start_is_read_back_only_when_it_makes_sense() {
  let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
  let nanos = |ago: Duration| {
    let then = now - ago;
    let since = then
      .duration_since(SystemTime::UNIX_EPOCH)
      .expect("after 1970");
    since.as_nanos().to_string()
  };
  let read = |value: &str| since_first_start(Some(value.as_ref()), now);
  let ms = Duration::from_millis;
  assert_eq!(read(&nanos(ms(4))), Some(ms(4)));
  assert_eq!(read(&nanos(ms(0))), Some(ms(0)));
  assert_eq!(read(&nanos(Duration::from_secs(11))), None, "too long ago");
  let later = (now + ms(5))
    .duration_since(SystemTime::UNIX_EPOCH)
    .expect("after 1970");
  assert_eq!(read(&later.as_nanos().to_string()), None, "later than now");
  assert_eq!(read("soon"), None);
  assert_eq!(since_first_start(None, now), None);
}
