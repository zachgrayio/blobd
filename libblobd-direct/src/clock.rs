use crate::object::Object;
use crate::object::WeakObject;
use crate::objects::ObjectId;
use parking_lot::Mutex;

struct Entry {
  id: ObjectId,
  object: WeakObject,
}

#[derive(Default)]
pub(crate) struct Clock {
  inner: Mutex<ClockInner>,
}

#[derive(Default)]
struct ClockInner {
  ring: Vec<Entry>,
  hand: usize,
}

impl Clock {
  pub fn len(&self) -> usize {
    self.inner.lock().ring.len()
  }

  pub fn register(&self, object: &Object) {
    let mut inner = self.inner.lock();
    object.set_clock_slot(inner.ring.len());
    inner.ring.push(Entry {
      id: object.id(),
      object: object.downgrade(),
    });
  }

  pub fn remove(&self, object: &Object) {
    let mut inner = self.inner.lock();
    let slot = object.clock_slot();
    if slot >= inner.ring.len() || inner.ring[slot].id != object.id() {
      return;
    }
    inner.ring.swap_remove(slot);
    object.set_clock_slot(usize::MAX);
    if slot < inner.ring.len() {
      if let Some(moved) = inner.ring[slot].object.upgrade() {
        moved.set_clock_slot(slot);
      }
    }
    if !inner.ring.is_empty() {
      inner.hand %= inner.ring.len();
    } else {
      inner.hand = 0;
    }
  }

  pub fn candidate(&self) -> Option<Object> {
    let mut inner = self.inner.lock();
    let attempts = inner.ring.len().saturating_mul(2);
    for _ in 0..attempts {
      let slot = inner.hand;
      inner.hand = (inner.hand + 1) % inner.ring.len();
      let entry = &inner.ring[slot];
      if let Some(object) = entry.object.upgrade() {
        if object.id() == entry.id && !object.is_pinned() && !object.take_reference() {
          return Some(object);
        }
      }
    }
    None
  }
}

#[cfg(test)]
mod tests {
  use super::Clock;
  use crate::object::Object;
  use crate::object::ObjectMetadata;
  use crate::object::ObjectState;
  use chrono::Utc;

  fn object(id: u128) -> Object {
    Object::new(
      id,
      ObjectState::Committed,
      ObjectMetadata {
        size: 0,
        created: Utc::now(),
        key: id.to_be_bytes().to_vec(),
        lpage_dev_offsets: Vec::new(),
        tail_page_dev_offsets: Vec::new(),
      },
      0,
    )
  }

  #[test]
  fn empty_clock_has_no_candidate() {
    let clock = Clock::default();
    assert_eq!(clock.len(), 0);
    assert!(clock.candidate().is_none());
    assert_eq!(clock.len(), 0);
  }

  #[test]
  fn candidate_advances_hand_in_registration_order() {
    let clock = Clock::default();
    let first = object(1);
    let second = object(2);
    let third = object(3);
    clock.register(&first);
    clock.register(&second);
    clock.register(&third);

    assert_eq!(clock.len(), 3);
    assert_eq!(clock.candidate().unwrap().id(), 1);
    assert_eq!(clock.candidate().unwrap().id(), 2);
    assert_eq!(clock.candidate().unwrap().id(), 3);
    assert_eq!(clock.candidate().unwrap().id(), 1);
  }

  #[test]
  fn referenced_object_gets_one_second_chance() {
    let clock = Clock::default();
    let first = object(1);
    let second = object(2);
    let third = object(3);
    clock.register(&first);
    clock.register(&second);
    clock.register(&third);

    first.touch();
    assert_eq!(clock.candidate().unwrap().id(), 2);
    assert!(!first.take_reference());
    assert_eq!(clock.candidate().unwrap().id(), 3);
    assert_eq!(clock.candidate().unwrap().id(), 1);
  }

  #[test]
  fn all_referenced_objects_are_cleared_before_selecting_oldest() {
    let clock = Clock::default();
    let objects = [object(1), object(2), object(3)];
    for object in &objects {
      clock.register(object);
      object.touch();
    }

    assert_eq!(clock.candidate().unwrap().id(), 1);
    assert!(objects.iter().all(|object| !object.take_reference()));
    assert_eq!(clock.candidate().unwrap().id(), 2);
  }

  #[test]
  fn removing_entries_repairs_swapped_slots_and_hand() {
    let clock = Clock::default();
    let first = object(1);
    let removed = object(2);
    let moved = object(3);
    clock.register(&first);
    clock.register(&removed);
    clock.register(&moved);
    assert_eq!(first.clock_slot(), 0);
    assert_eq!(removed.clock_slot(), 1);
    assert_eq!(moved.clock_slot(), 2);

    assert_eq!(clock.candidate().unwrap().id(), 1); // hand now points at slot 1
    clock.remove(&removed);
    assert_eq!(removed.clock_slot(), usize::MAX);
    assert_eq!(moved.clock_slot(), 1);
    assert_eq!(clock.len(), 2);
    assert_eq!(clock.candidate().unwrap().id(), 3);
    assert_eq!(clock.candidate().unwrap().id(), 1);

    clock.remove(&first);
    assert_eq!(first.clock_slot(), usize::MAX);
    assert_eq!(moved.clock_slot(), 0);
    assert_eq!(clock.candidate().unwrap().id(), 3);
    clock.remove(&moved);
    assert_eq!(clock.len(), 0);
    assert!(clock.candidate().is_none());
  }

  #[test]
  fn removing_last_entry_and_repeated_removal_are_safe() {
    let clock = Clock::default();
    let first = object(1);
    let second = object(2);
    clock.register(&first);
    clock.register(&second);
    assert_eq!(clock.candidate().unwrap().id(), 1);
    assert_eq!(clock.candidate().unwrap().id(), 2);

    clock.remove(&second);
    clock.remove(&second);
    assert_eq!(clock.len(), 1);
    assert_eq!(clock.candidate().unwrap().id(), 1);
    clock.remove(&first);
    assert_eq!(clock.len(), 0);
    assert!(clock.candidate().is_none());
  }

  #[test]
  fn dead_weak_entries_are_skipped() {
    let clock = Clock::default();
    let expired = object(1);
    clock.register(&expired);
    drop(expired);
    assert!(clock.candidate().is_none());

    let live = object(2);
    clock.register(&live);
    assert_eq!(clock.candidate().unwrap().id(), 2);
  }

  #[test]
  fn removing_an_unregistered_object_does_not_change_the_ring() {
    let clock = Clock::default();
    let registered = object(1);
    let unregistered = object(2);
    clock.register(&registered);

    clock.remove(&unregistered);

    assert_eq!(clock.len(), 1);
    assert_eq!(registered.clock_slot(), 0);
    assert_eq!(clock.candidate().unwrap().id(), 1);
  }

  #[test]
  fn pinned_objects_are_skipped_and_unpinned_objects_remain_eligible() {
    let clock = Clock::default();
    let first = object(1);
    let second = object(2);
    let third = object(3);
    clock.register(&first);
    clock.register(&second);
    clock.register(&third);

    let first_pin = first.try_pin().unwrap();
    let second_pin = second.try_pin().unwrap();
    assert_eq!(clock.candidate().unwrap().id(), 3);
    let third_pin = third.try_pin().unwrap();
    assert!(clock.candidate().is_none());

    drop(second_pin);
    assert_eq!(clock.candidate().unwrap().id(), 2);
    drop((first_pin, third_pin));
  }

  #[test]
  fn a_pinned_referenced_object_keeps_its_second_chance() {
    let clock = Clock::default();
    let first = object(1);
    let second = object(2);
    clock.register(&first);
    clock.register(&second);
    first.touch();
    let pin = first.try_pin().unwrap();

    assert_eq!(clock.candidate().unwrap().id(), 2);
    drop(pin);
    // The pinned pass did not consume the reference bit.
    assert_eq!(clock.candidate().unwrap().id(), 2);
    assert_eq!(clock.candidate().unwrap().id(), 1);
  }
}
