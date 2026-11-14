use crate::foreign_toplevels::cut;

#[test]
fn titles_and_app_ids_are_cut_to_4_kib_at_a_character() {
  assert_eq!(cut(String::from("Files")), "Files");
  assert_eq!(cut("a".repeat(5000)), "a".repeat(4096));
  // The two bytes of "é" would end at 4097.
  assert_eq!(cut(format!("{}é", "a".repeat(4095))), "a".repeat(4095));
}
