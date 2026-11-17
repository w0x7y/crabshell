//! A window's shared-memory buffers, up to three, reused once the
//! compositor releases them, and emptied while the window rests.

use crabshell_render::BufferAge;

/// The most buffers a window keeps.
pub(crate) const MAX_BUFFERS: usize = 3;

/// A buffer the compositor may be reading.
pub(crate) trait ShmBuffer {
  /// The size in pixels.
  fn size(&self) -> (u32, u32);
  /// Whether the compositor holds it: attached and not yet released.
  fn busy(&self) -> bool;
  /// Gives its memory back, keeping the buffer; it reads as zeros after.
  fn empty(&mut self);
}

/// One buffer, the frame it holds, and when it was last drawn.
pub(crate) struct Slot<B> {
  pub(crate) buffer: B,
  pub(crate) age: BufferAge,
  /// Counts draws, so the free buffer drawn last is picked first: it is
  /// the one with the least to catch up on.
  drawn: u64,
  /// Its memory was given back, until it is drawn again.
  emptied: bool,
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
          emptied: false,
        });
        self.slots.len() - 1
      }
      None => return None,
    };
    let slot = &mut self.slots[index];
    slot.drawn = draws;
    slot.emptied = false;
    Some(slot)
  }

  /// Once the compositor released the buffer drawn last, which holds the
  /// frame shown, gives back the memory of every other buffer it isn't
  /// reading. One that holds the buffer shown until the next commit
  /// would otherwise have the spare emptied between every two frames.
  /// The buffers are kept, so the compositor hears nothing, and one drawn
  /// again is repainted whole.
  pub(crate) fn trim(&mut self) {
    let Some(last) = self.slots.iter().max_by_key(|slot| slot.drawn) else {
      return;
    };
    if last.buffer.busy() {
      return;
    }
    let last = last.drawn;
    for slot in &mut self.slots {
      if !slot.emptied && !slot.buffer.busy() && slot.drawn != last {
        slot.buffer.empty();
        slot.age = BufferAge::default();
        slot.emptied = true;
      }
    }
  }

  /// How many buffers hold memory.
  pub(crate) fn held(&self) -> usize {
    self.slots.iter().filter(|slot| !slot.emptied).count()
  }

  /// How many buffers there are.
  #[cfg(test)]
  pub(crate) fn len(&self) -> usize {
    self.slots.len()
  }

  /// The buffer made `index`th, for tests.
  #[cfg(test)]
  pub(crate) fn slot(&self, index: usize) -> &Slot<B> {
    &self.slots[index]
  }
}
