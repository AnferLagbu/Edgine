# 框内核范式对齐工程 阶段 6 全量验证报告

> §9 七项验证门槛中六项达标（双架构 0w0e / clippy 0 / 核心审计全过 / host-tests 全过 / QEMU 双架构启动 2/2 / 生产反向依赖 0 文件 0 行），一项定量未达标：TCB 占比 56.7%（目标 <30%）。§3 六项验收中，反向依赖、services 权威、services 0 unsafe、验证门槛四项达标，TCB 占比与 framework 保留文件数（300 vs 规划 200）两项未达标。阶段 3 收尾提交引入的一处 QEMU 校验脚本陈旧字符串回归，已在本轮修复并复验。

本报告是 `docs/plan/framekernel-paradigm-enforcement.md` 阶段 6「全量验证」的一次性快照，采集时间为提交前工作区（含本轮脚本修复）。报告冻结当时事实，作为后续制定 plan 与启动修复工程的输入依据。所有数据均附命令或文件路径证据。

## 一、验证范围与方法

验证对象为该工程阶段 0~5 的累计产出，按工程 §9「验证门槛」七项 + §3「工程目标（验收标准）」六项逐条核验。方法：AGENTS.md §2.3 六门槛命令 + 工程专项脚本（`scripts/audit_*.py`）+ `ci/audit.sh full`。

顺序敏感项说明：`ci/audit.sh` 的 FP-06（`0.5i/6`）读取 `build/kernel.bin`，要求其为最近一次 aarch64 链接产物；`1/6` 双架构 check 的 x86_64 维经 `src/kernel/build.rs` 校验 `build/stage1.bin` 存在。二者需并存，故本轮先 `./ci/build.sh aarch64`（产出 aarch64 `kernel.bin`），再补 `make ARCH=x86_64 build/stage1.bin`（仅汇编引导码，不触碰 `kernel.bin`），最后 `./ci/audit.sh full`。

## 二、§9 七项验证门槛结论

| # | 门槛 | 结论 | 证据 |
|---|---|---|---|
| 1 | 双架构 cargo check --release 0 error / 0 warning | ✅ | `./ci/build.sh all` → `Passed: 5  Failed: 0`；`ci/audit.sh` `1/6` x86_64 与 aarch64 check 均 passed |
| 2 | clippy `-D warnings` 0 | ✅ | `ci/audit.sh` `2/6` clippy pedantic (x86_64, lib) passed；`2b/6` clippy `kernel_test` 维 + `host-test` 维均 passed |
| 3 | 核心审计全过 | ✅ | 见 §三 |
| 4 | host-tests 全过 | ✅ | `make test-host` 各套件 ok；`make test-kernel-host` 941 passed / 0 failed |
| 5 | QEMU 集成测试 | ✅ | `./scripts/qemu_boot_test.sh x86_64` 1/1（VFS ready + Ring 3 + KPTI-09）；`ci/audit.sh` `7/7` 双架构 2/2 通过 |
| 6 | 反向依赖计数下降至 0 | ✅ | `scripts/audit_reverse_deps.py`：扫描 318 文件，生产反向依赖 **0 文件 / 0 行**（起点 136 处/78 文件） |
| 7 | TCB 占比逐阶段下降至 <30% | ❌ | `scripts/audit_tcb_ratio.py`：**TCB ratio 56.7%**，Status `EXCEEDED`（目标 <30%） |

补充环境性说明（非本工程回归）：

- `ci/audit.sh full` `4/6` Lockbud：`error: no such command: lockbud`，工具未安装，脚本记 warn 不阻断。
- `ci/audit.sh full` `6/6` 模块级 SAFETY 不变式：含模块级 SAFETY 不变式的文件 2 个，脚本提示"框架特权层模块数 < 5"（warn，不阻断）。
- `./ci/build.sh all` 的 aarch64 维出现 `core` / `sha2` 的 future-incompat note，属依赖级提示，非本项目 warning。

## 三、核心审计明细

| 审计 | 结论 | 数据 |
|---|---|---|
| `audit_services_boundary.py` | ✅ 通过 | services 0 unsafe；3 项 MEDIUM 均为 services→services 跨模块依赖（net→proc、syscall→fs、timer→syscall），非 framework 渗透 |
| `audit_safety_coverage.py` | ✅ | framework unsafe 覆盖 100%（0.5 步骤：1699/1699；`3/6` 统计口径：unsafe blocks 1514 / SAFETY 注释 2068 = 136%） |
| `audit_invariants.py` | ✅ | I1–I6 全 PASS |
| `audit_coupling.py` | ✅ 通过 | 无新增模块间循环依赖 |
| `audit_comment_language.py` | ✅ | 0 违规 |
| `audit_once_cell.py` | ✅ | services 层 0 处 `spin::Once` 残留，全项目统一 `sync::once::OnceCell`（I-16） |
| `audit_c_naming.py` | ✅ | 0 C 风格命名残留（I-07） |
| `audit_tlb_receive_order.py` | ✅ | 接收侧三段次序正确（S-14） |
| `audit_aarch64_kernel_fp_free.py` | ✅ | 指令总数 327290，白名单外 FP/SIMD = 0（FP-06） |
| `audit_repr_c.py` / `audit_volatile_access.py` / `audit_static_mut.py` | ✅ | 内存安全防线三项通过 |
| `audit_reverse_deps.py` | ✅ | 生产反向依赖 0 文件 / 0 行 |
| `audit_deadlock_matrix.py` | ⚠ | 1 项 HIGH：`framework/arch/x86_64/smp_init.rs:196 AP_STARTUP_LOCK`。系既有项（阶段 0 记录已列），非本轮回归 |
| `audit_tcb_ratio.py` | ❌ | 56.7% > 30% |

## 四、§3 工程目标（验收标准）逐项核验

| # | 验收标准 | 结论 | 说明 |
|---|---|---|---|
| 1 | TCB 占比 < 30% | ❌ 未达标 | 实测 56.7%（framework 110,637 LoC raw / 70,971 effective；services 79,821 raw / 54,273 effective；smoltcp 57,814 排除；unsafe 2302 framework / 0 services） |
| 2 | framework→services 反向依赖 = 0 | ✅ 达标 | `audit_reverse_deps.py` 生产口径 0 文件 / 0 行（原 136 处/78 文件） |
| 3 | 功能层 services 权威 | ✅ 达标 | 阶段 1~5 逐批收口（§8 各批记录）：driver / fs 核心 / net 策略 / proc 策略 / syscall 业务权威实现均落 services |
| 4 | framework 仅机制/契约/安全代理（保留 200 文件，见 §6.6） | ⚠ 部分 | framework `.rs` 实测 300（不含 `tests/`）/ 318（含）；§6.6 的 200 文件为 2026-09-11 粗粒度规划快照，实测超出 |
| 5 | services 保持 0 unsafe（F1 不回归） | ✅ 达标 | `audit_tcb_ratio.py` unsafe services = 0；`audit_services_boundary.py` 通过。真实 unsafe 仅落在 vendored smoltcp（`services/net/smoltcp/src/phy/sys/*`，自带 `#![allow(unsafe_code)]`，3rd-party 锁定子树） |
| 6 | §2.3 五条验证门槛 | ✅ 达标 | 见 §二 门槛 1~5 |

## 五、本轮修复（阶段 3 直接导致的回归）

`scripts/qemu_boot_test.sh` 的 aarch64 分支在批次 Z ④ 引入校验：`grep -q "virtio-net: probed successfully (services bridge)" "$A64_LOG"`（脚本 L219）。阶段 3 收尾提交（`36b5de5d`）将 framework 侧探测日志串由 `"virtio-net: probed successfully (services bridge)"` 改名为 `"nic: probed successfully (services bridge)"`（`src/kernel/framework/net/init/probe.rs:42`），导致脚本 grep 串陈旧失配，`FAIL_OK=0` 下 `RESULT=1`，`ci/audit.sh full` `7/7` 误报"执行异常"。

该回归由本工程提交直接引入，按 AGENTS.md §12.5 属本轮必修项。修复：将脚本 grep 串对齐实际日志 `"nic: probed successfully (services bridge)"`，并补一行中文注释标注日志来源。复验：`ci/audit.sh full` `7/7` 输出 `QEMU 双架构启动测试: 2/2 通过`；aarch64 日志 `build/log/qemu_boot_aarch64.log` 第 73/74 行可见 `virtio-net: registered via services bridge (MAC=52:54:00:12:34:56)` 与 `nic: probed successfully (services bridge)`，证明 services→framework 单向桥链路实际走通。

## 六、结论与后续方向

- **形态与依赖方向已对齐 Asterinas 范式**：framework→services 生产反向依赖归零（项 2）、services 自研 0 unsafe（项 5）、功能层 services 权威（项 3）、§2.3 五项验证门槛全过（项 6）——这是本工程阶段 1~5 的核心成果。
- **治理成果正确但"度量未收敛"**：TCB 占比 56.7%（项 1）与 framework 保留文件数 300（项 4）未达规划阈值。二者同源——§6 的 TCB 归属清单在阶段 1~5 的复核中大量采用"服务对象准则"判定**保留 framework**（DECISION-F/P/Q/S/T/U/V 等），故实现未背离依赖方向，但 LoC/文件数未按原规划下降。
- **后续候选**：若需达成 §3 项 1/项 4，需新开专项，针对仍保留在 framework 的体积大项（`mm` 18,698 / `proc` 16,566 / `driver` 9,244 / `net` 7,582 / `sync` 5,595 / `syscall` 5,121 LoC）逐项复核"服务对象准则"下的进一步收敛空间。
