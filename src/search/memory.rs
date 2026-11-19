//! Memory for the search tables

use std::alloc::{self, Layout};
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::{Mutex, PoisonError};
use std::thread;

/// The most a CPU (central processing unit) fetches together: two 64-byte cache lines.
pub(super) const LINE_PAIR: usize = 128;

/// A value on cache lines aligned to [`LINE_PAIR`] and padded to a whole number of them.
///
/// This guarantees the cache line will be exclusive to this value and no cache sharing occurs.
#[derive(Clone, Copy, Default, Debug)]
#[repr(C, align(128))]
pub(super) struct Aligned<T>(pub(super) T);

const _: () = assert!(align_of::<Aligned<u8>>() == LINE_PAIR);

impl<T> Deref for Aligned<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Aligned<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// Zeroed heap memory, aligned to any power of two given to [`new`](Self::new).
///
/// Above the alignment `calloc` guarantees (usually 16 bytes on x64 and 8 bytes on x86), std cannot
/// use it, so it zeroes every byte itself, touching every memory page.
/// To keep `calloc`'s fast path, where the OS zeroes pages only as they are first touched by the
/// program, memory is requested with alignment 1 and aligned by hand within the allocation's slack.
pub(super) struct ZeroedMemory {
    /// The aligned start.
    start: NonNull<u8>,
    /// The allocated memory, and its layout, to free it.
    memory: NonNull<u8>,
    layout: Layout,
}

// SAFETY: `ZeroedMemory` is the only owner of its memory and is the only one to free it.
unsafe impl Send for ZeroedMemory {}
// SAFETY: `ZeroedMemory` never reads or writes its memory, so shared references to it cannot race.
unsafe impl Sync for ZeroedMemory {}

impl ZeroedMemory {
    /// At least `bytes` zeroed bytes aligned and padded to `align` bytes.
    ///
    /// `None` if the memory allocation failed.
    ///
    /// # Panics
    ///
    /// Panics if `bytes` is 0, or `align` is not a power of 2.
    pub(super) fn new(bytes: usize, align: usize) -> Option<Self> {
        assert!(align.is_power_of_two(), "alignment must be a power of 2");
        assert!(bytes != 0, "requested memory size must be more than 0");
        let padded = bytes.checked_next_multiple_of(align)?;
        let slack = align - 1; // The most bytes that rounding the start up to `align` can require.
        let layout = Layout::from_size_align(padded.checked_add(slack)?, 1).ok()?;
        // SAFETY: the layout is not zero-sized.
        let memory = NonNull::new(unsafe { alloc::alloc_zeroed(layout) })?;
        let offset = memory.as_ptr().align_offset(align);
        // SAFETY: `offset` is at most `slack`, so `padded` bytes from it are inside the allocation.
        let start = unsafe { memory.add(offset) };
        Some(Self { start, memory, layout })
    }

    /// The aligned start.
    #[inline]
    pub(super) const fn start(&self) -> NonNull<u8> {
        self.start
    }
}

impl Drop for ZeroedMemory {
    fn drop(&mut self) {
        // SAFETY: allocated with `layout` by the global allocator, deallocated only here.
        unsafe { alloc::dealloc(self.memory.as_ptr(), self.layout) };
    }
}

/// Types whose value of all-zero bytes is valid, and zero.
///
/// # Safety
///
/// All-zero bytes must be a valid value of the type.
pub(super) unsafe trait Zeroable {}

// SAFETY: an integer of all-zero bytes is 0.
unsafe impl Zeroable for i16 {}
// SAFETY: an array of all-zero bytes is an array of all-zero elements.
unsafe impl<T: Zeroable, const N: usize> Zeroable for [T; N] {}

/// A safe, typed, zeroed table on the heap, in [`ZeroedMemory`] on cache lines of its own.
pub(super) struct ZeroedBox<T: Zeroable> {
    memory: ZeroedMemory,
    table: PhantomData<T>,
}

// SAFETY: `ZeroedBox` is the only owner of `T`.
unsafe impl<T: Zeroable + Send> Send for ZeroedBox<T> {}
// SAFETY: a shared `ZeroedBox` only provides `&T`, which `T: Sync` makes safe to share.
unsafe impl<T: Zeroable + Sync> Sync for ZeroedBox<T> {}

impl<T: Zeroable> ZeroedBox<T> {
    /// Aborts if the memory allocation failed.
    pub(super) fn new() -> Self {
        const { assert!(size_of::<T>() > 0, "table type must not be zero-sized") };
        const { assert!(align_of::<T>() <= LINE_PAIR, "alignment must be at most `LINE_PAIR`") };
        let Some(memory) = ZeroedMemory::new(size_of::<T>(), LINE_PAIR) else {
            alloc::handle_alloc_error(Layout::new::<Aligned<T>>());
        };
        Self { memory, table: PhantomData }
    }
}

impl<T: Zeroable> Deref for ZeroedBox<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: `memory` is aligned for `T` and is a valid `T` (guaranteed by `Zeroable`).
        unsafe { self.memory.start().cast::<T>().as_ref() }
    }
}

impl<T: Zeroable> DerefMut for ZeroedBox<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: `memory` is aligned for `T` and is a valid `T` (guaranteed by `Zeroable`).
        // `&mut self` makes the borrow unique, since `memory` is reachable only through `self`.
        unsafe { self.memory.start().cast::<T>().as_mut() }
    }
}

/// Runs `work` on chunks of `items`, on every available core.
///
/// Each chunk but the last holds at least `min_chunk` items.
/// If `items.len() <= min_chunk`, `work` runs on the calling thread only.
/// `min_chunk` should be large enough that its work outweighs the cost of starting a thread.
pub(super) fn for_each_chunk_in_parallel<T: Send>(
    items: &mut [T],
    min_chunk: usize,
    work: impl Fn(&mut [T]) + Sync,
) {
    let threads = thread::available_parallelism().map_or(1, NonZeroUsize::get);
    let chunk_size = items.len().div_ceil(threads).max(min_chunk).max(1);
    let chunk_count = items.len().div_ceil(chunk_size);
    let chunks = Mutex::new(items.chunks_mut(chunk_size));
    let run = || {
        loop {
            // Release the lock before running `work`.
            let next = chunks.lock().unwrap_or_else(PoisonError::into_inner).next();
            let Some(chunk) = next else { break };

            work(chunk);
        }
    };

    thread::scope(|scope| {
        // Spawn `chunk_count - 1` threads, since the calling thread also runs `work`.
        for _ in 1..chunk_count {
            if thread::Builder::new().spawn_scoped(scope, run).is_err() {
                break;
            }
        }
        run(); // The calling thread works through the chunks too.
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn zeroed_memory_is_aligned_zeroed_and_padded() {
        for (bytes, align) in [(1, 64), (100, 128), (5000, 4096), (3 << 20, 2 << 20)] {
            let memory =
                ZeroedMemory::new(bytes, align).expect("a test-sized allocation should succeed");
            assert_eq!(memory.start().as_ptr().align_offset(align), 0);
            let end = memory.start().as_ptr() as usize + bytes.next_multiple_of(align);
            assert!(end <= memory.memory.as_ptr() as usize + memory.layout.size());
        }
        let table = ZeroedBox::<[[i16; 100]; 30]>::new();
        assert!(table.as_flattened().iter().all(|&n| n == 0));
        assert_eq!((&raw const *table).align_offset(LINE_PAIR), 0);
    }

    #[test]
    fn every_chunk_is_worked_once() {
        let mut items = vec![0_u32; 1000];
        for_each_chunk_in_parallel(&mut items, 7, |chunk| chunk.iter_mut().for_each(|n| *n += 1));
        assert!(items.iter().all(|&n| n == 1));
    }

    #[test]
    fn items_that_make_one_chunk_run_on_the_calling_thread() {
        let caller = thread::current().id();
        let mut items = vec![0_u32; 10];
        for_each_chunk_in_parallel(&mut items, 100, |chunk| {
            assert_eq!(thread::current().id(), caller, "one chunk must run on the calling thread");
            for n in chunk {
                *n += 1;
            }
        });
        assert!(items.iter().all(|&n| n == 1));
    }

    #[test]
    fn no_items_run_no_work() {
        let calls = AtomicUsize::new(0);
        for_each_chunk_in_parallel(&mut [0_u32; 0], 0, |_| {
            calls.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(calls.into_inner(), 0, "an empty slice must not run `work`");
    }
}
