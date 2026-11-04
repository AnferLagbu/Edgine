# Framekernel Bench Suite (M6.9)

## 目的

为框内核 (framekernel) 的关键路径建立性能基线, 在 CI 中以**绝对预算**判定性能指标.

涵盖 24 个关键路径:

| 名称                  | 分类    | 来源                                |
|----------------------|--------|------------------------------------|
| page_flags_bits      | mm     | privileged/mm PageFlags 位运算      |
| pte_set_flags        | mm     | privileged/mm PTE 标志位操作          |
| iomem_alias_check    | iomem  | privileged/iomem AliasRegistry 冲突检查 |
| capability_check     | sgeg   | privileged/sgeg 能力位检查           |
| dma_state_machine    | dma    | privileged/dma_buf SyncState 转换    |
| sha256_block         | sgeg   | privileged/sgeg/sha256 块压缩        |
| attribution_classify | freg   | functions/freg/attribution 分类      |
| recovery_decide      | freg   | functions/freg/recovery_policy 决策  |
| bitmap_scan          | pmm    | 物理页分配 (Bitmap)                  |
| eevdf_pick_requeue   | sched  | privileged/proc CfsRunQueue 选择路径 (E-07) |
| socket_wait_queue    | net    | net 等待队列唤醒与移除                |
| virtio_blk_io        | storage| virtio 块设备队列操作                |
| bpf_verifier_dispatch| ebpf   | eBPF 验证器 trait 派发               |
| sysctl_rw            | config | sysctl 注册/写入                    |
| blk_dev_dispatch     | block  | BlockDevice trait 派发              |
| vfs_poll_dispatch    | epoll  | VfsPollPolicy 派发                  |
| zap_dispatch         | egfs   | EGFS ZAP 派发                       |
| txg_dispatch         | egfs   | EGFS TXG 派发                       |
| dmu_dispatch         | egfs   | EGFS DMU 派发                       |
| spa_dispatch         | egfs   | EGFS SPA 派发                       |
| raidz_dispatch       | egfs   | EGFS RAID-Z 几何查询                 |
| arc_dispatch         | egfs   | EGFS ARC 缓存                       |
| zil_log_dispatch     | egfs   | EGFS ZIL 日志                       |
| zil_persist_dispatch | egfs   | EGFS ZIL 持久化                     |

## 编译与运行

```bash
# 编译 (release 必须, debug 下数字无意义)
cd host-tests && cargo build --release --bin framekernel-bench

# 直接运行 (输出 JSON 到 stdout)
cargo run --release --bin framekernel-bench

# 记录基线 (重录请用 --runs 5: 单样本会把宿主抖动钉成判据)
python3 scripts/record_bench_baseline.py --runs 5
# 自定义输出: python3 scripts/record_bench_baseline.py --out path/to/baseline.json
# --runs 缺省 1 = 单样本透传 (历史行为); 聚合器自检 (不跑 bench): --self-test

# 性能指标判定 (release 实测 vs baseline.json 的 budget_ns_per_op)
make -f Makefile.ci ci-bench
# 或在 audit 的 full 档里作为步骤 2f 自动执行
bash ci/audit.sh full
```

## 测量约定

- `iters`: 由每条 bench 的调用点写死 (1000 / 10000 / 100000), **没有**自适应放大——
  `measure()` 只做「一次预热 + 一次计时」 (`docs/plan/archive/framekernel-bench-measure-fix.md`
  的 G-07 收尾轮把双重归一化修掉了). 总测量窗 = `iters × 单操作耗时`, 24 项里 13 项 < 1ms
- `total_ns`: 计时轮 `f()` 调用的实测墙钟
- `ns_per_op`: `total_ns / iters` (整数 ns, 向下取整)
- `ns_per_op_frac`: 浮点 ns, 保留亚纳秒精度 (判定与预算派生都取这个字段)
- `ps_per_op`: 整数 ps, 精确 (与 `ns_per_op_frac` 由同一次读数派生)
- `ops_per_sec`: 吞吐量

## 判定口径: 每项一个绝对预算

判据在 `host-tests/tests/perf_metric_test.rs`, 不在 python 脚本里——本腿是**测试项**,
随时可以单独跑并看到整表读数, 不再是一个只报"回归/没回归"的门禁.

1. **预算来源**: `record_bench_baseline.py` 按「中位读数 × 10, 向上取整到两位有效数字」
   给每项钉 `budget_ns_per_op`, 与读数同批写入 `baseline.json`. 倍数只在记录器一处定义,
   测试侧真 import 该函数逐条对账 (预算被手改松或紧都会红).
2. **判定**: release 实测 `ns_per_op_frac > budget_ns_per_op` 即该条违规;
   基线缺条目与条目已从 bench 消失**双向都判红** (覆盖面不能靠删条目变干净).
3. **为什么不是相对 15%**: 相对判据把宿主当下状态读成回归. fc45 实测同一份代码交替 5 轮
   ⇒ 旧基线 2/5 红、中位数基线 3/5 红, 红项随宿主 load 0.91–1.83 轮换; 短窗条目 5 次采样
   摆幅可达 1.43 倍, 而唯一 ≥10ms 窗的 `bitmap_scan` (45ms) 也出现过 +20% 摆幅. ×10 只拦
   数量级走样 (真劣化通常是 ×100 级: 热路径多出一次分配、循环由 O(n) 变 O(n²)).
4. **重录即重钉预算**: 换 CPU / 编译器后跑 `make -f Makefile.ci ci-bench-record`,
   预算随本次中位一起重算——预算不跟着读数走就会要么恒红要么恒绿.

## CI 集成

`Makefile.ci` 的两个目标:

- `make -f Makefile.ci ci-bench` — 运行性能指标测试腿 (`--release --test perf_metric_test`);
  baseline.json 不存在时先按每项 5 次采样自动记录
- `make -f Makefile.ci ci-bench-record` — 重新记录读数与预算 (每项 5 次采样取中位)
- `make -f Makefile.ci ci` / `bash ci/audit.sh full` — 全量验收, 本腿作为 audit 步骤 2f 亦执行

## 注意事项

- bench 函数中的常量 batch 大小 (如 `BATCH: u64 = 64`) 是为了让单次 iter 有可测量的耗时
- 单次 iter 过快 (亚纳秒) 时**不会**放大 iters (无自适应逻辑), 故总测量窗 = `iters × 单 op`
  可能只有亚毫秒级, 读数抖动偏大——这正是判定改用绝对预算而非相对差的原因
- `black_box` 用于防止编译器消除 sink / 输入的副作用
- 真实的 SHA-256 块压缩在未优化代码中需要 ~10ns/块, 这里测得 0.025ns 是 release 模式 + 零输入的极限情形

## 重新记录基线的时机

1. 关键路径算法改进后 (预期性能提升)
2. 重大依赖升级 (例如换用新的 bitflags 版本)
3. CPU / 编译器升级 (CI runner 变更)
4. baseline.json 本身被误改 / 损坏

## 文件

- `baseline.json` — 当前基线 (机器 / 编译器 / 时间戳 + 24 条 `ns_per_op_frac` 读数与 `budget_ns_per_op` 预算 + `sched_criteria` 调度判据)
- `../src/framekernel_bench.rs` — bench 实现 + 78 个单元测试
- `../../scripts/record_bench_baseline.py` — 记录读数并派生绝对预算
- `../tests/perf_metric_test.rs` — 性能指标判定腿 (release 实测 vs 预算 + 接线锁)
- `../tests/bench_baseline_median_test.rs` — 记录器聚合口径的回归锁
