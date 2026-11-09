# 代码库熵与 AI 维护痕迹评估

> **结论**：当前代码库不是"结构失序"型混乱。分层、TCB 边界、审计门禁和测试行为覆盖都仍然成立；最明显的问题是**大量公开接口写出来却没有用户**，以及**同一窗口内反复改写刚写下的实现**。前者是"零件齐全、装配不足"，后者是"边写边推翻"。模板化 `// SAFETY` 注释与陈旧路径引用是第二位的、低风险但高存量的痕迹。

**快照与基线**：本报告基于工作区当前内容，HEAD 为 `a23d8a2`（2026-11-08 16:18 +0800）。工作区当时有 10 个已跟踪文件处于 modified、5 个未跟踪文件（由并行的其他 AI 维护产生）。本次评估全程只读：未修改源码与文档、未提交、未切分支；除本文件外未产生任何 diff。临时脚本均位于 `/tmp`。

## 1. 范围、口径与抽样方法

**分析窗口**

- 变更抖动窗口：2026-10-31～2026-11-08（7 天）与 2026-10-09～2026-11-08（30 天）。
- 存量快照窗口：截至 HEAD `a23d8a2` 的全仓文本快照，包含工作区未提交内容。

**统计口径（不同口径并列，不合并）**

- 代码体量：`.rs` 原始行数（含空行与注释）。自有代码采用 `audit_tcb_ratio.py` 同源排除规则：排除 `src/kernel/functions/net/smoltcp/` 与 `src/kernel/privileged/tests/`。
- TCB 占比：同时给出 effective 行数口径（剔除空行与纯注释）与 raw 行数口径。
- 零引用公开接口：三种口径并列——顶层 `pub fn` 文本口径、含类型/常量的文本口径、`audit_unwired_pub_fn.py` 的 INFO 口径。详见第 7 节。
- 注释：以 `src/kernel` 内所有 `//`、`///`、`//!` 与 `// SAFETY` 行计数，排除 `smoltcp`。
- 测试：`host-tests/` 下 `#[test]` 函数计数，不区分单元/集成。
- 文档台账：`docs/plan/` 下 `状态：` 字段计数；`docs/plan/archive/` 按目录惯例视为历史快照，不纳入"当前待办"。

**抽样方法**

- 注释抽样：在 57,549 条普通注释与 2,062 条 `// SAFETY` 行中，以固定随机种子 `seed=20260808` 均匀随机抽取 45 条普通注释 + 20 条 `// SAFETY`。抽样单位为"注释行"，非文件。
- 测试抽样：在 `host-tests/tests/` 的 180 个文件、1,329 个 `#[test]` 中，以 `seed=4242` 均匀随机抽取 25 个测试函数；抽取单位为"测试函数"，然后读取其函数体断言。
- 返工分析：对 `scheduler.rs`、`process.rs`、`proc_ops.rs` 三个热点文件，取窗口内所有提交的删除行，用 `git blame <commit>^` 回溯每一删除行的引入提交日期，统计"删除行中引入于 2026-10-25 之后"的比例。该方法只覆盖三个文件，不外推为全仓比例。

**只读声明**：所有结论来自 `git log/show/blame`、文本索引与只读脚本；未运行会写入仓库的构建或审计命令。`audit_unwired_pub_fn.py` 仅输出报告，不写文件。

## 2. 结论摘要

1. **结构没有失序。** TCB effective 占比 56.8%（raw 58.8%），`functions/` 保持 0 unsafe；28 个受控审计脚本与 `ci/audit.sh` 1,037 行构成的门禁密度仍然很高；测试侧未出现"只断言成功"的系统性失守。
2. **最明显的 AI 维护痕迹是"写出来但没接上"。** 2,005 个顶层 `pub fn` 中 222 个（11.1%）在仓内零文本引用；`apic.rs` 公开面 54% 无调用者，`secure_boot.rs` 80%，`dedup.rs` 77%；`freg/recoverable.rs`、`functions/sgeg/{sessions,secure_boot,identity}.rs` 属于整模块只被 re-export。
3. **第二位痕迹是同窗口返工。** 8 天内 13 条 `fix`；`scheduler.rs` 14 次、`process.rs` 与 `proc_ops.rs` 各 12 次提交；三个热点文件窗口内删除的 532 行中，46.2% 是 2026-10-25 之后才加入的行。
4. **第三位是注释"形似"。** 575/2,062 条 `// SAFETY`（27.9%）是两条复制模板；266 条完全相同的"调用方保证指针/类型有效（详见上下文）"被套在完全不同的危险操作上。另有 42 条注释路径指向重命名前已不存在的文件。
5. **"内核很多零件"的直觉成立，但"零件是否真的不可达"尚未证明。** 本文只证明"文本零引用"，未做从根出发的可达性分析；该缺口在第 6 节单列，不能把 222 当作死代码结论。

## 3. 熵的存量与流向

### 3.1 体量结构

自有代码 raw 214,397 行（`privileged/` 126,080 + `functions/` 88,317，已排除 smoltcp 与 privileged tests）；测试 48,585 行（187 个 `.rs`）；文档 61,876 行（171 个 `.md`）。三者比例约为 **66.0% : 15.0% : 19.0%**。包含 vendored smoltcp 的 `src/` 总量为 288,224 行 / 739 个文件；`src/kernel` 为 280,735 行 / 703 个文件。

取证：`/tmp/size_stats.py`（`rglob('*.rs')` 行数）+ `audit_tcb_ratio.py` 的 `count_loc_raw` 排除规则。

### 3.2 TCB 占比

`privileged/` 126,080 raw / 76,890 effective；`functions/` 88,317 raw / 58,538 effective。按 effective 口径 **56.8%**，按 raw 口径 **58.8%**。`functions/` 侧 unsafe 行 0，硬规则 F1 仍成立。

现存数值口径不一致：脚本 docstring 与注释仍写"58.1%"，`docs/plan/framekernel-paradigm-enforcement.md:47` 写"阶段 6 实测 56.7%，当前审计 56.5%"，`docs/plan/progress-active-tasks.md:538` 写"57.78%"，既有报告 `driver-subsystem-coverage-assessment.md` 基线为 56.5%。本报告实测 56.8%（effective），差异并列记录于第 7 节。

### 3.3 公开面零引用

顶层 `pub fn` **222/2,005（11.1%）** 在仓内零文本引用，分布在 76 个文件。`audit_unwired_pub_fn.py` 当前输出 **435 条 INFO**（R1 零引用，不阻断），另有 362 条豁免；既有报告 `zero-ref-pub-fn-disposition-verdict.md` 记录的是 442 条，说明近期有约 7 条被删除或接线。二者口径不同：脚本统计所有 `pub fn` 声明（含 impl 方法与 `pub(crate)` 等），本文只统计行首顶层 `pub fn`。

集中面：`apic.rs` 20 个、`ioapic.rs` 10 个、`functions/fs/egfs/dedup.rs` 10 个、`functions/sgeg/identity.rs` 9 个、`arch/aarch64/mmu.rs` 9 个、`functions/sgeg/secure_boot.rs` 8 个、`privileged/klog/mod.rs` 6 个、`functions/fs/devpts.rs` 6 个。

### 3.4 条件编译组合

内核自有 feature 面（排除 smoltcp）：**18 个不同 feature 名**，**532 处** `#[cfg(feature = ...)]`。分布高度集中：`host-test` 227、`kernel_test` 208、`lockdep` 112、`initramfs` 10、`fault_injection` 8、`json_export` 5，其余各 1–2 处。

CI 只对 5 条运行/构建腿做真构建：`smp`、`preempt`、`kaslr`、`freg` 各自真启动并断言 capabilities 行翻转对应位，`lockdep` 走构建面探针（`scripts/lockdep_feature_probe.sh`）。因此 18 个 feature 并未形成全组合矩阵；风险不在"组合爆炸已发生"，而在**未采样组合的语义等价性没有验证**。

### 3.5 变更抖动与流向

- 7 天窗口：**66 个提交 / 218 个不同文件**。
- 30 天窗口：**269 个提交 / 984 个不同文件**。
- 热点文件窗口内提交数：`src/kernel/privileged/proc/scheduler.rs` **14**；`src/kernel/privileged/proc/process.rs` **12**；`src/kernel/privileged/proc/proc_ops.rs` **12**。

流向判断：熵在"新增公开面"与"同窗口改写"两个方向净增；在"删除死代码/收口重复路径"方向有局部偿还（例如 8 天内零引用脚本读数从 442 降到 435），但偿还速度低于新增与返工速度。

### 3.6 文档台账与漂移

- `docs/` 共 171 个 `.md` / 61,876 行；本报告生成前 `docs/report/` 有 22 个报告。
- `docs/plan/` 当前 `状态：` 条目 788 条，其中 `[X]` 730、`[]` 28，**开放率 3.6%**。`docs/plan/archive/` 未计入。
- 漂移实证：`python3 tools/check_doc_paths.py` 报 3 条，全部在 `docs/design/freg-design.md`，引用不存在的 `privileged/freg/state.rs`（2 次）与 `slot.rs`。
- 源码注释内另有 **42 条**路径引用找不到对应文件（按 `src` + `docs` 后缀匹配），典型：`src/kernel/functions/fs/procfs.rs:14 → kernel/fs/procfs/procfs.rs`、`ramfs.rs:15`、`intcast.rs:3 → privileged/lib/intcast.rs`、`clone.rs:94 → privileged/syscall/clone.rs`。
- TCB 阈值在活跃文档/脚本中有三处不同取值，详见第 7 节。

### 3.7 门禁密度

受控审计脚本 **28 个**（`scripts/` 25 + `scripts/tests/` 2 + `tools/audit_unsafe.py` 1）；`ci/audit.sh` **1,037 行**；`full` 档含 10 条存量审计腿 + 2 条自测腿 + Lockbud + QEMU。按自有代码 214,397 行计，约 **每千行 0.13 个审计脚本**。

门禁性质判断：现有门禁以**结构约束**为主（0 unsafe、SAFETY 覆盖、死代码抑制、模块循环、一次初始化、锁序），对"功能是否接线""错误是否被吞"覆盖较弱——这正好解释了为什么第 4.1 与 4.6 节的问题能在全绿门禁下长期存在。

## 4. AI 维护痕迹分项

### 4.1 表面完成、内部未接线

**证据（模块级）**

| 文件 | 零引用 / 顶层 `pub fn` | 已接线部分 | 未接线部分 |
|---|---|---|---|
| `privileged/arch/x86_64/apic.rs` | **20/37（54%）** | `init`、`eoi`、`get_id`、`send_ipi`、timer 链在 `smp_init.rs:138,299,300,336`、`x86_64/mod.rs:56,241,248,311`、`timer/clockevent.rs:161,297` 有真实调用 | LINT0/1 配置 6、delivery 常量 4、ISR/TMR/IRR 读取 5、level IPI 4、`get_version` 1 |
| `privileged/arch/x86_64/ioapic.rs` | 10/30 | — | delivery 常量、`set_id`、`set_irq_level` 等 |
| `functions/sgeg/secure_boot.rs` | **8/10** | 无任何外部 `secure_boot::` 引用 | 整模块仅被 `sgeg/mod.rs` re-export |
| `functions/sgeg/identity.rs` | 9/41 | 外部 `identity::` 命中全部指向 privileged 自己的 identity 模块 | 本文件仅被 re-export |
| `functions/sgeg/sessions.rs` | 无顶层 `pub fn`；类型零外部消费者 | 无 | `sgeg/mod.rs:62` 单点 re-export |
| `functions/fs/egfs/dedup.rs` | 10/13 | — | CAS 整套接口 |
| `privileged/freg/recoverable.rs` | `Snapshot` trait **0 个 impl** | 无 | 83 行整模块仅被 `freg/mod.rs:58,69` re-export |

**三类区分**

- **(a) 明确未来预留：文本证据约为 0。** 对上述文件 grep `未来|预留|待迁移|TODO|尚未|未接线` 无命中；唯一沾边的是 `functions/syscall/dispatch.rs:28` 把 TPM 列为"待迁移"，但 `secure_boot.rs` 文件本身没有预留标记。
- **(b) 本该接线但漏了：约 180 个。** 主体是硬件/诊断面与旧模块面。关键佐证：这些 API 不是最近新增后忘了接线——`apic.rs`、`sessions.rs`、`recoverable.rs` 最后改动都是 2026-10-29 的 clippy 批（`dc66c3ff`），`recoverable.rs` 的引入点是 2026-10-04 的子树重命名（`14db9bf9`）。这是长期存在、从未接通的公开接口表面。
- **(c) 整个模块只被 re-export：至少 4 处**——`freg/recoverable.rs`、`functions/sgeg/sessions.rs`、`functions/sgeg/secure_boot.rs`、`functions/sgeg/identity.rs`。另 `functions/sgeg/persist.rs` 的 `save_database/load_database/remove_database` 被 `crypto.rs:305/319/333` 与 `identity.rs:114/480/494` 调用，但这些包装函数自身零消费者，构成**传递性死岛**（见第 6 节）。

口径风险：零引用是文本口径，只通过 C ABI/FFI 或宏生成调用的函数会被误判；本节的 222 是**文本零引用**，不等于不可达。

### 4.2 模板化 SAFETY 注释与陈旧引用

- `src/kernel` 共 2,062 条 `// SAFETY`。其中 **575 条（27.9%）是两条完全相同的模板**：
  - **309 条**：`// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作`
  - **266 条**：`// SAFETY: 调用方保证指针/类型有效 (详见上下文)`
- 第二条是典型的"把责任推给不存在的调用者"：同一句被套在 `aarch64/mmu.rs:215`、`idt/table.rs:152`、`smp_init.rs:297`、`copy_user.rs:503` 等语义完全不同的危险操作上，`详见上下文` 没有指向任何可核对的前置条件。
- 注释总量 58,451 条；短注释（≤6 字符）7,752（13.3%），纯分隔线 2,557（4.4%）。
- 随机 45 条普通注释分类：约 **50% 复述/结构标签**（`/// LBA 格式字节数`、`/// 信号量数组`、`// Tick 管理`、`// ====`），约 **45% 解释为什么/契约**（`build.rs:54` 绝对路径原因、`clockevent.rs:229` 启动上下文打印原因、`epoll.rs:133` 忽略 size 的理由），约 **5% 可能过期/含混**。
- 陈旧引用：42 条源码注释路径失效（见 3.6）；`docs/design/freg-design.md` 3 条路径失效。

### 4.3 同窗口返工、不收敛

窗口内 13 条 `fix` 提交（`f8927c5a`、`8403fc56`、`8bbc7d4e`、`cf42cebb`、`6962aba2`、`a97a1ff4`、`13bd1fb1`、`3f6fe712`、`7ed36cc4`、`55bcb1a8`、`1fc4c204`、`ef97a5fc`、`aeeb1886`），无 revert、无 hotfix。

`scheduler.rs`、`process.rs`、`proc_ops.rs` 三文件窗口内 23 个提交共删除 **532 行**，其中 **246 行（46.2%）** 的引入提交日期在 2026-10-25 之后，即"删掉自己刚加的行"。最典型：

| 提交 | 日期 | 删除行 | 其中同窗口新增 |
|---|---|---|---|
| `4d9ff5d3` | 2026-11-07 | 61 | **49** |
| `7b0f0259` | 2026-11-01 | 66 | **48** |
| `18f02d51` | 2026-11-02 | 91 | **40** |
| `2b68eaaf` | 2026-11-02 | 81 | 21 |
| `0d52786f` | 2026-11-01 | 19 | 13 |

结合 4.1：这些返工集中在"新接口 + 新调度/进程路径"，说明代码是在反复改写同一批刚写下的实现，而不是在稳定接口上增量接线。该分析只覆盖三个文件，不代表全仓返工率。

### 4.4 同类写法不一致

在已知三组之外，核实到以下 5 组：

1. **错误表示并存**：`Result<_, Errno>` 173 处、`Result<_, KernelError>` 57 处、领域错误 12 处、裸 `i32` errno 约定 124 处、`bool` 611 处、`Option` 481 处。同一层内至少 5 种失败表达。
2. **ID 类型三种形态**：`pub type Pid = u32`；`PwmId(u64)`、`DomainId(u64)`、`SessionId(u32)`、`DeviceId(u32)` 等 8 个 newtype；以及大量裸 `u32 pid/tgid` 直接传参。
3. **`CapDomain`**：`privileged/sgeg/types.rs:157` 为 `u16`，`functions/sgeg/policy.rs:36` 为 `u8`。
4. **`AuditLog` 两套**：`privileged/sgeg/audit.rs:13` 与 `functions/sgeg/audit.rs:102`。
5. **`Session` 四套定义**：`privileged/proc/session.rs:72,125`（Session + SessionManager）、`functions/sgeg/sessions.rs:59,81,346`（Session + SessionTable + SessionManager），另有 `privileged/sgeg/session.rs` 的 PWM 会话上下文。
6. **页大小双表示**：`PAGE_SIZE` 常量在 `config/memory.rs:20`，但 `4096` 仍作为无关缓冲区容量硬编码在 `ftrace.rs:36`、`ebpf.rs:48`、`ringbuf.rs:28`、`kexec.rs:48` 等；`0x1000` 另有 101 处。

### 4.5 跨文件同义不同值

- **TCB 阈值 30/20/64**：脚本 30%（`scripts/audit_tcb_ratio.py:22`）、`tools/check_tcb.sh` 20%（第 86–89 行）、用户裁定基线 64%。仓库内 64% / 64.3% 只出现在归档计划的历史测量（`docs/plan/archive/2026-07-07-workspace-refactor.md:5`、`docs/plan/archive/smoltcp-framekernel-wrapper.md:146`），活跃文档/代码中没有任何 64% 落点。
- **aarch64 UART 地址双拼写**：`0x09000000` 与 `0x0900_0000` 并存（各 4 次与 3 次）。
- **aarch64 硬编码地址**：实测 `privileged/arch/aarch64` 下有 **34 个不同的长地址字面量**；既有材料称"19 项"，口径不同（详见第 7 节）。
- **页大小**：`PAGE_SIZE`、裸 `4096`、`0x1000` 三种表示并存（见 4.4 第 6 组）。

### 4.6 "假装成功"的错误处理

- 规模：`src/kernel` 内 `let _ =` **485 处**；其中匹配 `init/sync/flush/write/save/register` 的 **128 处**；`if let Err(` 31 处；`.ok();` 仅 7 处。
- **已核实点 1**：`privileged/egdf/mod.rs:874-890`。`egdf_register_driver` 内 `let _ = driver.init();`，随后无条件置 `state: DeviceState::Ready` 并返回 id——注册成功与初始化成功被解耦。
- **已核实点 2**：`functions/fs/egfs/egfs_data.rs:1420-1446`。`sync()` 无返回值判定，末尾固定 `0`；`spa.write_uberblock_to_disk()` 返回 `()`（`spa.rs:472` 内部 `if !ub.is_valid() { return; }`），`txg_group.transition()` 返回值被丢弃，且没有屏障/flush 语义。
- **同类扩充**：`ioapic.rs:88 let _ = init_one(base_addr)`、`x86_64/mod.rs:260 let _ = gdt_init()`、`driver/mod.rs:164,170 let _ = bus_init()/display_init()`、`input/keyboard.rs:773 let _ = driver.init()`、`display/mod.rs:366 let _ = fb.init()`。数量级为**百级**，集中于启动编排与驱动初始化。

## 5. 查证后不成立的项

这一节与第 4 节同等重要：以下担心经量化后不成立，或只在局部成立。

- **"测试自证"不成立。** 全量扫描 1,329 个测试：仅含单个 `is_ok()/is_some()` 且无 `assert_eq/assert_ne` 的测试为 **0 个（0.0%）**。`host-tests` 中 "mock" 仅 38 处，基本是注释说明"host 环境无法注入 MockIoMem"或历史模块名，**没有发现**"mock 替代被测对象再断言 mock 行为"的模式。
- **"过度抽象"整体不成立。** 纯 re-export 壳粗口径 50 个文件 / 921 行；剔除 `mod.rs` 聚合器、`mechanism.rs` 大聚合（183 行）与 `rust/`、`user/` 壳后为 **27 个 / 约 310 行**。这些绝大多数是 `functions→privileged` 门面或 TCB unsafe 边界代理（`privileged/proc_elf.rs` 45 行、`privileged/sgeg_pwm.rs` 70 行），属分层必需，不是噪音。57 个 `pub trait` 中 26 个 ≤1 个 impl、7 个 0 impl（`DisplayController`、`IpcResource`、`KgdbSerial`、`NetworkDevice`、`PciScanner`、`RecoverableDomain`、`Snapshot`）；其中硬件抽象面是 host-test 注入缝，真正死的只有 `Snapshot` 与 `RecoverableDomain`。
- **无 revert / hotfix。** 窗口内 13 条 `fix` 全部是收口型修复，无回退提交。
- **无恒真校验。** `assert!(true` 0 处；无符号参数上的 `>= 0` 只出现在 `max_ext >= 0x8000_0006` 这类十六进制特性位比较中。
- **`# Errors` 标题无空壳。** 1,224 个 `/// # Errors`/`# Panics`/`# Safety`/`# Examples` 标题逐条检查，后续均有正文，空标题为 **0**。
- **命名一致性（局部）成立。** `is_initialized` 共 28 处，没有 `initialized()` 变体。
- **无显式"未来预留"标记。** 在 4.1 的重点文件中，`未来|预留|TODO|尚未` 文本命中为 0；不能把"零引用"直接解释为"刻意预留"。

局部例外（不推翻上述结论，但需单列）：12 个测试文件、75 个测试函数（5.6%）直接对源码文本或"符号是否存在"做断言；随机 25 个样本中命中 3 个确定 + 3 个结构性（12%–24%，样本偏高）。典型：`kpti_x86_user_table_test.rs:131` 断言源码包含 `assert!(`；`socket_multicast_membership_test.rs:225` 断言 `pub fn mc_join` 等签名存在；`td11_12_13_klog_cleanup_test.rs:50`、`plan_b_inode_test.rs:26`、`td08_kernel_error_test.rs:50`、`aarch64_pa_va_conversion_unify_test.rs:108` 断言模块/常量存在或不存在。这类测试防"实现被悄悄删掉"，不防逻辑错误。

## 6. 零件与装配：直接调用者不等于使用者

**直接调用者 ≠ 使用者。** 文本引用只证明"A 调用了 B"，不证明"从系统入口能到达 B"。`functions/sgeg/persist.rs` 是典型：`save_database/load_database/remove_database` 有明确调用者（`crypto.rs:305/319/333`、`identity.rs:114/480/494`），但调用者本身没有消费者——形成**传递性死岛**。同理，`functions/sgeg/constant_time.rs` 对 `slice_eq` 有调用，`slice_eq` 对 `ct_eq` 有调用，但未必有从根可达的路径。

**方法学缺口（必须显式记录）**：本报告**没有进行从根出发的可达性分析**。因此：

- 第 4.1 节的 222 个是"文本零引用"，不是"不可达集合"；
- 不能据此给出"真实死代码有多少"的猜测数字；
- 不能把"零引用"直接等同于"应删除"。

**建议的四个根**（后续若补做可达性分析，从这些入口出发）：

1. **启动路径**：`stage1` → `kernel_main` → init 编排入口（`audit_orphaned_init.py` 所守护的 `no_mangle` 入口）。
2. **syscall 分发**：`functions/syscall/dispatch.rs` 的编号分发表（`audit_unwired_pub_fn.py` R2 守护）。
3. **中断与异常入口**：x86_64 的 IDT 表项、aarch64 的 exception vector，以及 timer/IPI 回调注册点。
4. **驱动注册表**：`privileged/pci` 枚举 + `driver/mod.rs` 的 bus/display/input 注册链 + `egdf/mod.rs` 设备注册表。

**该方法能回答的问题**：哪些公开 API 真正从入口可达；哪些模块是"有调用者但无根路径"的传递性死岛；接线缺口应按哪个根补齐；删除候选集的真实规模。**该方法不能回答**的问题：运行时动态注册（函数指针表）是否覆盖某个函数；feature 未开启时的替代路径是否存在。这两个只能靠类型感知的调用图或运行期覆盖，当前均未做。

## 7. 数值口径修正专节

以下数值在多轮分析中出现口径混用，逐条记录并给出取用建议。

**7.1 零引用公开函数（四个数不可混用）**

| 数值 | 口径 | 取用建议 |
|---|---|---|
| **222 / 2,005（11.1%）** | 行首顶层 `pub fn`，文本零引用；排除 smoltcp/tests | 用于"顶层公开面有多少无文本消费者" |
| 291 / 3,075（9.5%） | 含类型、常量、trait 等的更大符号集 | 不与 222 并列或换算；仅作历史对照 |
| **435 条 INFO** | `audit_unwired_pub_fn.py` 当前输出（R1，非阻断），另有 362 条豁免 | 用于门禁/存量跟踪；脚本口径含 impl 方法与 `pub(crate)` |
| 442 条 | 既有报告 `zero-ref-pub-fn-disposition-verdict.md` 的基线 | 与 435 差 7，属近期删除/接线后的漂移；引用时注明快照日期 |

**7.2 aarch64 硬编码地址**

实测 34 个不同长地址字面量（正则 `0x[0-9A-Fa-f_]{6,}`，扫描 `privileged/arch/aarch64`），既有材料称"19 项"。差异来源未核实：可能是"仅代码、排除注释/测试"或"仅 MMIO 基址"的过滤口径。**建议**：引用时写明"34 个不同字面量（宽口径）"，不要把 19 与 34 混为同一指标。

**7.3 `proc_ops.rs` 位置**

实际位于 `src/kernel/privileged/proc/proc_ops.rs`，不是 `functions/proc/proc_ops.rs`。讨论热点文件与返工时必须用前者。

**7.4 TCB 阈值三处取值与"64% 基线"**

| 位置 | 取值 | 性质 |
|---|---|---|
| `scripts/audit_tcb_ratio.py:8,22,131-135` | 30% | 脚本内硬编码，默认 `--soft` 仅警告、exit 0 |
| `AGENTS.md:63` | 30% | 文档描述（该行当前未被他人改动） |
| `docs/explain/guide-dev.md:55` | `< 30%` | 活跃开发指南仍写"重复直到 TCB < 30%" |
| `tools/check_tcb.sh:86-89` | 20% | 第二套软阈值，打印 WARN/PASS，不 exit 1 |
| 用户裁定基线 | 64% | 仓库内**无活跃落点** |

**关于"64% 基线在仓库内无任何落点"的核实结果（与既有表述并列，不调和）**：

- 64% / 64.3% **确实存在**于归档计划的历史测量：`docs/plan/archive/2026-07-07-workspace-refactor.md:5`（"从 64% 降至 <30%"）、`docs/plan/archive/smoltcp-framekernel-wrapper.md:146`（"TCB 64.3%"）。但它们描述的是**当时的起点测量**，不是当前基线。
- 2026-10-26 裁定确实存在：`docs/plan/framekernel-paradigm-enforcement.md:47` 写明"不再追求 <30% 硬目标，改为「尽可能缩小 TCB」；阶段 6 实测 56.7%，当前审计 56.5%"。**该裁定没有给出 64% 这个替代值**，也没有给出任何具体替代阈值。
- 因此准确表述是：**30% 已被 2026-10-26 裁定放弃；仓库内没有生效的替代阈值（64% 是用户口径，未被写入代码或活跃文档）；现有脚本仍在用 30% 报警。** 不能写成"64% 是唯一记录"。

**7.5 提交数与文件数**

7 天窗口在当前快照下为 **66 个提交 / 218 个文件**；早前一轮同一窗口记录为 56 个提交，差异来自并行的其他 AI 在工作区内继续提交。引用时须注明快照 HEAD。

## 8. 建议（按性价比排序）

**当下便宜的风险消除**

1. **对 4 个"只 re-export"模块做出二选一处置**：`freg/recoverable.rs`、`functions/sgeg/sessions.rs`、`functions/sgeg/secure_boot.rs`、`functions/sgeg/identity.rs` 的包装层。要么补一个从 syscall/启动根出发的真实调用点，要么降为 `pub(crate)`/删除。影响面小、可立即验证。
2. **批量修注释**：把 266 条"调用方保证指针/类型有效（详见上下文）"改成逐点的前置条件；修正 42 条失效路径与 `freg-design.md` 3 条引用。低风险、纯文本。
3. **统一 TCB 阈值单源**：删除 `tools/check_tcb.sh` 的 20% 与 `docs/explain/guide-dev.md:55` 的 30%，让脚本读一个常量；按 2026-10-26 裁定，把脚本语义改为"趋势跟踪 + 可选阈值"，避免继续用已否决的 30% 报警。
4. **修复两处"假装成功"**：`egdf/mod.rs` 注册时传播 `init` 失败（失败则置 `Failed`/返回错误）；`egfs_data.rs` 的 `sync()` 返回真实结果并补 barrier/flush。两处都是单点改动，配套回归测试。

**长期结构债（需先做可达性，不建议立即动）**

5. **实现"从根出发"的可达性脚本**（第 6 节四个根），把 222 个文本零引用收敛成"可达 / 传递性死岛 / 真死"三类；在此之前不要按 222 做批量删除。
6. **收敛同类表示**（`Session`、`AuditLog`、`CapDomain`、错误返回约定）。这属于跨模块接口重构，应在可达性分析之后、按子系统分批进行，不做一次性大重构。
7. **把"新公开 API 必须有根调用者或为 `pub(crate)`"变成 CI 约束**：当前 `audit_unwired_pub_fn.py` 的 R1 仅是 INFO；在完成存量甄别后再开 `--strict`，否则会一次性产生数百条阻断。

## 9. 未验证、推测、估算清单

**文本口径的局限（数字是下界或近似）**

- 222 个零引用是**文本零引用**，无法识别 FFI、宏展开、函数指针表、trait 动态分发产生的调用；因此它既可能把"实际被调用"误判为零引用，也可能漏掉"文本有引用但逻辑不可达"。
- 42 条失效路径依赖"后缀匹配"启发式，可能漏报相对路径引用，也可能把外部工具路径误判。
- 34 个 aarch64 地址是正则宽口径，包含可能不是 MMIO 地址的长常量。

**依赖抽样的结论**

- 注释分类（50% 复述 / 45% 为什么 / 5% 过期）来自 45 条随机样本，不是全量分类。
- 测试自证比例来自 25 个随机样本与全量弱断言扫描的组合；"源码文本断言"的 5.6% 是文件级统计，样本中 12%–24% 偏高，二者并列。
- 返工率 46.2% 仅覆盖 `scheduler.rs`、`process.rs`、`proc_ops.rs` 三个文件，不能外推为全仓返工率。

**完全未做**

- **从根出发的可达性分析**（第 6 节）——未做，因此没有"真实死代码规模"的结论。
- **类型感知调用图**（trait 分发、函数指针、宏展开）——未做。
- **运行期覆盖**（QEMU 启动后的实际调用覆盖）——未做。
- **feature 组合矩阵验证**——未做；只统计了 18 个 feature 与 5 条 CI 腿。
- **双架构语义等价性检查**——未做；本报告只做了文本与结构统计。
- **模块测试覆盖的精确映射**——只做了"模块名在 host-tests 中是否出现"的粗略检查；31 个顶层模块中仅 `mem_alloc` 零名字命中，但这不等于它没有测试。
- **性能与运行时行为**——未做。

**工作区状态带来的不确定性**

- 工作区有 10 个已跟踪文件处于 modified（由并行的其他 AI 维护），本报告基于当前内容；HEAD 一旦推进，3.1–3.5 的行数与提交数会变化。
