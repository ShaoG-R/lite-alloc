#![no_main]
use libfuzzer_sys::fuzz_target;
use lite_alloc::{reset_heap, single_threaded::SegregatedBumpAllocator};
use std::alloc::{GlobalAlloc, Layout};

fuzz_target!(|data: &[u8]| {
    // Reset global state and mock heap memory before each Fuzz iteration
    // 每次 Fuzz 迭代开始前，重置全局状态和模拟堆内存
    unsafe {
        SegregatedBumpAllocator::reset();
        reset_heap();
    }

    let allocator = SegregatedBumpAllocator::new();
    // Record allocated blocks: (pointer, layout)
    // 记录已分配的块：(pointer, layout)
    let mut ptrs: Vec<(*mut u8, Layout)> = Vec::new();

    let mut cursor = 0;
    while cursor < data.len() {
        // Read opcode (1 byte)
        // 读取操作码 (1 byte)
        let op = data[cursor];
        cursor += 1;

        if op % 2 == 0 {
            // --- Alloc ---
            // Need 2 bytes for size
            // 需要 2 bytes 作为 size
            if cursor + 2 > data.len() {
                break;
            }
            let s1 = data[cursor] as usize;
            let s2 = data[cursor + 1] as usize;
            cursor += 2;

            // Combine into usize, max 65535, min 1
            // 组合成 usize, 最大 65535, 最小 1
            let size = ((s2 << 8) | s1).max(1);

            // Fixed Alignment = 8 (common alignment)
            // 固定 Alignment = 8 (常见的对齐)
            let align = 8;

            if let Ok(layout) = Layout::from_size_align(size, align) {
                let ptr = unsafe { allocator.alloc(layout) };
                if !ptr.is_null() {
                    // Write some data to trigger potential out-of-bounds write detection
                    // 写入一些数据以触发潜在的越界写入检测
                    unsafe {
                        std::ptr::write_bytes(ptr, 0xCC, size);
                    }
                    ptrs.push((ptr, layout));
                }
            }
        } else {
            // --- Dealloc ---
            if ptrs.is_empty() {
                continue;
            }

            // Need 1 byte for index
            // 需要 1 byte 作为索引
            if cursor + 1 > data.len() {
                break;
            }
            let idx_byte = data[cursor] as usize;
            cursor += 1;

            // Map random byte to ptrs range
            // 将随机 byte 映射到 ptrs 范围
            let idx = idx_byte % ptrs.len();
            let (ptr, layout) = ptrs.swap_remove(idx);
            unsafe {
                allocator.dealloc(ptr, layout);
            }
        }
    }

    // Iteration ends, free remaining objects
    // 迭代结束，释放剩余的对象
    for (ptr, layout) in ptrs {
        unsafe {
            allocator.dealloc(ptr, layout);
        }
    }
});
