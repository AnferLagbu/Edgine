# 审计修复分册 10：SAFETY 覆盖清零与审计工具准确性

> 修复 audit_unsafe.py 窗口逻辑缺陷（127 处 SAFETY 误报）、SAFETY 真实缺失清零、TD-22/clippy 预存问题清理、边界代理豁免、nvme halt 架构抽象、volatile 访问模式对齐。
> 来源：audit-fix-01（B01 系列）延伸 + 分册 05（DECISION-071/075）之后的收口轮；编号 10 因 06-09 已被其他主题占用（06=services/fs、07=services/net-ipc-credo、08=user-build-docs、09=hard-rules-deadcode）。

## 工程计划 A: 审计工具准确性（audit_unsafe.py 窗口缺陷）

### 背景

- **B01-27. audit_unsafe.py `_scan_safety_window` 属性块状态机缺陷**
  - 描述：`tools/audit_unsafe.py` 向上扫描 SAFETY 注释时，把多行属性（`#[expect(...)]`）的闭合行 `)]` 误判为"非注释代码"直接终止扫描；且属性内容行含 `)`（字符串内括号）会误退 `in_attr` 状态、吞掉其后的真实 SAFETY 注释。导致大量"已有 SAFETY 注释仍报缺失"的误报。
  - 方案：`_scan_safety_window` 改为「闭合行 → 内容 → 开始行」三段状态机（B01-27）：整行为 `)]` 的闭合行进入属性状态；内容行一律跳过（不再用含 `)` 提前退出）；遇 `#[` 开始行恢复常规扫描。新增 `ATTR_CLOSE_RE`。
  - 状态：[X]（修复后 127 处误报中 108 处识别为已有 SAFETY，仅 20 处真实缺失；负向验证未引入漏报）

- **B01-25. ci-lint.yml 审计退出码强制（B01-25 模式）**
  - 描述：`ci-lint.yml` 各审计步骤用 `cmd | tee`，管道退出码 = `tee` 的 0，审计失败被掩盖（fail-open）。
  - 方案：捕获脚本真实退出码（`set +e` + `$?`），非 0 时 `::error::` + `exit 1`；SAFETY/边界/死锁三步统一；artifact 路径修正为脚本实际输出 `target/audit/services-boundary.json`。
  - 状态：[X]（工作区已改；`audit_safety_coverage.py --missing-only` gap→exit 1、`audit_deadlock_matrix.py` CRITICAL→exit 1、`audit_services_boundary.py` HIGH→exit 1 均与 CI 语义一致）

- **B01-28. ci/audit.sh clippy 命令与 CI 对齐**
  - 描述：`ci/audit.sh` 的 clippy 段（`cargo clippy --lib -- -D warnings -W pedantic -W cargo`）与 CI（ci-x86.yml clippy-pedantic job）不一致：无 `--release` 导致 dev 模式编译诊断代码触发 `similar_names` 假阳性；缺 cast_* 四个豁免（DECISION-041）导致大量 cast 错误；`-W clippy::cargo` 触发依赖多版本 bitflags 误报。本地入口永远失败，无法作为 §2.3 验证入口。
  - 方案：命令对齐 CI——加 `--release --lib --bins --examples`、`-D clippy::pedantic` + 四个 `-A clippy::cast_*` 豁免、去掉 `-W clippy::cargo`；保留 `-D warnings` 更强门禁（B01-16）。
  - 状态：[X]（修复后 `./ci/audit.sh quick` 全绿，"clippy pedantic (lib): passed"）

### 待办

- **B10-01. ci/audit.sh SAFETY 基线 127→0 同步**
  - 描述：`ci/audit.sh` `EXPECTED_MAX_SAFETY_MISSING=127` 是工具误报时代的旧基线，修复后真实缺失已为 0，保留 127 会让 CI 门槛虚高。
  - 方案：基线改为 0（任何缺失即 CI 失败），注释说明 B01-27 修复消除误报。
  - 状态：[X]（基线=0；`audit.sh quick` SAFETY 段 "缺漏 0 ≤ 0 基线" 通过）

## 工程计划 B: SAFETY 覆盖清零与注释语言

### 背景

- **B10-02. SAFETY 真实缺失 20 处逐点补注释**
  - 描述：工具修复后剩余 20 处真实缺失（`unsafe fn`/`unsafe extern "C"`/`ref` 函数指针/`block`），分布于 nvme、barrier、sleep、slab、vmm、scheduler、scheduler_ex、firmware、e1000、keyboard、exception、x86_64/mod、uart、psci 等。
  - 方案：在声明行正上方补真实 `// SAFETY:` 注释（声明级 + 内部 block 经窗口机制自动覆盖）；nvme 私有 `unsafe fn` 的 SAFETY 与其 pub 包装器 `# Safety` 契约一致。
  - 状态：[X]（framework 全量 SAFETY 覆盖 100%，缺 SAFETY = 0）

- **B10-03. TD-22 预存英文注释 67 处中文化（用户授权修复）**
  - 描述：`audit_comment_language.py` 报 67 处纯英文段落注释（34 文件），全部为历史遗留预存问题（HEAD 即存在，源自 DECISION-039 UserContext 迁移等），非本次引入，但阻断 audit.sh 与 CI comment-language job。
  - 方案：按用户 2026-08-30 授权，逐处翻译为中文化（技术术语保留英文 + 中文说明）；services 子树仅改注释不改逻辑。
  - 状态：[X]（TD-22 0 违规）

- **B10-04. clippy pedantic 预存 26 处修复（用户授权修复）**
  - 描述：CI 标准 `-D clippy::pedantic -A cast_*` 报 26 处错误（ref_as_ptr×6、manual_let_else×5、missing_panics_doc×3、similar_names×3、used_underscore_binding×2、items_after_statements×2、large_stack_arrays、cast_lossless、unnecessary_wraps、trivially_copy_pass_by_ref、missing_errors_doc），14 文件，HEAD 即存在。
  - 方案：按用户 2026-08-30 授权，逐处加 `#[expect(clippy::xxx, reason="中文")]` 或最小重构（dma 下划线绑定改名、e1000 6 字节参数改传值、storage cast 改 `u64::from`、scheduler ref_as_ptr 改 `addr_of!`）。
  - 状态：[X]（x86_64 与 aarch64 release clippy 均 0 error；`-D warnings` 亦通过）

### 待办

- **B10-05. build.sh forbidden asm 检查 fail-open → fail-closed**
  - 描述：`ci/build.sh` `check_forbidden_patterns` 对无 cfg 门控的裸 `asm!` 原仅打印警告并 `return 0`（fail-open），无法阻断 CI；登记时的触发源（`framework/driver/storage/mod.rs` 处裸 `asm!("pushfq; pop {0}")`，无 cfg 门控）已在后续 storage 重构中消除。
  - 方案：改为 **fail-closed**（DECISION-076 C 项修订）：检出无 cfg 门控裸 asm 时 `return 1` 阻断 CI；同时排除 cargo 构建产物 `target/`（vendored 依赖 `probe.rs` 非本仓库源码，否则误判）。
  - 状态：[X]（实测：真实仓库 clean→RC=0；合成未门控 asm→RC=1；`./ci/build.sh all` 通过）

## 工程计划 C: 边界豁免与驱动/同步抽象

### 待办

- **B10-06. PROXY_ALLOWANCE 代理豁免（5 处 HIGH 自拦截误报）**
  - 描述：`audit_services_boundary.py` 对 services 代理层文件（debug/mod.rs、ebpf_verifier.rs、ipc/msgq.rs、proc/coredump.rs）转发 framework 公开 API 的既定设计误报为边界穿透。
  - 方案：新增 `PROXY_ALLOWANCE` 白名单（文件 + 禁条组合，与 VENDORED_EXCLUDE 同机制精细豁免）。
  - 状态：[X]（boundary 审计 0 违规）

- **B10-07. storage 驱动裸 asm hlt → framework::cpu::arch::halt**
  - 描述：storage 驱动内 `core::arch::asm!("hlt")` 裸汇编在双架构驱动层，绕过架构抽象。
  - 方案：替换为 `crate::framework::cpu::arch::halt()`（arch 抽象，双架构可用）。
  - 状态：[X]（替换点现位于 `services/driver/storage/mod.rs`；QEMU x86_64 启动验证通过，VFS ready + Ring 3 init）

- **B10-08. audit_volatile_access.py pi_mutex 原子访问模式对齐**
  - 描述：`audit_volatile_access.py` 对 pi_mutex `effective_priority`（AtomicU32）按 `self.field.get()`（UnsafeCell 模式）检测，找不到访问而 fail-closed 误报。
  - 方案：`RISKY_FIELDS` 增加 `atomic` 访问模式（识别 `.field.load()/.store()`，原子类型自带对齐 + volatile 语义 + 内存序）；保持 fail-closed（无原子访问仍报错）。
  - 状态：[X]（4/4 高风险字段通过）

## DECISION-076（2026-08-30）

- **C 项：build.sh asm 检查 fail-closed 调整 — 已实施**
  - 背景：`ci/build.sh check_forbidden_patterns` 对无 cfg 门控裸 asm 原仅警告 fail-open；曾计划改 fail-closed（发现违规 return 1）。
  - 决策：改为 fail-closed（B10-05）。原登记时唯一预存裸 asm（storage/mod.rs:207）已在后续 storage 重构消除，改造不再阻断现有 CI；并排除构建产物 `target/`（vendored `probe.rs` 非源码）。
- **TCB 占比搁置**
  - 背景：`audit_tcb_ratio.py` 显示 TCB 61.2%，超过软目标 30%。
  - 决策：延续 DECISION-070（不以 TCB 占比为指标，以可维护性为目标），本会话确认搁置，不为此引入架构改动。
- **TD-22 / clippy 预存问题修复授权**
  - 决策：用户 2026-08-30 授权修复 TD-22 67 处英文注释中文化（B10-03）与 clippy pedantic 26 处（B10-04），作为让 CI 恢复全绿的必要预存清理。

## 验证门槛（§2.3 全量）

| 门槛 | 结果 |
|---|---|
| 核心审计（boundary/safety/deadlock/coupling/comment/once_cell/c_naming/invariants/repr_c/volatile/static_mut） | 0 违规 |
| `./ci/build.sh all`（双架构 release + host-tests + link） | Passed 5 / Failed 0 |
| clippy release x86_64 + aarch64（CI 标准） | 0 error |
| `cargo clippy --release -- -D warnings`（§2.3） | 0 error |
| host-tests | 10 passed |
| QEMU x86_64 启动 | VFS ready + Ring 3 init 通过 |
