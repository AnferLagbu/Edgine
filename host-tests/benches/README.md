# Framekernel Bench Suite (M6.9)

## 目的

为框内核 (framekernel) 的关键路径建立性能基线, 在 CI 中持续检测回归.

涵盖 10 个关键路径:

| 名称                  | 分类    | 来源                                |
|----------------------|--------|------------------------------------|
| page_flags_bits      | mm     | privileged/mm PageFlags 位运算      |
| pte_set_flags        | mm     | privileged/mm PTE 标志位操作          |
| iomem_alias_check    | iomem  | privileged/iomem AliasRegistry 冲突检查 |
| capability_check     | sgeg  | privileged/sgeg 能力位检查           |
| dma_state_machine    | dma    | privileged/dma_buf SyncState 转换    |
| sha256_block         | sgeg  | privileged/sgeg/sha256 块压缩        |
| attribution_classify | freg| functions/freg/attribution 分类   |
| recovery_decide      | freg| functions/freg/recovery_policy 决策|
| bitmap_scan          | pmm    | 物理页分配 (Bitmap)                 |
| btree_id_lookup      | proc   | 进程表 PID 查找                     |

## 编译与运行

```bash
# 编译 (release 必须, debug 下数字无意义)
cd host-tests && cargo build --release --bin framekernel-bench

# 直接运行 (输出 JSON 到 stdout)
cargo run --release --bin framekernel-bench

# 记录基线
python3 scripts/record_bench_baseline.py
# 自定义输出: python3 scripts/record_bench_baseline.py path/to/baseline.json

# 回归检查 (默认阈值 15% + 1ns 绝对噪声过滤)
python3 scripts/check_bench_regression.py
# 自定义阈值: python3 scripts/check_bench_regression.py --threshold 0.10
```

## 测量约定

- `iters`: 自适应放大, 直到总耗时 >= 50ms 或达到 10M ops 上限
- `total_ns`: 单次 `f()` 调用的总耗时
- `ns_per_op`: `total_ns / iters` (整数 ns, 向下取整)
- `ns_per_op_frac`: 浮点 ns, 保留亚纳秒精度 (推荐用于跨运行对比)
- `ps_per_op`: 整数 ps, 精确 (用于整数比较)
- `ops_per_sec`: 吞吐量

## 噪声过滤

`check_bench_regression.py` 使用两层过滤避免假阳性:

1. **零值过滤**: baseline 或 current 为 0.0 视为编译器完全优化掉的工作, 跳过
2. **绝对差过滤**: `|current - baseline| < 1.0ns` 视为亚纳秒测量噪声, 跳过 (不论相对差多大)
3. **相对差过滤**: 通过 1+2 后, `delta_pct > threshold` (默认 15%) 才算回归

零值条目说明: release 模式下, 一些超简单操作 (位运算 / 简单分支) 会被 LLVM 完全优化, 测量结果为 0.0 ns/op. 这些条目基线本身就不稳定, 不参与回归判定.

## CI 集成

`Makefile.ci` 新增两个目标:

- `make -f Makefile.ci ci-bench` — 自动检测 baseline.json 是否存在, 不存在则先记录; 存在则运行回归检查
- `make -f Makefile.ci ci-bench-record` — 主动重新记录基线 (性能优化 / 算法调整后使用)
- `make -f Makefile.ci ci` — 全量 CI, 包含 ci-bench

## 注意事项

- bench 函数中的常量 batch 大小 (如 `BATCH: u64 = 64`) 是为了让单次 iter 有可测量的耗时
- 单次 iter 过快 (亚纳秒) 时, 自适应逻辑会放大 iters; 但单 op 本身在 release 下仍可能被优化掉
- `black_box` 用于防止编译器消除 sink / 输入的副作用
- 真实的 SHA-256 块压缩在未优化代码中需要 ~10ns/块, 这里测得 0.025ns 是 release 模式 + 零输入的极限情形

## 重新记录基线的时机

1. 关键路径算法改进后 (预期性能提升)
2. 重大依赖升级 (例如换用新的 bitflags 版本)
3. CPU / 编译器升级 (CI runner 变更)
4. baseline.json 本身被误改 / 损坏

## 文件

- `baseline.json` — 当前基线 (机器 / 编译器 / 时间戳 + 10 条 ns_per_op_frac)
- `../src/framekernel_bench.rs` — bench 实现 + 11 个单元测试
- `../../scripts/record_bench_baseline.py` — 记录基线
- `../../scripts/check_bench_regression.py` — 回归检查
