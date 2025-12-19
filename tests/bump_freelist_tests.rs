use lite_alloc::reset_heap;
use lite_alloc::single_threaded::BumpFreeListAllocator;
use std::alloc::{GlobalAlloc, Layout};
use std::sync::{Mutex, MutexGuard};

static TEST_MUTEX: Mutex<()> = Mutex::new(());

struct SafeAllocator {
    inner: BumpFreeListAllocator,
    _guard: MutexGuard<'static, ()>,
}

impl SafeAllocator {
    fn new() -> Self {
        let guard = TEST_MUTEX.lock().unwrap();
        unsafe {
            BumpFreeListAllocator::reset();
            reset_heap();
            Self {
                inner: BumpFreeListAllocator::new(),
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
            BumpFreeListAllocator::reset();
            reset_heap();
        }
    }
}

#[test]
fn test_basic() {
    let allocator = SafeAllocator::new();
    let layout = Layout::from_size_align(32, 8).unwrap();
    let ptr = allocator.alloc(layout);
    assert!(!ptr.is_null());
    unsafe { ptr.write_bytes(0xAA, 32) };
    allocator.dealloc(ptr, layout);
}

#[test]
fn test_reuse_lifo() {
    let allocator = SafeAllocator::new();
    let layout = Layout::from_size_align(32, 8).unwrap();

    let ptr1 = allocator.alloc(layout);
    allocator.dealloc(ptr1, layout);

    // BumpFreeList is LIFO, simple stack.
    let ptr2 = allocator.alloc(layout);

    assert_eq!(ptr1, ptr2);
}

#[test]
fn test_no_coalescing() {
    let allocator = SafeAllocator::new();
    let layout = Layout::from_size_align(32, 8).unwrap();

    let ptr1 = allocator.alloc(layout); // lower address
    let ptr2 = allocator.alloc(layout); // higher address

    allocator.dealloc(ptr1, layout);
    allocator.dealloc(ptr2, layout);

    // List: [ptr2, ptr1]

    // Alloc 64 bytes.
    // Since it doesn't coalesce, ptr2 (32B) is too small, ptr1 (32B) is too small.
    // Must bump alloc new space.
    let layout_large = Layout::from_size_align(64, 8).unwrap();
    let ptr3 = allocator.alloc(layout_large);

    assert_ne!(ptr3, ptr1);
    assert_ne!(ptr3, ptr2);

    // However, if we alloc 32B, we get ptr2 (LIFO head)
    let ptr4 = allocator.alloc(layout);
    assert_eq!(ptr4, ptr2);
}

#[test]
fn test_alignment_padding() {
    let allocator = SafeAllocator::new();
    // 1-byte alloc, 1-byte align
    let layout_tiny = Layout::from_size_align(1, 1).unwrap();
    let ptr1 = allocator.alloc(layout_tiny);

    // Should be at least 16 aligned
    assert_eq!(ptr1 as usize % 16, 0);

    // Next alloc should be at ptr1 + 16 (min size 16)
    let ptr2 = allocator.alloc(layout_tiny);
    assert_eq!(ptr2 as usize - ptr1 as usize, 16);
}

#[test]
fn test_grow_memory() {
    let allocator = SafeAllocator::new();
    let layout = Layout::from_size_align(100 * 1024, 16).unwrap();
    let ptr = allocator.alloc(layout);
    assert!(!ptr.is_null());
    unsafe { ptr.write_bytes(1, 100 * 1024) };
}

#[cfg(feature = "realloc")]
#[test]
fn test_realloc_extend() {
    let allocator = SafeAllocator::new();
    let layout = Layout::from_size_align(32, 8).unwrap();
    let ptr = allocator.alloc(layout);

    // This is the top of heap. Extending should be easy.
    let new_ptr = allocator.realloc(ptr, layout, 64);
    assert_eq!(ptr, new_ptr);
}
