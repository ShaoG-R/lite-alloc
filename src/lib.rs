#![no_std]

#[cfg(not(target_arch = "wasm32"))]
extern crate std;

#[cfg(not(target_arch = "wasm32"))]
extern crate alloc;

/// WebAssembly memory page count.
///
/// WebAssembly 内存页数量。
#[derive(Eq, PartialEq, Debug, Clone, Copy)]
pub struct PageCount(pub usize);

impl PageCount {
    pub fn size_in_bytes(self) -> usize {
        self.0 * PAGE_SIZE
    }
}

/// WebAssembly page size, in bytes (64KB).
///
/// WebAssembly 页大小，单位字节（64KB）。
pub const PAGE_SIZE: usize = 65536;

// Remove MemoryGrower trait, use function directly
// 移除 trait MemoryGrower，直接写成函数
#[cfg(target_arch = "wasm32")]
#[inline(always)]
pub unsafe fn grow_memory(pages: usize) -> usize {
    core::arch::wasm32::memory_grow(0, pages)
}

#[cfg(not(target_arch = "wasm32"))]
mod host_memory {
    use super::PAGE_SIZE;
    use std::alloc::{Layout, alloc, dealloc};
    use std::cell::RefCell;
    use std::ptr;

    // Simulate 128MB of addressable WASM memory space per thread
    // 模拟每线程 128MB 的可寻址 WASM 内存空间
    const MOCK_MEMORY_SIZE: usize = 128 * 1024 * 1024;

    struct MockMemory {
        base_ptr: *mut u8,
        current_pages: usize,
    }

    impl MockMemory {
        fn new() -> Self {
            unsafe {
                let layout = Layout::from_size_align(MOCK_MEMORY_SIZE, PAGE_SIZE).unwrap();
                let ptr = alloc(layout);
                if ptr.is_null() {
                    // Panic immediately if we can't allocate the mock heap
                    // 如果无法分配模拟堆，立即 Panic
                    panic!("Failed to allocate mock WASM memory");
                }
                // Initialize memory to zero, similar to WASM behavior
                // 将内存初始化为零，类似于 WASM 行为
                ptr::write_bytes(ptr, 0, MOCK_MEMORY_SIZE);
                Self {
                    base_ptr: ptr,
                    current_pages: 0,
                }
            }
        }
    }

    impl Drop for MockMemory {
        fn drop(&mut self) {
            unsafe {
                let layout = Layout::from_size_align(MOCK_MEMORY_SIZE, PAGE_SIZE).unwrap();
                dealloc(self.base_ptr, layout);
            }
        }
    }

    // Thread local storage for the mock memory
    // 模拟内存的线程局部存储
    std::thread_local! {
        static MEMORY: RefCell<MockMemory> = RefCell::new(MockMemory::new());
    }

    pub unsafe fn grow_memory_impl(pages: usize) -> usize {
        MEMORY.with(|mem| {
            let mut mem = mem.borrow_mut();

            // Check if we have enough space in our pre-allocated buffer
            // 检查预分配缓冲区中是否有足够的空间
            if (mem.current_pages + pages) * PAGE_SIZE > MOCK_MEMORY_SIZE {
                return usize::MAX;
            }

            // Calculate the start of the new memory block as an absolute address
            // 计算新内存块的起始绝对地址
            let start_addr = mem.base_ptr as usize + mem.current_pages * PAGE_SIZE;

            // Since our allocators expect the return value to be (Address / PAGE_SIZE),
            // and they will reconstruct the address by (RetVal * PAGE_SIZE),
            // we must return the absolute page index.
            // 由于我们的分配器期望返回值为 (Address / PAGE_SIZE)，
            // 并且它们将通过 (RetVal * PAGE_SIZE) 重建地址，
            // 我们必须返回绝对页索引。
            let ret_page_index = start_addr / PAGE_SIZE;

            // Advance the usage counter
            // 增加使用计数
            mem.current_pages += pages;

            // Zero out the newly allocated pages (emulate WASM grow behavior)
            // Note: We zeroed the whole block on init, but reset_memory might not zero everything
            // if we optimize it. Let's ensure it's zeroed here if we change reset policy.
            // For now, init zeros everything and reset zeros used, so it's fine.
            // 将新分配的页面清零（模拟 WASM 增长行为）
            // 注意：我们在初始化时将整个块清零，但如果进行优化，reset_memory 可能不会清零所有内容。
            // 如果我们要更改重置策略，请确保在此处清零。
            // 目前，初始化会清零所有内容，重置会清零已使用的内容，所以没问题。

            ret_page_index
        })
    }

    pub unsafe fn reset_memory() {
        MEMORY.with(|mem| {
            let mut mem = mem.borrow_mut();
            // Zero out the used memory so the next test starts clean
            // 将已使用的内存清零，以便下一个测试从干净的状态开始
            let used_bytes = mem.current_pages * PAGE_SIZE;
            if used_bytes > 0 {
                unsafe {
                    ptr::write_bytes(mem.base_ptr, 0, used_bytes);
                }
            }
            mem.current_pages = 0;
        });
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub unsafe fn grow_memory(pages: usize) -> usize {
    unsafe { host_memory::grow_memory_impl(pages) }
}

/// For Test/Bench only: Reset the mock heap memory of the current thread
///
/// 仅用于测试/Bench：重置当前线程的模拟堆内存
#[cfg(not(target_arch = "wasm32"))]
pub fn reset_heap() {
    unsafe {
        host_memory::reset_memory();
    }
}

pub mod single_threaded {
    mod bump_freelist;
    mod freelist;
    mod segregated_bump;

    pub use bump_freelist::BumpFreeListAllocator;
    pub use freelist::FreeListAllocator;
    pub use segregated_bump::SegregatedBumpAllocator;
}
