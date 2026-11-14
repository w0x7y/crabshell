//! Pairing ext toplevel handles with wlr toplevels.

use crate::ext_toplevels::pair;

const FOOT: (&str, &str) = ("foot", "~");
const NEWS: (&str, &str) = ("firefox", "news");
const FILM: (&str, &str) = ("mpv", "film");

#[test]
fn each_handle_takes_the_oldest_untaken_window_of_its_names() {
  let (mut first, mut kept, mut second, mut third, mut lonely) = (None, Some(9), None, None, None);
  let wlr = [(3, FOOT), (5, NEWS), (7, FOOT), (9, NEWS)];
  pair(
    vec![
      (FOOT, &mut first),
      // Paired before with the second news window, so the first is free.
      (NEWS, &mut kept),
      (FOOT, &mut second),
      (FOOT, &mut third),
      (FILM, &mut lonely),
    ],
    &wlr,
  );
  assert_eq!(first, Some(3));
  assert_eq!(kept, Some(9), "a pair made before stays");
  assert_eq!(second, Some(7));
  assert_eq!(third, None, "both foot windows are taken");
  assert_eq!(lonely, None, "no window of its names yet");

  // A window listed later pairs then.
  let wlr = [(3, FOOT), (5, NEWS), (7, FOOT), (9, NEWS), (11, FILM)];
  pair(
    vec![(FOOT, &mut first), (FILM, &mut lonely), (NEWS, &mut kept)],
    &wlr,
  );
  assert_eq!(lonely, Some(11));
  assert_eq!(first, Some(3));
}

#[test]
fn a_handle_whose_window_went_pairs_again() {
  // The window it paired with, 3, went before the handle did.
  let mut stale = Some(3);
  let wlr = [(8, FOOT)];
  pair(vec![(FOOT, &mut stale)], &wlr);
  assert_eq!(stale, Some(8), "free to pair with the new window");
}
