//! Choosing a window's buffer: reuse a released one, keep at most three.

use std::cell::Cell;
use std::rc::Rc;

use crate::buffers::{Buffers, MAX_BUFFERS, ShmBuffer};

/// A buffer whose busy state the test sets.
struct Fake {
  size: (u32, u32),
  busy: Rc<Cell<bool>>,
  number: usize,
}

impl ShmBuffer for Fake {
  fn size(&self) -> (u32, u32) {
    self.size
  }

  fn busy(&self) -> bool {
    self.busy.get()
  }
}

/// Buffers that make numbered fakes of `size`, each busy once drawn,
/// as an attached buffer is until released.
struct Harness {
  buffers: Buffers<Fake>,
  made: Vec<Rc<Cell<bool>>>,
}

impl Harness {
  fn new() -> Harness {
    Harness {
      buffers: Buffers::new(),
      made: Vec::new(),
    }
  }

  /// Picks a buffer of `size` and marks it attached; returns its number.
  fn draw(&mut self, size: (u32, u32)) -> Option<usize> {
    let made = &mut self.made;
    let slot = self.buffers.next(size, || {
      let busy = Rc::new(Cell::new(false));
      made.push(Rc::clone(&busy));
      Some(Fake {
        size,
        busy,
        number: made.len() - 1,
      })
    })?;
    slot.buffer.busy.set(true);
    Some(slot.buffer.number)
  }

  fn release(&self, number: usize) {
    self.made[number].set(false);
  }
}

#[test]
fn a_released_buffer_is_reused() {
  let mut h = Harness::new();
  assert_eq!(h.draw((10, 10)), Some(0));
  h.release(0);
  assert_eq!(h.draw((10, 10)), Some(0));
  assert_eq!(h.buffers.len(), 1);
}

#[test]
fn at_most_three_are_made_while_all_are_busy() {
  let mut h = Harness::new();
  assert_eq!(h.draw((10, 10)), Some(0));
  assert_eq!(h.draw((10, 10)), Some(1));
  assert_eq!(h.draw((10, 10)), Some(2));
  assert_eq!(h.draw((10, 10)), None, "all three busy");
  assert_eq!(h.buffers.len(), MAX_BUFFERS);
  h.release(1);
  assert_eq!(h.draw((10, 10)), Some(1));
}

#[test]
fn the_free_buffer_drawn_last_is_picked_first() {
  let mut h = Harness::new();
  for _ in 0..3 {
    h.draw((10, 10));
  }
  // Buffer 2 holds the newest frame, so it has the least to repaint.
  for number in 0..3 {
    h.release(number);
  }
  assert_eq!(h.draw((10, 10)), Some(2));
  // Then 1 and 0, the only free ones: now 0 holds the newest frame,
  // though it isn't the last made.
  assert_eq!(h.draw((10, 10)), Some(1));
  assert_eq!(h.draw((10, 10)), Some(0));
  for number in 0..3 {
    h.release(number);
  }
  assert_eq!(h.draw((10, 10)), Some(0));
}

#[test]
fn buffers_of_another_size_are_dropped() {
  let mut h = Harness::new();
  h.draw((10, 10));
  h.draw((10, 10));
  assert_eq!(h.draw((20, 10)), Some(2));
  assert_eq!(h.buffers.len(), 1);
}

#[test]
fn a_buffer_that_cant_be_made_draws_nothing() {
  let mut buffers: Buffers<Fake> = Buffers::new();
  assert!(buffers.next((10, 10), || None).is_none());
  assert_eq!(buffers.len(), 0);
}
