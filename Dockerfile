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
RUN cargo fuzz build fuzz_target_1 --sanitizer address || true

# 设置默认命令
CMD ["cargo", "fuzz", "run", "fuzz_target_1", "--", "-max_total_time=60"]
