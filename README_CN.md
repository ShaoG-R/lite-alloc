# Lite Alloc

![License](https://img.shields.io/badge/license-MIT-blue.svg)

Lite Alloc 是一个为 Rust 编写的轻量级、单线程内存分配器库，专为 **WebAssembly (Wasm)** 和**嵌入式系统**设计。它的核心目标是在单线程环境中提供最小的代码体积（binary footprint）和最高的性能。

> **警告**：这些分配器是**单线程**的。虽然它们实现了 `Sync` trait 以满足 `GlobalAlloc` 的接口要求，但在多线程环境中使用会导致未定义行为 (UB)。请仅在 Wasm 或特定的单线程嵌入式环境中使用。
>
> **注意**：本项目目前处于**实验阶段**。尽管核心功能已经实现，但尚未进行全面的测试覆盖。在生产环境中使用前请务必谨慎测试。

[English Documentation](./README.md)

## 分配策略

Lite Alloc 提供了三种不同的分配器实现，你可以根据具体的应用场景，在代码体积、性能和内存效率之间做出最佳权衡。

### 1. `BumpFreeListAllocator`
结合了 Bump Pointer（指针碰撞）和无序空闲链表的极简分配器。

-   **优点**：
    -   **极致的代码体积**。
    -   **快速分配**：Bump 分配为 O(1)，复用为 O(N)。
    -   **零开销**：无需初始化。
-   **缺点**：
    -   **碎片化**：不会合并（coalesce）相邻的空闲块。长期运行可能导致内存碎片化从而 OOM。
-   **适用场景**：短生命周期的任务、Serverless 函数、或者对二进制体积有严格要求的场景。

### 2. `SegregatedBumpAllocator`
混合分配器，使用隔离空闲链表（分箱/Segregated Free Lists）处理小对象，Bump Pointer 处理大对象。

-   **特性**：
    -   为 16B, 32B, 64B, 和 128B 的小对象提供专用固定桶。
    -   大对象（> 128B）回退到 Bump Pointer 分配（且**不会被复用**）。
-   **优点**：
    -   小对象的分配和释放均为严格的 **O(1)**。
    -   非常适合大量小对象分配的负载。
-   **缺点**：
    -   大对象无法复用，内存消耗可能较高。
-   **适用场景**：已知分配模式（大量小对象）的 Wasm Serverless 函数或脚本。

### 3. `FreeListAllocator`
通用的、支持内存块合并的有序空闲链表分配器。

-   **特性**：
    -   维护一个按内存地址排序的空闲链表。
    -   **合并（Coalescing）**：在释放时自动合并相邻的空闲块，以减少碎片。
-   **优点**：
    -   **高内存效率**：能够有效回收和合并内存。
    -   适合需要长期运行的程序。
-   **缺点**：
    -   相比 Bump 类分配器，分配和释放速度较慢（搜索链表 O(N)）。
    -   代码体积稍大。
-   **适用场景**：通用的、需要长期运行且关注内存复用的应用程序。

## 使用方法

将 `lite-alloc` 添加到你的 `Cargo.toml` 中。

```toml
[dependencies]
lite-alloc = "0.1.0"
```

在 `no_std` / Wasm 项目中将其中一个配置为全局分配器：

```rust
use lite_alloc::single_threaded::BumpFreeListAllocator;

#[global_allocator]
static ALLOCATOR: BumpFreeListAllocator = BumpFreeListAllocator::new();

fn main() {
    // 你的代码
}
```

或者选择其他策略：

```rust
use lite_alloc::single_threaded::{FreeListAllocator, SegregatedBumpAllocator};

// 使用 FreeListAllocator 用于通用场景
#[global_allocator]
static ALLOCATOR: FreeListAllocator = FreeListAllocator::new();

// 或者使用 SegregatedBumpAllocator 用于极小任务
// #[global_allocator]
// static ALLOCATOR: SegregatedBumpAllocator = SegregatedBumpAllocator::new();
```

## 许可证

本项目采用 MIT 许可证。详情请参阅 [LICENSE](./LICENSE) 文件。
