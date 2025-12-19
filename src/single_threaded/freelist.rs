use crate::PAGE_SIZE;
use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
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
unsafe impl Sync for FreeListAllocator {}

/// A non-thread-safe allocator using a free list.
/// Complexity of allocation and deallocation is O(length of free list).
///
/// The free list is sorted by address, and adjacent memory blocks are merged when inserting new blocks.
///
/// 一个使用空闲链表的非线程安全分配器。
/// 分配和释放操作的时间复杂度为 O(空闲链表长度)。
///
/// 空闲链表按地址排序，并且在插入新块时会合并相邻的内存块。
pub struct FreeListAllocator {
    free_list: UnsafeCell<*mut FreeListNode>,
}

impl FreeListAllocator {
    pub const fn new() -> Self {
        FreeListAllocator {
            free_list: UnsafeCell::new(EMPTY_FREE_LIST),
        }
    }
}

const EMPTY_FREE_LIST: *mut FreeListNode = usize::MAX as *mut FreeListNode;

/// Stored at the beginning of each free segment.
/// Note: This could be packed into 1 word (using low bits to mark this case,
/// and only using the second word when allocation size is larger than 1 word).
///
/// 存储在每个空闲段的开头。
/// 注意：可以将其放入 1 个字中（使用低位标记该情况，
/// 然后仅在分配大小大于 1 个字时使用第二个字）
struct FreeListNode {
    next: *mut FreeListNode,
    size: usize,
}

const NODE_SIZE: usize = core::mem::size_of::<FreeListNode>();

// Safety: No one else owns the raw pointer, so we can safely transfer
// FreeListAllocator to another thread.
//
// 安全性：除我们之外没有人拥有原始指针，因此我们可以安全地将
// FreeListAllocator 转移到另一个线程。
unsafe impl Send for FreeListAllocator {}

unsafe impl GlobalAlloc for FreeListAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // 1. Force fixed alignment to 16 bytes (covering u8 to u128/v128)
        // This saves you from complex dynamic alignment logic reading layout.align()
        // 1. 强制固定对齐为 16 字节 (覆盖 u8 到 u128/v128)
        // 这样你就不用读取 layout.align() 来做复杂的动态对齐逻辑了
        const MIN_ALIGN: usize = 16;

        // 2. If user requests more aggressive alignment (e.g. 4KB page alignment), must handle or fail
        // For size, you can choose not to support alignment > 16 (return null or panic)
        // 2. 如果用户请求了更变态的对齐 (比如 4KB 对齐的页)，必须处理或失败
        // 为了体积，你可以选择直接不支持超过 16 的对齐（直接返回 null 或 panic）
        if layout.align() > MIN_ALIGN {
            return null_mut();
        }

        // 3. Calculate size: round up to multiple of 16
        // Assume NODE_SIZE is also 16 bytes or smaller
        // 3. 计算大小：向上取整到 16 的倍数
        // 假设 NODE_SIZE 也是 16 字节或更小
        let size = layout.size().max(NODE_SIZE);
        // Fast bitwise round up to 16
        // 快速位运算取整 (等同于 round_up to 16)
        let size = (size + 15) & !15;

        let mut free_list: *mut *mut FreeListNode = self.free_list.get();
        // Search the free list
        // 搜索空闲链表
        loop {
            // SAFETY: Dereferencing free_list is safe
            // SAFETY: 解引用 free_list 是安全的
            if unsafe { *free_list == EMPTY_FREE_LIST } {
                break;
            }

            let node = unsafe { *free_list };
            let node_size = unsafe { (*node).size };

            if size <= node_size {
                let remaining = node_size - size;
                // If remaining space is large enough, keep it in the list
                // 如果剩余空间足够大，我们将其保留在链表中
                if remaining >= NODE_SIZE {
                    unsafe {
                        (*node).size = remaining;
                        return (node as *mut u8).add(remaining);
                    }
                } else {
                    // Otherwise, allocate the whole block
                    // 否则，整个块都分配出去
                    unsafe {
                        *free_list = (*node).next;
                        return node as *mut u8;
                    }
                }
            }
            // SAFETY: Move to next node.
            // SAFETY: 移动到下一个节点。
            unsafe {
                free_list = ptr::addr_of_mut!((*node).next);
            }
        }

        // No space found in free list.
        // 未在空闲链表中找到空间。
        let requested_bytes = round_up(size, PAGE_SIZE);
        // SAFETY: Call global grow_memory (shimmed on non-wasm)
        let previous_page_count = unsafe { crate::grow_memory(requested_bytes / PAGE_SIZE) };
        if previous_page_count == usize::MAX {
            return null_mut();
        }

        let ptr = (previous_page_count * PAGE_SIZE) as *mut u8;
        // SAFETY: Recursive call to self to add new memory block.
        // SAFETY: 递归调用自身，添加新的内存块。
        unsafe {
            self.dealloc(
                ptr,
                Layout::from_size_align_unchecked(requested_bytes, PAGE_SIZE),
            );
            self.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        debug_assert!(ptr.align_offset(NODE_SIZE) == 0);
        let ptr = ptr as *mut FreeListNode;
        let size = full_size(layout);
        // SAFETY: Pointer arithmetic
        // SAFETY: 指针算术
        // Used to merge with the next node if adjacent.
        // 用于在相邻时与下一个节点合并。
        let after_new = unsafe { offset_bytes(ptr, size) };

        // SAFETY: Get UnsafeCell pointer
        // SAFETY: 获取 UnsafeCell 指針
        let mut free_list: *mut *mut FreeListNode = self.free_list.get();
        // Insert into free list, sorted by pointer descending.
        // 插入到空闲链表中，该链表按指针降序存储。
        loop {
            // SAFETY: Dereference free_list to check if empty or compare address
            // SAFETY: 解引用 free_list 检查是否为空或比较地址
            if unsafe { *free_list == EMPTY_FREE_LIST } {
                // SAFETY: Write new node and insert at head
                // SAFETY: 写入新节点并插入链表头
                unsafe {
                    (*ptr).next = EMPTY_FREE_LIST;
                    (*ptr).size = size;
                    *free_list = ptr;
                }
                return;
            }

            // SAFETY: *free_list is a valid node pointer because we checked EMPTY_FREE_LIST above
            // SAFETY: *free_list 是一个有效的节点指针，因为我们上面检查了 EMPTY_FREE_LIST
            if unsafe { *free_list == after_new } {
                // Merge new node into the node after it.
                // 将新节点合并到此节点之后的节点中。

                // SAFETY: Access fields
                // SAFETY: 访问字段
                let new_size = unsafe { size + (**free_list).size };
                let next = unsafe { (**free_list).next };

                // SAFETY: Check next continuity
                // SAFETY: 检查 next 连续性
                if unsafe { next != EMPTY_FREE_LIST && offset_bytes(next, (*next).size) == ptr } {
                    // Merge into the node before this node, and the one after.
                    // 合并到此节点之前的节点，以及之后的节点。
                    // SAFETY: Update next size, remove current node
                    // SAFETY: 更新 next 的大小，移除当前节点
                    unsafe {
                        (*next).size += new_size;
                        *free_list = next;
                    }
                    return;
                }
                // Edit node in free list, move its position and update its size.
                // 编辑空闲链表中的节点，移动其位置并更新其大小。
                // SAFETY: Pointer operations
                // SAFETY: 指针操作
                unsafe {
                    *free_list = ptr;
                    (*ptr).size = new_size;
                    (*ptr).next = next;
                }
                return;
            }

            if unsafe { *free_list < ptr } {
                // If adjacent, merge to the end of current node
                // 如果相邻，则合并到当前节点的末尾
                // SAFETY: ptr comparison and offset_bytes are pointer arithmetic
                // SAFETY: 这里的 ptr 比较和 offset_bytes 都是指针算术
                if unsafe { offset_bytes(*free_list, (**free_list).size) == ptr } {
                    // Merge into the node before this node (and potentially after).
                    // 合并到此节点之前的节点，以及之后的节点。
                    // SAFETY: Only need to update size
                    // SAFETY: 只需更新大小
                    unsafe {
                        (**free_list).size += size;
                    }
                    // Since we merged new node to the end of existing node, no need to update pointers, just change size.
                    // 因为我们将新节点合并到现有节点的末尾，所以不需要更新指针，只需更改大小。
                    return;
                }
                // Create a new free list node
                // 创建一个新的空闲链表节点
                // SAFETY: List insertion
                // SAFETY: 链表插入
                unsafe {
                    (*ptr).next = *free_list;
                    (*ptr).size = size;
                    *free_list = ptr;
                }
                return;
            }
            // SAFETY: Move pointer
            // SAFETY: 移动指针
            unsafe {
                free_list = ptr::addr_of_mut!((**free_list).next);
            }
        }
    }
    #[cfg(feature = "realloc")]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // 1. Calculate original block size (consistent with alloc/dealloc)
        // 1. 计算原块大小 (与 alloc/dealloc 一致)
        let old_size = full_size(layout);
        // 2. Calculate new block size (aligned)
        // 2. 计算新块大小 (对齐)
        let new_full_size = (new_size.max(NODE_SIZE) + 15) & !15;

        // case A: Shrinking
        if new_full_size <= old_size {
            let diff = old_size - new_full_size;
            // If remaining space is large enough, split and free the remainder
            // 如果剩余空间足够大，切分并释放剩余部分
            if diff >= NODE_SIZE {
                unsafe {
                    let remainder = ptr.add(new_full_size);
                    // Construct a Layout for freeing
                    // align=16 is safe because all our blocks are 16-aligned
                    // 构造一个 Layout 用于释放
                    // align=16 是安全的，因为我们所有的块都是 16 对齐
                    let remainder_layout = Layout::from_size_align_unchecked(diff, 16);
                    self.dealloc(remainder, remainder_layout);
                }
            }
            return ptr;
        }

        // case B: Growing
        // Try to merge backwards (In-place grow)
        // Our list is [Sorted Descending by Address]
        // 尝试向后合并 (In-place grow)
        // 我们的链表是【地址降序】 (Descending)
        // Check if `ptr + old_size` is a free node.
        let needed = new_full_size - old_size;
        let target_addr = unsafe { ptr.add(old_size) as *mut FreeListNode };

        let mut prev = self.free_list.get();
        loop {
            let curr = unsafe { *prev };
            if curr == EMPTY_FREE_LIST {
                break;
            }

            // List Descending: 2000 -> 1000 -> 500
            // If curr (2000) > target (1500), continue searching
            // If curr (1500) == target (1500), found it
            // If curr (1000) < target (1500), means target is not in list (missed)
            // 链表降序: 2000 -> 1000 -> 500
            // 如果 curr (2000) > target (1500)，继续找
            // 如果 curr (1500) == target (1500)，找到
            // 如果 curr (1000) < target (1500)，说明 target 不在链表中 (已错过)

            if curr < target_addr {
                // Missed
                break;
            }

            if curr == target_addr {
                // Found adjacent free block
                // Check size
                let node_size = unsafe { (*curr).size };
                if node_size >= needed {
                    // Merge!
                    // 1. Remove 'curr' from free list
                    unsafe {
                        *prev = (*curr).next;
                    }

                    // 2. If 'curr' had extra space, put the remainder back
                    let remaining_in_node = node_size - needed;
                    if remaining_in_node >= NODE_SIZE {
                        unsafe {
                            // Create remainder node
                            let remainder_addr = (curr as *mut u8).add(needed) as *mut FreeListNode;
                            (*remainder_addr).size = remaining_in_node;

                            // Insert remainder back into list.
                            // Since remainder_addr > curr (sub-part of curr, higher address).
                            // And *prev points to Next (which is < curr).
                            // So Remainder > Next.
                            // Remainder should replace Curr's position.
                            (*remainder_addr).next = (*curr).next;
                            *prev = remainder_addr;
                        }
                    }
                    return ptr;
                }
                // Adjacent block exists but too small.
                break;
            }

            // Next
            unsafe {
                prev = ptr::addr_of_mut!((*curr).next);
            }
        }

        // Default Fallback: Alloc new, Copy, Dealloc old
        // 默认回退: Alloc new, Copy, Dealloc old
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

fn full_size(layout: Layout) -> usize {
    let grown = layout.size().max(NODE_SIZE);
    (grown + 15) & !15
}

/// Round up value to the nearest multiple of increment, where increment must be a power of 2.
/// If `value` is already a multiple of increment, it remains unchanged.
///
/// 将值向上取整到增量的最接近倍数，增量必须是 2 的幂。
/// 如果 `value` 是增量的倍数，则保持不变。
fn round_up(value: usize, increment: usize) -> usize {
    debug_assert!(increment.is_power_of_two());
    // Calculate `value.div_ceil(increment) * increment`,
    // utilizing the fact that `increment` is always a power of 2 to avoid integer division,
    // as it is not always optimized away.
    // 计算 `value.div_ceil(increment) * increment`，
    // 利用 `increment` 总是 2 的幂这一事实避免使用整数除法，
    // 因为它并不总是会被优化掉。
    multiple_below(value + (increment - 1), increment)
}

/// Round down value to the nearest multiple of increment, where increment must be a power of 2.
/// If `value` is a multiple of `increment`, it remains unchanged.
///
/// 将值向下取整到增量的最接近倍数，增量必须是 2 的幂。
/// 如果 `value` 是 `increment` 的倍数，则保持不变。
fn multiple_below(value: usize, increment: usize) -> usize {
    debug_assert!(increment.is_power_of_two());
    // Calculate `value / increment * increment`,
    // utilizing the fact that `increment` is always a power of 2 to avoid integer division,
    // as it is not always optimized away.
    // 计算 `value / increment * increment`，
    // 利用 `increment` 总是 2 的幂这一事实避免使用整数除法，
    // 因为它并不总是会被优化掉。
    value & increment.wrapping_neg()
}

unsafe fn offset_bytes(ptr: *mut FreeListNode, offset: usize) -> *mut FreeListNode {
    unsafe { (ptr as *mut u8).add(offset) as *mut FreeListNode }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reset_heap;

    struct SafeAllocator(FreeListAllocator);

    impl SafeAllocator {
        fn new() -> Self {
            Self(FreeListAllocator::new())
        }

        fn alloc(&self, layout: Layout) -> *mut u8 {
            unsafe { self.0.alloc(layout) }
        }

        fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { self.0.dealloc(ptr, layout) }
        }
    }

    impl Drop for SafeAllocator {
        fn drop(&mut self) {
            reset_heap();
        }
    }

    #[test]
    fn test_basic_allocation() {
        let allocator = SafeAllocator::new();
        let layout = Layout::new::<u64>();
        let ptr = allocator.alloc(layout);
        assert!(!ptr.is_null());

        unsafe {
            *ptr.cast::<u64>() = 0xDEADBEEF;
            assert_eq!(*ptr.cast::<u64>(), 0xDEADBEEF);
        }

        allocator.dealloc(ptr, layout);
    }

    #[test]
    fn test_allocation_order_descending() {
        let allocator = SafeAllocator::new();
        let layout = Layout::from_size_align(16, 16).unwrap();

        // 第一次分配，会触发 gro_memory (64KB)，并全部加到 FreeList (Head -> Node(size=64KB))
        // alloc 会找到这个大块，由于 size 小，它会在 high address 切个块出来给你？
        // 让我们看看 alloc 逻辑:
        // if remaining >= NODE_SIZE { (*node).size = remaining; return node + remaining; }
        // 意味着，它保留了 block 的低地址部分作为新的 FreeNode，返回了高地址部分给用户。
        // 所以第一个分配的地址应该是 PageEnd - 16。
        let ptr1 = allocator.alloc(layout);

        // 第二次分配，应当再次从那个剩余的 FreeNode (位于低地址) 的末尾切一块。
        // 所以 ptr2 应该在 ptr1 之前。
        let ptr2 = allocator.alloc(layout);

        assert!(!ptr1.is_null());
        assert!(!ptr2.is_null());

        // 指针地址应该递减：ptr1 > ptr2
        assert!(ptr1 > ptr2);

        // 它们应该是紧挨著的：Block: [... | ptr2 | ptr1 | end]
        assert_eq!(ptr1 as usize - ptr2 as usize, 16);
    }

    #[test]
    fn test_reuse() {
        let allocator = SafeAllocator::new();
        let layout = Layout::from_size_align(16, 16).unwrap();

        let ptr1 = allocator.alloc(layout);
        allocator.dealloc(ptr1, layout);

        // 释放后，ptr1 对应的块被放回 FreeList。
        // 因为它是刚刚分配出的最高地址块，且我们没有其他操作，它应该会被合并回大块，或者作为独立块。
        // 再次分配时，应该优先使用最高地址的块 (Search order)。
        // 如果合并回去，它就成了大块的一部分（高位），下次 alloc 切割时，会切同样的地址。
        // 如果没合并（中间有断开），它可能是独立的 Node。 Search 顺序是 Descending。
        // 大块在低地址，ptr1 在高地址。 Search 先遇到 ptr1 还是 大块？
        // FreeList 是 Descending。所以先遇到 ptr1。
        // 如果 ptr1 能满足大小，就用 ptr1。
        let ptr2 = allocator.alloc(layout);
        assert_eq!(ptr1, ptr2);
    }

    #[test]
    fn test_coalescing_merge() {
        let allocator = SafeAllocator::new();
        let layout = Layout::from_size_align(128, 16).unwrap();

        // 分配 3 个块。顺序：ptr1 (high) > ptr2 > ptr3 (low)
        let ptr1 = allocator.alloc(layout);
        let ptr2 = allocator.alloc(layout);
        let ptr3 = allocator.alloc(layout);

        assert_eq!(ptr1 as usize - ptr2 as usize, 128);
        assert_eq!(ptr2 as usize - ptr3 as usize, 128);

        // 释放中间的 (ptr2)。
        // 此时 FreeList: [...BigBlock..., ptr2] (如果没和其他合并)
        allocator.dealloc(ptr2, layout);

        // 释放顶部的 (ptr1)。
        // ptr1 > ptr2. ptr1 是高地址。
        // dealloc 插入 ptr1。它发现 ptr1 > ptr2 (Head).
        // 且 ptr2 + 128 == ptr1。应该导致 ptr2 (Node) 扩展大小，吞并 ptr1。
        // 结果: FreeList 中有一个合并后的块 [ptr2, ptr1] (Size 256).
        allocator.dealloc(ptr1, layout);

        // 释放底部的 (ptr3)。
        // ptr3 < ptr2.
        // 插入 ptr3。找到 Head (ptr2_ptr1_merged).
        // 检查: ptr3 + 128 == ptr2 ? Yes.
        // 合并：ptr3 变成新的 Head，Size += 256. -> [ptr3, ptr2, ptr1] (Size 384).
        allocator.dealloc(ptr3, layout);

        // 现在申请一个 384 字节的大块。
        // 它应该完美匹配我们刚刚合并出的那个块 (位于 ptr3)。
        let layout_large = Layout::from_size_align(384, 16).unwrap();
        let ptr_large = allocator.alloc(layout_large);

        assert!(!ptr_large.is_null());
        // 如果 alloc 逻辑是 "如果正好大小相等，就移除节点并返回整个节点指针"，
        // 那么应该返 ptr3。
        // 如果 alloc 逻辑是 "如果剩余 >= NODE_SIZE 才分割"，这里剩余 0，所以不分割。
        // 直接返回节点地址。
        assert_eq!(ptr_large, ptr3);
    }

    #[test]
    fn test_memory_growth_multi_page() {
        let allocator = SafeAllocator::new();
        // 分配 40KB。
        let layout = Layout::from_size_align(40 * 1024, 16).unwrap();
        let ptr1 = allocator.alloc(layout);
        assert!(!ptr1.is_null());

        // 再分配 40KB。当前页 (64KB) 剩 24KB，不够。
        // 触发 grow_memory。
        // 分配器会申请新页。返回新页的高地址部分。
        let ptr2 = allocator.alloc(layout);
        assert!(!ptr2.is_null());
        assert_ne!(ptr1, ptr2);

        // 两个指针应该相距较远 (至少 40KB)
        let dist = if ptr1 > ptr2 {
            ptr1 as usize - ptr2 as usize
        } else {
            ptr2 as usize - ptr1 as usize
        };
        assert!(dist >= 40 * 1024);
    }

    #[test]
    fn test_alignment_large() {
        let allocator = SafeAllocator::new();
        // 目前实现只支持 max align 16。
        // 如果请求 32，应该返回 null。 (代码里写了 check)
        let layout_bad = Layout::from_size_align(32, 32).unwrap();
        let ptr = allocator.alloc(layout_bad);
        assert!(ptr.is_null());
    }

    #[test]
    fn test_fragmentation_fill_hole() {
        let allocator = SafeAllocator::new();
        let layout = Layout::from_size_align(64, 16).unwrap();

        // Pointers decrease: p1 > p2 > p3
        let _p1 = allocator.alloc(layout);
        let p2 = allocator.alloc(layout);
        let p3 = allocator.alloc(layout);

        // Free p2. 制造一个 64B 的空洞。
        allocator.dealloc(p2, layout);

        // 再分配 64B。
        // Allocator 策略是遍历 FreeList (Descending)。
        // 列表里有: [Head (Rest of Memory at Low Address), p2 (at Higher Address)]
        // p2 > Head. 所以 p2 排在前面。
        // alloc 查 p2，大小正好。
        // 应该重用 p2。

        let p4 = allocator.alloc(layout);
        assert_eq!(p4, p2);

        // 确保 p3 还没被分配出去
        let _p5 = allocator.alloc(layout);
        // p5 应该是从 Head (Low Address) 切出来的，应该小于 p3
        assert!(_p5 < p3);
    }
    #[test]
    fn test_realloc_shrink_in_place() {
        let allocator = SafeAllocator::new();
        let layout = Layout::from_size_align(128, 16).unwrap();
        let ptr = allocator.alloc(layout);
        assert!(!ptr.is_null());

        unsafe {
            ptr.write_bytes(0xAA, layout.size());
        }

        // Shrink to 64
        let new_size = 64;
        let new_layout = Layout::from_size_align(new_size, 16).unwrap();

        // We need to call realloc from GlobalAlloc trait
        let realloc_ptr = unsafe { allocator.0.realloc(ptr, layout, new_size) };

        // Custom impl is In-Place.
        #[cfg(feature = "realloc")]
        assert_eq!(ptr, realloc_ptr);

        #[cfg(not(feature = "realloc"))]
        assert_ne!(ptr, realloc_ptr);

        unsafe {
            assert_eq!(*realloc_ptr.cast::<u8>(), 0xAA);
        }

        allocator.dealloc(realloc_ptr, new_layout);
    }

    #[test]
    fn test_realloc_grow_in_place() {
        let allocator = SafeAllocator::new();
        let layout = Layout::from_size_align(64, 16).unwrap();

        let ptr1 = allocator.alloc(layout);
        let ptr2 = allocator.alloc(layout);

        assert_eq!(ptr1 as usize - ptr2 as usize, 64);

        allocator.dealloc(ptr1, layout);

        let new_size = 128;
        let ptr2_new = unsafe { allocator.0.realloc(ptr2, layout, new_size) };

        #[cfg(feature = "realloc")]
        assert_eq!(ptr2, ptr2_new);

        #[cfg(not(feature = "realloc"))]
        assert_ne!(ptr2, ptr2_new);

        allocator.dealloc(ptr2_new, Layout::from_size_align(new_size, 16).unwrap());
    }
}
