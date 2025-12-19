use lite_alloc::reset_heap;
use lite_alloc::single_threaded::FreeListAllocator;
use std::alloc::{GlobalAlloc, Layout};
use std::sync::{Mutex, MutexGuard};

// Global lock to serialize tests because the allocator uses global mutable state
// 全局锁用于序列化测试，因为分配器使用全局可变状态
static TEST_MUTEX: Mutex<()> = Mutex::new(());

struct SafeAllocator {
    inner: FreeListAllocator,
    _guard: MutexGuard<'static, ()>,
}

impl SafeAllocator {
    fn new() -> Self {
        let guard = TEST_MUTEX.lock().unwrap();
        unsafe {
            // Reset allocator generic state and mock memory
            // 重置分配器通用状态和模拟内存
            FreeListAllocator::reset();
            reset_heap();
            Self {
                inner: FreeListAllocator::new(),
                _guard: guard,
            }
        }
    }

    fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { self.inner.alloc(layout) }
    }

    fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    #[cfg(feature = "realloc")]
    fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }
}

impl Drop for SafeAllocator {
    fn drop(&mut self) {
        unsafe {
            FreeListAllocator::reset();
            reset_heap();
        }
    }
}

#[test]
fn test_small_allocations_min_size() {
    let allocator = SafeAllocator::new();
    // Request minimal size (1 byte), align 1
    // Layout size must be > 0.
    let layout = Layout::from_size_align(1, 1).unwrap();
    let ptr = allocator.alloc(layout);

    assert!(!ptr.is_null());
    // Should align to 16
    assert_eq!(ptr as usize % 16, 0);

    // Write to it to ensure it's valid
    unsafe { ptr.write(0xFF) };

    allocator.dealloc(ptr, layout);
}

#[test]
fn test_alignment_upgrade_force_16() {
    let allocator = SafeAllocator::new();
    // Request 8 byte alignment
    let layout = Layout::from_size_align(8, 8).unwrap();
    let ptr = allocator.alloc(layout);

    assert!(!ptr.is_null());
    assert_eq!(ptr as usize % 16, 0); // Must be upgraded to 16

    allocator.dealloc(ptr, layout);
}

#[test]
fn test_unsupported_alignment_large() {
    let allocator = SafeAllocator::new();
    // Request 32 byte alignment (not supported by simple implementation of FreeListAllocator)
    let layout = Layout::from_size_align(32, 32).unwrap();
    let ptr = allocator.alloc(layout);

    // Should fail
    assert!(ptr.is_null());
}

#[test]
fn test_split_block_behavior() {
    let allocator = SafeAllocator::new();
    // Alloc 128 bytes
    let layout_large = Layout::from_size_align(128, 16).unwrap();
    let ptr1 = allocator.alloc(layout_large);
    assert!(!ptr1.is_null());

    // Free it to put it into the list
    allocator.dealloc(ptr1, layout_large);

    // Alloc 112 bytes.
    // Remaining = 128 - 112 = 16 bytes.
    // Assuming internal NODE_SIZE <= 16, this should trigger a split if remaining >= NODE_SIZE.
    // If it splits, it returns a pointer.
    let layout_small = Layout::from_size_align(112, 16).unwrap();
    let ptr2 = allocator.alloc(layout_small);
    assert!(!ptr2.is_null());

    // Determine if it was the same block split
    // If it split, ptr2 should be higher address (returned 'remaining' part) or lower?
    // Looking at source:
    // if remaining >= NODE_SIZE { (*node).size = remaining; return (node as *mut u8).add(remaining); }
    // It keeps the bottom part as free node, and returns the top part.
    // So ptr2 should be ptr1 + 16.
    // Note: ptr1 is now invalid, but we use its address value for comparison.
    // Wait, ptr1 was the address of the 128 byte block.
    // Node is at ptr1.
    // Node reduced to size 16.
    // Returns ptr1 + 16.

    let diff = unsafe { ptr2.offset_from(ptr1) };

    // If NODE_SIZE is indeed <= 16, it splits.
    if std::mem::size_of::<usize>() * 2 <= 16 {
        // likely 16 on 64-bit, 8 on 32-bit.
        // If it splits:
        assert_eq!(diff, 16);

        // Alloc the remaining 16
        let layout_tiny = Layout::from_size_align(16, 16).unwrap();
        let ptr3 = allocator.alloc(layout_tiny);
        assert_eq!(ptr3, ptr1); // User gets the bottom part now
    } else {
        // If NODE_SIZE > 16 (unlikely for this simple struct), it wouldn't split
        // And ptr2 would probably be ptr1 (taking whole block)
        // OR it might have matched another block, but we only have one free block.
    }
}

#[test]
fn test_coalescing_fragmentation_random_free() {
    let allocator = SafeAllocator::new();
    let count = 10;
    let layout = Layout::from_size_align(64, 16).unwrap();
    let mut ptrs = vec![std::ptr::null_mut(); count];

    // Alloc contiguous blocks
    for i in 0..count {
        ptrs[i] = allocator.alloc(layout);
        assert!(!ptrs[i].is_null());
        if i > 0 {
            // Should be adjacent high-to-low or low-to-high?
            // FreeList grows by searching list.
            // Ideally we get sequential addresses if we just alloc.
            // Usually low to high if we just grow memory and slice it.
            // But let's not assume order, just that they don't overlap.
        }
    }

    // Determine the extent
    let _min_ptr = ptrs.iter().min().unwrap();
    let _max_ptr = ptrs.iter().max().unwrap();

    // Free in random order (evens then odds)
    // Evens: 0, 2, 4... - create holes
    for i in (0..count).step_by(2) {
        allocator.dealloc(ptrs[i], layout);
    }
    // Odds: 1, 3, 5... - fill holes and coalesce
    for i in (1..count).step_by(2) {
        allocator.dealloc(ptrs[i], layout);
    }

    // Now all freed and should be coalesced into one big chunk (or chunks if pages were disjoint).
    // If they were allocated from one growth, they should be one chunk.

    // Alloc total size check
    let total_size = count * 64;
    let layout_total = Layout::from_size_align(total_size, 16).unwrap();
    let ptr_all = allocator.alloc(layout_total);

    // If coalescing works perfect, this should succeed without growing memory (assuming we didn't fragmentation excessively)
    // Actually, if we just alloc 10 chunks, they are likely contiguous.
    // Freeing them all should merge them back.
    assert!(!ptr_all.is_null());

    allocator.dealloc(ptr_all, layout_total);
}

#[test]
fn test_alloc_large_grows_memory() {
    let allocator = SafeAllocator::new();
    // 64KB page size. Alloc 100KB.
    let layout = Layout::from_size_align(100 * 1024, 16).unwrap();
    let ptr = allocator.alloc(layout);

    assert!(!ptr.is_null());

    // Write end (bounds check)
    unsafe {
        ptr.add(100 * 1024 - 1).write(0xAA);
    }

    allocator.dealloc(ptr, layout);
}

#[test]
fn test_double_free_corruption_check() {
    // Note: Double free is UB. We can't safely test it without expecting a crash or corruption.
    // In this controlled test environment with mocks, we might just corrupt the list.
    // We strictly won't test double free here as it violates API contract.
    // Instead we test: Alloc -> Free -> Alloc -> Check integrity.

    let allocator = SafeAllocator::new();
    let layout = Layout::from_size_align(64, 16).unwrap();
    let ptr = allocator.alloc(layout);
    unsafe {
        ptr.write_bytes(0xCC, 64);
    }
    allocator.dealloc(ptr, layout);

    let ptr2 = allocator.alloc(layout);
    assert!(!ptr2.is_null());
    // Content is undefined, but check pointer
    unsafe {
        ptr2.write_bytes(0xDD, 64);
    }
    allocator.dealloc(ptr2, layout);
}
