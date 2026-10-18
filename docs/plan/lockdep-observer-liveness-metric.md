# lockdep 观察者效应消减与整机 liveness 量化度量

> **定位**：把 P-30 诊断中确认的两处"观测手段本身失真"收口——① lockdep 簿记的**观察者效应**（元锁串行化 + 嵌套日志放大持锁窗，使 lockdep build 上的 convoy 判读不能对应生产）；② "整机是否冻结"目前只能靠**稀有 boot 的人工 `bt` 判读**（且 KPTI tick 读数有伪影），缺一个可量化、可回归的 liveness 指标。本工程只做**低扰动化**与**度量脚本化**，不改生产锁语义。
>
> **来源**：`docs/report/p30-boot-freeze-rootcause-diagnosis.md` §三（boot3 convoy = lockdep 观察者效应）与 §五「遗留」项 B；判据背景见 `docs/plan/net-e2e-tcp-echo-flakiness.md` P-28（QEMU HMP 现场快照 + KPTI tick 读数）。
>
> **关系**：与 P-30 根治解耦——P-30 已坐实生产（非 lockdep）镜像不复现冻结；本工程处理的是**调试/测试基础设施的可信度**，不触碰 `#[cfg]` 门外的生产路径。
>
> **关联**：`docs/explain/ref-lock-order.md`（锁序）；`docs/report/p27-e2e-flake-forensic-report.md`（全局冻结判型与 `LOG_EMIT_LOCK` 无超时自旋教训）；`.github/workflows/ci-bench.yml`（性能回归基线）。

## 1. 立项依据

### 1.1 观察者效应的机制

- `LOCK_DEP_MAP: IrqSpinLock<LockDepMap>`（`src/kernel/privileged/sync/lockdep.rs:329`）是**全局单锁**，守护锁依赖图。每把被跟踪的 `IrqSpinLock` 在 acquire/release 时都要回调 `sync::acquire`/`sync::release`，而后者内部**多次**短暂获取 `LOCK_DEP_MAP`（查 kind、查 name、`add_dependency`、pop 持有栈：`lockdep.rs:534/540/565/582/600/630` 等）。
- 由此，各核的锁簿记被 `LOCK_DEP_MAP` **串行化**：真正要观察的并发锁行为，被观察工具自身的元锁竞争重新塑形。
- `lockdep_log!`（`LOG_EMIT` 嵌套）在持 `LOCK_DEP_MAP` 或其邻接锁的路径上放大数据面持锁窗，进一步放大扰动。
- 整条簿记在 `#[cfg(any(debug_assertions, feature = "lockdep"))]` 下存在，**生产 release 构建不含**——故 lockdep build 上"看到"的 convoy 无生产对应物。

**后果**：P-30 曾在 lockdep build 上把 boot3 的"四核死自旋 convoy"当成真病灶，实为观察者效应叠加 KPTI 读数伪影。度量工具不可信 → 判型代价高。

### 1.2 liveness 度量的现状缺口

- "整机是否冻结"目前靠稀有 boot 上人工挂 gdb `bt`，逐核判 `idle_entry`/内核文本/`[halted]` 分布，无自动判据。
- `TICK_IRQ_COUNT` 是内核 `.bss` 虚拟地址，gdb `x/1xg` 走被附加 CPU 的当前 CR3；该 CPU 在用户态时读回**用户指针垃圾**（≥1M）而非真 tick（KPTI 读数伪影），需挑选"rip ∈ 内核文本窗"的核读数并三条件 AND 判冻——这套逻辑目前散落在一次性 soak 脚本里，未沉淀为可回归指标。

## 2. 源码调研结论（要点）

| 面 | 位置 | 现状 |
| --- | --- | --- |
| 元锁 | `sync/lockdep.rs:329` `LOCK_DEP_MAP` | 全局单 `IrqSpinLock`，acquire/release 簿记串行化 |
| 簿记调用点 | `sync/lockdep.rs:523/617`（`acquire`/`release`） | 单次操作多次取元锁；`lockdep_log!` 嵌套放大 |
| 存在性门 | `#[cfg(any(debug_assertions, feature = "lockdep"))]` | 生产 release 不含簿记 |
| 现场快照 | `scripts/qemu_debug.sh` / P-28 `E2E_MON=1` HMP | 已有 monitor 抓取雏形，判冻逻辑未固化 |
| tick 读数 | 内核 `.bss` `TICK_IRQ_COUNT`（KPTI 伪影） | 需选内核文本窗 CPU 才可信 |

## 3. 设计立场

- **只做低扰动化，不改锁语义**：消减观察者效应 ≠ 削弱 lockdep 检测力（violation 判据必须等价）；优先"缩小 `LOCK_DEP_MAP` 临界区 / 批量无锁读 per-CPU 持有栈 / 日志出锁域"，保持 release 语义与生产路径零改动。
- **度量优先于机制改造**：leg 1（liveness 量化）成本与风险都低于 leg 2（lockdep 簿记重构），先落地 leg 1 即可把 P-30 那类"稀有 boot 人工判读"转成 CI 可回归指标；leg 2 视 leg 1 暴露的失真程度再排期。
- **判据锚定语义而非地址巧合**：liveness 指标要抗 KPTI 读数伪影（用内核态自报 tick 或选内核窗 CPU + 多信号 AND），把"冻结"定义成可机器判定的不变式（tick 单调推进 ∧ 至少一核 RIP 变化 ∧ echo 吞吐非零）。

## 4. 施工条目

### D-1 整机 liveness 量化判据（脚本 + 判冻不变式）

描述：把散落在一次性 soak 脚本里的"读 tick + `bt` 健康核 + echo 吞吐"三信号，固化为一个可复用、可入 CI 报告的 liveness 探针；输出机器可判的 ALIVE/FROZEN 与量化 tick 速率。
方案：抽 `other/p30_soak.sh` 的成熟逻辑进 `scripts/` 常驻探针：① 选 `rip ∈` 内核文本窗的核读 tick（抗 KPTI 伪影），三条件 AND 判冻；② 叠加 echo 吞吐计数与 per-核 RIP 变化位作交叉验证；③ 以 exit code + 结构化行输出，供 CI 与回归比对。复用 P-28 `E2E_MON` HMP 雏形，不新增内核插桩（避免 §1.1 观察者效应与 P-27 的 IRQ/锁域 printk 死锁教训）。
状态：[]

### D-2 lockdep 簿记低扰动化

描述：缩小 `LOCK_DEP_MAP` 临界区、把违规日志移出持 `LOCK_DEP_MAP` 的窗口，使 lockdep build 上的锁并发行为更贴近生产（降低 convoy 假象），且**保持 violation 判据语义等价**。
方案：① `acquire`/`release` 内多次 `LOCK_DEP_MAP.lock()` 合并为单次临界区（缓存 kind/name 后再判）；② `lockdep_log!` 改为"锁内取快照数据、锁外发射"，杜绝持元锁串口自旋；③ 评估 per-CPU 持有栈改无锁读；④ 以 D-1 指标与既有 lockdep 单测（`host-tests` 相关用例）验证检测力不降。**属调试基础设施核心改造，工程量中高，须先有 D-1 作度量托底。**
状态：[]

### D-3 CI 接入

描述：把 D-1 探针作为 boot 相关改动的可选 liveness 门禁（或定期回归 job）接入。
方案：接 `.github/workflows/ci-bench.yml` 或新增轻量 boot-liveness job（受 self-hosted/QEMU 资源约束，先本地/定时跑，稳定后再评估入强制门禁）。
状态：[]

## 5. 验收

- D-1：同一负载下探针给出稳定、可重放的 ALIVE/FROZEN 与 tick 速率；对已知 flaky（net-e2e tcp-echo）能量化区分"真冻结/读数伪影"。
- D-2：改造前后 lockdep violation 判定结果一致（回归单测），lockdep build 上 convoy 假象相对下降（用 D-1 指标对比）。
- D-3：接入后 §2.3 门槛不受扰动，生产 release 构建行为逐字节不变（`audit_aarch64_kernel_fp_free` 等专项照旧）。
