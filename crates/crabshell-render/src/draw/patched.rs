//! Blurred box shadows drawn from a small one worked out once. Away from
//! its corners a blurred box depends only on how far a pixel is from the
//! nearest edge, so a large one is its four corners and four edges
//! stretched along: the same as a box just large enough for its corners
//! not to meet, with the same corners, blur and place within a pixel.

use super::BlurredBox;
use super::kept::Held;

/// What a blurred box looks like, but for its size: the bits of each
/// corner's radius and the blur, then where each edge falls within its
/// pixel, left, right, top and bottom.
pub(super) type Key = [u32; 9];

/// Coverage of the small box a large one is drawn from.
pub(super) struct Patch {
  /// Out of 255, row by row.
  mask: Vec<u8>,
  width: usize,
  /// How far from an edge, in pixels, the coverage changes along it.
  zone: i64,
  /// The pixel each edge falls in, left, right, top and bottom.
  edges: [i64; 4],
  _held: Held,
}

/// How far from an edge a box of `corners` blurred by `sigma` changes
/// along it, in pixels: the largest corner and the blur's reach.
fn zone(corners: [f32; 4], sigma: f32) -> i64 {
  let largest = corners.into_iter().fold(0.0, f32::max);
  (largest + 3.0 * sigma).ceil() as i64 + 1
}

/// Where `shape`'s edges fall in whole pixels, and within them.
fn edges(shape: tiny_skia::Rect) -> ([i64; 4], [f32; 4]) {
  let sides = [shape.left(), shape.right(), shape.top(), shape.bottom()];
  let whole = sides.map(|side| side.floor() as i64);
  let within = std::array::from_fn(|index| sides[index] - whole[index] as f32);
  (whole, within)
}

impl Patch {
  /// The key `shape`, with `corners` and blurred by `sigma`, is drawn
  /// from, or `None` if it is too small for its corners and edges to be
  /// apart, or too large to place within a pixel.
  pub(super) fn key(shape: tiny_skia::Rect, corners: [f32; 4], sigma: f32) -> Option<Key> {
    let size = shape.width().max(shape.height());
    if size.is_nan() || size >= 1e7 {
      return None;
    }
    let zone = zone(corners, sigma);
    let ([left, right, top, bottom], within) = edges(shape);
    if right - left < 2 * zone + 2 || bottom - top < 2 * zone + 2 {
      return None;
    }
    let [a, b, c, d] = corners.map(f32::to_bits);
    let [l, r, t, u] = within.map(f32::to_bits);
    Some([a, b, c, d, sigma.to_bits(), l, r, t, u])
  }

  /// The bytes the small box for `key` takes.
  pub(super) fn bytes(key: &Key) -> u64 {
    let width = Patch::side(key) as u64;
    width * width
  }

  /// The side of the small box's square of pixels for `key`.
  fn side(key: &Key) -> usize {
    let (pad, far) = Patch::sides(key);
    (far + pad + 2) as usize
  }

  /// Where the small box's left and top edges fall, and its right and
  /// bottom ones.
  fn sides(key: &Key) -> (i64, i64) {
    let corners = std::array::from_fn(|index| f32::from_bits(key[index]));
    let sigma = f32::from_bits(key[4]);
    // Room for the blur's reach before the left and top edges.
    let pad = (3.0 * sigma).ceil() as i64 + 2;
    (pad, pad + 2 * zone(corners, sigma) + 2)
  }

  /// The small box for `key`, worked out pixel by pixel, its bytes
  /// `held` against the budget.
  pub(super) fn new(key: &Key, held: Held) -> Patch {
    let corners = std::array::from_fn(|index| f32::from_bits(key[index]));
    let sigma = f32::from_bits(key[4]);
    let within: [f32; 4] = std::array::from_fn(|index| f32::from_bits(key[5 + index]));
    let zone = zone(corners, sigma);
    let (pad, far) = Patch::sides(key);
    let edges = [pad, far, pad, far];
    let shape = tiny_skia::Rect::from_ltrb(
      pad as f32 + within[0],
      pad as f32 + within[2],
      far as f32 + within[1],
      far as f32 + within[3],
    )
    .expect("invariant: a patch's box is wider than its zone");
    let width = Patch::side(key);
    let blurred = BlurredBox::new(shape, corners, sigma);
    let mut mask = Vec::with_capacity(width * width);
    for row in 0..width {
      let xs = (0..width).map(|column| column as f32 + 0.5);
      blurred.row(row as f32 + 0.5, xs, &mut mask);
    }
    Patch {
      mask,
      width,
      zone,
      edges,
      _held: held,
    }
  }

  /// Pushes the coverage of the large box `shape` at buffer pixels
  /// `columns` of row `row` onto `mask`.
  pub(super) fn row(
    &self,
    shape: tiny_skia::Rect,
    row: i64,
    columns: impl Iterator<Item = i64>,
    mask: &mut Vec<u8>,
  ) {
    let (edges, _) = edges(shape);
    // A pixel near an edge is the patch's as far from its own; one past
    // the zones of both is any of the middle ones.
    let near = |at: i64, (first, last): (i64, i64), (start, end): (i64, i64)| {
      if at - first <= self.zone {
        start + (at - first)
      } else if last - at <= self.zone {
        end - (last - at)
      } else {
        start + self.zone + 1
      }
    };
    let [left, right, top, bottom] = self.edges;
    let y = near(row, (edges[2], edges[3]), (top, bottom));
    let Some(line) = usize::try_from(y)
      .ok()
      .and_then(|y| self.mask.get(y * self.width..(y + 1) * self.width))
    else {
      mask.extend(columns.map(|_| 0));
      return;
    };
    mask.extend(columns.map(|column| {
      let x = near(column, (edges[0], edges[1]), (left, right));
      usize::try_from(x)
        .ok()
        .and_then(|x| line.get(x))
        .copied()
        .unwrap_or(0)
    }));
  }
}
