//! TT (transposition table): results of earlier searches, keyed by Zobrist key and shared by every
//! search thread.
//!
//! Lock-free: each entry is one atomic word, holding 17 bits of the key and
//! the result, so a reader never sees half of one write and half of another.
//! The bucket comes from the key's high bits and the 17 stored ones are its
//! lowest, so a position missing from the table matches another's entry at
//! most once in 16,384 probes (8 entries, 2^17 checks), and far less often
//! in a bucket not yet full, since a probe stops at the first empty slot.
//! The move is checked for legality before it is played,
//! so such a match costs a wrong score at worst.
//!
//! The static evaluation is not stored: computing it is cheaper than the room it would take.

use std::fmt;
use std::hint;
use std::ops::{Deref, DerefMut};
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};

use super::memory::{ZeroedMemory, for_each_chunk_in_parallel};
use crate::board::Move;

/// What a stored score says about the true score.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Bound {
    /// The true score is at most this: no move reached alpha.
    Upper = 1,
    /// The true score is at least this: a move reached beta.
    Lower = 2,
    Exact = 3,
}

impl Bound {
    const fn from_bits(bits: u64) -> Option<Self> {
        match bits {
            1 => Some(Self::Upper),
            2 => Some(Self::Lower),
            3 => Some(Self::Exact),
            _ => None,
        }
    }

    /// Whether a stored `score` with this bound settles a search with the window
    /// `alpha..beta`.
    #[inline]
    pub(crate) const fn cuts(self, score: i32, alpha: i32, beta: i32) -> bool {
        match self {
            Self::Exact => true,
            Self::Lower => score >= beta,
            Self::Upper => score <= alpha,
        }
    }
}

/// An entry read back from the table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Hit {
    /// Best move found, if any. Not checked for legality: it may be another position's.
    pub mv: Option<Move>,
    /// Relative to the stored node: see [`super::worker`] for the mate adjustment.
    pub score: i32,
    /// Plies searched below the node, 0 for quiescence search. Stored up to [`MAX_DEPTH`].
    pub depth: i32,
    pub bound: Bound,
    /// PV (principal variation) flag: whether the node was ever on the principal variation, or
    /// is now. Such lines are pruned less, since they are the ones that end up deciding the move.
    pub pv: bool,
}

// Entry layout, from the least significant bit: the key check, 17 bits; the move, 16; the
// score, 16; the depth, 7; the bound, 2; the PV flag, 1; the generation, 5.
const CHECK_BITS: u32 = 17;
const CHECK_SHIFT: u32 = 0;
const MOVE_SHIFT: u32 = CHECK_SHIFT + CHECK_BITS;
const SCORE_SHIFT: u32 = MOVE_SHIFT + 16;
const DEPTH_SHIFT: u32 = SCORE_SHIFT + 16;
const BOUND_SHIFT: u32 = DEPTH_SHIFT + 7;
const PV_SHIFT: u32 = BOUND_SHIFT + 2;
const GENERATION_SHIFT: u32 = PV_SHIFT + 1;
const CHECK_MASK: u64 = (1 << CHECK_BITS) - 1;
const DEPTH_MASK: u64 = 0x7F;
/// Deepest result stored: a deeper one is stored as this deep, which only undersells it.
const MAX_DEPTH: i32 = 0x7F;
/// Generations count searches, modulo this: enough that an entry from a search this many ago,
/// which looks fresh again, has long been replaced, even at two searches a move when pondering.
const GENERATIONS: u8 = 32;

// The generation takes the bits left.
const _: () = assert!(GENERATIONS.is_power_of_two());
const _: () = assert!(GENERATION_SHIFT + GENERATIONS.trailing_zeros() == u64::BITS);

/// The bits of `key` an entry keeps, to tell its position from others in the bucket.
#[inline]
const fn key_check(key: u64) -> u64 {
    key & CHECK_MASK
}

/// Packs one entry into 64 bits; `check` is the key's [`key_check`].
fn pack(check: u64, entry: &Hit, generation: u8) -> u64 {
    // Scores fit by construction; out of range values would corrupt neighbors.
    debug_assert!(i16::try_from(entry.score).is_ok(), "score must fit in an `i16`");
    debug_assert!(entry.depth >= 0, "depth must not be negative");
    debug_assert!(generation < GENERATIONS, "generation must be below `GENERATIONS`");
    debug_assert!(check <= CHECK_MASK, "check must fit in `CHECK_MASK`");
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked above")]
    let (score, depth) = (entry.score as i16 as u16, entry.depth.min(MAX_DEPTH) as u8);
    check << CHECK_SHIFT
        | u64::from(entry.mv.map_or(0, Move::to_bits)) << MOVE_SHIFT
        | u64::from(score) << SCORE_SHIFT
        | u64::from(depth) << DEPTH_SHIFT
        | (entry.bound as u64) << BOUND_SHIFT
        | u64::from(entry.pv) << PV_SHIFT
        | u64::from(generation) << GENERATION_SHIFT
}

/// The key check of a packed entry.
#[inline]
const fn check_of(data: u64) -> u64 {
    (data >> CHECK_SHIFT) & CHECK_MASK
}

/// The fields of a packed entry, with its generation, or `None` for an empty entry.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "each field is masked to its width, and scores are stored as `i16` bits"
)]
#[inline]
fn unpack(data: u64) -> Option<(Hit, u8)> {
    let bound = Bound::from_bits((data >> BOUND_SHIFT) & 0b11)?;
    let mv = Move::from_bits((data >> MOVE_SHIFT) as u16);
    let hit = Hit {
        mv: (mv != Move::NULL).then_some(mv),
        score: i32::from((data >> SCORE_SHIFT) as u16 as i16),
        depth: i32::from(((data >> DEPTH_SHIFT) & DEPTH_MASK) as u8),
        bound,
        pv: (data >> PV_SHIFT) & 1 != 0,
    };
    Some((hit, (data >> GENERATION_SHIFT) as u8))
}

/// One packed entry; 0 when empty.
#[derive(Default)]
struct Entry(AtomicU64);

impl Entry {
    #[inline]
    fn load(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    #[inline]
    fn store(&self, data: u64) {
        self.0.store(data, Ordering::Relaxed);
    }
}

const ENTRIES_PER_BUCKET: usize = 8;

/// The entries a key may go in: one cache line, so a probe costs one memory access.
///
/// 64 bytes is the line size of x86-64 and most ARM64 CPUs (central processing
/// units); Apple's are 128, which a 64-byte bucket still never straddles.
/// It is fixed at compile time, since `repr(align)` takes a
/// literal, and std cannot read the CPU's line size.
#[repr(C, align(64))]
struct Bucket([Entry; ENTRIES_PER_BUCKET]);

const _: () = assert!(size_of::<Bucket>() == 64);

/// Huge pages are 2 MiB on x86-64, and on ARM64 with 4 KiB base pages.
const HUGE_PAGE: usize = 2 << 20;

/// The table's buckets, in zeroed memory the allocator need not write: see [`ZeroedMemory`].
///
/// A table of a huge page or more is put on huge pages where the OS allows:
/// a random probe then rarely misses the TLB (translation lookaside buffer)
/// as well as the cache, which 4 KiB pages covering a few MiB cannot avoid.
/// On Windows, that means large pages, which only a
/// process with the right to lock pages in memory gets.
/// On Linux, it means memory that starts on a huge page and is advised to use them.
struct Buckets {
    /// The first bucket.
    first: NonNull<Bucket>,
    len: usize,
    /// Owns the buckets' memory, to give it back once they are dropped.
    #[expect(dead_code, reason = "only held, to be dropped with the buckets")]
    memory: Memory,
}

/// Where the buckets' memory came from.
enum Memory {
    Heap(ZeroedMemory),
    #[cfg(windows)]
    LargePages(large_pages::LargePages),
}

impl Memory {
    const fn start(&self) -> NonNull<u8> {
        match self {
            Self::Heap(memory) => memory.start(),
            #[cfg(windows)]
            Self::LargePages(memory) => memory.start(),
        }
    }
}

// SAFETY: `Buckets` is the only owner of its memory, which holds only atomics.
unsafe impl Send for Buckets {}
// SAFETY: the buckets hold only atomics, so shared references to them cannot race.
unsafe impl Sync for Buckets {}

impl Buckets {
    /// `count` zeroed buckets, at least one.
    ///
    /// `None` if the memory allocation failed.
    fn zeroed(count: usize) -> Option<Self> {
        let len = count.max(1);
        let bytes = len.checked_mul(size_of::<Bucket>())?;
        let memory = Self::huge_pages(bytes).or_else(|| {
            let align = if bytes >= HUGE_PAGE { HUGE_PAGE } else { align_of::<Bucket>() };
            let memory = ZeroedMemory::new(bytes, align)?;
            #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
            if bytes >= HUGE_PAGE {
                transparent_huge_pages::advise(memory.start(), bytes);
            }
            Some(Memory::Heap(memory))
        })?;
        Some(Self { first: memory.start().cast(), len, memory })
    }

    /// Windows large pages for a table of at least a huge page, if the process may use them.
    #[cfg(windows)]
    fn huge_pages(bytes: usize) -> Option<Memory> {
        (bytes >= HUGE_PAGE)
            .then(|| large_pages::LargePages::new(bytes))
            .flatten()
            .map(Memory::LargePages)
    }

    #[cfg(not(windows))]
    const fn huge_pages(_bytes: usize) -> Option<Memory> {
        None
    }
}

impl Deref for Buckets {
    type Target = [Bucket];

    #[inline]
    fn deref(&self) -> &[Bucket] {
        // SAFETY: `first` is aligned for `Bucket` and starts `len` buckets inside `memory`, which
        // lives as long as `self`. All-zero bytes are valid buckets, since they hold only atomics.
        unsafe { slice::from_raw_parts(self.first.as_ptr(), self.len) }
    }
}

impl DerefMut for Buckets {
    #[inline]
    fn deref_mut(&mut self) -> &mut [Bucket] {
        // SAFETY: `first` is aligned for `Bucket` and starts `len` valid buckets inside `memory`.
        // `&mut self` makes the borrow unique, since `memory` is reachable only through `self`.
        unsafe { slice::from_raw_parts_mut(self.first.as_ptr(), self.len) }
    }
}

/// Windows large pages, which a process may only use with the right to lock
/// pages in memory (`SeLockMemoryPrivilege`), granted by an administrator.
///
/// Without it, or without enough free physical memory in large
/// pages, nothing is allocated and the table takes ordinary pages.
#[cfg(windows)]
mod large_pages {
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::sync::OnceLock;

    type Handle = *mut c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Luid {
        low_part: u32,
        high_part: i32,
    }

    #[repr(C)]
    struct LuidAndAttributes {
        luid: Luid,
        attributes: u32,
    }

    #[repr(C)]
    struct TokenPrivileges {
        privilege_count: u32,
        privileges: [LuidAndAttributes; 1],
    }

    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const TOKEN_QUERY: u32 = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;
    const ERROR_SUCCESS: u32 = 0;
    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const MEM_LARGE_PAGES: u32 = 0x2000_0000;
    const PAGE_READWRITE: u32 = 0x04;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        safe fn GetCurrentProcess() -> Handle;
        safe fn GetLargePageMinimum() -> usize;
        safe fn GetLastError() -> u32;
        fn CloseHandle(handle: Handle) -> i32;
        fn VirtualAlloc(address: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
        fn AdjustTokenPrivileges(
            token: Handle,
            disable_all: i32,
            new_state: *const TokenPrivileges,
            buffer_length: u32,
            previous_state: *mut TokenPrivileges,
            return_length: *mut u32,
        ) -> i32;
    }

    /// Whether the process may lock pages, enabling the right if it has it. Checked once.
    fn may_lock_pages() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(enable_lock_pages)
    }

    fn enable_lock_pages() -> bool {
        let mut token: Handle = ptr::null_mut();
        let access = TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY;
        // SAFETY: `token` is a valid place for the handle.
        if unsafe { OpenProcessToken(GetCurrentProcess(), access, &raw mut token) } == 0 {
            return false;
        }
        let name: Vec<u16> = "SeLockMemoryPrivilege\0".encode_utf16().collect();
        let mut luid = Luid { low_part: 0, high_part: 0 };
        // SAFETY: `name` is NUL-terminated UTF-16, and `luid` a valid place for the result.
        let found = unsafe { LookupPrivilegeValueW(ptr::null(), name.as_ptr(), &raw mut luid) };
        let enabled = found != 0 && {
            let attributes = SE_PRIVILEGE_ENABLED;
            let privileges = TokenPrivileges {
                privilege_count: 1,
                privileges: [LuidAndAttributes { luid, attributes }],
            };
            let new_state = &raw const privileges;
            let (previous, length) = (ptr::null_mut(), ptr::null_mut());
            // SAFETY: `token` is open for adjusting, `privileges` a valid list of one, and the
            // previous state is not requested.
            let adjusted =
                unsafe { AdjustTokenPrivileges(token, 0, new_state, 0, previous, length) };
            // Without the right, it still succeeds, and only says so here.
            adjusted != 0 && GetLastError() == ERROR_SUCCESS
        };
        // SAFETY: `token` was opened above, and is closed only here.
        unsafe { CloseHandle(token) };
        enabled
    }

    /// Zeroed memory on large pages, which are aligned to their size, and given back when
    /// dropped.
    pub(super) struct LargePages(NonNull<u8>);

    // SAFETY: `LargePages` is the only owner of its memory and is the only one to free it.
    unsafe impl Send for LargePages {}
    // SAFETY: `LargePages` never reads or writes its memory,
    // so shared references to it cannot race.
    unsafe impl Sync for LargePages {}

    impl LargePages {
        /// At least `bytes` zeroed bytes on large pages.
        ///
        /// `None` if the process may not use large pages, or the memory allocation failed.
        pub(super) fn new(bytes: usize) -> Option<Self> {
            let page = GetLargePageMinimum();
            if page == 0 || !may_lock_pages() {
                return None;
            }
            let size = bytes.checked_next_multiple_of(page)?;
            let kind = MEM_RESERVE | MEM_COMMIT | MEM_LARGE_PAGES;
            // SAFETY: new memory, touching nothing of the program's.
            let memory = unsafe { VirtualAlloc(ptr::null_mut(), size, kind, PAGE_READWRITE) };
            NonNull::new(memory.cast()).map(Self)
        }

        pub(super) const fn start(&self) -> NonNull<u8> {
            self.0
        }
    }

    impl Drop for LargePages {
        fn drop(&mut self) {
            // SAFETY: allocated by `VirtualAlloc`, released only here.
            // `MEM_RELEASE` takes a size of 0 and frees the whole allocation.
            unsafe { VirtualFree(self.0.as_ptr().cast(), 0, MEM_RELEASE) };
        }
    }
}

/// Linux's transparent huge pages, which by default back only the memory a program asks for,
/// as `madvise` does here. Configured to back all memory, they need nothing.
#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
mod transparent_huge_pages {
    use std::ffi::{c_int, c_void};
    use std::ptr::NonNull;

    use super::HUGE_PAGE;

    const MADV_HUGEPAGE: c_int = 14;

    unsafe extern "C" {
        fn madvise(address: *mut c_void, length: usize, advice: c_int) -> c_int;
    }

    /// Asks for huge pages for the `bytes` from `start`, which is on a huge page.
    pub(super) fn advise(start: NonNull<u8>, bytes: usize) {
        // Whole huge pages, all within the range.
        let length = bytes - bytes % HUGE_PAGE;
        // SAFETY: advice about memory the caller owns, page-aligned, changing none of it.
        // A failure only leaves ordinary pages.
        unsafe { madvise(start.as_ptr().cast(), length, MADV_HUGEPAGE) };
    }
}

/// Fixed-size hash table of search results.
///
/// Probing and storing take `&self`, so the threads of a search share it; starting a
/// search or clearing the table takes `&mut self`, so neither happens while one runs.
///
/// Entries are only stored during a search, and slots
/// only emptied by [`Self::clear`], between searches.
/// So a bucket's slots fill in order: past an empty one, every
/// slot is empty, and a probe or store stops at the first.
pub struct TranspositionTable {
    buckets: Buckets,
    /// Counts searches, so entries from earlier ones are replaced first.
    generation: u8,
    /// Whether no search has run since the table was made or cleared, so it is still empty and
    /// clearing it need not write, or even touch, its memory.
    untouched: bool,
}

impl TranspositionTable {
    /// Default size in MiB (mebibytes, 2^20 bytes).
    pub const DEFAULT_MIB: usize = 16;

    /// A table of approximately `mebibytes` MiB, at least one bucket.
    ///
    /// # Panics
    ///
    /// Panics if the memory allocation failed; see [`Self::try_new`].
    #[must_use]
    pub fn new(mebibytes: usize) -> Self {
        Self::try_new(mebibytes).expect("memory for the transposition table should be available")
    }

    /// Like [`Self::new`], but `None` instead of panicking if the memory allocation failed.
    ///
    /// The memory comes zeroed from the allocator, which an empty table is.
    /// The OS hands out zeroed pages as they are first touched, so even a huge
    /// table is ready at once, rather than after writing every byte on one core.
    /// Windows large pages, the exception, are all found and zeroed up front.
    ///
    /// An OS that overcommits memory, as Linux does by default, may grant more than
    /// it has: then this succeeds and the process is killed once the table fills.
    /// Nothing in std can tell beforehand.
    #[must_use]
    pub fn try_new(mebibytes: usize) -> Option<Self> {
        let count = mebibytes.saturating_mul(1 << 20) / size_of::<Bucket>();
        let buckets = Buckets::zeroed(count)?;
        Some(Self { buckets, generation: 0, untouched: true })
    }

    /// Forgets every entry, on every core for a large table,
    /// since zeroing gigabytes on one takes seconds.
    ///
    /// It uses plain `memset`s, which write whole cache lines without
    /// reading them first, unlike atomic stores of each entry.
    /// A table no search has used since it was made or cleared is left alone, its memory untouched.
    pub fn clear(&mut self) {
        const MIN_BUCKETS_PER_THREAD: usize = 1 << 16;
        if self.untouched {
            return;
        }
        for_each_chunk_in_parallel(&mut self.buckets, MIN_BUCKETS_PER_THREAD, |buckets| {
            // SAFETY: `buckets` is borrowed uniquely, and all-zero bytes are empty buckets of
            // atomics.
            unsafe { ptr::write_bytes(buckets.as_mut_ptr(), 0, buckets.len()) };
        });
        self.untouched = true;
    }

    /// Marks the start of a search, before any entry is stored, so entries from earlier
    /// searches are replaced first.
    pub fn new_search(&mut self) {
        self.generation = (self.generation + 1) % GENERATIONS;
        self.untouched = false;
    }

    #[inline]
    const fn generation(&self) -> u8 {
        self.generation
    }

    /// Maps the key over the buckets with a multiply, which spreads keys as well as a modulo
    /// without the division. It reads the key's high bits, leaving the low ones to the check.
    #[inline]
    fn bucket(&self, key: u64) -> &Bucket {
        let len = self.buckets.len();
        #[expect(clippy::cast_possible_truncation, reason = "the product's high half is < len")]
        let index = ((u128::from(key) * len as u128) >> 64) as usize;
        // SAFETY: `key < 2^64`, so `key * len < 2^64 * len`, whose high half is below `len`.
        unsafe { hint::assert_unchecked(index < len) };
        &self.buckets[index]
    }

    /// Starts loading the bucket of `key` into the cache, ahead of a [`Self::probe`].
    ///
    /// A wrong key only wastes the load.
    #[inline]
    pub(crate) fn prefetch(&self, key: u64) {
        let address = ptr::from_ref(self.bucket(key));
        #[cfg(target_arch = "x86_64")]
        {
            use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
            // SAFETY: SSE is part of x86-64, and a prefetch never faults or changes memory.
            unsafe { _mm_prefetch::<_MM_HINT_T0>(address.cast::<i8>()) };
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: a prefetch never faults or changes memory, and touches no register but its
        // input.
        unsafe {
            std::arch::asm!(
                "prfm pldl1keep, [{address}]",
                address = in(reg) address,
                options(nostack, readonly, preserves_flags)
            );
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let _ = address;
    }

    /// The entry stored for `key`, if any.
    #[inline]
    pub(crate) fn probe(&self, key: u64) -> Option<Hit> {
        let check = key_check(key);
        for entry in &self.bucket(key).0 {
            match entry.load() {
                // Slots fill in order: the rest are empty too.
                0 => return None,
                data if check_of(data) == check => return unpack(data).map(|(hit, _)| hit),
                _ => {}
            }
        }
        None
    }

    /// Stores a search result for `key`, replacing the entry for the same position, else the
    /// first empty one, else the least useful one: the shallowest, counting older searches as
    /// shallower. Only during a search, after [`Self::new_search`].
    pub(crate) fn store(&self, key: u64, entry: Hit) {
        debug_assert!(!self.untouched, "must only store during a search, after `new_search`");
        let check = key_check(key);
        let generation = self.generation();
        let bucket = self.bucket(key);
        let mut victim = &bucket.0[0];
        let mut victim_value = i32::MAX;
        for slot in &bucket.0 {
            let data = slot.load();
            let Some((old, old_generation)) = unpack(data) else {
                // Empty: nothing is less useful, and the slots after it are empty too, so the
                // position is not in the bucket.
                slot.store(pack(check, &entry, generation));
                return;
            };
            if check_of(data) == check {
                // A much shallower non-exact result from this search is not worth the deeper one,
                // but a best move where it has none still is.
                let entry = if entry.bound != Bound::Exact
                    && entry.depth + 4 < old.depth
                    && old_generation == generation
                {
                    match (entry.mv, old.mv) {
                        (Some(mv), None) => Hit { mv: Some(mv), ..old },
                        _ => return,
                    }
                } else {
                    // Keep the best move if the new result has none.
                    Hit { mv: entry.mv.or(old.mv), ..entry }
                };
                slot.store(pack(check, &entry, generation));
                return;
            }
            let age = i32::from((GENERATIONS + generation - old_generation) % GENERATIONS);
            let value = old.depth - 8 * age;
            if value < victim_value {
                victim = slot;
                victim_value = value;
            }
        }
        victim.store(pack(check, &entry, generation));
    }

    /// Per mille of the table filled by the current search, from a sample, as UCI's `hashfull`.
    #[must_use]
    pub fn hashfull(&self) -> usize {
        let generation = self.generation();
        let sample = self.buckets.iter().take(1000 / ENTRIES_PER_BUCKET);
        let entries = sample.len() * ENTRIES_PER_BUCKET;
        let used = sample
            .flat_map(|bucket| &bucket.0)
            .filter(|entry| unpack(entry.load()).is_some_and(|(_, g)| g == generation))
            .count();
        used * 1000 / entries
    }
}

impl fmt::Debug for TranspositionTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TranspositionTable")
            .field("buckets", &self.buckets.len())
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{MoveKind, Square};

    fn hit(depth: i32, score: i32, bound: Bound) -> Hit {
        let mv = Move::new(Square::E2, Square::E4, MoveKind::DoublePush);
        Hit { mv: Some(mv), score, depth, bound, pv: depth % 2 == 0 }
    }

    /// A table of `mebibytes` MiB, with a search started.
    fn searching(mebibytes: usize) -> TranspositionTable {
        let mut tt = TranspositionTable::new(mebibytes);
        tt.new_search();
        tt
    }

    #[test]
    fn stores_and_probes() {
        let mut tt = searching(1);
        assert_eq!(tt.probe(42), None);
        let entry = hit(7, -31_900, Bound::Lower);
        tt.store(42, entry);
        assert_eq!(tt.probe(42), Some(entry));
        assert_eq!(tt.probe(43), None);
        tt.clear();
        assert_eq!(tt.probe(42), None);
    }

    #[test]
    fn packs_extreme_values() {
        for (score, depth, pv) in
            [(i32::from(i16::MIN), 0, true), (i32::from(i16::MAX), MAX_DEPTH, false), (0, 1, true)]
        {
            let entry = Hit { mv: None, score, depth, bound: Bound::Exact, pv };
            let generation = GENERATIONS - 1;
            let data = pack(CHECK_MASK, &entry, generation);
            assert_eq!(unpack(data), Some((entry, generation)));
            assert_eq!(check_of(data), CHECK_MASK);
        }
        // Deeper results are stored as deep as fits.
        let deep = Hit { mv: None, score: 0, depth: 200, bound: Bound::Lower, pv: false };
        assert_eq!(unpack(pack(0, &deep, 0)).map(|(hit, _)| hit.depth), Some(MAX_DEPTH));
    }

    #[test]
    fn same_position_keeps_its_move_and_deeper_result() {
        let tt = searching(1);
        tt.store(1, hit(10, 50, Bound::Exact));
        // Far shallower and not exact: kept out.
        tt.store(1, hit(2, 0, Bound::Upper));
        assert_eq!(tt.probe(1).map(|h| h.depth), Some(10));
        // A result without a move keeps the old one.
        tt.store(1, Hit { mv: None, ..hit(12, 70, Bound::Lower) });
        assert_eq!(tt.probe(1), Some(hit(12, 70, Bound::Lower)));
        // Far shallower, but with a move where the entry has none: only the move is taken.
        let without_move = Hit { mv: None, ..hit(12, 70, Bound::Upper) };
        tt.store(2, without_move);
        tt.store(2, hit(3, -40, Bound::Upper));
        assert_eq!(tt.probe(2), Some(Hit { mv: hit(3, -40, Bound::Upper).mv, ..without_move }));
    }

    #[test]
    fn a_full_bucket_replaces_the_shallowest_or_oldest() {
        let mut tt = searching(0);
        assert_eq!(tt.buckets.len(), 1, "a 0 MiB table must have one bucket");
        let full = u64::try_from(ENTRIES_PER_BUCKET).expect("small value should fit");
        for key in 1..=full {
            tt.store(
                key,
                hit(i32::try_from(key).expect("small value should fit") * 10, 0, Bound::Exact),
            );
        }
        tt.store(full + 1, hit(1, 0, Bound::Exact));
        assert_eq!(tt.probe(1), None, "the shallowest entry must be replaced");
        assert!((2..=full + 1).all(|key| tt.probe(key).is_some()));
        // Two searches later, the shallow old entry counts for less than a fresh one, and the
        // deep old ones still count for more.
        tt.new_search();
        tt.new_search();
        tt.store(full + 2, hit(1, 0, Bound::Exact));
        assert_eq!(tt.probe(full + 1), None);
        assert!((2..=full).chain([full + 2]).all(|key| tt.probe(key).is_some()));
    }

    #[test]
    fn a_bucket_fills_in_order_and_keeps_one_entry_per_position() {
        let tt = searching(0);
        for key in 1..=3 {
            tt.store(key, hit(5, 0, Bound::Exact));
        }
        tt.store(2, hit(9, 0, Bound::Exact));
        let checks: Vec<u64> = tt.buckets[0].0.iter().map(|entry| check_of(entry.load())).collect();
        assert_eq!(checks, [1, 2, 3, 0, 0, 0, 0, 0]);
        assert_eq!(tt.probe(2).map(|h| h.depth), Some(9));
        assert_eq!(tt.probe(4), None);
    }

    #[test]
    fn entries_from_eight_searches_ago_still_count_as_old() {
        let mut tt = searching(0);
        for key in 1..=4 {
            tt.store(key, hit(20, 0, Bound::Exact));
        }
        // Eight searches later, as four moves of pondering take.
        for _ in 0..8 {
            tt.new_search();
        }
        for key in 5..=8 {
            tt.store(key, hit(5, 0, Bound::Exact));
        }
        assert_eq!(tt.hashfull(), 500);
        // The old deep entries go before the fresh shallow ones.
        tt.store(9, hit(1, 0, Bound::Exact));
        assert_eq!(tt.probe(1), None);
        assert!((5..=9).all(|key| tt.probe(key).is_some()));
        assert_eq!(tt.hashfull(), 625);
    }

    #[test]
    fn clearing_an_untouched_table_leaves_it_empty() {
        let mut tt = TranspositionTable::new(1);
        tt.clear();
        assert!(tt.buckets.iter().flat_map(|bucket| &bucket.0).all(|entry| entry.load() == 0));
        tt.new_search();
        tt.store(5, hit(3, 0, Bound::Lower));
        tt.clear();
        assert_eq!(tt.probe(5), None);
    }

    #[test]
    fn keys_differing_only_in_high_bits_share_no_entry_in_a_big_table() {
        // Same low bits, so the same check: told apart by their buckets.
        let tt = searching(1);
        let (a, b) = (0x0000_0000_0000_1234, 0x8000_0000_0000_1234);
        tt.store(a, hit(5, 10, Bound::Exact));
        assert_eq!(tt.probe(b), None);
    }

    #[test]
    fn clearing_a_big_table_empties_every_part_and_keeps_the_generation() {
        // Large enough to be split between threads.
        let mut tt = searching(16);
        let keys: Vec<u64> =
            (0..10_000u64).map(|n| n.wrapping_mul(0x9E37_79B9_7F4A_7C15)).collect();
        for &key in &keys {
            tt.store(key, hit(1, 0, Bound::Exact));
        }
        tt.clear();
        assert!(keys.iter().all(|&key| tt.probe(key).is_none()));
        assert!(tt.buckets.iter().flat_map(|bucket| &bucket.0).all(|entry| entry.load() == 0));
        assert_eq!(tt.generation(), 1);
    }

    #[test]
    fn hashfull_counts_this_search() {
        let mut tt = searching(1);
        assert_eq!(tt.hashfull(), 0);
        for key in 0..100_000u64 {
            tt.store(key.wrapping_mul(0x9E37_79B9_7F4A_7C15), hit(1, 0, Bound::Exact));
        }
        assert!(tt.hashfull() > 500, "{}", tt.hashfull());
        tt.new_search();
        assert_eq!(tt.hashfull(), 0);
    }
}
