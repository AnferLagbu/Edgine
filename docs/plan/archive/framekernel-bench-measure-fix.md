# framekernel-bench 度量口径修复（measure 0ns 折叠）

> 0-1 句话说清"为什么有这个计划": [framekernel_bench.rs](file:///home/anfer/Code/Edgine/host-tests/src/framekernel_bench.rs) 的 `measure()` 与各 bench 函数返回值语义错配（双重归一化 + 自适应放大从未生效），致 [baseline.json](file:///home/anfer/Code/Edgine/host-tests/benches/baseline.json) 23 条中 5 条 `ps_per_op=0`、其余整体低估约 10^4 倍，`check_bench_regression.py` 失去回归检出能力。
> 来源: G-07 平行实现消除收尾轮（[eliminate-parallel-implementations.md](./eliminate-parallel-implementations.md)）验收时发现。用户裁定：baseline 先按现状重录，口径修复登记为本独立任务。

## DECISION-081（实施裁定：落点选择与对推荐 A 的偏离披露）

- **描述**：`measure()` 与 bench 函数返回值语义收敛的**落点**选择，以及对本文档「工程计划 A」推荐路径的偏离披露。

- **方案（实际实施，偏离推荐 A，逐条披露理由）**：

  1. **归一化落点：保留在 bench 内，`measure()` 不再二次归一化**（原推荐 A 为「23 个 bench 统一改为返回总耗时 `elapsed.as_nanos()`」）。该路径实施时确认不可行：部分 bench 的单操作口径**含 `BATCH` 倍数**（如 `dma_state_machine_bench` 定义 1 op = 1 次状态机迁移，而每轮 `BATCH` 轮即 2 次迁移，见其尾部 `total_ops = iters * BATCH * 2`），`measure()` 无从得知该倍数，若由 measure 归一化则必然算错；且逐一改写 23 处归一化表达式 + 逐个核对倍数，改动面显著大于「改 `measure()` + 改 `run_all()` 签名」，与 §12.2 外科手术式修改相悖。故取推荐 A 的**变体**：`measure()` 直接透传 bench 自报的 `ps_per_op`，并把 `iterations` / `total_ns` 补齐为自洽三元组（`iterations` = 实际轮数、`total_ns` = 计时轮实测墙钟、`ps_per_op` = bench 自报单操作值）。

  2. **自适应放大整体移除**（原条目方案为「把放大后的 `iters` 传入闭包」，即默认放大保留）。实施时确认放大**不应**保留，两条独立理由：(a) 与第 1 条同源 —— 放大倍数无法被 `measure()` 归一化，放大后只是重复同一固定工作量，记账必然再次失真；(b) 部分 bench 体为 O(n) / 二次复杂度（`dmu_dispatch` 的 `get_obj`/`obj_count` 线性扫描、`zap_dispatch` 键名 `format`、`txg_dispatch` 脏块 Vec 累积），放大 `iters` 会造成运行时爆炸。故改为「**一次预热 + 一次计时**」：预热一次消除首次调用的冷 cache / 惰性初始化偏差，计时轮即为记账轮。

  3. **`dma_state_machine` 的 `black_box` 修正属原「范围」条目外的附加项（如实披露）**：口径修复后该条目仍为 0（`total_ns=90`）—— 根因**不是**口径而是编译器在证明状态迁移确定性后消除了整个循环（12.8M 次迁移仅 90ns，属常量折叠）。修法是 `std::hint::black_box(&mut s)` 阻断折叠。此项不在原「范围」条目所列（原范围仅含 `measure()` + `run_all()` + baseline 重录 + 阈值口径评估），但它是本任务验证门槛「无条目因整数截断折叠为 0」可达成的前置条件，故一并修正并在此披露。

  4. **3 个 bench 的返回值归一化经用户授权一并修正（本轮追加，超出原「范围」声明）**：实施第 1 条后逐条核对发现，23 条中 **20 条**按文件既有约定（第 142 行注释「归一化到 "单操作时间": 总耗时 (ns) / (iters * BATCH), 转 ps 避免精度损失」）返回**已归一化 ps_per_op**，另有 **3 条**（`pte_set_flags_bench` / `sha256_block_bench` / `attribution_classify_bench`，均为 2026-06-05 初版 `08eda4eae` 代码，**非本轮引入**）返回**原始总耗时 ns**。在「bench 返回 ps_per_op」的新契约下，三者被当作 ps 再除以 1000，致 `pte_set_flags` 与 `attribution_classify`（iters=100000）记录值 **100× 膨胀**（`sha256_block` 因 iters=1000 恰好抵消）。原「双重归一化」条目的描述断言「全部 bench 返回已归一化 ps_per_op」**不准确**，已据实订正。用户裁定「本轮一并修正」，故三处各补 `elapsed.saturating_mul(1_000) / iters as u128` 与中文注释，与既有约定对齐；随后再次重录 baseline。此为「度量口径自洽」门槛可诚实达成的必要条件。

  5. **`check_bench_regression.py` 未改动**：噪声阈值口径复核结论见「验证门槛 → 回归门禁有效」条目的详情。

- **状态**: [X]

## 工程计划 A: measure 与 bench 函数返回值语义收敛

### 背景

- **双重归一化（核心缺陷）**
  - 描述: `measure()` 的注释声明契约「`f()` 返回总耗时 (ns)」，但 23 条 bench 中 **20 条**实际返回**已归一化的 ps_per_op**（如 `zap_dispatch_bench` 尾部 `elapsed.saturating_mul(1_000) / iters`、`page_flags_bench` 尾部 `elapsed.saturating_mul(1_000) / (iters * PAGE_FLAGS_BATCH)`），另有 **3 条**（`pte_set_flags_bench` / `sha256_block_bench` / `attribution_classify_bench`）返回**原始总耗时 ns** —— 即「度量口径」在 23 条内本身就不统一。对前 20 条，`measure()` 把已归一化的 ps_per_op 当作 `total_ns`，随后又做一次 `total_ns.saturating_mul(1_000) / iters` —— 同一份耗时被归一化两次。**订正**：本条初稿断言「全部 bench 返回已归一化 ps_per_op」，并把 `pte_set_flags_bench` 误举为归一化范例 —— 该函数当时返回原始 ns，属该错配的另一半，订正依据见 DECISION-081 第 4 条。
  - 方案: 二选一。(A) bench 函数统一改为返回总耗时（`elapsed.as_nanos()`），归一化只在 `measure()` 中做一次 —— 与 `measure()` 现有注释契约一致，且 `iterations` / `total_ns` / `ops_per_sec` 字段语义自洽。(B) 废弃 `measure()` 的归一化与自适应放大，直接透传 bench 返回的 `ps_per_op` —— 改动更小，但 `iterations` 字段失去意义。推荐 A。
  - 状态: [X]
  - 详情: 实际取 **A 的变体**（归一化保留在 bench 内 + `measure()` 透传 + 三元组自洽），理由见 DECISION-081 第 1 条。`measure()` 注释已同步重写，明示新契约「`f(iters)` 返回单操作皮秒数（bench 内已按含 BATCH 倍数的总操作数归一化）」，避免后人按旧契约改回。`iterations` 字段**未**失去意义 —— 现取「实际轮数」且与传给 bench 的 `iters` 同源。

- **自适应放大从未生效**
  - 描述: `measure()` 以 `iters = (iters * 10).min(10_000_000)` 放大取样规模，但 `run_all()` 传入的闭包捕获的是**字面量**（如 `|| zap_dispatch_bench(10_000)`），放大值从未传给 `f()`；放大循环只是重复调用同一个固定工作量的 `f()`，并按放大后的 `iters` 归一化。
  - 方案: `measure()` 签名改为 `Fn(u64) -> u128`，循环内把放大后的 `iters` 传入闭包；`run_all()` 各闭包由 `|| xxx_bench(N)` 改为 `|iters| xxx_bench(iters)` —— bench 函数本就以 `iters` 为形参，23 处重复字面量随之消除。
  - 状态: [X]
  - 详情: 签名与编排器按要求收敛 —— `measure<F: Fn(u64) -> u128>`，`run_all()` 23 条全部改为**直接传 bench 函数**（`measure("zap_dispatch", "unkfs", 10_000, zap_dispatch_bench)`），闭包与 23 处重复字面量一并消除。**但自适应放大被整体移除**（非「把放大值传入闭包」），理由见 DECISION-081 第 2 条（BATCH 倍数无法被 measure 归一化 + O(n)/二次复杂度 bench 放大后运行时爆炸），改为「一次预热 + 一次计时」。

- **现状量化（重录后的 baseline 留档）**
  - 描述: 因 `iters` 恒被放大到 10_000_000，记录值 = `floor(实际 ps_per_op / 10_000)`。重录后 baseline 23 条**全部** `iterations=10000000`；`ps_per_op=0` 者 5 条（`page_flags_bits` / `capability_check` / `dma_state_machine` / `vfs_poll_dispatch` / `raidz_dispatch`），其真实单操作耗时均 < 10ns 故被整数截断折叠；`zil_persist_dispatch` 记录 729 ps，对应实际约 7.29 µs/op（与逐位 CRC32 处理 4096B 块的量级吻合）。
  - 方案: 本任务修复轮一并重录 baseline，故当前值仅作对照留档，不作为长期基线。
  - 状态: [X]
  - 详情: 对照留档与修复后实测均已取到，量化了缺陷因子。**修复前**（`iterations` 恒 10_000_000、记录值 = 实际/10⁴）：5 条 `ps_per_op=0`，其余如 `page_flags_bits` 记录 0.0001xxx ns、`zil_persist_dispatch` 729 ps。**修复后**（23 条：`iterations` 取各 bench 真实轮数、记录值即真实单操作耗时）：`page_flags_bits` 1.257ns、`dma_state_machine` 0.295ns、`vfs_poll_dispatch` 2.458ns、`raidz_dispatch` 1.08ns、`capability_check` 2.23ns（5 个原 0 条目全部给出真实小数值），`zil_persist_dispatch` 7435.199ns ≈ 7.44µs。**数值恰为旧值 ×10⁴ 量级**，与「记录值 = 实际/10⁴」的因子推导互证。自洽性核对：`total_ns` 为计时轮实测墙钟（含 bench 内 setup，故略大于 bench 自计时），对「1 轮 = 1 op」条目有 `total_ns ≈ iterations × ns_per_op_frac`（如修正后的 `pte_set_flags`：100_000 轮 × 3.61ns/op ≈ 361µs，与 `total_ns` 量级吻合）；对含 BATCH 倍数的条目则相差一个倍数因子，故该自洽式**仅在 1 轮 = 1 op 时**成立。

- **回归检出能力失效（连带影响）**
  - 描述: `check_bench_regression.py` 对 `baseline=0` 条目走「至少为 0, 跳过」分支，另对 `|diff| < 1.0ns` 一律按噪声跳过。当前 23 条中仅 `zil_persist_dispatch` 可能触发告警 —— 与 AGENTS.md §8「性能基线每次 PR 更新」及 §12.4 目标驱动验证的意图不符。
  - 方案: 修复度量口径后在同一轮重录 baseline，并复核噪声阈值口径（`1.0ns` 绝对阈值对修复后的亚纳秒快路径是否仍合适，或改为「相对百分比 + 绝对下限」双条件）。
  - 状态: [X]
  - 详情: 失效已消除 —— baseline 重录后 23 条**全部非 0**，不再有任何条目落入「baseline=0 跳过」分支，故每条都恢复受保护。噪声阈值口径复核结论：**现有规则足够，不改代码**。既有规则为双条件（`compare()` 内 `MIN_ABS_NS = 1.0` / `REL_NOISE_THRESHOLD = 0.50`）：`|diff| >= 1.0ns` 时按 15% 相对门限判定；`|diff| < 1.0ns` 时改用 50% 相对门限。实测依据：多轮 bench 的 ns 级条目抖动约 ±6%（如 `attribution_classify` 606↔643、`arc_dispatch` 62↔66），15% 门限有约 2.5× 裕度；而 1–3ns 量级条目抖动显著更大（`vfs_poll_dispatch` 单轮内 −19.3%），故 `MIN_ABS_NS=1.0` 抑制亚纳秒抖动是**必要的**。已知取舍（如实登记，非缺陷）：`MIN_ABS_NS=1.0` 使 `<1ns` 条目（现为 `dma_state_machine` 0.297ns）等效把门限抬到 50%，即这类条目只能检出 >=50% 的退化 —— 但反向验证已证明 50% 以上的亚纳秒退化确实能被捕获（见「回归门禁有效」详情）。

### 验证门槛

- **度量口径自洽**
  - 描述: 每条 bench 的 `iterations` 与实际工作量一致，`total_ns` 与 `ps_per_op × iterations / 1000` 自洽，无条目因整数截断折叠为 0（亚纳秒条目应给出真实小数值而非 0）。
  - 方案: 修复后运行 `cargo run --release --bin framekernel-bench` 逐条核对上述两项。
  - 状态: [X]
  - 详情: 三项逐条核对通过。①`iterations` 与工作量一致：现由 `run_all()` 单一来源（`measure(..., iters, bench_fn)`）同时决定记账值与传给 bench 的实参，23 条记录值分别为 1000/10_000/100_000 三档，与各 bench 的实际轮数一一对应；②无折叠为 0：23/23 条 `ps_per_op > 0`（最小者 `dma_state_machine` 297ps），5 个原 0 条目全部给出真实小数值；③自洽式须加限定 —— 该式**仅在「1 轮 = 1 op」时成立**，含 `BATCH` 倍数的条目（`page_flags`/`iomem`/`capability`/`dma`/`socket`/`virtio_blk`/`sysctl`/`blk_dev`）相差一个倍数因子，此为设计使然（归一化与 op 计数同源于 bench 内），非遗漏。另：初稿门槛遗漏了「23 条内口径须统一」这一必要项，实施中发现 3 条返回原始 ns 的 bench 并已修正（见 DECISION-081 第 4 条），故本条按修订后口径判定为通过。

- **回归门禁有效**
  - 描述: baseline 重录后 `python3 scripts/check_bench_regression.py` PASS；反向验证 —— 人为放大某条 bench 工作量约 20% 时能被检出为回归。
  - 方案: 重录 + 临时注入退化的反向验证（验证后回滚）。
  - 状态: [X]
  - 详情: ①PASS：重录后 `python3 scripts/check_bench_regression.py` → `[PASS] 所有条目均在阈值范围内, 无回归.` rc=0（23 项基线 / 23 项当前，无条目落入 WARN 的「至少为 0, 跳过」分支）。②反向验证（**两轮，覆盖两条判定路径**，均在 `/tmp` 副本上注入退化、验毕删除，未改仓库基线）：**第一轮**（度量口径修正后、3 bench 归一化修正前）—— 亚纳秒路径：`raidz_dispatch` 基线 0.50ns vs 当前 0.99ns，`|diff|=0.49ns < 1.0ns` 走相对门限，`+98% >= 50%` ⇒ 判定为回归（**证明「亚纳秒退化不因 MIN_ABS_NS 被吞掉」**）；纳秒路径：`pte_set_flags` 基线 250 vs 当前 360（`+44%`）⇒ 回归；改善向 `arc_dispatch` 基线 200 vs 当前 62（`−69%`）⇒ 归入 `[OK] 性能改善`，**不误报为回归**；总 `rc=1`。**第二轮**（3 bench 归一化修正后、最终基线）—— `pte_set_flags` 基线 2.5ns vs 当前 3.61ns（`+44.2%`）与 `raidz_dispatch` 基线 0.50ns vs 当前 1.67ns（`+234%`）双双判为回归，`rc=1`。门禁的「检出退化 / 不误报改善 / 失败即非零退出」三项均已实证。

### 范围

- **不在本任务内**
  - 描述: bench 集合的增删与内核实现迁移（属 G-07 平行实现消除，已收尾）；bench 函数算法本体与其测量对象改动。
  - 方案: 仅动 `measure()` 与 `run_all()` 闭包签名 + baseline 重录 + `check_bench_regression.py` 阈值口径评估。
  - 状态: [X]
  - 详情: 范围主体守持 —— `check_bench_regression.py` **零改动**（复核结论：现有双条件噪声规则足够）；bench 集合增删与内核实现迁移未触碰（属 G-07，已收尾）。**两处经披露/授权的范围外改动**：①`dma_state_machine_bench` 加 `black_box` 阻断常量折叠（DECISION-081 第 3 条，属门槛达成的前置条件）；②`pte_set_flags` / `sha256_block` / `attribution_classify` 三处返回值归一化（DECISION-081 第 4 条，经用户裁定「本轮一并修正」）。两者均**未改动** bench 的测量对象与算法本体 —— 仅改动「返回值表达方式」（归一化补全）与「阻断编译器优化」（black_box），故与「不测内部实现」「测用户可见行为」的测试规范不冲突。