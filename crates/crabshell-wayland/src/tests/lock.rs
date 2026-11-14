//! Session locks without a compositor: one that lacks the protocol.

use super::capture_logs;
use crate::lock::Locks;

#[test]
fn without_the_protocol_asking_to_lock_is_logged_once_and_never_locks() {
  let mut locks = Locks::new(None);
  let logs = capture_logs(|| {
    assert!(locks.manager().is_none());
    assert!(locks.manager().is_none());
  });
  assert_eq!(
    logs.matches("lacks ext-session-lock-v1").count(),
    1,
    "{logs}"
  );
}
