# 子系统接线覆盖评估

> 结论：按「已实现 / 已接线 / 已验证」三档口径，当前树的能力宣称仍必须以**已接线**为上限——已实现面显著大于已接线面。与上一轮全景审计相比有实质推进（wasm/WASI 已接入 syscall 分发），但新增暴露出一处门禁结构性盲区：`audit_unwired_pub_fn.py` 的 R2 腿只覆盖 syscall 编号的一个子集，其「0 项未接线」不能解释为全量无缺口。

**快照与基线**：HEAD `a7b6d87b`（2026-11-13 20:26 +0800），取证日期 2026-11-14。全程只读：未修改源码、未提交、未运行会写仓库的构建命令；`audit_unwired_pub_fn.py` 以默认（非 `--json`、非 `--strict`）方式执行，仅输出报告。

## 一、口径与判据

沿用 `docs/report/subsystem-gap-deep-audit.md` 的三档定义：

- **已实现**：代码存在且参与编译。
- **已接线**：存在一条从 syscall 分发或硬件中断路径出发的真实调用者。
- **已验证**：存在真正执行被检行为的测试；`read_to_string` / `include_str!` 读源码再做文本匹配的断言不计入。

三档之间不存在自动蕴含关系：「编译通过」不证明「可达」，「可达」不证明「行为正确」。本文只测前两档，第三档单列其失真来源。

## 二、门禁读数与其口径

`python3 scripts/audit_unwired_pub_fn.py` 在 `a7b6d87b` 上的读数：

| 项 | 读数 |
|---|---|
| 扫描文件数 | 618 |
| 扫描 `pub fn` | 5,971 |
| 豁免（`#[no_mangle]` / 顶层 re-export / trait impl） | 365 |
| 扫描 `SYS_` / `EG_` 编号 | 36 |
| R2 未接线 syscall（CRITICAL） | 0 |
| R1 未分类零引用 `pub fn`（HIGH） | 0 |
| R1 已分类零引用（INFO） | 430 |
| R3 `pub mod` 零引用（WARN） | 0 |

两点必须并读：

其一，**R2 = 0 是子集读数**。脚本第 247-248 行的收集器只处理 `syscall/types.rs`，但实际扫描到的编号只有 36 个；而同一文件里可解析出 `pub const SYS_*: u64 = <数字>;` 形态的常量有 247 条（`grep -c '^pub const SYS_'` 口径为 259 行，差额来自声明形态不同的条目）。计数口径与文件内容不匹配，说明该腿的收集面窄于「types.rs 全部常量」。因此 R2 = 0 只能证明「被收集到的那 36 个编号都有臂」，不能证明全量无缺口。

其二，**R1 INFO = 430 是降噪而非接线**。这 430 项零引用事实成立，只是已登记进 `docs/plan/syscall-followup.md` 的机器可读分类区块，故从 HIGH 降为 INFO。把 HIGH 归零读成「没有未接线接口」是误读。

## 三、常量先于实现的缺口

对 `src/kernel/privileged/syscall/types.rs` 的常量名与两侧分发文件（`functions/syscall/dispatch.rs`、`privileged/syscall/dispatch.rs`）出现的 token 取差集，得到 11 条**有编号常量、无分发臂**：

| 常量 | 编号 |
|---|---|
| `SYS_PREAD64` | 17 |
| `SYS_PWRITE64` | 18 |
| `SYS_PSELECT6` | 270 |
| `SYS_ACCEPT4` | 288 |
| `SYS_RENAMEAT2` | 316 |
| `SYS_PREADV2` | 327 |
| `SYS_PWRITEV2` | 328 |
| `SYS_OPENAT2` | 437 |
| `SYS_FACCESSAT2` | 439 |
| `SYS_EPOLL_PWAIT2` | 441 |
| `SYS_FCHMODAT2` | 452 |

这批编号对第三方用户态不是中性的：它们全部落在 Linux 已分配区，且 `pread64`/`pwrite64`（17/18）是基础文件 I/O 面，`accept4`（288）是 socket 常规路径。缺失分发臂时调用方拿到的是分发兜底的错误返回值而非该编号应有的语义，属于「编号存在即被视作能力」的口径风险。补充：`SYS_PWRITE64` 与 `SYS_ACCEPT4` 在 `privileged/proc/domain.rs:28-69` 的域权限表中出现，但仍无分发臂——调用方会先被鉴权、再落入无臂兜底。

## 四、子系统级接线现状（相对上一轮审计的变化）

上一轮全景审计列出的未接线清单中，**wasm/WASI 已经闭合**：`functions/syscall/dispatch.rs:76` 调用 `dispatch_wasm`，同文件 `:1216-1227` 定义并向 `EG_WASM_LOAD` / `RUN` / `GET_INFO` / `UNLOAD` 四条私有序号分发到 `functions/wasm/registry`。这条旧结论不应继续引用。

其余各项在本快照上仍未接线：

- **netfilter**：判决函数 `nf_hook` 定义于 `privileged/net/netfilter.rs:346`，唯一调用者是同文件 `:423-425` 的兼容封装 `hook()`；`hook()` 在全仓 `src/kernel` 内没有生产调用者（同名的 `hook(` 命中均为 `privileged/driver/hotplug.rs:331` / `:364` 的热插拔回调，与 netfilter 无关）。规则可增删但判决不进包路径。
- **tickless**：`enter_tickless` / `exit_tickless` 定义于 `functions/timer/tickless.rs:178` / `:217`，调用点 `:504` / `:510` / `:516-517` 全部位于同一文件的测试函数 `tickless_state_machine_and_hrtimer_wiring` 内；idle 路径仍走周期 tick。
- **用户态驱动中断转发**：`privileged/egdf/user_driver.rs:359` 的 `egdf_forward_irq` 由 `functions/egdf/user_driver.rs:153-154` 包装，但两侧 `syscall` 目录内均无该入口的调用者。
- **零引用面规模**：R1 INFO 430 项中，`privileged/freg/recoverable.rs`、`functions/sgeg/{sessions,secure_boot,identity}.rs` 等属于整模块只被 re-export（上一轮审计已逐条点名，本快照未变）。

## 五、已验证档的失真来源

`host-tests/tests/` 现有 190 个测试文件，其中 **135 个**含至少一处 `read_to_string` / `include_str!` 读源码再匹配的断言。这类断言对源文本形状负责，对行为不负责；它们混在同一套「全绿」读数里，使「测试通过」无法直接换算为「行为已验证」。

门禁侧的第二个失真来源是 `ci/audit.sh:149-154` 的 `skip_leg()`：它只累加未执行腿计数并打印警告，不改变退出码。`:855` 的注释明确写着「工具缺失 → skip_leg 记入未执行腿数（不改退出码）」。于是「未执行」与「执行且通过」在验收读数上不可区分，任何「全部腿通过」的表述都需要附未执行腿清单才有解释力。

## 六、对计划的意义

三档口径下，可安全对外宣称的能力 = 已接线 ∩（有行为测试或人工实测）。当前树同时存在三类折扣：

1. **编号层折扣**：11 条编号无臂，调用方得到的是兜底错误而非 ENOSYS 明确语义。
2. **子系统层折扣**：netfilter 判决、tickless 状态机、用户态驱动中断转发三块「实现完整但无生产调用者」。
3. **验收层折扣**：135/190 测试文件含文本断言，`skip_leg` 不改退出码，使「全绿」上限低于「行为已验证」。

作为后续计划的输入，这三类折扣应分别对应「补臂 / 接线 / 改造测试与门禁」三组条目，而不是合并成一条「完善子系统」。
