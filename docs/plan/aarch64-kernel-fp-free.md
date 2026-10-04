# aarch64 内核零浮点工程（KPTI-18b）

> 本文件是 [kpti-complete-project.md](./kpti-complete-project.md) 中 **KPTI-18b（aarch64 用户态 FPU 上下文跨 syscall 不保留）** 的独立工程文档。
>
> 目标：使 aarch64 内核在 EL1 **不执行任何 FP/SIMD 指令**，从而恢复"EL0 用户 FP 上下文跨 SVC/IRQ 保留"的契约，无需在异常帧里增存 V0–V31/FPCR/FPSR。
>
> 关联裁定：DECISION-064（长期方案四层候选，本文件第 2 节给出对其的实证修正）。

## 1. 根因与性质

**根因**：`boot/aarch64/start.S:74-76` 显式设置 `CPACR_EL1.FPEN = 0b11`（trap none），并在 `el2_entry` 清 `CPTR_EL2.TFP`（bit 10）——内核**主动放开**了 EL1 的 FP/SIMD 访问。使能条件成立后，编译器生成的隐式 NEON（结构体清零/memset 内联）与 `services`/`framework` 侧显式 `f64`/`f32` 都会在 EL1 执行 FP/SIMD。而 EL0 边界（SVC/IRQ）的 `ExceptionFrame`（35×8，仅 x0–x30 + elr/spsr/sp）**不含 V0–V31/FPCR/FPSR** ⇒ 内核一旦执行 FP/SIMD，用户 V0–V31 即被破坏。

**性质判定**：**契约级缺陷，非"潜在风险"**。仅因当前 `src/user/init` 不使用浮点而**暂不可观测**。

**关键认知（本轮修正）**：根因不是"边界没保存"，而是"内核在 EL1 执行 FP/SIMD 却无声明"。因此 DECISION-064 列出的候选 ①②③（全量保存帧 / lazy FPU / 仅 caller-saved）都是"每次进出边界付代价"的补偿，均不采用；采用第 ④ 条"**内核零 FP/SIMD**"。

## 2. 实证结论（本轮四项决定性实验，均已复现）

| # | 命题 | 实测结论 |
|---|---|---|
| E1 | `-C target-feature=+general-regs-only`（Linux `-mgeneral-regs-only` 的直觉等价）可抑制隐式 NEON | **无效**。与基线逐指令完全一致（同一 snippet 均 482 insns / FP 3 / SIMD 16）。rustc 报 `unknown and unstable feature ... general-regs-only`（仍透传 LLVM 但无效果）⇒ **DECISION-064 中"该路径不存在"的结论成立** |
| E2 | `-neon` 是唯一有效杠杆 | **成立**。标量浮点降级为软浮点 libcall（`__adddf3`/`__gtdf2`），NEON 自动向量化消失。snippet 实测 29 insns / FP 0 / SIMD 0 |
| E3 | `-neon` 会产生不可抑制的 warning | **成立**。rustc issue #116344：`target feature 'neon' must be enabled to ensure that the ABI ... it will become a hard error in a future release!`；`-A unsupported_target_feature` 实测**无法抑制** ⇒ 直接加 flag 会破坏 F5「0 warning」，且未来工具链升级会变 hard error |
| E4 | `aarch64-unknown-none-softfloat` 是零 warning 等效替代 | **成立**。官方 target spec 载明 `features: +v8a,+strict-align,-neon`、`llvm-target: aarch64-unknown-none` ⇒ 等效 `-neon` 且**零 neon warning** |

**两 target 的唯一差异（实测 `rustc --print target-spec-json`）**：

```
aarch64-unknown-none           : features = "+v8a,+strict-align,+neon"
aarch64-unknown-none-softfloat : features = "+v8a,+strict-align,-neon"
```

`data-layout` 与 `llvm-target` 完全相同 ⇒ **仅 `+neon` → `-neon`**，`+strict-align` 两者本就有，**不引入新的对齐风险面**。

**E5（副产物）：`-neon` 会折断内核自身的 FP 汇编**

`arch/aarch64/context.rs` 的 `context_switch_asm`（`global_asm!`）在内核内显式保存/恢复 V0–V31 与 FPCR/FPSR。`-neon` 随之禁用 LLVM 集成汇编器的 FP 寄存器支持，导致 **36 个汇编错误**：

- `error: instruction requires: fp-armv8` × 32（16 × `stp q*` 保存 + 16 × `ldp q*` 恢复）
- `error: expected writable system register or pstate` × 2（`msr fpcr` / `msr fpsr`）
- `error: expected readable system register` × 2（`mrs fpcr` / `mrs fpsr`）

**解法只需 2 行**：在该 `global_asm!` 块内加 `.arch_extension fp` + `.arch_extension simd`（实测有效，**无需新建 `.S` 文件、无需改 Makefile**）。

**E6：软浮点例程确由 `compiler_builtins` 提供**

`aarch64-linux-gnu-nm` 确认 `__adddf3` / `__floatsidf` / `__fixsfsi` / `__floattidf` 等符号存在于 rlib ⇒ 链接可行（早先"不存在"的误判源于正则过严）。

**E7：smoltcp 的 `mem*` 早已是整数 libcall，与 `-neon` 无关**

`nm` 实测当前 aarch64 内核对象中 `memcpy` / `memset` / `memmove` / `memcmp` / `strlen` 均为 `U`（未定义），由 `compiler_builtins` 定义（`build-std-features=["compiler-builtins-mem"]`）⇒ **大块内存拷贝本来就是整数实现，不因 `-neon` 退化**；只有小块已知尺寸的 inline 展开（`stp q` → `stp x`）与自动向量化循环消失。

**E8：软浮点 target 触发上游 future-incompat 报告（本仓不可修，决定接受不压制）**

切换到 `aarch64-unknown-none-softfloat` 后，构建时 cargo 打印：

```
warning: the following packages contain code that will be rejected by a future version of Rust:
core v0.0.0 (.../library/core), sha2 v0.11.0
```

归因（三项对照实验，临时 target-dir 均已清理）：

| 项 | 实测结论 |
|---|---|
| 触发条件 | **仅**软浮点 target 触发。旧 target（`aarch64-unknown-none`）+ 全新 target-dir ⇒ 0 报告；软浮点 target + 全新 target-dir ⇒ 有报告 ⇒ **与本仓增量缓存无关** |
| 报告来源 | `core`（stdarch 的 `aarch64/neon`、`aarch64/sve`、`aarch64/sve2`、`arm_shared/neon`，共 25006 条）与 `sha2 v0.11.0`（自身 `#[target_feature(enable="sha2")]` / `enable="sha3"`，共 4 条）。引入链 `sha2 ← ed25519-dalek ← kernel`；**smoltcp 完全不在报告内**（也不依赖 sha2） |
| 诊断文本 | rustc issue #134375：`enabling the 'neon' target feature on the current target is unsound due to ABI issues`（lint 文本固定写 `neon`，故 sha2 自身的 `sha2/sha3` 触发点也显示为 neon） |
| 是否阻断 CI | **否**。实测 `RUSTFLAGS="-D warnings"` + 软浮点 target ⇒ **exit 0**：cargo 对依赖 crate 施加 `--cap-lints`，future-incompat 报告只读打印、不升级为 error。`.github/workflows/ci-aarch64.yml` 的 `RUSTFLAGS: -D warnings` 因此不受影响 |
| 能否本仓消除 | **不能**。诊断在依赖 crate 内部（`core` 来自 `-Zbuild-std`，`sha2` 来自 crates.io）；唯一"消除"手段是压制报告（关闭 `[future-incompat-report]` 或 `--cap-lints allow`），而压制会连带掩盖未来真实的上游不兼容 |
| 上游先例（Asterinas 0.18.1，本地 `other/asterinas-0.18.1`） | 同 target（`osdk/src/arch.rs` 的 `Arch::Aarch64 => "aarch64-unknown-none-softfloat"`）、同 build-std 参数（`osdk/src/commands/util.rs` 的 `-Zbuild-std=core,alloc,compiler_builtins`）⇒ 同样吃到该告警；全仓 grep `future-incompat` **0 命中**（无任何压制配置），aarch64/loongarch CI 正常跑 `make check` / `make kernel` 且绿灯 |

⇒ **决定：接受并登记为监测项，不压制**。依据三条：(1) 与上游先例一致；(2) F5「双架构 0 warning」的语义指向**本仓 crate 的 rustc 告警**（`cargo check` 与 `clippy -D warnings` 三维实测均 0 warning），本条是 cargo 层的**依赖**报告，机制不同；(3) 压制会失去上游修复的可见性。

**端到端探针实证（临时探针已全部还原）**：

- `aarch64-unknown-none` + `-neon` + 2 行 `.arch_extension` → 构建通过、链接通过（0 未定义符号）、产物 **FP 0 / SIMD 32**（331427 insns）。
- `aarch64-unknown-none-softfloat`（无需 `-neon` flag）+ 2 行 `.arch_extension` → **零 neon warning**、构建+链接通过、产物 **FP 0 / SIMD 32**（331369 insns）。
- 仅剩 SIMD 32 = `context_switch_asm` 白名单内的 16 × `stp q*` + 16 × `ldp q*`。

**现状基线（`build/kernel.bin` 反汇编实测，326905 insns）**：

- `.text`：**FP 3462**（`fmov` 914 / `fmul` 806 / `fadd` 755 / `fsub` 287 / `fdiv` 145 / `fcmp` 127 / `fcsel` 111 / `fcvt` 78 / `fneg` 66 / `fabs` 40 / …）+ **SIMD 925**（`stp q*` 337 / `movi` 207 / `ldr` 125 / `str` 88 / `ldp` 52 / `mov` 34 / …）。
- `.text.boot` 106 / `.kpti_trampoline` 6 / `.vectors` 769：FP 0 / SIMD 0。
- 与 DECISION-064 记录的「913 `fmov` / 337 `stp q*`」吻合（现测 914 / 337）。

**性能账（三笔分开记，方向不一致）**：

| 影响面 | 方向 | 依据 |
|---|---|---|
| 内核自有 `f64`/`f32` → 整数化 | **变快** | 消除软浮点 libcall；且调用频次极低（`fragmentation_score` 无生产调用者） |
| smoltcp `cubic.rs` 的 `f64` → 软浮点 | **变慢，但不在热路径** | 每 ACK 更新一次（≈每 RTT 每连接）；DHCP/DNS 控制流几乎不碰。cubic 只用四则+比较（`powi`/`mul_add` 早已展开），无 libm 缺失 |
| 编译器隐式 NEON 消失 | **小幅变慢** | 见 E7；实测总体代码量 326905 → 331427（**+1.4%**） |

结论：**净效果略快**。软浮点为 IEEE-754 正确实现，smoltcp 拥塞控制的数值行为不变。此回退只影响 aarch64 内核产物；x86_64 与 host-tests（host 目标）不受影响 ⇒ `host-tests/benches/baseline.json` **不会自动捕获**此回退。

**对 DECISION-064 的修正**：

- 第 (1) 层"编译期 `-C target-feature=-neon` 抑制隐式 NEON" —— 杠杆正确，但**不可直接加 flag**（E3 破 F5），须改走 **target 三元组** `aarch64-unknown-none-softfloat`。
- 第 (2) 层"源码消除内核 `f64/f32` + 反汇编审计脚本" —— 由"必须"降为"**加固项**"：E2/E4 已证明 FP-free **由 flag 单独即可保证**（对全量代码，含 smoltcp 与未来新增代码）。本轮**仅做比率族 + 简单格式化**，Wu 定点重写暂缓（见 DECISION-079）。
- 第 (3) 层"线程 FP 状态懒式保存（对齐 `TIF_FOREIGN_FPSTATE`）" —— **不再必要**。内核零 FP 后，用户 V0–V31 在内核期间不被触碰，`context_switch_asm` 已有的全量保存/恢复即覆盖跨线程切换场景。

## DECISION-079（KPTI-18b 实施裁定）

- **描述**：KPTI-18b 走"内核零隐式 FP/SIMD"路线。施工前需裁定实现路径、审计手段与源码整数化范围。
- **方案（用户裁定四项）**：
  1. **实现路径 = 方案 A + 源码整数化**：切换 aarch64 内核构建 target 三元组为 `aarch64-unknown-none-softfloat`，并在 `global_asm!` 内加 `.arch_extension fp` / `.arch_extension simd`（E5）；同时把内核自有 `f64`/`f32` 改整数运算。
  2. **审计 = 一并新增反汇编白名单审计脚本**：对 aarch64 内核产物反汇编扫描 FP/SIMD，白名单 = `context_switch_asm` 的 32 条 `stp/ldp q*` + 4 条 `mrs/msr fpcr,fpsr`，判据「白名单外 FP/SIMD 计数 = 0」，挂入 `ci/audit.sh`。
  3. **整数化范围 = 10 个内核源文件**（比率族 7 + 简单格式化 3）；**Wu 定点重写暂缓**并登记为后继项（见 FP-08）。
  4. **比率类公共 API 统一千分比 `u64`（0..1000）**。
- **状态**：[X]
- **详情（裁定 3 的理由）**：`framework/driver/display/framebuffer.rs::draw_line_aa` 是 11 个候选文件中**唯一**的"真实浮点算法"（Wu 抗锯齿，f32），其余均为比率或格式化（整数化后可用 host-test 精确核对）。其整数化（f32→定点）会改变像素输出（±1 alpha 级），而调用方只有 `display/self_test.rs`、**不在 QEMU 启动路径** ⇒ **无任何测试可验证**；同时 flag 已保证其 FP 计数 = 0 ⇒ **收益为零、风险非零**。按 AGENTS §12.3（简约为默认）/ §12.4（成功标准必须可验证），剥离为后继项。
- **详情（裁定 1 的构建面耦合）**：`Makefile:9` 的 `RUST_TARGET` **同时服务内核与用户态构建**（`Makefile:152` 用户态、`Makefile:204` 内核共用）⇒ 直接改它会把 userland 一并锁死在软浮点 ABI。必须**新增内核专用变量**（内核用 softfloat、用户态保持 `aarch64-unknown-none`）。

## 任务清单

- **FP-01. 计划与决策固化**
  - 描述：施工前把 KPTI-18b 的实证结论与四项裁定写入专门计划文件（AGENTS §9.1 决策先记录）。
  - 方案：新建本文件，并在 [kpti-complete-project.md](./kpti-complete-project.md) 的 KPTI-18 条目补状态与指针。
  - 状态：[X]
  - 详情：实证 E1–E7、性能账、对 DECISION-064 的三处修正、裁定四项均已落文。

- **FP-02. aarch64 内核构建 target 切换为 `aarch64-unknown-none-softfloat`**
  - 描述：使内核产物 FP 指令数归零（E2/E4）。
  - 方案：**拆分为内核与用户态两个 target 变量**——`Makefile` 新增内核专用目标（aarch64 取 `aarch64-unknown-none-softfloat`、x86_64 不变），内核构建命令与 `RUST_LIB*` 路径改用它；用户态构建（`Makefile:152`/`196`）与 `src/user/.cargo/config.toml` **保持不动**。同步以下引用点（排除 `src/user/` 与 `docs/plan/archive/`）：
    - `Makefile`：`RUST_TARGET` 拆分 + `RUST_LIB*`（79-82）+ 内核构建（204/211/218/222/227）
    - `Makefile.ci:68`、`ci/build.sh:154,159`、`ci/audit.sh:148`
    - `.github/workflows/ci-lint.yml:254`、`ci-x86.yml:197`、`ci-aarch64.yml:26,42,61`
    - `tools/elfld/build.sh:43`、`scripts/requirements.sh:11,604,605,875`、`src/rust/.cargo/config.toml:7`
  - 状态：[X]
  - 详情（落地形态）：`Makefile` 新增**内核专用变量** `RUST_TARGET_KERNEL`（aarch64 = `aarch64-unknown-none-softfloat`，x86_64 与本 target 同值并附注释说明无 softfloat 之分）；内核构建命令与 `RUST_LIB*` 四径（release / test-release / chaos-release / test-debug）改用它；用户态 `RUST_TARGET`（`Makefile` 用户态构建分支与 `src/user/.cargo/config.toml`）保持 `aarch64-unknown-none`（用户程序仍可用 FP/SIMD）。
  - 详情（同步点）：`Makefile.ci`、`ci/build.sh`（aarch64 与 all 两分支）、`ci/audit.sh`（双架构 check 循环）、`.github/workflows/ci-aarch64.yml`（target 安装 + check；QEMU job 同时装 `aarch64-unknown-none` 与 `-softfloat`，因用户态仍需前者）、`.github/workflows/ci-lint.yml`（rustdoc aarch64）、`.github/workflows/ci-x86.yml`（clippy aarch64）、`scripts/requirements.sh`（安装目标 + 检查清单 + 推荐计数）、`src/rust/.cargo/config.toml`（新增 `[target.aarch64-unknown-none-softfloat]` 段，rustflags 与 `aarch64-unknown-none` 逐项一致）。
  - 详情（对方案清单的两处修正）：① `tools/elfld/build.sh:43` **不改** —— 该脚本用 `$CC` 构建**用户态** `elfld.so`，其 `TARGET` 变量只赋值、不参与任何命令（全文件无 `$TARGET` 引用），属用户态范畴；② 无需新增 `-neon` flag（E3 已排除该路径：破坏 F5，且未来工具链升级变 hard error）。

- **FP-03. `context_switch_asm` 加 `.arch_extension`**
  - 描述：`-neon` 使 LLVM 集成汇编器禁用 FP 寄存器，`context.rs` 的 `global_asm!` 报 36 个错误（E5）。
  - 方案：在 `arch/aarch64/context.rs` 的 `global_asm!` 块内（`.section .text.context_switch, "ax"` 之后）加 2 行 `.arch_extension fp` / `.arch_extension simd`，并附中文说明"为何必须显式启用"。
  - 状态：[X]
  - 详情：已在该 `global_asm!` 块内（`.section .text.context_switch, "ax"` 之后）落 2 行 `.arch_extension fp` / `.arch_extension simd` 及中文说明；未新建 `.S` 文件、未改 Makefile。与 FP-02 同轮落地（否则构建中断）。此块是反汇编审计白名单的唯一来源（其余 FP/SIMD 必须为 0）。

- **FP-04. 内核自有 `f64` 整数化 —— 比率族（7 文件）**
  - 描述：消除内核公共 API 上的浮点暴露（对齐 Linux arm64 `-mgeneral-regs-only` 的形态约束）。
  - 方案：统一 **千分比 `u64`（0..1000）**。
    - `framework/mm/pmm_trait.rs`：trait 签名 `fragmentation_score -> f64` → `-> u64`（千分比）+ `FallbackPmmPolicy` 默认实现整数化。
    - `services/mm/pmm_policy.rs`：impl + 单元测试断言改定点（`0.7→700`、`0.35→350`、`0.79→1000`、`0.15→150`、`0.0→0`）。
    - `framework/mm/swap_trait.rs` / `services/mm/swap_policy.rs`：`should_wakeup_kswapd` 函数体整数化（**返回类型 `bool` 不变**）。
    - `services/mm/swap.rs`：`usage_ratio -> f64` → `-> u64`。
    - `framework/mm/slab.rs`：`hit_rate` / `utilization` → `u64`（原为百分数 `*100.0`，统一到千分比）。
    - `services/fs/unkfs/arc_trait.rs`：trait + impl `hit_rate -> f64` → `-> u64`，并同步其文件内测试。
  - 状态：[X]
  - 详情（`fragmentation_score` 调用面）：全仓**无生产调用者**（仅 `#[cfg(test)]` 内调用）；`should_wakeup_kswapd` 生产调用者唯一 = `framework/mm/swap.rs:941`；`slab::hit_rate`/`utilization` 与 `swap::usage_ratio` 当前无调用者。参照形态：`framework/fs/vfs/dcache.rs` 的 `dcache_hit_rate -> (u64, u64)`（已是整数）。`host-tests/src/framekernel_bench.rs` 的 `HostArcCache::hit_rate -> f64` 经核实是 **host-only 独立 trait**（非内核 `arc_trait` 的副本引用，无编译耦合）⇒ **不改**，作为预存平行实现另行报告。
  - 详情（本轮更正的既有断言缺陷）：`services/mm/pmm_policy.rs` 原测试断言 `assert!((… - 0.79).abs() < 1e-9)` 与其推导注释 `0.7*0.7 + 1.0*0.3 = 0.79` **算术错误**（`0.7*0.7 + 1.0*0.3 = 0.79` 实为 `0.49 + 0.3 = 0.79`，但公式为 `(1-free)*7/10 + fail*3/10`，`free = 0.3` 时 `(1-0.3)*0.7 = 0.49`，须配 `fail` 比例；原断言取 `free_ratio = 0`、`fail_ratio = 1.0` 时实算 `1.0`）。原 f64 实现返回 `1.0`，与断言 `0.79` 本就不符 —— 属**预存缺陷**（非本轮引入）。本轮按千分比公式统一修正为 `1000`，并已登记于本详情。**补充（本轮普查）**：该断言之所以长期未暴露，是因为所在 `#[cfg(test)]` 模块**从未被任何门槛编译**（`[lib] test = false` + 依赖不激活 `cfg(test)`）—— 属全仓 104 文件 / 770 例的同类问题，已单独立项，见 [kernel-unit-test-harness-unification.md](./kernel-unit-test-harness-unification.md)。

- **FP-05. 内核自有 `f64` 整数化 —— 简单格式化（3 文件）**
  - 描述：清除按需路径上的显式浮点格式化。
  - 方案：改整数除法/取余。
    - `services/fs/procfs_core.rs:179-207`：cpuinfo MHz / bogomips 的 `as f64 / 1_000_000.0` → 整数商余（`hz / 1_000_000` + `hz % 1_000_000 / 10_000` 两位小数）。
    - `services/driver/virtio/blk.rs:237`：容量 MB → `(capacity * 512) / (1024 * 1024)`。
    - `framework/driver/storage/mod.rs:90`：同上。
  - 状态：[X]
  - 详情：三处均改整数商余（MHz = `hz / 1_000_000` + `(hz % 1_000_000) / 10_000` 两位小数；MB = `sectors * 512 / (1024 * 1024)`）。已 grep 确认**无 host-test 断言**这些格式化字符串 ⇒ 风险低；逐处核对输出与整数化前一致（MHz 显示小数位对齐：整数化前的 f64 打印为 `xxx.xxxxxx` 全精度，现收敛为两位小数，属格式化精度收窄，语义不变）。

- **FP-06. 反汇编白名单审计脚本**
  - 描述：把"内核零 FP/SIMD"从依赖编译 flag 变为可 CI 强制的判据。
  - 方案：新增 `scripts/audit_aarch64_kernel_fp_free.py`（或既有命名风格下的等价名），对 aarch64 内核产物 `build/kernel.bin` 反汇编扫描：统计 `.text`/`.text.boot`/`.vectors` 等的 FP/SIMD 指令，**白名单** = `context_switch_asm` 段内 16 × `stp q*` + 16 × `ldp q*` + `mrs/msr fpcr,fpsr` × 4；判据「白名单外 FP/SIMD 计数 = 0」，不通过则非零退出（故障关闭：不可检查 = 违规）。挂入 `ci/audit.sh`。
  - 状态：[X]
  - 详情（实际形态）：脚本为 `scripts/audit_aarch64_kernel_fp_free.py`，白名单**按符号界定而非按段** —— 链接脚本把 `.text.*` 合并进 `.text`，aarch64 内核 ELF **不存在独立的 `.text.context_switch` 段**（实测 `objdump -h` 仅有 `.text.boot`/`.text`/`.kpti_trampoline`/`.vectors`/`.rodata`/…），故以符号 `context_switch_asm` 的地址范围为界（实测 `ffff00004015a288`–`0x…a484`，共 508 字节 / 127 条指令）。
  - 详情（判据与白名单收紧）：判据 = **白名单范围外 FP/SIMD = 0**；同时白名单区间内的 FP 相关指令必须严格匹配两种形态之一 —— `stp/ldp qN, qN'`（必须为 2 个 q 寄存器）或 `mrs/msr` 访问 `fpcr|fpsr`（不得带 q 寄存器），且计数须等于预期值（**stp/ldp 32 条 + fpcr/fpsr 4 条**）。形态或计数不符同样判违规 ⇒ 白名单本身也受约束，任何改动都必须显式更新脚本。
  - 详情（识别方式）：匹配前先剥离 objdump 的 `<符号>` 注解与 `//` 行尾注释，再匹配操作数中的 q/v/s/h/d/b 寄存器（避免符号名与 `0xb0` 类立即数误匹配；已用正/负对照验证：`fadd d0`/`movi v0.16b`/`mrs fpsr` 均捕获，`x 寄存器 + #0xb0` 不误报）。
  - 详情（fail-closed 分支，全部实测 exit 1）：产物缺失 / 非 64 位 ELF / `e_machine != 0xB7`（AArch64）（附"先跑 `./ci/build.sh aarch64`"提示）/ 无可用 objdump 或反汇编失败或输出为空 / 符号 `context_switch_asm` 出现次数 ≠ 1。反汇编工具按 `aarch64-linux-gnu-objdump` → `llvm-objdump` → `objdump` 顺序探测，取首个可产出 `Disassembly of section` 者。
  - 详情（接入与前置条件）：挂入 `ci/audit.sh` 的 `0.5i/6` 步（quick 模式亦跑，退出码经 `PIPESTATUS` 等价方式显式捕获，不用 `| tail` 掩盖）。**前置** = `build/kernel.bin` 须为最近一次 aarch64 链接的产物 —— 双架构共用该输出路径且 `./ci/build.sh all` 最后链接 x86_64，因此验收顺序须为 `./ci/build.sh all` → `./ci/build.sh aarch64` → `./ci/audit.sh quick`（脚本与 `ci/audit.sh` 的注释均已载明）。

- **FP-07. §2.3 五门槛验证**
  - 描述：本轮交付的验收判据。
  - 方案：`./ci/build.sh all`（双架构 0 error / 0 warning + host tests + link）、`./ci/audit.sh quick`（含新增 FP-free 审计）、`make test-host`、`make test-unit`、`./scripts/qemu_boot_test.sh all`（双架构 boot 里程碑）。
  - 状态：[X]
  - 详情（§2.3 五门槛实测结果，按 `build all` → `build aarch64` → `audit quick` → `test-host` → `test-unit` → `qemu_boot_test.sh all` 顺序）：① `./ci/build.sh all` = Passed 5 / Failed 0；② `./ci/build.sh aarch64` = Passed 2 / Failed 0（aarch64 链接通过），`./ci/audit.sh quick` = **exit 0**（含新增 `0.5i/6` FP-06 步为 ✓）；③ `make test-host` = exit 0；④ `make test-unit` = QEMU 33「ALL TESTS PASSED」；⑤ `./scripts/qemu_boot_test.sh all` = **2/2 通过**（x86_64：`VFS ready` + Ring 3 init + KPTI 隔离断言；aarch64：`VFS ready` + virtio-net NetOps 桥探测 + EL0 init + KPTI-09 隔离断言）。
  - 详情（aarch64 运行期专项判据）：最终 `build/kernel.bin`（331202 条指令）反汇编 = 白名单外 FP/SIMD **0**，白名单内 `stp/ldp q` **32** + `fpcr/fpsr` **4**，与预期值一致 ⇒ KPTI-18b 的"EL1 零 FP/SIMD"在产物层可复核。
  - 详情（跑序依赖与途中发现的预存工具问题，均未在本轮改动）：① `make test-unit` 首次失败于 build.rs 的「构建产物缺失 `build/user/init.bin`」—— 原因是 `make ARCH=<另一架构>` 的 arch 戳记（`build/log/.arch`）不匹配时会强制 clean 并删除 `build/kernel.{bin,flat,map}`，而 `.bin` 未被同步重建；按序 `make ARCH=x86_64 user` 后通过。② 同源现象：`kernel.flat` 被该 clean 删除后，`./scripts/qemu_boot_test.sh` 在**戳记已匹配**时不触发重建，直接输出「kernel.flat 缺失, 跳过测试」并计为失败（本次 x86_64 侧 1/2）—— 需先 `./ci/build.sh x86_64` 补产物。二者均属预存工具链行为（arch 戳记 clean × 跳过逻辑），本轮未改，登记备查。

- **FP-08. Wu 抗锯齿定点重写（后继项，登记不实施）**
  - 描述：`framework/driver/display/framebuffer.rs::draw_line_aa` 的 f32 是内核自有源码中最后的真实浮点算法。
  - 方案：f32 → 定点（Q16）重写，含 `fpart`/`rfpart`/`gradient`/alpha 计算的定点化。
  - 状态：[]（后继项，本轮不实施）
  - 详情：**本轮不实施**，理由见 DECISION-079 裁定 3。登记触发条件：当显示子系统接入 QEMU/真实硬件并具备像素级回归测试手段时再实施（届时可校验 ±1 alpha 差异）。

## 验证门槛

- AGENTS §2.3 五条门槛全过（双架构 build / clippy 0 warning / 核心审计 / host-tests / QEMU 双架构）—— 实测结果见 FP-07 详情。
- 专项：aarch64 内核产物反汇编 **白名单外 FP/SIMD = 0**（FP-06 脚本，带预期值：白名单内 `stp/ldp q` 32 条 + `fpcr/fpsr` 4 条）—— 实测 0 / 32 / 4，达成。
- 回归：FP-04 的 4 个公共 API 签名变更后，`services/*` 与 `framework/*` 内全部调用点与单元测试断言同步通过。
- 反向验证：x86_64 产物与 host-tests 不受影响（target 未变，`baseline.json` 不红）。

## 风险与回退

- **软浮点 ABI 切换是本质改动**：`aarch64-unknown-none-softfloat` 改变 crate ABI（浮点参数/返回改走通用寄存器）。缓解：内核自有源码整数化后跨边界不传 `f64`；`context_switch_asm` 的 `extern "C"` 只传指针；已在探针中验证构建+链接通过。**运行期（QEMU boot）必须在 FP-02/FP-03 之后单独验证**，避免与 FP-04/FP-05 的源码改写混淆归因。
- **隐式 NEON 消失**：`+1.4%` 代码量（E7），仅影响 aarch64 内核算术密度与小结构体清零/拷贝的内联形态；不改变正确性。
- **F9 死代码**：`ProcessContext.fpcr`(@656)/`fpsr`(@664) 在 KPTI-18a 后已被 `context_switch_asm` 真实引用，不构成死字段。
- **上游 future-incompat 告警（E8）**：软浮点 target 会引出 `core`/`sha2` 的 future-incompat 报告，**决定接受不压制**（上游 Asterinas 同形态亦无压制，且 `-D warnings` 下实测 exit 0、不阻断 CI）。监测项：若未来 rustc 把该 lint 升级为 hard error，则须与上游同步判断（届时唯一手段是升级依赖/工具链，而非压制）。
- **FP-06 审计算术的前置依赖（工具面）**：该门禁读 `build/kernel.bin`，其成败依赖"最近一次链接为 aarch64"。双架构共用输出路径 + `build.sh all` 末位链接 x86_64 + `make` arch 戳记 clean 三者叠加，会让"先 build all 再 audit"的默认顺序出现红（脚本已给明确提示）。若后续希望消除该耦合，可选方向：为 aarch64 链接产物引入带架构后缀的输出路径（属独立工程，本轮不做）。
- **回退**：把内核 target 变量改回 `aarch64-unknown-none` 并撤掉 `.arch_extension` 2 行即可整轮回退（FP-04/FP-05 的整数化为独立可回退项）。