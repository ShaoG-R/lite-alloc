use crate::{PAGE_SIZE, grow_memory};
use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::{self, null_mut},
};

/// Safety Warning:
/// Allocators in this module are designed for [Single Threaded] environments.
/// `Sync` is implemented only to satisfy `GlobalAlloc` trait requirements.
/// Using this allocator in a multi-threaded environment will lead to Undefined Behavior (UB).
/// Please ensure it is used only in single-threaded environments (e.g., WASM or single-threaded embedded).
///
/// 安全性警示 (Safety Warning):
/// 本模块中的分配器均为【单线程】设计。
/// 实现了 `Sync` 仅为了满足 `GlobalAlloc` trait 的要求。
/// 在多线程环境中使用此分配器会导致未定义行为 (UB)。
/// 请确保只在单线程环境（如 WASM 或单线程嵌入式环境）中使用。
unsafe impl Sync for BumpFreeListAllocator {}

/// Minimal Bump Pointer + Unordered Free List Allocator.
///
/// 极简 Bump Pointer + 无序链表分配器。
///
/// # Features
/// - **Extreme Size**: Removes binning and merging logic to minimize code size.
/// - **Fast Startup**: No initialization overhead.
/// - **Fragmentation**: Does not merge memory, long-running processes will cause OOM. Only suitable for short-lived tasks.
///
/// # 特性
/// - **极致体积**：移除分箱和合并逻辑，代码量最小化。
/// - **快速启动**：无初始化开销。
/// - **碎片化**：不合并内存，长期运行会导致 OOM。仅适用于短生命周期任务。
pub struct BumpFreeListAllocator;

impl BumpFreeListAllocator {
    pub const fn new() -> Self {
        Self
    }
}

// Linked list node: must store size because we have only one mixed list
// 链表节点：必须存储大小，因为我们只有一个混杂的链表
struct Node {
    next: *mut Node,
    size: usize,
}

// --------------------------------------------------------------------------
// Global State
// 全局状态
// --------------------------------------------------------------------------

// Single unordered free list head
// 单个无序空闲链表头
static mut FREE_LIST: *mut Node = null_mut();

// Bump Pointer State
// Bump Pointer 状态
static mut HEAP_TOP: usize = 0;
static mut HEAP_END: usize = 0;

unsafe impl GlobalAlloc for BumpFreeListAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // 1. Unify alignment to 16 bytes.
        // This simplifies all pointer calculations and adapts to Wasm SIMD.
        // 1. 统一对齐到 16 字节
        // 这简化了所有指针计算，并且适配 Wasm SIMD
        let align_req = layout.align().max(16);
        let size = layout.size().max(16);

        // Ensure size is also a multiple of 16 for easier management
        // 确保 size 也是 16 的倍数，方便后续管理
        let size = (size + 15) & !15;

        // 2. Try to allocate from the free list (First Fit).
        // Iterate through the list to find the first block that is large enough.
        // Note: This is an O(N) operation. However, in short-lived applications, the list is usually short.
        // 2. 尝试从空闲链表分配 (First Fit)
        // 遍历链表找到第一个足够大的块。
        // 注意：这是 O(N) 操作。但在短生命周期应用中，链表通常很短。
        unsafe {
            let mut prev = ptr::addr_of_mut!(FREE_LIST);
            let mut curr = *prev;

            while !curr.is_null() {
                if (*curr).size >= size {
                    // Found a suitable block: remove from list
                    // 找到合适的块：从链表中移除
                    *prev = (*curr).next;
                    return curr as *mut u8;
                }
                // Move to next node
                // 移动到下一个节点
                prev = ptr::addr_of_mut!((*curr).next);
                curr = *prev;
            }
        }

        // 3. No suitable block in the free list -> Use Bump Pointer allocation
        // 3. 链表中没有合适的块 -> 使用 Bump Pointer 分配
        // self.bump_alloc is unsafe
        unsafe { self.bump_alloc(size, align_req) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // 1. Calculate size (must be consistent with calculation in alloc)
        // 1. 计算大小 (必须与 alloc 中的计算方式一致)
        let size = layout.size().max(16);
        let size = (size + 15) & !15;

        // 2. Insert into free list at head (O(1)).
        // No merging, simply thread it through.
        // 2. 头插法插入空闲链表 (O(1))
        // 不进行合并，直接通过
        unsafe {
            let node = ptr as *mut Node;
            (*node).size = size;
            (*node).next = FREE_LIST;
            FREE_LIST = node;
        }
    }

    #[cfg(feature = "realloc")]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Optimization: Check if at heap top, if so, extend in place
        // 优化：检查是否在堆顶，如果是则原地扩容
        let old_size = (layout.size().max(16) + 15) & !15;
        let req_new_size = (new_size.max(16) + 15) & !15;

        // Accessing mutable statics requires unsafe block
        let heap_top = unsafe { HEAP_TOP };

        if ptr as usize + old_size == heap_top {
            let diff = req_new_size.saturating_sub(old_size);
            if diff == 0 {
                return ptr;
            }

            // Try to extend heap top
            // 尝试扩容堆顶
            unsafe {
                if HEAP_TOP + diff <= HEAP_END {
                    HEAP_TOP += diff;
                    return ptr;
                }

                // Request more pages
                // 申请更多页面
                let pages_needed =
                    ((HEAP_TOP + diff - HEAP_END + PAGE_SIZE - 1) / PAGE_SIZE).max(1);
                if grow_memory(pages_needed) != usize::MAX {
                    HEAP_END += pages_needed * PAGE_SIZE;
                    HEAP_TOP += diff;
                    return ptr;
                }
            }
        }

        // Default fallback
        // 默认回退
        unsafe {
            let new_ptr = self.alloc(Layout::from_size_align_unchecked(new_size, layout.align()));
            if !new_ptr.is_null() {
                ptr::copy_nonoverlapping(ptr, new_ptr, layout.size());
                self.dealloc(ptr, layout);
            }
            new_ptr
        }
    }
}

impl BumpFreeListAllocator {
    unsafe fn bump_alloc(&self, size: usize, align: usize) -> *mut u8 {
        unsafe {
            let mut ptr = HEAP_TOP;
            // Alignment handling
            // 对齐处理
            ptr = (ptr + align - 1) & !(align - 1);

            if ptr + size > HEAP_END || ptr < HEAP_TOP {
                let bytes_needed = (ptr + size).saturating_sub(HEAP_END);
                let pages_needed = ((bytes_needed + PAGE_SIZE - 1) / PAGE_SIZE).max(1);

                let prev_page = grow_memory(pages_needed);
                if prev_page == usize::MAX {
                    return null_mut();
                }

                if HEAP_END == 0 {
                    let memory_start = prev_page * PAGE_SIZE;
                    ptr = memory_start;
                    ptr = (ptr + align - 1) & !(align - 1);
                    HEAP_END = memory_start + pages_needed * PAGE_SIZE;
                } else {
                    HEAP_END += pages_needed * PAGE_SIZE;
                }
            }

            HEAP_TOP = ptr + size;
            ptr as *mut u8
        }
    }

    /// Testing only: Reset the internal state.
    /// Safety: usage is inherently unsafe if allocator is in use.
    ///
    /// 仅测试用：重置内部状态。
    /// 安全性：如果分配器正在使用，未定义的行为。
    pub unsafe fn reset() {
        unsafe {
            FREE_LIST = null_mut();
            HEAP_TOP = 0;
            HEAP_END = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reset_heap;
    use core::alloc::Layout;
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
                BumpFreeListAllocator::reset(); // Reset allocator state
                reset_heap(); // Reset memory mock
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
    fn test_basic_allocation() {
        let allocator = SafeAllocator::new();
        let layout = Layout::new::<u64>();
        let ptr = allocator.alloc(layout);
        assert!(!ptr.is_null());

        unsafe {
            *ptr.cast::<u64>() = 42;
            assert_eq!(*ptr.cast::<u64>(), 42);
        }

        allocator.dealloc(ptr, layout);
    }

    #[test]
    fn test_multiple_allocations() {
        let allocator = SafeAllocator::new();
        let layout = Layout::new::<u32>();

        let ptr1 = allocator.alloc(layout);
        let ptr2 = allocator.alloc(layout);

        assert!(!ptr1.is_null());
        assert!(!ptr2.is_null());
        assert_ne!(ptr1, ptr2);

        // Bump pointer should advance by at least size (aligned to 16)
        let diff = (ptr2 as usize).wrapping_sub(ptr1 as usize);
        assert!(diff >= 16);
    }

    #[test]
    fn test_memory_grow() {
        let allocator = SafeAllocator::new();
        // PAGE_SIZE is 65536. Allocate 40KB twice to trigger growth.
        let layout = Layout::from_size_align(40 * 1024, 16).unwrap();

        let ptr1 = allocator.alloc(layout);
        let ptr2 = allocator.alloc(layout);

        assert!(!ptr1.is_null());
        assert!(!ptr2.is_null());
        assert_ne!(ptr1, ptr2);

        unsafe {
            ptr1.write_bytes(1, layout.size());
            ptr2.write_bytes(2, layout.size());
        }
    }

    #[test]
    fn test_freelist_reuse() {
        let allocator = SafeAllocator::new();
        let layout = Layout::from_size_align(128, 16).unwrap();

        let ptr1 = allocator.alloc(layout);
        allocator.dealloc(ptr1, layout);

        let ptr2 = allocator.alloc(layout);
        // Should reuse the same memory block (LIFO freelist where ptr1 was just added)
        assert_eq!(ptr1, ptr2);
    }

    #[test]
    fn test_realloc_in_place() {
        let allocator = SafeAllocator::new();
        let layout = Layout::from_size_align(32, 16).unwrap();
        let ptr = allocator.alloc(layout);
        assert!(!ptr.is_null());

        unsafe {
            ptr.write_bytes(0xAA, layout.size());
        }

        // Reallocate to a larger size, should be in-place if possible
        let new_size = 64;
        let new_layout = Layout::from_size_align(new_size, 16).unwrap();
        let realloc_ptr = unsafe { allocator.inner.realloc(ptr, layout, new_size) };

        #[cfg(feature = "realloc")]
        assert_eq!(ptr, realloc_ptr); // Should be in-place
        #[cfg(not(feature = "realloc"))]
        assert_ne!(ptr, realloc_ptr); // Fallback allocation moves data (alloc before dealloc)
        unsafe {
            assert_eq!(*realloc_ptr.cast::<u8>(), 0xAA); // Content should be preserved
            assert_eq!(*realloc_ptr.add(31).cast::<u8>(), 0xAA); // Original content preserved
            realloc_ptr
                .add(32)
                .write_bytes(0xBB, new_size - layout.size()); // Write to new part
            assert_eq!(*realloc_ptr.add(32).cast::<u8>(), 0xBB);
        }
        allocator.dealloc(realloc_ptr, new_layout);
    }

    #[test]
    fn test_realloc_not_in_place() {
        let allocator = SafeAllocator::new();
        let layout1 = Layout::from_size_align(32, 16).unwrap();
        let ptr1 = allocator.alloc(layout1); // This will be HEAP_TOP
        assert!(!ptr1.is_null());

        let layout2 = Layout::from_size_align(32, 16).unwrap();
        let ptr2 = allocator.alloc(layout2); // This will prevent ptr1 from being HEAP_TOP
        assert!(!ptr2.is_null());
        assert_ne!(ptr1, ptr2);

        unsafe {
            ptr1.write_bytes(0xAA, layout1.size());
        }

        // Reallocate ptr1 to a larger size. It's not HEAP_TOP, so it should move.
        let new_size = 64;
        let new_layout = Layout::from_size_align(new_size, 16).unwrap();
        let realloc_ptr = unsafe { allocator.inner.realloc(ptr1, layout1, new_size) };

        assert!(!realloc_ptr.is_null());
        assert_ne!(ptr1, realloc_ptr); // Should not be in-place
        unsafe {
            assert_eq!(*realloc_ptr.cast::<u8>(), 0xAA); // Content should be preserved
            assert_eq!(*realloc_ptr.add(31).cast::<u8>(), 0xAA); // Original content preserved
        }

        allocator.dealloc(realloc_ptr, new_layout);
        allocator.dealloc(ptr2, layout2);
    }

    #[test]
    fn test_realloc_shrink() {
        let allocator = SafeAllocator::new();
        let layout = Layout::from_size_align(64, 16).unwrap();
        let ptr = allocator.alloc(layout);
        assert!(!ptr.is_null());

        unsafe {
            ptr.write_bytes(0xCC, layout.size());
        }

        // Reallocate to a smaller size
        let new_size = 32;
        let new_layout = Layout::from_size_align(new_size, 16).unwrap();
        let realloc_ptr = unsafe { allocator.inner.realloc(ptr, layout, new_size) };

        #[cfg(feature = "realloc")]
        assert_eq!(ptr, realloc_ptr); // Shrinking can often be in-place

        #[cfg(not(feature = "realloc"))]
        assert_ne!(ptr, realloc_ptr);

        unsafe {
            assert_eq!(*realloc_ptr.cast::<u8>(), 0xCC); // Content should be preserved
            assert_eq!(*realloc_ptr.add(31).cast::<u8>(), 0xCC); // Original content preserved
        }
        allocator.dealloc(realloc_ptr, new_layout);
    }
}
