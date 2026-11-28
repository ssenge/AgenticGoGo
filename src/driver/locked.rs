//! The interior mutability of a `Relentless`, with the interface of `RefCell`/`Cell` and the thread safety of a lock: an
//! instance is `Send + Sync`, so a driver can hold forks of it in any Rust parallelism.
//!
//! Only one thread works on an instance at a time (`Relentless::enter` panics for a second one), so a lock is never
//! contended; it is there for the compiler's sake. As with `RefCell`, a `borrow_mut` that overlaps another borrow of the
//! same field on the same thread is a bug of the caller's: it deadlocks where `RefCell` panicked.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// A value behind a lock, read with `borrow()` and changed with `borrow_mut()`. A poisoned lock is taken over: the panic
/// that poisoned it is already on its way out of the thread that had it.
pub struct Locked<T>(RwLock<T>);

impl<T> Locked<T> {
    /// A new cell holding `value`.
    pub fn new(value: T) -> Locked<T> {
        Locked(RwLock::new(value))
    }

    /// Read access, as `RefCell::borrow`.
    pub fn borrow(&self) -> RwLockReadGuard<'_, T> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Write access, as `RefCell::borrow_mut`.
    pub fn borrow_mut(&self) -> RwLockWriteGuard<'_, T> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Write access if nobody holds the cell, as `RefCell::try_borrow_mut` (a `Drop` must not wait or panic).
    pub fn try_borrow_mut(&self) -> Option<RwLockWriteGuard<'_, T>> {
        match self.0.try_write() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

/// A small `Copy` value, read with `get()` and written with `set()`, as `Cell`.
pub struct Slot<T: Copy>(Mutex<T>);

impl<T: Copy> Slot<T> {
    /// A new slot holding `value`.
    pub fn new(value: T) -> Slot<T> {
        Slot(Mutex::new(value))
    }

    fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The value.
    pub fn get(&self) -> T {
        *self.lock()
    }

    /// Replaces the value.
    pub fn set(&self, value: T) {
        *self.lock() = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slot and a cell read back what was written, and shared reads overlap as `RefCell`'s do.
    #[test]
    fn the_cells_behave_as_refcell_and_cell_do() {
        let slot = Slot::new(1u64);
        slot.set(slot.get() + 1);
        assert_eq!(slot.get(), 2);
        let cell = Locked::new(vec![1]);
        cell.borrow_mut().push(2);
        let (a, b) = (cell.borrow(), cell.borrow());
        assert_eq!((a.len(), b.len()), (2, 2));
    }

    /// A lock a panic poisoned is still usable.
    #[test]
    fn a_poisoned_cell_is_taken_over() {
        let cell = std::sync::Arc::new(Locked::new(0));
        let c = cell.clone();
        let _ = std::thread::spawn(move || {
            let _g = c.borrow_mut();
            panic!("poison");
        })
        .join();
        *cell.borrow_mut() = 5;
        assert_eq!(*cell.borrow(), 5);
    }
}
