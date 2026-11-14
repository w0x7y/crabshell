use crate::windowsets::coordinates;

#[test]
fn coordinates_are_u32s_up_to_16_of_them() {
  let bytes = |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| v.to_ne_bytes()).collect() };
  assert_eq!(coordinates(&[]), Some(Vec::new()));
  assert_eq!(coordinates(&bytes(&[1, 1000])), Some(vec![1, 1000]));
  assert_eq!(coordinates(&bytes(&[7; 16])), Some(vec![7; 16]));
  assert_eq!(coordinates(&bytes(&[7; 17])), None);
}
