# 使用官方 Rust Nightly 镜像，因为 cargo-fuzz 需要 nightly 工具链
FROM rustlang/rust:nightly

# 设置工作目录
WORKDIR /app

# 安装 cargo-fuzz
RUN cargo install cargo-fuzz

# 复制项目文件
# 注意：请确保目录下有 .dockerignore 忽略 target/ 等目录，以加快构建速度
COPY . .

# 预先编译 fuzz target 以便缓存依赖（可选，但推荐）
# 这里我们尝试先编译一下，如果不成功也没关系，运行时会编译
# 使用 --sanitizer address 是默认选项，通常最有用
RUN cargo fuzz build --sanitizer address || true

# 默认测试所有 Fuzz Target (默认配置 & realloc 配置)
# 遍历每个 target，分别运行默认和开启 realloc feature 的情况
CMD ["bash", "-c", "targets=\"freelist bump_freelist segregated_bump\"; \
    for target in $targets; do \
    echo \"\n\n[+] Testing $target (Default Features) ...\"; \
    cargo fuzz run $target -- -max_total_time=60 || exit 1; \
    echo \"\n\n[+] Testing $target (Feature: realloc) ...\"; \
    cargo fuzz run $target --features realloc -- -max_total_time=60 || exit 1; \
    done"]
