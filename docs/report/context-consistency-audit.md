# 项目上下文一致性与二义性审计（系统性认知调研快照）

> 核心结论：对 Edgine 全仓源码与文档交叉印证后，确认门禁体系当前健康（boundary/invariants 实测全绿），但存在 5 项文档—代码不一致与 3 项未明确事项；其中 TCB 占比未达标（实测 raw ≈ 60.7%）是 framekernel 范式工程的主战场，fd 命名空间重叠则是 Socket 层 P3 续接工程（HANDOFF-socket-layer-P3）的直接根因。

本报告为一次性结果快照，定位是后续修复工程与 plan 制定的输入依据；所有结论均附可复现证据，事实以报告发布时的工作区状态（`main` @ `383956fe`，含未提交的 socket P3 改动）为准。

## 1. 审计范围与方法

调研覆盖：仓库结构与模块职责（`src/kernel/privileged` 与 `src/kernel/functions` 双子树）、工程规范（`AGENTS.md`、`docs/explain/spec-engineering.md`、`docs/README.md`）、计划与决策台账（`docs/plan/progress-active-tasks.md`、`future-roadmap.md`、`framekernel-paradigm-enforcement.md`、`socket-layer-completion.md`、`unresolved-issues-2026-08-09.md`）、CI 与门禁（`ci/audit.sh`、`ci/build.sh`、`.github/workflows/`）、构建配置（`src/kernel/Cargo.toml`、`rust-toolchain.toml`）、测试体系（`host-tests/`、`scripts/qemu_boot_test.sh`、内核 `#[cfg(test)]` 内联用例），并实测重跑了核心审计脚本与 TCB 行数统计以印证文档口径。

## 2. 文档—代码不一致

### 2.1 失效的文档引用（archive 迁移后路径未同步）

`docs/explain/spec-engineering.md` 三处（L3、L9、L103）引用 `docs/plan/engineering-discipline.md`，`docs/explain/explain-framekernel.md` L126 引用 `framekernel-compliance.md`——这两个文件在 `docs/plan/` 顶层均不存在，实际位于 `docs/plan/archive/`（历史归档迁移遗留）。explain 文档是新人必读入口，失效引用会直接把读者引向 404；`docs/plan/archive/` 内多处对二者的相对链接同样失效。修复方向是把 explain 层引用改指 archive 路径，或按 §6 规范判断该内容是否已该退役。

### 2.2 roadmap 的 SLAAC 状态漂移

`docs/plan/future-roadmap.md` F5 状态行（L82）写 "Phase 6 DHCPv6/SLAAC 远期待办"，L92 时间线亦写 "Phase 6 DHCPv6 远期"；但 SLAAC 实际已按 P6a 简约实装完成——commit `cb044381`（P6a SLAAC 简约实现）、`8fb80544`（IPv6 SLAAC 联调支持与内核网络修复），且 `src/kernel/Cargo.toml` L55 已启用 smoltcp 的 `proto-ipv6-slaac` feature、L67 启用 `auto-icmp-echo-reply`。roadmap 中真正远期的只剩 DHCPv6。这属 AGENTS.md §9.2 定义的"文档与代码不同步"预存问题，应单开 PR 同步 F5 状态表述。

### 2.3 boot canary 断言文案自相矛盾

`src/kernel/lib.rs` 两处 boot stack canary panic 文案对同一块启动栈给出不同尺寸：L522 写 `Stack size=256KB`（kernel_init 前验证点），L915 写 `Stack size=128KB`（Ring 3 入口前验证点）。两处校验的是同一个 canary（`check_boot_stack_canary()`），文案数字必有一处失真。若未来真的触发 canary panic，误导性的尺寸数字会把排障方向带偏；需对照启动栈的实际分配定义（trampoline/初始化栈的尺寸来源）修正错误的一处。此为诊断文案层面的缺陷，不影响 canary 机制本身的正确性。

### 2.4 "clippy 0 warning" 的真实口径

§2.3 验证门槛中的 "clippy 0 warning" 并非 pedantic 语义全达标，而是三层兜底机制达成的结果：

| 机制 | 实测规模 | 证据 |
|---|---|---|
| crate 级 `#![allow(clippy::*)]` | `src/kernel/lib.rs` 内 46 处（含逐条理由注释，如 L189 `cast_ptr_alignment` 硬件对齐豁免） | `grep -c '#!\[allow(clippy' src/kernel/lib.rs` |
| 函数/表达式级 `#[expect]` | `src/kernel` 全树 1635 处 | `grep -r '#\[expect' src/kernel --include='*.rs'` |
| cast 类永久保留决策 | DECISION-041：1700+ 处已知安全 cast 保留原状、CI 不阻断 | `docs/plan/stage-engineering-master.md` L132、L408 |

这一口径本身是有意决策（DECISION-040/041，避免海量低价值改造），不是问题；问题是审查与新人认知时不能把 "0 warning" 误读为 "零抑制"。建议后续在 `docs/explain/spec-engineering.md` 或门禁文档中把该口径显式写明。

### 2.5 TCB 占比未达标且短期难达标

`docs/plan/framekernel-paradigm-enforcement.md` §3 验收项 1（TCB < 30%）与项 4（privileged 收敛至 200 文件，实测 300）仍为未达成状态（该文档 L47、L50）。阶段 6 报告口径为 56.7%（privileged 110,637 vs functions 79,821 LoC，排除 smoltcp）；本次按 raw 行重新实测为 privileged 121,619 vs functions（除 smoltcp）78,834 ≈ **60.7%**——两个口径均未达标且较报告口径进一步上升（P3 期间 sm_fi.rs 大量新增在 privileged）。含义：任何新增 privileged 代码都会被 PR 审查（AGENTS.md §7.2）追问，机制/策略分离（§4.1 归属决策树）在新增代码上必须严格执行；TCB 缩减本身需要独立的下沉批次推进，不是本轮 socket 工程的责任范围。

## 3. 未明确事项（需用户裁决）

### 3.1 Cargo.toml 死 feature 开关与 F9/铁律 0 的张力

`src/kernel/Cargo.toml` `[features]`（L75-108）中，以下开关在 `src/kernel` 源码（`cfg(feature = ...)`）与构建脚本（Makefile / ci / scripts）中均零引用，属完全未接线的预留开关：`mdns`、`mqtt`、`sntp`、`smtp`、`tftp`、`snmp`、`netbios`、`http_client`。另有少量引用但不在任何构建组合中启用的：`kaslr`（cfg 2 处）、`preempt`（1 处）、`json_export`（5 处）；`async` 有 62 处 cfg 引用，属在用开关。零引用的网络协议开关疑似 smoltcp 协议生态调研期的残留，与 F9 死代码零容忍、`spec-engineering.md` 铁律 0"不为将来预留扩展点"存在张力。此张力未见任何台账登记（progress-active-tasks / unresolved-issues 均无对应条目），需用户裁决：删除，还是登记为豁免条目。

### 3.2 socket 层双路径并行实现的收敛归属

`src/kernel/functions/net/smoltcp_impl.rs` 存在一套面向 SocketTable trait 的 `bind_fd`/`listen_fd`/`accept_fd`（L258、L276、L293，经 `fw_net_socket::sm_net_*` 顶层 re-export 调 privileged）；而 P1-P3 主线实装在 privileged `net/init/sm_fi.rs`（syscall 直连路径）。两条路径并存、语义同源，`docs/plan/socket-layer-completion.md` 仅要求"核对避免分裂"，未指定收敛归属（哪条是主线、另一条何时删除或改为其上层）。按 `docs/plan/duplicate-impl-convergence.md` 的既有治理方向，这属于需要单独立项的并行实现收敛问题，归属决策应由用户做出。

### 3.3 工作区文档规范改动与 design/ 迁移的对应关系

当前工作区（未提交）中 `AGENTS.md` 与 `docs/README.md` 均有改动（新增 `docs/design/` 目录规范，§6 文档四分类），同时 `docs/design/freg-design.md` 以 untracked 新目录形式存在、`docs/explain/freg-stack-design.md` 被标记删除——三者构成一次自洽的"设计文档迁入 design/"重组织。但该重组织未见于任何 plan 条目，无法从台账确认它是否即用户规划的独立工程、还是随 socket 工程顺带的文档动作。接手方在提交前应与之确认归属，避免把不相关的工作区改动混入 `feat(net)` commit（本次 P3 交接文档 HANDOFF-socket-layer-P3.md 中已约定 commit  scope）。

## 4. 交叉印证的正面结论

作为对照，以下认知经实测确认健康，可作为后续工程的信任基线：

- **边界与安全门禁实际通过**：`audit_functions_boundary.py` 0 违规（EXIT=0）、`audit_invariants.py` 六项 I1-I6 全部 PASS，当前工作区未因 P3 改动破坏 F1/F2 边界。
- **依赖方向合规**：privileged→functions 生产反向依赖为 0（framekernel-paradigm-enforcement §3 验收项 2，`audit_reverse_deps.py` 口径）。
- **smoltcp vendored 零修改**：`functions/net/smoltcp/` 受 `audit_smoltcp_purity.py` + `SMOLTCP_LOCAL_SRC_HASH` 门禁锁定，P1-P3 的全部 socket 实装均未触碰 vendored 源码。
- **CI 门禁链完整**：`ci/audit.sh`（quick/full 两模式）覆盖 §2.2 全部核心审计 + 双架构编译 + clippy + fmt，且 QEMU 里程碑断言为 fail-closed（`FAIL_OK=0`）。
- **P3 进行中改动的功能面已验证**：单连接与多连接（5/5）TCP echo e2e 通过、无 panic；唯一未达成项是 accept fd 回收（fd 2→3→4→5→6 未复用），其根因即下述第 5 节。

## 5. 与 P3 接手工程的衔接

第 2.5 节（TCB 膨胀压力）与第 3.2 节（双路径收敛）直接约束 P3 续接的实施方案：fd 命名空间修复选择方案 C（重排 `FdPlan::SMOLTCP` 与 VFS `[0, 64)` 使其不重叠）会把改动集中到 privileged `proc/fd_alloc.rs` 与 `net/init/sm_fi.rs`，加剧 TCB 占比审查压力，PR 描述需援引本报告说明"预存架构缺口的必要修复"；`sm_fi.rs` 索引耦合的 (i)/(ii) 处理方向（base 减法 vs 索引空间重排）属决策灰色地带，须由用户拍板后施工。fd 命名空间重叠的完整根因链与实施清单见 `other/HANDOFF-socket-layer-P3.md` §3-§4，本报告不重复。
