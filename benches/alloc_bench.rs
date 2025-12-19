use criterion::{Criterion, criterion_group, criterion_main};
use lite_alloc::single_threaded::{
    BumpFreeListAllocator, FreeListAllocator, SegregatedBumpAllocator,
};
use std::alloc::{GlobalAlloc, Layout};
use std::vec::Vec;

// ============================================================================
// Test Infrastructure
// ============================================================================

// Simple LCG (Linear Congruential Generator) for deterministic "random" sizes
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        SimpleRng { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }

    fn range(&mut self, min: usize, max: usize) -> usize {
        let r = self.next_u64() as usize;
        min + (r % (max - min))
    }
}

trait BenchmarkAllocator: GlobalAlloc {
    unsafe fn reset_env();
    fn create() -> Self;
}

impl BenchmarkAllocator for FreeListAllocator {
    unsafe fn reset_env() {
        lite_alloc::reset_heap();
        unsafe { FreeListAllocator::reset() };
    }
    fn create() -> Self {
        FreeListAllocator::new()
    }
}

impl BenchmarkAllocator for BumpFreeListAllocator {
    unsafe fn reset_env() {
        lite_alloc::reset_heap();
        unsafe {
            BumpFreeListAllocator::reset();
        };
    }
    fn create() -> Self {
        BumpFreeListAllocator::new()
    }
}

impl BenchmarkAllocator for SegregatedBumpAllocator {
    unsafe fn reset_env() {
        lite_alloc::reset_heap();
        unsafe { SegregatedBumpAllocator::reset() };
    }
    fn create() -> Self {
        SegregatedBumpAllocator::new()
    }
}

fn bench_fn_simple_cycle<A: BenchmarkAllocator>(b: &mut criterion::Bencher) {
    unsafe {
        A::reset_env();
    }
    let allocator = A::create();
    b.iter(|| {
        let layout = Layout::new::<u64>();
        unsafe {
            let ptr = allocator.alloc(layout);
            allocator.dealloc(ptr, layout);
        }
    })
}

fn bench_fn_fragmentation<A: BenchmarkAllocator>(b: &mut criterion::Bencher) {
    unsafe {
        A::reset_env();
    }
    let allocator = A::create();
    b.iter(|| {
        let mut rng = SimpleRng::new(0xDEADBEEF);
        let mut ptrs = Vec::with_capacity(1000);

        // 1. Allocate a bunch of random objects
        for _ in 0..500 {
            let size = rng.range(8, 256);
            let align = 8;
            let layout = Layout::from_size_align(size, align).unwrap();
            unsafe {
                let ptr = allocator.alloc(layout);
                ptrs.push((ptr, layout));
            }
        }

        // 2. Free every other one (create fragmentation)
        let mut survivors = Vec::with_capacity(500);
        for (i, (ptr, layout)) in ptrs.into_iter().enumerate() {
            if i % 2 == 0 {
                unsafe {
                    allocator.dealloc(ptr, layout);
                }
            } else {
                survivors.push((ptr, layout));
            }
        }

        // 3. Allocate more objects (some should fill holes, some grow)
        for _ in 0..200 {
            let size = rng.range(8, 128);
            let align = 8;
            let layout = Layout::from_size_align(size, align).unwrap();
            unsafe {
                let ptr = allocator.alloc(layout);
                survivors.push((ptr, layout));
            }
        }

        // 4. Cleanup everything
        for (ptr, layout) in survivors {
            unsafe {
                allocator.dealloc(ptr, layout);
            }
        }
    })
}

fn bench_fn_sequential<A: BenchmarkAllocator>(b: &mut criterion::Bencher) {
    unsafe {
        A::reset_env();
    }
    let allocator = A::create();
    b.iter(|| {
        let count = 1000;
        let layout = Layout::new::<u64>();
        let mut ptrs = Vec::with_capacity(count);

        // Batch Alloc
        for _ in 0..count {
            unsafe {
                ptrs.push(allocator.alloc(layout));
            }
        }

        // Batch Dealloc (LIFO)
        while let Some(ptr) = ptrs.pop() {
            unsafe {
                allocator.dealloc(ptr, layout);
            }
        }
    })
}

// ============================================================================
// Benchmark Groups
// ============================================================================

fn bench_group_simple_cycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("simple_alloc_dealloc_cycle");
    group.bench_function("FreeList", bench_fn_simple_cycle::<FreeListAllocator>);
    group.bench_function(
        "BumpFreeList",
        bench_fn_simple_cycle::<BumpFreeListAllocator>,
    );
    group.bench_function(
        "SegregatedBump",
        bench_fn_simple_cycle::<SegregatedBumpAllocator>,
    );
    group.finish();
}

fn bench_group_fragmentation(c: &mut Criterion) {
    let mut group = c.benchmark_group("fragmentation_workload");
    group.bench_function("FreeList", bench_fn_fragmentation::<FreeListAllocator>);
    group.bench_function(
        "BumpFreeList",
        bench_fn_fragmentation::<BumpFreeListAllocator>,
    );
    group.bench_function(
        "SegregatedBump",
        bench_fn_fragmentation::<SegregatedBumpAllocator>,
    );
    group.finish();
}

fn bench_group_sequential(c: &mut Criterion) {
    let mut group = c.benchmark_group("ideal_sequential_batch");
    group.bench_function("FreeList", bench_fn_sequential::<FreeListAllocator>);
    group.bench_function("BumpFreeList", bench_fn_sequential::<BumpFreeListAllocator>);
    group.bench_function(
        "SegregatedBump",
        bench_fn_sequential::<SegregatedBumpAllocator>,
    );
    group.finish();
}

criterion_group!(
    benches,
    bench_group_simple_cycle,
    bench_group_fragmentation,
    bench_group_sequential
);
criterion_main!(benches);
