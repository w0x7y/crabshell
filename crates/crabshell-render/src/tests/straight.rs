//! Straight edges: where no corner's curve reaches a pixmap and a shape's
//! edges lie on whole pixels, a clip, a cut-out shadow and a mask region
//! use the rectangle the shape covers instead of filling a mask, and the
//! blends they lead to work on whole words. Each must give what the mask
//! and the per-channel blend give, to the bit.

use tiny_skia::{FillRule, IntSize, Mask, PathBuilder, Pixmap, Transform};

use crate::draw::{composite, composite_within, floor_byte, over_word, rounded_rect};
use crate::draw::{straight_inside, straight_rows};

const SCALES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

/// The mask tiny-skia fills for `shape` with `corners` in a `size` pixmap.
fn filled((width, height): (u32, u32), shape: tiny_skia::Rect, corners: [f32; 4]) -> Mask {
  let mut path = PathBuilder::new();
  rounded_rect(&mut path, shape, corners);
  let mut mask = Mask::new(width, height).expect("a mask");
  let path = path.finish().expect("a path");
  mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
  mask
}

#[test]
fn a_straight_shape_covers_what_its_filled_mask_does_in_every_tile() {
  let (mut whole, mut banded, mut curved) = (0, 0, 0);
  for scale in SCALES {
    for radii in [
      [0.0; 4],
      [1.9; 4],
      [2.9, 0.0, 0.0, 0.0],
      [3.3; 4],
      [10.0; 4],
      [22.5, 0.0, 7.0, 15.0],
    ] {
      // A box laid out in logical pixels, snapped to the pixel grid.
      let (x, y, w, h) = (7.3_f32, 5.6_f32, 190.2_f32, 130.7_f32);
      let edges = [x, y, x + w, y + h].map(|edge| (edge * scale).round());
      let corners = radii.map(|radius| radius * scale);
      // Every tile it reaches, and one cut at a 300 by 150 window's edge.
      let (columns, rows) = ((edges[2] / 64.0) as u32 + 1, (edges[3] / 64.0) as u32 + 1);
      for (column, row) in (0..=columns).flat_map(|column| (0..=rows).map(move |row| (column, row)))
      {
        let (left, top) = (column * 64, row * 64);
        let size = (
          64.min(300 - left.min(300)).max(1),
          64.min(150 - top.min(150)).max(1),
        );
        let shape = tiny_skia::Rect::from_ltrb(
          edges[0] - left as f32,
          edges[1] - top as f32,
          edges[2] - left as f32,
          edges[3] - top as f32,
        )
        .expect("a shape");
        let mask = filled(size, shape, corners);
        let width = size.0 as usize;
        if let Some([l, t, r, b]) = straight_inside(size, shape, corners) {
          whole += 1;
          for (at, &coverage) in mask.data().iter().enumerate() {
            let (px, py) = ((at % width) as u32, (at / width) as u32);
            let inside = (l..r).contains(&px) && (t..b).contains(&py);
            let label = format!("tile {column},{row} at {scale} with {radii:?}, pixel {px},{py}");
            assert_eq!(coverage, if inside { 255 } else { 0 }, "{label}");
          }
        } else {
          curved += 1;
        }
        if let Some([l, t, r, b]) = straight_rows(size, shape, corners) {
          banded += 1;
          for py in t..b {
            for px in 0..size.0 {
              let coverage = mask.data()[py as usize * width + px as usize];
              let label = format!("tile {column},{row} at {scale} with {radii:?}, row {py}");
              assert_eq!(
                coverage,
                if (l..r).contains(&px) { 255 } else { 0 },
                "{label}"
              );
            }
          }
        }
      }
    }
  }
  assert!(
    whole > 100 && banded > 100 && curved > 20,
    "{whole} {banded} {curved}"
  );
}

#[test]
fn a_shape_off_whole_pixels_is_never_straight() {
  let shape = tiny_skia::Rect::from_ltrb(3.5, 4.0, 50.0, 40.0).expect("a shape");
  assert_eq!(straight_inside((64, 64), shape, [0.0; 4]), None);
  assert_eq!(straight_rows((64, 64), shape, [0.0; 4]), None);
}

#[test]
fn a_blend_of_whole_words_matches_the_blend_channel_by_channel() {
  for alpha in 0..=255u32 {
    for channel in 0..=255u32 {
      for under in 0..=255u32 {
        // Each lane different, past premultiplied too, so lanes can't
        // borrow from each other unseen.
        let from = [channel, 255 - channel, channel / 3, alpha].map(|n| n as u8);
        let to = [under, 255 - under, under ^ 0x5a, under / 2].map(|n| n as u8);
        let rest = 255 - u16::from(from[3]);
        let expected: [u8; 4] = std::array::from_fn(|lane| {
          let kept = ((u16::from(to[lane]) * rest + 127) / 255) as u8;
          from[lane].saturating_add(kept)
        });
        let got = over_word(u32::from_le_bytes(to), u32::from_le_bytes(from)).to_le_bytes();
        assert_eq!(got, expected, "{from:?} over {to:?}");
      }
    }
  }
}

#[test]
fn a_level_is_floored_as_a_cast_floors_it() {
  let mut checks = 0;
  // Every value it is given lies from 0 to 255.
  let mut check = |value: f32| {
    if value <= 255.0 {
      assert_eq!(floor_byte(value), value as u8, "{value}");
      checks += 1;
    }
  };
  for level in 0..=255u32 {
    let whole = level as f32;
    check(whole);
    check(f32::from_bits(whole.to_bits() + 1));
    if level > 0 {
      check(f32::from_bits(whole.to_bits() - 1));
    }
    for step in 1..64 {
      check(whole + step as f32 / 64.0);
    }
  }
  let mut bits = 0;
  while f32::from_bits(bits) < 256.0 {
    check(f32::from_bits(bits));
    bits += 4099;
  }
  assert!(checks > 250_000, "{checks}");
}

#[test]
fn a_composite_within_an_area_leaves_the_rest_and_matches_inside() {
  let layer = Pixmap::from_vec(
    (0..30u32 * 20)
      .flat_map(|at| {
        let alpha = [255, 140, 0, 30][at as usize % 4];
        [(at % 97) as u8, (at % 61) as u8, 40, alpha].map(|n: u8| n.min(alpha))
      })
      .collect(),
    IntSize::from_wh(30, 20).expect("a size"),
  )
  .expect("a layer");
  let under: Vec<u8> = (0..64u32 * 48)
    .flat_map(|at| [(at % 251) as u8, 120, (at % 7 * 30) as u8, 255])
    .collect();
  let mut mask = Mask::new(64, 48).expect("a mask");
  mask
    .data_mut()
    .iter_mut()
    .enumerate()
    .for_each(|(at, m)| *m = (at * 7 % 256) as u8);
  let area = [10, 5, 31, 30];
  for opacity in [1.0, 0.6] {
    for mask in [None, Some(&mask)] {
      let mut whole = under.clone();
      let mut pixmap = tiny_skia::PixmapMut::from_bytes(&mut whole, 64, 48).expect("a pixmap");
      composite(&mut pixmap, (4, -3), &layer, opacity, mask);
      let mut part = under.clone();
      let mut pixmap = tiny_skia::PixmapMut::from_bytes(&mut part, 64, 48).expect("a pixmap");
      composite_within(&mut pixmap, (4, -3), &layer, opacity, mask, area);
      for (at, (got, (all, before))) in part
        .chunks(4)
        .zip(whole.chunks(4).zip(under.chunks(4)))
        .enumerate()
      {
        let (x, y) = (at as u32 % 64, at as u32 / 64);
        let inside = (area[0]..area[2]).contains(&x) && (area[1]..area[3]).contains(&y);
        let label = format!("{x},{y} at {opacity}, masked {}", mask.is_some());
        assert_eq!(got, if inside { all } else { before }, "{label}");
      }
    }
  }
}

/// A 40 by 40 box at 20, 10 in a 100 by 70 window on clear, with no
/// background and `shadow`, drawn whole at `scale`.
fn shadowed(radius: f32, shadow: crabshell_ui::Shadow, scale: f32) -> super::Buffer {
  use crabshell_ui::{Row, Style};
  let mut scene = super::Scene::new();
  let boxed = Row::new().style(
    Style::new()
      .left(20.0)
      .top(10.0)
      .width(40.0)
      .height(40.0)
      .radius(radius)
      .shadow(shadow),
  );
  let window = scene.window(scene.panel().child(boxed), 100.0, 70.0);
  scene.full(&window, scale)
}

#[test]
fn a_cut_out_shadow_is_the_shadow_cut_by_the_boxs_own_corners() {
  use crabshell_ui::{Color, Shadow};
  let color = Color::rgba(0, 0, 0, 200);
  // The case that showed it: shadow under the box's corner squares.
  let drawn = shadowed(
    8.0,
    Shadow::new(3.0, 5.0, 6.0, color).spread(-6.0).cut_out(),
    1.0,
  );
  assert!(drawn.pixel(58, 47)[3] > 30, "{:?}", drawn.pixel(58, 47));
  for scale in SCALES {
    for radius in [0.0, 8.0, 14.0] {
      for spread in [-6.0, -3.0, 0.0, 2.0] {
        let shadow = Shadow::new(3.0, 5.0, 6.0, color).spread(spread);
        let cut = shadowed(radius, shadow.cut_out(), scale);
        let plain = shadowed(radius, shadow, scale);
        // The box as its background fills it, in each 64 pixel tile.
        let edges = [20.0, 10.0, 60.0, 50.0].map(|edge: f32| (edge * scale).round());
        for (x, y) in (0..cut.width).flat_map(|x| (0..cut.height).map(move |y| (x, y))) {
          let (left, top) = (x / 64 * 64, y / 64 * 64);
          let size = (64.min(cut.width - left), 64.min(cut.height - top));
          let shape = tiny_skia::Rect::from_ltrb(
            edges[0] - left as f32,
            edges[1] - top as f32,
            edges[2] - left as f32,
            edges[3] - top as f32,
          )
          .expect("a shape");
          let tile = filled(size, shape, [radius * scale; 4]);
          let covered = tile.data()[((y - top) * size.0 + x - left) as usize];
          let expected = u32::from(plain.pixel(x, y)[3]) * (255 - u32::from(covered)) / 255;
          let got = u32::from(cut.pixel(x, y)[3]);
          let label = format!("{x},{y} at {scale}, radius {radius}, spread {spread}");
          assert!(
            got.abs_diff(expected) <= 3,
            "{label}: {got}, not {expected}"
          );
        }
      }
    }
  }
}
