use crate::{PAGE_SIZE, grow_memory};

/// 安全性警示 (Safety Warning):
/// 本模块中的分配器均为【单线程】设计。
/// 实现了 `Sync` 仅为了满足 `GlobalAlloc` trait 的要求。
/// 在多线程环境中使用此分配器会导致未定义行为 (UB)。
/// 请确保只在单线程环境（如 WASM 或单线程嵌入式环境）中使用。
unsafe impl Sync for SegregatedBumpAllocator {}
use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::null_mut,
};

/// 固定分箱 + Bump Pointer 回退的极简分配器。
///
/// # 设计哲学
/// - **极简体积**：放弃合并（Coalescing）与拆分（Splitting），移除所有复杂的链表遍历和元数据头。
/// - **速度优先**：小对象分配/释放均为严格的 O(1)。
/// - **场景定位**：Wasm Serverless 函数、短生命周期脚本、或内存充裕但对启动速度/体积敏感的场景。
///
/// # 内存布局
/// - **Bin 0**: 16 Bytes (用于 Box<u8>, small structs)
/// - **Bin 1**: 32 Bytes
/// - **Bin 2**: 64 Bytes
/// - **Bin 3**: 128 Bytes
/// - **Large**: > 128 Bytes，直接使用 Bump Pointer 分配，不复用。
pub struct SegregatedBumpAllocator;

impl SegregatedBumpAllocator {
    pub const fn new() -> Self {
        SegregatedBumpAllocator
    }

    /// ⚠️ 仅用于测试/Bench：重置全局状态
    pub unsafe fn reset() {
        unsafe {
            BINS = [null_mut(); 4];
            HEAP_TOP = 0;
            HEAP_END = 0;
        }
    }
}

// 单链表节点，嵌入在空闲内存块中
struct Node {
    next: *mut Node,
}

// --------------------------------------------------------------------------
// 全局静态状态 (在单线程 Wasm 中是安全的)
// --------------------------------------------------------------------------

// 4个桶的头指针。BINS[0] -> 16B, [1] -> 32B, [2] -> 64B, [3] -> 128B
static mut BINS: [*mut Node; 4] = [null_mut(); 4];

// Bump Pointer (堆顶指针)
static mut HEAP_TOP: usize = 0;
// 当前已申请的 Wasm 内存边界
static mut HEAP_END: usize = 0;

unsafe impl GlobalAlloc for SegregatedBumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // 1. 大对齐处理
        // 固定 Bins 默认保证 16 字节对齐。
        // 如果用户请求 > 16 字节对齐（非常罕见），直接通过 Bump 分配来处理对齐。
        if layout.align() > 16 {
            return unsafe { self.bump_alloc(layout.size(), layout.align()) };
        }

        // 2. 计算分类
        let size = layout.size().max(16);

        // 3. 尝试查表复用 (Small Alloc)
        if let Some(index) = get_index(size) {
            unsafe {
                let head = BINS[index];
                if !head.is_null() {
                    // Hit: 弹出链表头 (LIFO)
                    let next = (*head).next;
                    BINS[index] = next;
                    return head as *mut u8;
                }
            }

            // Miss: Bin 为空，回退到 Bump 分配
            // 直接分配对应 Bin 大小的块，而不是 layout.size()，以便将来 dealloc 能正确归位
            let block_size = 16 << index;
            return unsafe { self.bump_alloc(block_size, 16) };
        }

        // 4. 大对象处理 (> 128 Bytes)
        // 直接 Bump 分配，不走 Bin
        unsafe { self.bump_alloc(size, 16) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // 1. 如果是对齐要求很高的块，它一定不是来自 Bins，
        //    且我们没有元数据记录它的大小，所以直接丢弃（泄漏）。
        if layout.align() > 16 {
            return;
        }

        let size = layout.size().max(16);

        // 2. 尝试归还到 Bins
        if let Some(index) = get_index(size) {
            let node = ptr as *mut Node;
            // 头插法 (O(1))
            unsafe {
                (*node).next = BINS[index];
                BINS[index] = node;
            }
            return;
        }

        // 3. 大对象 (> 128 Bytes)
        // 策略选择：为了极简体积，放弃大对象复用。
        // 它们会随 Wasm 实例销毁而回收。
    }

    #[cfg(feature = "realloc")]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // 1. 确定旧块的实际容量
        let old_size = layout.size().max(16);
        let old_capacity = if layout.align() > 16 {
            old_size
        } else if let Some(index) = get_index(old_size) {
            16 << index
        } else {
            old_size
        };

        // 2. 如果新大小 <= 旧容量，直接复用 (In-place shrink)
        if new_size <= old_capacity {
            return ptr;
        }

        // 3. 尝试原地扩容 (In-place grow at HEAP_TOP)
        // 只有当 ptr 恰好在堆顶时才可能。
        let heap_top = unsafe { HEAP_TOP };
        if ptr as usize + old_capacity == heap_top {
            let diff = new_size - old_capacity;
            let heap_end = unsafe { HEAP_END };

            // 检查是否有足够的剩余空间或扩容
            unsafe {
                if HEAP_TOP + diff <= heap_end {
                    HEAP_TOP += diff;
                    return ptr;
                }

                let pages_needed =
                    ((HEAP_TOP + diff - heap_end + PAGE_SIZE - 1) / PAGE_SIZE).max(1);
                if grow_memory(pages_needed) != usize::MAX {
                    HEAP_END += pages_needed * PAGE_SIZE;
                    HEAP_TOP += diff;
                    return ptr;
                }
            }
        }

        // 4. 默认回退：Alloc + Copy + Dealloc
        unsafe {
            let new_ptr = self.alloc(Layout::from_size_align_unchecked(new_size, layout.align()));
            if !new_ptr.is_null() {
                // copy old_size, not old_capacity, because data is only valid up to old_size
                core::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size());
                self.dealloc(ptr, layout);
            }
            new_ptr
        }
    }
}

impl SegregatedBumpAllocator {
    /// 核心 Bump Pointer 分配逻辑
    unsafe fn bump_alloc(&self, size: usize, align: usize) -> *mut u8 {
        unsafe {
            let mut ptr = HEAP_TOP;

            // 处理对齐: (ptr + align - 1) & !(align - 1)
            // 对于 align=16，即 (ptr + 15) & !15
            ptr = (ptr + align - 1) & !(align - 1);

            // 检查溢出或容量不足
            if ptr + size > HEAP_END || ptr < HEAP_TOP {
                // 需要多少页？
                let bytes_needed = (ptr + size).saturating_sub(HEAP_END);
                let pages_needed = ((bytes_needed + PAGE_SIZE - 1) / PAGE_SIZE).max(1);

                let prev_page = grow_memory(pages_needed);
                if prev_page == usize::MAX {
                    return null_mut(); // OOM
                }

                // 如果是初次分配 (HEAP_END == 0)，需要初始化 ptr
                if HEAP_END == 0 {
                    // prev_page 应该是 0 (或者现有内存大小)
                    // Wasm memory_grow 返回旧的页数
                    let memory_start = prev_page * PAGE_SIZE;
                    ptr = memory_start;
                    // 再次对齐
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
}

// --------------------------------------------------------------------------
// 辅助函数
// --------------------------------------------------------------------------

/// 根据大小获取 Bin 索引。
/// 0 -> 16B, 1 -> 32B, 2 -> 64B, 3 -> 128B
/// 返回 None 表示是大对象。
#[inline(always)]
fn get_index(size: usize) -> Option<usize> {
    if size > 128 {
        return None;
    }

    // 利用 CLZ (Count Leading Zeros) 指令快速计算 log2
    // size 16 (10000) -> index 0
    // size 17..32 -> index 1
    // ...
    let size_val = size as usize;

    // next_power_of_two 确保 17 变成 32
    let power_of_two = size_val.next_power_of_two();
    let zeros = power_of_two.leading_zeros();

    // 计算基准偏移。
    // u32: leading_zeros(16) = 27.  Target index = 0. => 27 - 27 = 0
    // u64: leading_zeros(16) = 59.  Target index = 0. => 59 - 59 = 0
    #[cfg(target_pointer_width = "32")]
    const BASE: u32 = 27;

    #[cfg(target_pointer_width = "64")]
    const BASE: u32 = 59;

    Some((BASE - zeros) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Use a global lock to serialize tests because SegregatedBumpAllocator uses global `static mut` state.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_allocator(f: impl FnOnce()) {
        let _guard = TEST_LOCK.lock().unwrap();
        unsafe {
            // 1. Reset internal global state
            SegregatedBumpAllocator::reset();

            // 2. Reset host memory simulation (lib.rs)
            crate::reset_heap();
        }
        f();
    }

    #[test]
    fn test_small_alloc_reuse() {
        with_clean_allocator(|| {
            let allocator = SegregatedBumpAllocator::new();
            let layout = Layout::from_size_align(16, 8).unwrap(); // Bin 0: 16B

            unsafe {
                // 1. Alloc (Miss -> Bump)
                let ptr1 = allocator.alloc(layout);
                assert!(!ptr1.is_null());
                *ptr1 = 0xAA;

                // 2. Dealloc (Push to Bin 0)
                allocator.dealloc(ptr1, layout);

                // 3. Alloc (Hit -> Reuse)
                let ptr2 = allocator.alloc(layout);
                assert_eq!(ptr1, ptr2, "Should reuse the same pointer from bin");
                // Intrusive list overwrites the first bytes with 'next' pointer, so data is not preserved.
            }
        });
    }

    #[test]
    fn test_cross_bin_isolation() {
        with_clean_allocator(|| {
            let allocator = SegregatedBumpAllocator::new();

            unsafe {
                // Bin 0 (16B)
                let l0 = Layout::from_size_align(10, 1).unwrap();
                let p0 = allocator.alloc(l0);

                // Bin 1 (32B)
                let l1 = Layout::from_size_align(20, 1).unwrap();
                let p1 = allocator.alloc(l1);

                assert_ne!(p0, p1);

                // Dealloc 16B block
                allocator.dealloc(p0, l0);

                // Alloc 32B block - should NOT reuse 16B block
                let p2 = allocator.alloc(l1);
                assert_ne!(p0, p2);

                // Alloc 16B block - should reuse p0
                let p3 = allocator.alloc(l0);
                assert_eq!(p0, p3);
            }
        });
    }

    #[test]
    fn test_large_alloc_bypass() {
        with_clean_allocator(|| {
            let allocator = SegregatedBumpAllocator::new();
            // > 128 Bytes -> Large alloc (Bump directly)
            let layout = Layout::from_size_align(200, 16).unwrap();

            unsafe {
                let p1 = allocator.alloc(layout);
                assert!(!p1.is_null());

                // Dealloc (No-op usually for large allocs in this simple allocator)
                allocator.dealloc(p1, layout);

                // Alloc again. Since dealloc is no-op, it allocates new space.
                let p2 = allocator.alloc(layout);
                assert_ne!(
                    p1, p2,
                    "Large objects should not be implicitly reused in this implementation"
                );
            }
        });
    }

    #[test]
    fn test_memory_growth() {
        with_clean_allocator(|| {
            let allocator = SegregatedBumpAllocator::new();
            // Alloc enough to force page growth. PAGE_SIZE is 64KB.
            // Let's alloc 40KB twice.
            let layout = Layout::from_size_align(40 * 1024, 16).unwrap();

            unsafe {
                let p1 = allocator.alloc(layout);
                assert!(!p1.is_null());

                let p2 = allocator.alloc(layout);
                assert!(!p2.is_null());
                assert_ne!(p1, p2);

                // Verify they don't overlap
                let dist = (p2 as usize).wrapping_sub(p1 as usize);
                assert!(dist >= 40 * 1024);
            }
        });
    }

    #[test]
    fn test_high_alignment() {
        with_clean_allocator(|| {
            let allocator = SegregatedBumpAllocator::new();
            // 32B size, but 128B alignment.
            // Should bypass bins (max align 16) and use bump with align pad.
            let layout = Layout::from_size_align(32, 128).unwrap();

            unsafe {
                let p1 = allocator.alloc(layout);
                assert!(!p1.is_null());
                assert_eq!(p1 as usize % 128, 0);

                allocator.dealloc(p1, layout); // Should be no-op or handled gracefully
            }
        });
    }

    #[test]
    fn test_realloc_shrink() {
        with_clean_allocator(|| {
            let allocator = SegregatedBumpAllocator::new();
            // Bin 3: 128B
            let layout = Layout::from_size_align(100, 16).unwrap();
            // capacity will be 128

            unsafe {
                let ptr = allocator.alloc(layout);
                ptr.write_bytes(0xCC, 100);

                // Shrink to 80. Still Bin 3 (128B). Should be inplace.
                let new_size = 80;
                let new_ptr = allocator.realloc(ptr, layout, new_size);

                #[cfg(feature = "realloc")]
                assert_eq!(ptr, new_ptr);
                #[cfg(not(feature = "realloc"))]
                assert_ne!(ptr, new_ptr); // Default moves

                assert_eq!(*new_ptr, 0xCC);
            }
        });
    }

    #[test]
    fn test_realloc_grow_large_at_top() {
        with_clean_allocator(|| {
            let allocator = SegregatedBumpAllocator::new();
            // Large alloc > 128B.
            let layout = Layout::from_size_align(256, 16).unwrap();

            unsafe {
                let ptr = allocator.alloc(layout);
                ptr.write_bytes(0xAA, 256);

                // This should be at HEAP_TOP.

                let new_size = 512;
                let new_ptr = allocator.realloc(ptr, layout, new_size);

                #[cfg(feature = "realloc")]
                assert_eq!(ptr, new_ptr); // Should grow in place
                #[cfg(not(feature = "realloc"))]
                assert_ne!(ptr, new_ptr);

                assert_eq!(*new_ptr, 0xAA);
                assert_eq!(*new_ptr.add(255), 0xAA);
            }
        });
    }

    #[test]
    fn test_realloc_move() {
        with_clean_allocator(|| {
            let allocator = SegregatedBumpAllocator::new();
            // 1. Alloc something to block bottom
            let l1 = Layout::from_size_align(64, 16).unwrap();
            let _ = unsafe { allocator.alloc(l1) };

            // 2. Alloc target block (Large)
            let layout = Layout::from_size_align(256, 16).unwrap();
            let ptr = unsafe { allocator.alloc(layout) };

            // 3. Alloc something else to block top (so ptr is NOT at HEAP_TOP)
            let l3 = Layout::from_size_align(64, 16).unwrap();
            let _p3 = unsafe { allocator.alloc(l3) };

            unsafe {
                ptr.write_bytes(0xBB, 256);

                let new_size = 512;
                let new_ptr = allocator.realloc(ptr, layout, new_size);

                assert_ne!(ptr, new_ptr); // Must move
                assert_eq!(*new_ptr, 0xBB);
            }
        });
    }
}
