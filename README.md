# Lite Alloc

![License](https://img.shields.io/badge/license-MIT-blue.svg)

Lite Alloc is a lightweight, single-threaded memory allocator library for Rust, specifically designed for **WebAssembly (Wasm)** and **embedded systems**. It focuses on minimizing code size (binary footprint) and maximizing performance in single-threaded environments.

> **Warning**: These allocators are **single-threaded**. While they implement `Sync` to satisfy the `GlobalAlloc` trait, using them in a multi-threaded environment will result in Undefined Behavior (UB). Use them only in environments typically known to be single-threaded, such as Wasm or specific embedded targets.
>
> **Note**: This project is currently **experimental**. While core functionality is implemented, it has not yet undergone comprehensive test coverage (including edge case validation). Please test thoroughly before using in production environments.

[中文文档](./README_CN.md)

## Allocator Strategies

Lite Alloc provides three distinct allocator implementations, allowing you to choose the best trade-off between code size, performance, and memory efficiency for your specific use case.

### 1. `BumpFreeListAllocator`
A minimalist allocator combining a Bump Pointer with an unsorted Free List.

-   **Pros**:
    -   **Extremely small binary size**.
    -   **Fast allocation**: O(1) for bump allocation, O(N) for reuse.
    -   **Zero overhead**: No initialization cost.
-   **Cons**:
    -   **Fragmentation**: Does not merge (coalesce) adjacent free blocks. Long-running applications may eventually run out of memory (OOM) due to fragmentation.
-   **Best For**: Short-lived tasks, Serverless functions, or applications where code size is the critical constraint.

### 2. `SegregatedBumpAllocator`
A hybrid allocator using Segregated Free Lists (Bins) for small objects and a Bump Pointer for large objects.

-   **Features**:
    -   Fixed bins for: 16B, 32B, 64B, and 128B.
    -   Large objects (> 128B) fallback to a simple Bump Pointer (and are **not reused**).
-   **Pros**:
    -   **O(1) Allocation/Deallocation** for small objects.
    -   Very fast for workloads dominated by small, fixed-size allocations.
-   **Cons**:
    -   Larger memory footprint for large objects (no reuse).
-   **Best For**: Wasm Serverless functions or scripts with known allocation patterns (lots of small objects).

### 3. `FreeListAllocator`
A general-purpose allocator using a sorted linked list with block coalescing.

-   **Features**:
    -   Maintains a free list sorted by memory address.
    -   **Coalescing**: Merges adjacent free blocks upon deallocation to reduce fragmentation.
-   **Pros**:
    -   **High Memory Efficiency**: efficiently reclaims and merges memory.
    -   Suitable for long-running applications.
-   **Cons**:
    -   Slower allocation/deallocation (O(N) search) compared to the Bump allocators.
    -   Slightly larger code size.
-   **Best For**: General-purpose long-running applications where memory reuse is critical.

## Usage

Add `lite_alloc` to your `Cargo.toml`.

To use one of the allocators as your global allocator in a `no_std` / Wasm project:

```rust
use lite_alloc::single_threaded::BumpFreeListAllocator;

#[global_allocator]
static ALLOCATOR: BumpFreeListAllocator = BumpFreeListAllocator::new();

fn main() {
    // Your code here
}
```

Or choose another strategy:

```rust
use lite_alloc::single_threaded::{FreeListAllocator, SegregatedBumpAllocator};

// Use FreeListAllocator for general purpose
#[global_allocator]
static ALLOCATOR: FreeListAllocator = FreeListAllocator::new();

// OR Use SegregatedBumpAllocator for tiny tasks
// #[global_allocator]
// static ALLOCATOR: SegregatedBumpAllocator = SegregatedBumpAllocator::new();
```

## License

This project is licensed under the MIT License. See the [LICENSE](./LICENSE) file for details.
