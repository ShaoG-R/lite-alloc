use lite_alloc::reset_heap;
use lite_alloc::single_threaded::SegregatedBumpAllocator;
use std::alloc::{GlobalAlloc, Layout};
use std::sync::{Mutex, MutexGuard};

static TEST_MUTEX: Mutex<()> = Mutex::new(());

struct SafeAllocator {
    inner: SegregatedBumpAllocator,
    _guard: MutexGuard<'static, ()>,
}

impl SafeAllocator {
    fn new() -> Self {
        let guard = TEST_MUTEX.lock().unwrap();
        unsafe {
            SegregatedBumpAllocator::reset();
            reset_heap();
            Self {
                inner: SegregatedBumpAllocator::new(),
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
            SegregatedBumpAllocator::reset();
            reset_heap();
        }
    }
}

#[test]
fn test_small_bins() {
    let allocator = SafeAllocator::new();

    // Bin 0: <= 16
    let layout16 = Layout::from_size_align(10, 1).unwrap();
    let ptr1 = allocator.alloc(layout16);
    assert!(!ptr1.is_null());

    // Bin 1: <= 32
    let layout32 = Layout::from_size_align(20, 1).unwrap();
    let ptr2 = allocator.alloc(layout32);
    assert!(!ptr2.is_null());

    allocator.dealloc(ptr1, layout16);
    // Reuse bin 0
    let ptr3 = allocator.alloc(layout16);
    assert_eq!(ptr1, ptr3);

    // Bin 1 should be unaffected
    assert_ne!(ptr3, ptr2);
}

#[test]
fn test_large_bypass() {
    let allocator = SafeAllocator::new();

    // Large > 128
    let layout_large = Layout::from_size_align(200, 16).unwrap();
    let ptr1 = allocator.alloc(layout_large);

    allocator.dealloc(ptr1, layout_large);

    // Should NOT reuse for SegregatedBump (implementation detail: large allocs are just bumped and leaked/freed-at-end)
    let ptr2 = allocator.alloc(layout_large);
    assert_ne!(ptr1, ptr2);
}

#[test]
fn test_mixed_bins() {
    let allocator = SafeAllocator::new();
    let l16 = Layout::from_size_align(16, 16).unwrap();
    let l32 = Layout::from_size_align(32, 16).unwrap();
    let l64 = Layout::from_size_align(64, 16).unwrap();
    let l128 = Layout::from_size_align(128, 16).unwrap();

    let p1 = allocator.alloc(l16);
    let p2 = allocator.alloc(l32);
    let p3 = allocator.alloc(l64);
    let p4 = allocator.alloc(l128);

    // All unique
    let buckets = [p1, p2, p3, p4];
    for i in 0..4 {
        for j in i + 1..4 {
            assert_ne!(buckets[i], buckets[j]);
        }
    }

    allocator.dealloc(p2, l32);
    allocator.dealloc(p4, l128);

    // Alloc 32 again -> reuse p2
    let p2_new = allocator.alloc(l32);
    assert_eq!(p2, p2_new);

    // Alloc 128 again -> reuse p4
    let p4_new = allocator.alloc(l128);
    assert_eq!(p4, p4_new);
}

#[test]
fn test_high_alignment_bypass() {
    let allocator = SafeAllocator::new();
    // Size 16 fits Bin 0, but alignment 128 forces bypass
    let layout = Layout::from_size_align(16, 128).unwrap();

    let ptr = allocator.alloc(layout);
    assert!(!ptr.is_null());
    assert_eq!(ptr as usize % 128, 0);

    allocator.dealloc(ptr, layout);
    // Cannot reuse because it didn't go to Bin 0 (because of align)
    // nor can it solve align requirement from Bin 0 easily.
}

#[cfg(feature = "realloc")]
#[test]
fn test_realloc_bin_growth() {
    let allocator = SafeAllocator::new();
    // Alloc 16
    let l16 = Layout::from_size_align(16, 16).unwrap();
    let ptr = allocator.alloc(l16);
    unsafe { ptr.write(0x11) };

    // Grow to 32
    let ptr_new = allocator.realloc(ptr, l16, 32);
    assert_ne!(ptr, ptr_new); // Must move to new bin/block
    unsafe { assert_eq!(*ptr_new, 0x11) };
}
