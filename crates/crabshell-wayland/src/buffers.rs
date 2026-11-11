//! A window's two or three shared-memory buffers, reused once the
//! compositor releases them.

use crabshell_render::BufferAge;

/// The most buffers a window keeps.
pub(crate) const MAX_BUFFERS: usize = 3;

/// A buffer the compositor may be reading.
pub(crate) trait ShmBuffer {
  /// The size in pixels.
  fn size(&self) -> (u32, u32);
  /// Whether the compositor holds it: attached and not yet released.
  fn busy(&self) -> bool;
}

/// One buffer, the frame it holds, and when it was last drawn.
pub(crate) struct Slot<B> {
  pub(crate) buffer: B,
  pub(crate) age: BufferAge,
  /// Counts draws, so the free buffer drawn last is picked first: it is
  /// the one with the least to catch up on.
  drawn: u64,
}

/// A window's buffers.
pub(crate) struct Buffers<B> {
  slots: Vec<Slot<B>>,
  draws: u64,
}

impl<B: ShmBuffer> Buffers<B> {
  pub(crate) fn new() -> Buffers<B> {
    Buffers {
      slots: Vec::new(),
      draws: 0,
    }
  }

  /// A buffer of `size` to draw into: the free one drawn most recently,
  /// or a new one from `create` while there are fewer than three. Buffers
  /// of another size are dropped first; the compositor frees each once
  /// it lets go. `None` if all three are busy or `create` fails.
  pub(crate) fn next(
    &mut self,
    size: (u32, u32),
    create: impl FnOnce() -> Option<B>,
  ) -> Option<&mut Slot<B>> {
    self.slots.retain(|slot| slot.buffer.size() == size);
    self.draws += 1;
    let draws = self.draws;
    let free = self
      .slots
      .iter()
      .enumerate()
      .filter(|(_, slot)| !slot.buffer.busy())
      .max_by_key(|(_, slot)| slot.drawn)
      .map(|(index, _)| index);
    let index = match free {
      Some(index) => index,
      None if self.slots.len() < MAX_BUFFERS => {
        self.slots.push(Slot {
          buffer: create()?,
          age: BufferAge::default(),
          drawn: 0,
        });
        self.slots.len() - 1
      }
      None => return None,
    };
    let slot = &mut self.slots[index];
    slot.drawn = draws;
    Some(slot)
  }

  /// How many buffers there are.
  #[cfg(test)]
  pub(crate) fn len(&self) -> usize {
    self.slots.len()
  }
}
