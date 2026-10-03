# AGENTS.md

> QueenX 内核工程 agent 工作指南.
>
> **硬规则**: 见 [§5 硬规则（零容忍）](#5-硬规则零容忍违反即拒收). 违反任一即拒收 PR.
>
> **必读** (开始任何任务前): 本文件 + `docs/README.md` + `docs/explain/` 下所有文档.
>
> **关联文档**: `docs/README.md`（文档格式规范）/ `docs/explain/spec-engineering.md`（编码规范，权威规则）/ `docs/explain/`（架构与子系统文档）

## 1. 仓库布局

| 目录 | 用途 | 维护者 |
|---|---|---|
| `src/kernel/framework/` | TCB 子树（允许 unsafe，硬件抽象） | 用户 决策 / AI 实施 |
| `src/kernel/services/` | 100% safe Rust 子树（策略与业务） | 用户 决策 / AI 实施 |
| `src/kernel/services/net/smoltcp/` | smoltcp vendored（3rd-party，锁定） | 升级时 用户 授权 + AI |
| `src/user/`, `src/userland/`, `src/rust/` | 用户态程序与工具 | AI 实施 / 用户 审查 |
| `host-tests/` | 主机端单元/集成测试（no_std + std） | AI 实施 / 用户 审查 |
| `docs/plan/` | 工程与任务计划 | 用户 决策 + AI 撰写 |
| `docs/explain/` | 项目引导与解释 | 用户 决策 + AI 撰写 |
| `docs/report/` | 生命周期内的结果报告（诊断/审计/评估/测试/性能），作为制定计划与修复工程的输入；发布即冻结 | 用户 决策 + AI 撰写 |
| `scripts/`, `ci/`, `tools/` | 工具与 CI 脚本 | AI 实施 / 用户 审查 |

> **项目分工**: 用户 负责方向决策与边界约束，AI（LLM agent）负责具体实施. 见 §9.1.

## 2. 构建与测试

### 2.1 构建命令

```bash
./ci/build.sh all                  # x86_64 + aarch64, 0 error / 0 warning
./ci/build.sh x86_64              # 单架构 (开发时)
make test-host                     # host-tests
make test-kernel-host              # 内核单元测试 (host 侧 #[cfg(test)] 内联用例)
./scripts/qemu_boot_test.sh x86_64 # QEMU 集成 (改动 boot 时必跑)
```

### 2.2 核心审计脚本（硬规则门槛）

| 脚本 | 作用 | 对应 §5 硬规则 | CI 门禁 |
|---|---|---|---|
| `audit_services_boundary.py` | services 0 unsafe + 顶层 re-export 强制 | F1 + F2 | 强制 |
| `audit_safety_coverage.py` | framework unsafe 块 SAFETY 100% 覆盖 | F4 | 强制 |
| `audit_deadlock_matrix.py` | 锁顺序 + 中断上下文 + 递归锁检测 | F8 | 强制 |
| `audit_coupling.py` | 跨模块循环依赖 | F3 | 强制 |
| `audit_comment_language.py` | 中文注释强制 | F7 | 强制 |
| `audit_invariants.py` | 6 安全不变式断言 | I1-I6 | 强制 |
| `audit_once_cell.py` | OnceCell 模式统一 | 附加门禁 | 强制 |
| `audit_c_naming.py` | C 命名规范 | 附加门禁 | 强制 |
| `audit_repr_c.py` / `audit_volatile_access.py` / `audit_static_mut.py` | repr(C) 字段错位 / volatile 访问 / static mut 防线 | 附加门禁 | 强制 |
| `audit_tcb_ratio.py` | TCB 占比统计（软 < 30%）| 软约束 | 报告 |

> **附加门禁**：OnceCell / C 命名 / repr(C) / volatile / static_mut 已接入 CI 强制，但不占 §5 的 F1-F9 零容忍编号（属内存/风格专项防线，映射 I2/I5/I6 不变式）.
> **完整清单**：仓库共 22 个 `audit_*.py`（本表为常用核心门禁项，非穷举）. `block_registration`(I-43) / `tlb_receive_order`(S-14) / `aarch64_kernel_fp_free`(FP-06) 等专项亦在 `ci/audit.sh` 强制但未列入本表; 其余为按需扩展审计（未接入 CI）.

### 2.3 验证门槛

每轮开发完成，**必须** 全部满足：

1. 双架构 `cargo check --release` 0 error / 0 warning
2. clippy 0 warning (`cargo clippy --release -- -D warnings`) + `cargo fmt --check` 0 漂移（kernel / host-tests / src/rust 三 crate，见 §3）
3. 核心审计全部通过（见 §2.2）+ GitHub Actions
4. host-tests 全部通过
5. QEMU 集成测试通过（如改动 boot/架构相关）
6. host 侧内核单元测试 0 failed（`make test-kernel-host`，等价 `cd src/kernel && cargo test --features host-test --lib`）

## 3. 工具链

- **Rust 工具链** 锁定在仓库根 `rust-toolchain.toml`（`nightly-2026-10-02`，含 rust-src / llvm-tools-preview / rustfmt / clippy 与三交叉目标；CI 与本地均以该文件为唯一来源）
- **Edition：** 2024
- **目标架构：** x86\_64（主）+ aarch64（次）
- **`rustfmt.toml`：** 仓库根 `rustfmt.toml`（4 空格缩进 + 垂直尾逗号；kernel / host-tests / src/rust 统一受检）
- **Clippy 配置：** `src/rust/clippy.toml`（cognitive-complexity-threshold = 25）
- **`cargo-deny` 配置：** `src/rust/deny.toml`（许可证/漏洞/版本治理）

## 4. 架构责任分离（核心）

### 4.1 一句话判据

**要 unsafe 吗？要 → framework。不要 → services。涉及硬件/MMU/中断/上下文切换？→ framework。纯算法/策略/业务？→ services.**

**归属决策树（2026-09-11 依 Asterinas framekernel 标准补全，TCB 最小化关键）**：

```
Q1: 该功能必须 unsafe 吗（直接碰硬件/页表/裸内存）？
 ├─ 否 → services（纯策略/功能）
 └─ 是 → Q2: 它是"机制"还是"功能"？
       ├─ 机制（页表/上下文切换/寄存器原语/同步/安全代理）→ framework
       └─ 功能（驱动/文件系统/网络栈/进程/信号/syscall）→ Q3
             Q3: 能否封装为 safe API 供 services 用？
              ├─ 能 → framework 留机制原语 + 封装 safe API（IoMem/IoPort/
             │        DmaStream/UserPtr），功能实现在 services（0 unsafe）
              └─ 不能（self-referential / FFI ABI / 中断上下文）→ framework 薄层
```

> 要点：**"要 unsafe" ≠ "放 framework"**。驱动等要 unsafe 的**功能**应由 framework 封装 safe API 后实现在 services——这是 Minimalism 准则（TCB 最小化）的落地关键。

### 4.2 6 安全不变式

修改 framework 时必须逐项自检（详见 `docs/explain/explain-framekernel.md` 与 `docs/explain/spec-engineering.md`）：

- **I1** 内核态 CPU 状态不可被 services 篡改
- **I2** 内核内存不可被 services 非法访问
- **I3** 用户态 CPU 状态只能通过 framework 安全入口
- **I4** 用户内存只能通过 framework 安全代理
- **I5** 外设 MMIO/PIO 只能通过 framework 安全代理
- **I6** 外设 DMA 不可写入内核内存

> 违反任一不变式 = 重新设计（不允许补丁式补救）.

## 5. 硬规则（零容忍，违反即拒收）

| # | 规则 | 检查方式 |
|---|---|---|
| F1 | services 层 0 unsafe | `#![deny(unsafe_code)]` + `audit_services_boundary.py` |
| F2 | services 禁止访问 framework 内部模块 | `audit_services_boundary.py` 黑名单 |
| F3 | 新增代码禁止引入模块间循环依赖 | `audit_coupling.py` |
| F4 | framework 任何 unsafe 块必须配 `// SAFETY:` 注释 | `audit_safety_coverage.py` |
| F5 | 双架构编译 0 warning 0 error | `./ci/build.sh all` |
| F6 | 核心审计全部通过 | 见 §2.2 |
| F7 | 中文注释强制 | `audit_comment_language.py` 0 violations |
| F8 | 公共 API 中文文档注释 | clippy `missing_docs_in_crate_items` |
| F9 | 新增代码禁止任何类型的死代码注释（`#[allow(dead_code)]` / `#[allow(unused)]` 等），无豁免，无例外 | Rust 编译器 dead_code lint |

> **编码规范（建议性）**: 见 `docs/explain/spec-engineering.md`. 软规范非拒收条款.

## 6. 文档规范说明

- **`docs/plan/`**（任务规划）：强制使用结构化格式 — 每条目 = `描述：` + `方案：` + `状态：[]/[X]` + 可选 `详情：`
- **`docs/explain/`**（描述性说明）：**禁用**结构化字段；采用 H1/H2 + 自然段落 + 表格 + 代码片段 + 列表的自由描述风格
- **`docs/report/`**（生命周期内的结果报告：诊断/审计/评估/测试/性能等）：用途是作为**制定 plan 与启动修复工程的输入依据**；内容描述规则与 explain 一致（自由描述风格，禁结构化字段与状态标记）；定位为"某次执行结果的一次性快照"，发布即冻结（冻结事实陈述，不冻结其指导价值）. 历史 audit-* 仍在 `docs/plan/` 原位保留, 新报告写 `docs/report/`
- **`docs/plan/archive/`**：保留所有历史格式（含日期），作为历史快照不再修改

**核心原则**: 文档状态由 git 提交历史承载（`git log -- <path>` / `git blame`），不在文档内写日期；文件名不带日期前缀.

## 7. Git 规范

### 7.1 Commit 规范

- **主题行**：命令式，≤ 72 字符.
- **常见前缀**：`Fix` / `Add` / `Remove` / `Refactor` / `Rename` / `Implement` / `Enable` / `Clean up` / `Bump` / `docs` / `test` / `build` / `chore`.
- **Scope 标识**：`feat(net): W4.2.3 socket_open_stub Tcp/Udp 实装`

### 7.2 PR 规范

- 一个 PR 一个主题. CI 必须全部通过.
- TCB 占比上升需在 PR 描述中说明原因与后续降低计划.
- 新增 framework unsafe 块需 review 多名 reviewer.

### 7.3 Remote 操作约定

> **本节是远程 Git 操作（`push` / `pull` / `fetch` / `clone`）的前置规范**.

进行任何远程 Git 操作前，**必须**先查询当前 git 状态信息，遵循用户的实际配置（不预设约定）：

- **查询当前 remote 配置**：`git remote -v` 查看远程仓库 URL 与命名
- **查询当前分支与上游**：`git branch -vv` + `git status` 查看本地分支与远程跟踪
- **查询远程默认分支**：`git symbolic-ref refs/remotes/origin/HEAD`（如果配置）或 `git remote show <remote>`
- **查询未推送提交**：`git log --oneline @{u}..` 查看 ahead 数

**遵循用户喜好**：

- AI **不预设** remote 命名约定（如"必须叫 `origin`"），不预设推送策略（如"必须 rebase"），不预设分支命名（如"必须叫 `main`"）
- 根据查询结果，**复用用户当前的命名与配置**
- 若用户未配置某项，**提问**而非假设（见 §12.1）

**禁止行为**：

- 在未查询状态下硬编码 `origin` / `main` / `git push` / `git pull --rebase` 等命令
- 假设远程默认分支是 `main`（可能是 `master` / `develop` 等）
- 假设 remote 名称（可能是 `origin` / `upstream` / 其他）

**示例**（流程而非命令模板）：

```
1. 查询: git remote -v → 确认 remote 名 + URL
2. 查询: git branch -vv → 确认当前分支 + 上游
3. 查询: git status → 确认 working tree 干净 + ahead/behind
4. 提问: 推送策略 (rebase / merge / fast-forward) 与目标分支
5. 执行: 用户确认后, 按用户实际配置执行 git push
```

## 8. 测试规范

- **每个 bug 修复加回归测试**（附 issue 引用）.
- **测用户可见行为**（公共 API），不测内部实现.
- 修改跨模块接口必须补充集成测试（host-tests）.
- 公共 API 必须有单元测试（no_std 单元 + host-tests 集成）.
- 性能基线（`host-tests/benches/baseline.json`）每次 PR 更新.

## 9. 预存问题处理

### 9.1 决策灰色地带

遇到方案 A vs B 选择时，AI 应停下询问用户而非自行选择（属"决策者"职责）。**绝不盲从 Linux 实现**（§12.1 强调）。

### 9.2 文档与代码不同步

当代码实装完成但 plan 文档状态仍标 `[]` 或未更新时，视为预存问题，立即同步. 流程：grep + git log + 改为 `[X]` + 重跑 §2.3 验证门槛.

### 9.3 死代码零容忍

新增代码和预存代码一律禁止任何类型的死代码注释（`#[allow(dead_code)]` 等），无豁免. 硬件规范常量必须通过实现使用路径消除. 见 §5 F9.

### 9.4 AI 输出审查清单

任何 AI 输出（代码/文档/脚本）在合并前必须经用户审查：

- **架构合规**：未越过 §5 硬规则（F1-F9），未引入 services unsafe
- **安全注释**：framework `unsafe` 块都有 `// SAFETY:` 注释（F4）
- **决策溯源**：关键设计选择有对应 commit 消息或 plan 文档记录
- **测试覆盖**：新增代码有单元测试，跨模块接口有集成测试
- **文档同步**：API 改动对应 docs/explain 或 docs/plan 同步更新
- **不留 TODO**：无 `// TODO(TRACK-...)` 未处理项
- **风格一致**：命名/注释/格式符合 `docs/explain/spec-engineering.md`
- **不盲目重构**：未对用户未要求的部分做"顺手优化"（§12.2）

AI 输出若不通过上述审查，视为预存问题，必须修复后才能合并.

## 10. 源码调研要求（强制流程）

> **本节是实施前的强制流程规则**. 与 §12.1 编码前先思考联动: 先思考, 再调研, 再规划, 再施工.

### 10.1 调研范围

开始实施用户指派的工程前，**必须**调研与工程相关的源码，构成清晰的源码认知后再规划施工.

调研范围（按工程相关性排序）：

- **直接相关模块**：当前工程要修改/扩展的源文件、所在目录、相邻模块
- **依赖模块**：被调用方、被调用方所在子树（framework / services）的公开 API
- **约束相关**：安全不变式（§4.2 I1-I6）、硬规则（§5 F1-F9）、编码规范（`docs/explain/spec-engineering.md`）
- **相似工程**：git log 中同类工程的历史实现（`git log --oneline -- <related_path>`）
- **设计文档**：`docs/explain/` 下相关章节，确认当前设计意图

### 10.2 认知构建要求

调研应**形成可追溯的源码认知**，而非简单浏览：

- 用 `grep` / `glob` / `read` 工具系统性搜索（不是随手 cat）
- 跟踪关键类型/函数/常量的**定义位置与所有使用点**
- 理解模块间的**调用关系与依赖方向**
- 识别**潜在冲突点**（与现有规范/规则/约束的兼容性）
- 对不清楚的地方**提问**（见 §12.1），不假装理解

### 10.3 规划与施工

源码认知构成后，**再**进入规划与施工：

- **规划**：基于认知编写简要计划（步骤 + 验证项，见 §12.4）
- **施工**：遵循 §12.2 外科手术式修改原则
- **同步**：文档与代码同步（见 §9.2）

### 10.4 跳过调研的后果

- 跳过调研直接施工 → **高概率引入 §6 硬规则违反**（F1/F2/F4 等）
- 跳过调研导致设计冲突 → 重做成本远大于调研成本
- 跳过调研做出错误假设 → 违反 §12.1"不要假设"准则

**质量保证**: 源码调研是工程质量的**前置门槛**，不是可选步骤.

## 11. AI 常见踩坑

| 踩坑 | 解决 |
|---|---|
| 把 unsafe 写进 services/ | 编译失败，改用 framework 公开的 safe API |
| 在 services/ 用 `println!` | no_std 不可用，改用 `klog::printk` 或 framework 日志 API |
| 中断上下文持 Mutex 或分配 `GFP_KERNEL` | 死锁，中断路径只持自旋锁并 disable IRQ |
| 修 bug 时顺手清理无关代码 | 禁止，每一行改动追溯到用户请求 |
| 在 services/ 直接 `use framework::arch::x86_64` | 边界审计拒绝，走顶层 re-export 公共 API |
| 跳过 SAFETY 注释 | `audit_safety_coverage.py` 检测，100% 强制 |
| 跨子系统硬编码常量 | 走 `framework::config` 或 `services::config` |
| 测试代码 `unwrap()` | 测试允许，生产代码禁止 |
| 提交前忘跑审计 | CI 会拦，不会合入 |
| 顺手添加"灵活配置" | 禁止，spec-engineering.md 铁律 0 严格适用 |
| 不读 AGENTS.md §12 AI 行为准则 | 必读！LLM 行为准则在此 |
| 引入 `#[allow(dead_code)]` | 零容忍（§5 F9），必须通过实现使用路径消除 |
| **跳过源码调研直接施工** | **§10 强制要求：调研 → 认知 → 规划 → 施工** |

## 12. AI 行为准则

> QueenX 项目的 **LLM 通用行为准则**. 与上述项目规则协同使用. 冲突时 **以前述项目规则为准**.

### 12.1 编码前先思考

**不要假设. 不要掩饰困惑. 明确呈现权衡.**

在实现之前：
- 明确写出你的假设. 如果不确定，就提问.
- 如果存在多种解释，先把它们列出来，不要默默自行选择.
- **决策灰色地带**（方案 A vs B 选择）：停下询问用户，不擅自选择. 见 §9.1.
- **实现路径选择（简约 vs 相对完整）**：用户委派的工程若涉及架构关键路径（调度器/同步原语/安全敏感模块/TCB 边界），或工程复杂度本身具有二义性（"最小 demo 还是工业级实装"），AI **必须先向用户提问** 二选一（简约实现 / 设计相对完整的实现），获得明确授权后再施工.见 §12.3.
- **实施前必须先做源码调研**（§10），构建清晰认知后再规划施工.

#### 12.1.1 长远视角

分析当前方案是否为内核长期演进的最优解，而非仅满足当下需求的最短路径.

#### 12.1.2 业界参考

同类问题参考其他内核（Linux / Redox / seL4 / Asterinas / Genode 等）的处理方式，吸纳其经过验证的设计优点，结合 QueenX 自身的架构约束独立推理. 目标有二：一是避免重复踩坑、少走弯路；二是在充分理解业界取舍本质后，探索出优于现有业界实现的新方案.

### 12.2 外科手术式修改

**只改必须改的内容. 只清理你自己造成的问题.**

编辑现有代码时：
- 不要"顺手优化"相邻代码、注释或格式.
- 不要重构没有坏掉的部分.
- 保持现有风格，即使你个人会写成别的样子.
- **如果发现无关的预存问题**：可以指出，但 **不要顺手删除**. 报告给用户，等待单开 PR 或明确授权.

检验标准：每一行改动都应当能直接追溯到用户请求.

### 12.3 简约实现为默认 + 显式询问前置

**默认路径：在保证质量的前提下采用简约实现.任何超出最小可行解的复杂度都需在代码中以 SIMPLIFIED 注释标记, 以便后续审查时识别简化意图与扩展边界.**

- **简约实现细则**（与 docs/explain/spec-engineering.md 一致）：
- 不为单次使用的代码创建抽象、不为"将来可能用到"预留扩展点.
- 不加入超出当前需求范围的功能、可配置性、灵活性.
- 三行重复代码优于一个过早抽象；自问"一个资深工程师会认为这太复杂了吗？".
- 如果你写了 200 行，但 50 行就够，就重写.
- **SIMPLIFIED 注释标记**（必填，简约实现路径专属）：
- 格式：`// SIMPLIFIED: <具体简化点>; <影响面>; <何时需扩展>`
- 位置：直接放在被简化代码的上方（函数体 / 结构体 / trait impl / 模块顶部均可）.
- 触发场景：跳过错误处理路径 / 合并分支 / 硬编码魔法数 / 省略 trait 抽象 / 简化为 if-else 而非 match / 复用其他模块内部接口 / 暂用全局可变替代局部状态 等.
- 反例：仅写 `// 简化实现` 或 `// TODO 完善` 不合格 — 必须说明"简化了什么 + 影响 + 何时需扩展".
- **关键例外（必须前置询问）**：
- 用户委派的工程若涉及**架构关键路径**（调度器 / 同步原语 / 安全敏感模块 / TCB 边界）或**工程复杂度本身具有二义性**（如"最小 demo 还是工业级实装"），AI **不得自行选择实现路径**，必须先通过 AskUserQuestion 给出方案对比（复杂度估算 / 验证成本 / 后续扩展难度），由用户显式授权后再施工.
- 提问模板：列方案 A（简约）+ 方案 B（相对完整）+ 推荐项 + 各自取舍（行数估算 / 测试覆盖 / 后续扩展成本 / 与 §2.3 验证门槛的契合度）.
- **「相对完整」的本项目定义**：指 **代码逻辑稳定且相对可靠完备** — 即路径 / 边界 / 错误分支完整闭合、具备单元测试覆盖、可长期稳定运行；**不**追求"工业级功能最全"或"对齐 Linux 全部语义".
- **禁止"事后自圆其说"**：已按 A 方案施工后才发现"应该选 B"，必须登记为预存问题（§9.2），不擅自追加范围.

**判断口诀**：
- 用户委派的是**实验 / 原型 / 验证工程** → 默认 A（简约），**无需询问**（除非是核心子系统关键决策）.
- 用户委派的是**生产 / 架构 / 长期演进工程** → 即使默认选 A 也**必须询问**（用户可显式说"用 A 即可"跳过）.
- 不确定时 → 询问（§12.1 决策灰色地带原则）.

### 12.4 目标驱动执行

**先定义成功标准，再循环推进，直到验证通过.**

把任务转换成可验证的目标：
- "添加校验" → "先为非法输入写测试，再让测试通过"
- "修复这个 bug" → "先写能复现它的测试，再让测试通过"
- "重构 X" → "确保改动前后测试都通过"

具体的项目验证门槛见 §2.3. 任何一项失败 → 本轮未完成.

**成功标准强度与 §12.3 实现路径联动**：成功标准的强度由 §12.3 选定的路径（简约 vs 相对完整）决定 — 简约路径下成功标准可以是"功能 demo + 核心测试通过"；相对完整路径下需加压"性能基准 + 全 syscall 兼容"等. 但 **§2.3 五条验证门槛（双架构 0w0e / clippy 0 warning / 三审计 / host-tests / QEMU）是底线, 任何路径都不可豁免**.

### 12.5 工程外问题代码处置

执行当前工程任务时，若发现与本次工程无关的问题代码，通过提问工具向用户确认处置方式（**规划方案并修复** / **记录到 docs/plan/** / **搁置与跳过**）. 禁止在用户未确认前自行处置. 因本次工程改动直接导致的遗留项（§9.3 范畴）必须在本轮修复，不需询问.

---

**如果这些准则正在发挥作用，你会看到：** diff 中不必要的改动更少，因为过度复杂而返工的次数更少，而且澄清性问题会出现在实现之前，而不是出错之后.