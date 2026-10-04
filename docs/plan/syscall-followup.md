# syscall 后续工程总纲（编号治理闭环后的剩余任务）

> 关联：B09-17/18（编号归位）+ syscall-dispatch-cleanup（问题 4/5 收敛）均已完成（commit `aea73cd1`）。
> 本总纲规划**剩余任务**：功能实装（T1）、分层迁移（T2）、半成品清除（T3）、审计核实（T4/T5/T6）、预存登记（T7）。
> 数据来源：分册 9 工程计划 D + syscall-dispatch-cleanup B2 分类 + 2026-09-15 源码核实。

## 描述

syscall 编号空间治理已闭环（EG_* 归位 SYS_*、双 dispatch 重叠归零）。剩余为三类：
1. **功能实装**：Linux 有标准编号但未实装的 syscall（T1）。
2. **分层收尾**：privileged 回退层 24 个"未迁移" syscall 转 functions（T2），其中半成品先清除（T3）。
3. **审计核实**：R3/R1/TODO 遗留（T4/T5/T6）+ aarch64 编号预存（T7）。

## 任务方案

### T1：R2 未实装 SYS_* 功能实装（P0，POSIX 兼容）

**范围**：分册 9 D-2 清单（~33 项，**实施前重扫核实**——sendfile 已实装 functions/fs/sendfile.rs，D-2 清单过时）。

**分组**（按 POSIX 兼容优先级）：

| 组 | 成员 | 优先级 |
|---|---|---|
| G1 文件 I/O 核心 | readv/writev/preadv/pwritev/statx/close_range/fchownat/utimensat/fallocate | P0 |
| G2 进程/信号 | prctl/arch_prctl/capget/capset/set_robust_list/get_robust_list/waitid | P0 |
| G3 网络 | recvmmsg/sendmmsg/socketpair | P1 |
| G4 内存 | mbind/userfaultfd | P1 |
| G5 多路复用 | inotify_init/epoll_pwait/ppoll | P1 |
| G6 时间 | clock_nanosleep/settimeofday/adjtimex | P1 |
| G7 文件系统 | pivot_root/chroot/setdomainname/execveat | P2 |

**方案**：每项 = functions 实现 → dispatch 接线（functions 层）→ host 单测 + 集成测试 → QEMU 冒烟（触路径）。
**验证**：每批双架构 0w0e + clippy 0 + host-tests + QEMU。
**待核实**：部分项可能已部分实装（如 sendfile）；process_vm_readv/writev 安全面特殊（跨进程读写，需权限校验设计）。

### T2：回退层 24 未迁移 syscall → functions 迁移（P1，F/S 分层）

**范围**：syscall-cleanup B2 登记的 24 项（functions 无对应、privileged 回退真实执行）：
read/write/rt_sigreturn/seccomp/prctl/io_uring_setup/enter/register/unshare/setns/bpf/kexec_load/tcgetpgrp/tcsetpgrp/execve/setrlimit/tgkill/sendfile/splice/SGEG_DISK_INSTALL/SGEG_HOTPLUG_STATUS/FB_OPEN/MMAP/RELEASE。

**方案**：逐批迁移——每项：functions 实现（或确认 functions 已实现如 sendfile）→ functions dispatch 接线 → 回退层删分支 + 契约注释更新 → 测试。
**分批**：
- 批 1 核心 I/O/进程：read/write/execve/rt_sigreturn/setrlimit/tgkill（用户态核心，优先级最高；实际迁移项为 read/write/execve/setrlimit——rt_sigreturn 已随 T3 清除，tgkill 归 T1 实装）
- 批 2 安全/机制：seccomp/prctl/tcgetpgrp/tcsetpgrp
- 批 3 网络/异步：sendfile/splice/io_uring×3
- 批 4 namespace/调试：unshare/setns/bpf/kexec_load
- 批 5 平台扩展：SGEG×2/FB×3

**验证**：每批双架构 0w0e + host-tests + QEMU（syscall 路径必跑）。
**注意**：迁移后 privileged 回退层仅剩"机制独有 27 + 哨兵 14"（B2 契约允许面），F/S 分层收尾。

### T3：半成品清除 + 用户态调用 audit（P1，安全面）

**前置**：用户态调用 audit——grep src/user + src/userland 对 24 项的调用（已初步：用户态暂不用 sendfile/readv 等高级项；核心 read/write/execve 必然被用）。
**方案**：
1. audit 产出"保留（有用户）/清除（无用户半成品）"清单。
2. 半成品判定：参数忽略（如 sys_tgkill 的 `_tgid` 未使用）、stub/占位返回、未完整实现。
3. **清除**：无用户的半成品回退分支 → 删除 → 恢复 ENOSYS（安全态，QEMU 兜底确认用户态不依赖）。
4. **保留**：有用户的核心（read/write/execve 等）→ 直接并入 T2 迁移（privileged 已实装则迁 functions）。
**验证**：双架构 0w0e + host-tests + QEMU boot（确认清除后用户态程序不回归）。
**注意**：T3 与 T2 联动（清除 = 迁移的前置筛除），可合并为"回退层收敛"工程，T3 先行 audit。

### T4：R3 零引用 pub mod 7 项核实（P2）

**范围**：dmu_trait/raidz_trait/spa_trait/txg_trait/zap_trait/zil_persist_trait/zil_trait（unkfs 的 trait 抽象层）。
**方案**：逐个核实（接线使用 → 保留；纯预留 → 删除或合并）。并入 UNKFS 收尾工程。

### T5：R1 pub fn 445 项甄别（P2，大）

**方案**：逐项分拣三态（已接线保留 / 转正式实装 / 删除）。审计工具驱动（audit_unwired_pub_fn），分批。
**与 T1 联动**：R1 中"应实现未实现"的归 T1 功能实装；纯死代码归删除。

### T6：TODO 转正式 33 项（P2）

**范围**：TRACK- 23 + 普通 10（分册 9 D-5）。
**方案**：实现治理（实装对应功能）；过期/无价值 TODO 直接删。独立工程。

### T7：aarch64 syscall 编号空间（P2，预存登记）

**问题**：Linux aarch64 编号 ≠ x86_64（如 aarch64 read=63 vs x86_64 read=0、bpf aarch64=280 vs x86_64=321）；dispatch 当前单编号表（x86_64 基准）。
**处置**：登记预存——若 aarch64 需跑 Linux 用户态二进制，需架构相关编号表（dispatch 按 target_arch 分派）。当前 aarch64 无用户态 syscall 场景（QEMU 冒烟仅内核），暂不阻塞。

## 优先级与依赖

```
T3 (audit 前置) → T2 (回退层收敛) ─┐
                                   ├→ 完成后 privileged 回退层 = 机制 27 + 哨兵 14（分层收尾）
T1 (功能实装, 独立并行) ───────────┘
T4/T5/T6 (审计核实, 低优先)
T7 (预存登记)
```

- **依赖**：T3 audit 是 T2 迁移范围的前置筛除；T1 独立可并行。
- **建议执行序**：T3 audit → T2 批 1-2 → T1 G1-G2（核心组）并行；T4-T6 排后；T7 仅登记。

## 验证门槛（每任务）

1. 双架构 `cargo check --release` 0w0e + clippy 0
2. host-tests 全量（新增 syscall 必有单测/集成测试）
3. **QEMU boot**（dispatch 改动必跑）
4. 核心审计（boundary/coupling——迁移 functions 后 F1 functions 0 unsafe 保持）

## 状态

- [X] T3：半成品清除 + 用户态调用 audit（2026-09-15 完成，见下方 T3 实施记录）
- [X] T2：回退层保留项 → functions 迁移（批 1-5 全部完成，见下方 T2 实施记录）
- [X] T1：R2 未实装 SYS_* 实装（G1-G7 全部完成，见下方 T1 实施记录）
- [X] T4：R3 零引用 pub mod 7 项核实（2026-09-18 完成，处置＝删除，见下方 T4 实施记录）
- [X] T5：R1 甄别（批 1 完成：R1 447 → **438**，处置＝只删确证无用 9 项；批 2 完成：438 项全量扫描已登记，**经 reviewer 四轮复核后按修订口径重划**（**T5 内**：删候选 **0** / 硬件原语完整性保留 **43**（仅登记）/ 待裁 **47**，T5 内合计 **90** ＝ 438 − 1（`write_log_line` 已试删删除）− 8 − 339；**接线 8 与未来功能 339 已按第四轮裁定一移出 T5**），未改代码；**A-2 已退回重做并重新定型**（判据升格三合一；原 23 项＝11 留 + 4 安全面 + 8 退桶；**第三轮**：`ct_eq_salt`/`ct_eq_password` ⇒ 族残缺入完整性保留 41→43，ramfs `split_path`/`validate_path` ⇒ 挂起待 T3 结论入 B-4 待裁 35→37，安全面桶清零），**施工方式＝逐项试删 + 既有五条门槛（不立项新工具）**，任一维硬失败即回退；**A-2 11 项试删已由 reviewer 第三轮授予开工**（解锁五条 ①②③④⑤ 已逐条核销），执行约束＝逐项独立提交 / 每项跑五门槛全量 / **QEMU boot 硬闸门** / ramfs 2 项不入本批；**第四轮开出后逐项复核实测发现 9 项判据不成立**（6 项「同族兄弟在用」＝**族残缺**形态、2 项台账 ② 判据事实错误、1 项无等价公共入口）⇒ 按裁定**退桶 9 项入待裁**（删候选 11 → **2**，待裁 37 → **46**），本轮仅 `write_log_line` / `format_duration` 2 项进入试删——`write_log_line` 五门槛 **5/5 全过** ⇒ **已删除**（commit `23681a14`；R1 438 → **437**），`format_duration` 判据待补 ⇒ **退桶**（A-6）⇒ **删候选清零（裁定四.1 达成）**；**T5-B 待裁 47 归零已完成**（B-5 归零表：13 等路线图 ⇒ 未来功能 / 32 判据待补 ⇒ 完整性保留 / 2 安全面待 T3 留待裁 ⇒ 桶数六次修订＝删候选 **0** / 完整性保留 **75** / 待裁 **2**，T5 内合计 **77**；裁定四.2、四.3 达成）；**T5-C 五条关闭验收（全部达成 ⇒ 本项置 `[X]`）**：① 删候选清零 ✅（`write_log_line` 已删；`format_duration` 判据待补退桶、ramfs 2 项试删门槛失败退桶）、② 待裁带三字段 ✅、③ 桶数闭合（**436**）+ 转移可追 ✅、④ **审计噪音治理已落地**（B09-21：台账 **B-6** 机器可读区块 **436** 项 + 脚本 fail-closed 降噪，实测 HIGH=0 / INFO=**436**）✅、⑤ 双向引用 ✅；**甲批 C-1（接线批次）**：8 项处置完毕（1 接线落地 + 7 退未来功能，见 **B-8**），R1 **437 → 436**。见「T5 实施记录」「T5 全量甄别台账」）
- [ ] T6-T7：登记排后（TODO 33 项 / aarch64 编号）
  - 丙批审查整改（分册 9 **D-8** 余项 7 项）：**7 项已落地**——A1 用户页表遍历持 `VMM_LOCK`（`41003e87`，原案「`MmStruct.rss_pages` 记账」因 `MmStruct` 无 per-process 归属不可达，经用户裁定改走方案 A：保留 cr3 遍历语义 + 同持 VMM_LOCK；公共 API 面未删）/ A3 OOMD victim 过滤僵尸（`93568c05`）/ A4 `terminated_count` 仅送达才累加（`4614e707`）/ B2 ext2 `i_ctime` 独立取值（`1b852e5d`）/ B4 换行（`3541b63c`）/ exfat 删静默成功覆写（`7079a94e`）/ F9 豁免残留清除（`29bee0a5`）。
  - exFAT 正式任务（D-5 ③）**维持登记**：删除静默成功覆写仅恢复「不支持」语义，三项前置（目录项定位 / 时间戳编解码 / 验证载体）仍缺。
  - R1 台账数字**未变动**（`CRITICAL=7 / HIGH=0 / WARN=0 / INFO=436`）：本批改动为 trait 覆写删除与 import 分组，未增减已分类的零引用 `pub fn`，清单算术保持闭合。

### T3 实施记录（2026-09-15）

**用户态调用 audit 结论**（src/user 存在；src/userland 不存在，AGENTS.md 记载过时）：

| 分类 | 项 | 依据 |
|---|---|---|
| 保留（有用户，T2 迁移） | read、write、execve | io.rs/eash/install/fbterm 全量使用 |
| 保留（有用户，T2 批 5） | SGEG_DISK_INSTALL | install/wizard/prepare.rs `boot_install` |
| 保留（有用户，T2 批 5） | FB_OPEN/MMAP/RELEASE | fbterm main.rs:179/184/271 直用 |
| 保留（无用户但真实实现，T2 迁移） | seccomp、prctl、tcgetpgrp、tcsetpgrp、setrlimit、unshare、setns、bpf、kexec_load、sendfile、splice、io_uring_setup、io_uring_enter、SGEG_HOTPLUG_STATUS | 实现均非 stub；用户态无常量/wrapper/调用 |
| **清除（无用户半成品/死分支）** | **tgkill、rt_sigreturn（回退分支）、io_uring_register** | 见下 |

**清除明细**（恢复 ENOSYS 安全态，types.rs 权威编号常量保留）：

1. **SYS_tgkill**：半成品——`_tgid` 参数忽略 + 语义错位（将 tid 当 pid 走 sys_kill）；用户态 wrapper 0 调用方。删 dispatch 分支 + `sys_tgkill` + 用户态 `tgkill` wrapper/常量。
2. **SYS_rt_sigreturn 回退分支**：死分支——pre-dispatch 特殊路径（dispatch.rs `is_rt_sigreturn`）无条件 `return` 拦截，回退层永不可达；`sys_rt_sigreturn` 残缺（仅清标志不恢复上下文）。删分支 + 函数；**SS_ONSTACK 清除逻辑（P1-I-45）迁移至可达的 pre-dispatch 路径**（顺带修复潜在缺口：原可达 sigreturn 路径从未清标志）；host-tests sigaltstack_test 源码断言同步指向新位置。
3. **SYS_io_uring_register**：恒 ENOSYS 桩，删除后落 `_ =>` 兜底 ENOSYS 行为不变。删分支 + iouring.rs 函数。

**半成品补充发现（不清除，登记 T2 处置）**：`sys_fb_release` 空 stub（恒 0 无释放动作，有用户 → T2 批 5 实装）；`sys_tcgetpgrp/sys_tcsetpgrp` `_fd` 参数忽略（POSIX 简化语义，T2 批 2 迁移时确认）；用户态 sys.rs `SYS_SGEG_HOTPLUG_STATUS` 常量无 wrapper 无调用（T2 批 5 补 wrapper）。

**验证**：build.sh all 5/5（双架构 0w0e + host-tests 全量 + asm 门控 + link）、clippy 双架构 0 warning、核心审计 8 项通过、QEMU boot（Ring 3/init）通过。

### T2 实施记录（批 1）

**迁移项**（4 项，functions 0 unsafe）：

| 项 | functions 落点 | 要点 |
|---|---|---|
| read/write | `functions/fs/io.rs`（`read_syscall`/`write_syscall`） | fd 路由策略（stdin/stdout/eventfd/signalfd/timerfd/inotify/VFS）整体迁入；fd 严格转换（try_from，失败 -EINVAL）随分支迁至 functions dispatch |
| execve | `functions/proc/exec.rs`（`execve_syscall`） | 指针校验 + argv 扫描（`api::read_u64_from_user` safe 读取）+ SUID 判定 + 委托 `proc_exec_replace`；`ExecveResult` 包装类型删除（唯一消费者是被删回退分支），functions 直接返回精确 Errno（EFAULT/ENOENT，与原 from_ret 映射一致） |
| setrlimit | `functions/proc/sysinfo.rs`（`setrlimit_syscall`，与 getrlimit 同落点） | RlimitTable 机制字段留在 privileged（DECISION-J 第十九批判据不变）；functions 做策略：校验 + `read_struct_from_user` 读取 + 特权判定（pid==1）+ 委托表更新 |

**机制统一**：write 路径的用户数据拷贝由 privileged dispatch 私有的 `copy_from_user_buf`（CR3 页表走查）统一到 `privileged::mm::copy_user`（`copy_from_user`/`copy_to_user`，异常表兜底，functions 已有使用先例）；并行实现收敛为单一权威。stdin 单字节写入改 `copy_to_user`（替代 unsafe `raw::write_u8`，后者随迁移失去唯一调用方删除）。

**privileged 回退层清理**：删 SYS_read/SYS_write/SYS_execve/SYS_setrlimit 4 分支 + `sys_read`/`sys_write`/`sys_execve`/`copy_from_user_buf`/`try_fd` + `privileged/syscall/execve.rs` 模块；types.rs 编号常量保留（编号空间权威不受迁移影响）。getrlimit 早于本批已迁 functions，故 privileged `proc/rlimit.rs` 的 `sys_getrlimit`/`sys_setrlimit` 两个策略入口函数在迁移后失去全部调用方（仅 re-export 自引用），随本批删除，re-export 收口为机制字段与查询辅助（`RLIM_INFINITY`/`RLIMIT_CORE`/`get_memlock_limit`）；`RlimitTable` 机制字段保留（DECISION-J 第十九批判据）。

**验证**：build.sh all 5/5、clippy 3 维（pedantic x86_64 + kernel_test + host-test）0 warning、核心审计 8 项通过（deadlock 唯一 HIGH 为预存 AP_STARTUP_LOCK 人工审查项，非本次引入）、QEMU boot（Ring 3/init）通过、QEMU kernel_test 468/468 全过（含用户态 init/eash 真实执行 read/write 路径）。

### T2 实施记录（批 2）

**迁移项**（4 项，functions 0 unsafe）：

| 项 | functions 落点 | 要点 |
|---|---|---|
| seccomp | `functions/proc/seccomp.rs`（`seccomp_syscall`） | 策略：operation 校验（STRICT/FILTER）+ no_new_privs 特权判定 + 委托机制状态变更（mode/filters 经 `process_with` + `SeccompState` 公开字段）；`MAX_FILTERS`/`DEFAULT_ACTION` 转 pub 留在 privileged（seccomp_check 机制共用权威），functions 引用 |
| prctl | `functions/proc/seccomp.rs`（`prctl_syscall`） | 仅实装 PR_SET/GET_SECCOMP + PR_SET/GET_NO_NEW_PRIVS 四 option，其余 ENOSYS（与原行为一致）；PR_* 常量随迁 |
| tcgetpgrp | `functions/proc/session.rs`（`tcgetpgrp_syscall`） | 委托 privileged `get_foreground_pgid`（机制查询辅助）；POSIX 简化 `_fd` 忽略（T3 登记项随本批确认） |
| tcsetpgrp | `functions/proc/session.rs`（`tcsetpgrp_syscall`） | 校验 pgid 属当前会话进程组（`process_for_each` 遍历）+ 委托 `SESSION_MANAGER.set_foreground_pgid` |

**机制保留（privileged）**：`SeccompState`/`SeccompFilter`/`SeccompRule`/`SeccompAction`/`SeccompMode`/`seccomp_check`/`add_rule`（seccomp_check 被 privileged 分发前置消费）；`SessionManager`/`SESSION_MANAGER`/`get_foreground_pgid`/`proc_setsid` 等会话机制与进程组策略。

**privileged 回退层清理**：删 SYS_seccomp/SYS_prctl/SYS_tcgetpgrp/SYS_tcsetpgrp 4 分支 + `sys_seccomp`/`sys_prctl_prctl`/`sys_tcgetpgrp`/`sys_tcsetpgrp` + PR_* 私有常量；proc/mod.rs re-export 收口（seccomp 移除策略入口、补充机制类型/常量导出；session `pub use session::*` 自动收口）。

**验证**：build.sh all 5/5、clippy 3 维 0 warning、核心审计通过、host-tests 全量、QEMU boot（Ring 3/init）通过。

### T2 实施记录（批 3）

**迁移项**（4 项，functions 0 unsafe）：

| 项 | functions 落点 | 要点 |
|---|---|---|
| sendfile | `functions/fs/sendfile.rs`（`sys_sendfile`，既有封装，本次接线） | 委托 privileged 机制 `privileged::syscall::sendfile::sys_sendfile`（VFS/IPC/pipe 访问 + unsafe 用户 offset 指针，机制留 privileged） |
| splice | `functions/fs/sendfile.rs`（`sys_splice`，既有封装，本次接线） | 同上，委托 `privileged::syscall::sendfile::sys_splice` |
| io_uring_setup | `functions/io/iouring.rs`（`io_uring_setup_syscall`） | 委托 privileged 机制 `io_uring_setup`（全局实例表 + ID 分配），functions 仅参数转换 + errno 映射 |
| io_uring_enter | `functions/io/iouring.rs`（`io_uring_enter_syscall`） | 委托 privileged 机制 `io_uring_enter`（SQE→CQE 处理） |

**机制保留（privileged）**：`privileged/syscall/sendfile.rs`（sys_sendfile/sys_splice 完整实现 + SPLICE_F_* 常量，functions 经 `privileged::syscall` 顶层 re-export 消费）；`privileged/io/iouring.rs`（IoUring/RingBuffer/Sqe/Cqe + io_uring_setup/enter/destroy/submit/reap 机制函数 + `sys_io_uring_submit_sqe`——EG_IO_URING_SUBMIT 机制独有，留在回退层）。

**privileged 回退层清理**：删 SYS_sendfile/SYS_splice/SYS_io_uring_setup/SYS_io_uring_enter 4 分支 + `sys_io_uring_setup`/`sys_io_uring_enter` 薄策略入口（参数转换类，迁 functions 后无调用方）；sendfile/splice 实现在 privileged 保留为机制库（functions 委托调用）。

**验证**：build.sh all 5/5、clippy 3 维 0 warning、核心审计通过、host-tests 全量、QEMU boot（Ring 3/init）通过。

### T2 实施记录（批 4）

**迁移项**（4 项，functions 0 unsafe）：

| 项 | functions 落点 | 要点 |
|---|---|---|
| unshare | `functions/proc/namespace.rs`（`unshare_syscall`） | 委托 `NamespaceSet::unshare`（机制：按 flags 创建新 ns 实例）；functions 仅 errno 映射 |
| setns | `functions/proc/namespace.rs`（`setns_syscall`） | 参数解析（CLONE_NEW_* 标志位与简化枚举双语义，B06-18 语义保留）+ CAP_SYS_ADMIN 特权判定（`sgeg::pwm_has_capability`，经顶层 re-export）+ 委托 `setns_by_type` |
| bpf | `functions/debug/ebpf.rs`（`bpf_syscall`，既有安全代理接线） | 委托 privileged `debug::sys_bpf`（机制） |
| kexec_load | `functions/driver/kexec.rs`（`kexec_syscall`，既有安全代理接线） | 委托 privileged `driver::sys_kexec`（extern "C" 机制函数） |

**机制保留（privileged）**：`privileged/proc/namespace.rs`（NamespaceSet/各 ns 实例/NsType/CLONE_NEW_*/NsRegistry/ns_register + unshare/setns_by_type 机制语义）；`privileged/debug`（sys_bpf + BPF 子系统）；`privileged/driver/kexec.rs`（sys_kexec + KexecSubsystem）。

**privileged 回退层清理**：删 SYS_unshare/SYS_setns/SYS_bpf/SYS_kexec_load 4 分支 + `sys_unshare`/`sys_setns` 策略入口；proc/mod.rs namespace re-export 收口（NamespaceSet 保留，sys_setns/sys_unshare 移除）；bpf/kexec 机制函数保留（functions 代理委托调用）。

**验证**：build.sh all 5/5、clippy 3 维 0 warning、核心审计通过（TD-22 注释中文化 100%）、host-tests 全量、QEMU boot（Ring 3/init）通过。

### T2 实施记录（批 5，收官）

**迁移项**（5 项，functions 0 unsafe）：

| 项 | functions 落点 | 要点 |
|---|---|---|
| SGEG_DISK_INSTALL | `functions/sgeg/storage/disk.rs`（`boot_install_syscall`） | cfg 门控镜像原回退层（x86_64 生产委托 `sys_boot_install`；kernel_test ENOSYS；aarch64 生产分支不编译走 `_ =>` 兜底）；机制含磁盘扇区写 + sgeg 权限检查留 privileged |
| SGEG_HOTPLUG_STATUS | 同上（`hotplug_status_syscall`） | 委托 `sys_hotplug_status`（unsafe 用户 buffer 写入 + 驱动状态读取）；用户态补 `hotplug_status` wrapper（T3 登记项） |
| FB_OPEN | `functions/driver/fb.rs`（`fb_open_syscall`，新建模块） | 委托 `sys_fb_open`（FB 驱动读取 + 用户 FbInfo 写入） |
| FB_MMAP | 同上（`fb_mmap_syscall`） | 委托 `sys_fb_mmap`（页表映射）；机制层新增 `FB_MAP_RECORD` 单槽记录映射区间供 release 解除 |
| FB_RELEASE | 同上（`fb_release_syscall`） | **自空 stub 实装**：依 `FB_MAP_RECORD` unmap 页表并清记录（SIMPLIFIED：单槽，多映射需扩展 per-process 表） |

**privileged 回退层清理**：删 SYS_SGEG_DISK_INSTALL/SYS_SGEG_HOTPLUG_STATUS/SYS_FB_OPEN/SYS_FB_MMAP/SYS_FB_RELEASE 5 分支；5 个机制函数 pub 化（经 `privileged::syscall` 顶层 `pub use dispatch::*` re-export 供 functions 委托）；`sys_fb_release` 空 stub 实装（FB_MAP_RECORD 记录 + unmap）。

**验证**：build.sh all 5/5、clippy 3 维 0 warning、核心审计通过、host-tests 全量、QEMU boot（Ring 3/init）通过。

**T2 收官结论**：21 保留项全部迁移（批 1-5）；privileged 回退层收敛为机制独有 + ENOSYS 哨兵，与分层契约一致（B2 契约允许面）。

### T1 实施记录（G1 文件 I/O 核心）

**重扫核实**（2026-09-15）：D-2 清单中 sendfile 已实装过时（T3 已核实）；除 inotify_init1 外，G1 全部 9 项在 functions/privileged dispatch 均无分支，确认未实装。

**实装项**（9 项，functions 0 unsafe）：

| 项 | functions 落点 | 机制/要点 |
|---|---|---|
| readv/writev | `functions/fs/io.rs`（`readv_syscall`/`writev_syscall`） | 逐 iovec 段委托 `read_syscall`/`write_syscall`（复用 fd 路由 + 校验）；iovec 数组经 `api::read_struct_from_user` safe 逐条解析（IOV_MAX=1024） |
| preadv/pwritev | 同上（`preadv_syscall`/`pwritev_syscall`） | **privileged 新增 `vfs_pread`/`vfs_pwrite`**（显式 offset 读/写，不更新 fd 当前偏移，SIMPLIFIED 不走 pcache 快路径）；functions 逐段委托 + pos<0 → EINVAL |
| statx | `functions/fs/stat.rs`（`statx_syscall` + `Statx` 结构） | 组装 Linux `struct statx`（256 字节）；SIMPLIFIED 仅填基础字段（mode/uid/gid/size/nlink/ino/时间戳），dev/btime 置 0 |
| close_range | `functions/fs/io.rs`（`close_range_syscall`） | 遍历 VFS 全局 fd 表 `[first,last]` 占用条目，逐个 `vfs_close`（先收集再关，避免表锁重入死锁）；SIMPLIFIED 仅 flags=0（UNSHARE/CLOEXEC → ENOSYS） |
| fchownat | `functions/fs/file_ops.rs`（`fchownat_syscall`） | SIMPLIFIED 仅 `dirfd==AT_FDCWD`（复用 `chown_syscall` UID/GID→PWM 查表 + `vfs_chown_ext`）；非 AT_FDCWD → ENOTSUP |
| utimensat | `functions/fs/stat.rs`（`utimensat_syscall`） | NULL times → 当前时间（tick/frequency 换算秒）；非 NULL 读用户 `timespec[2]`，sec==-1 表示不修改（u64::MAX）；委托 `vfs_utimensat_safe`（顶层 re-export 补 vfs_utimensat_safe） |
| fallocate | `functions/fs/file_ops.rs`（`fallocate_syscall`） | SIMPLIFIED 仅 mode=0（扩展文件大小到 offset+len，仅扩展不缩小）；基于 `vfs_fstat_safe` + `vfs_truncate_internal` 近似 |

**privileged 机制扩展**：`vfs_pread`/`vfs_pwrite`（handle.rs，显式 offset 不更新 fd 偏移）+ 顶层 re-export（vfs/mod.rs 补 vfs_pread/vfs_pwrite/vfs_utimensat_safe）。

**验证**：build.sh all 5/5、clippy 3 维 0 warning、核心审计通过、host-tests 全量、QEMU boot（Ring 3/init）通过。

### T1 实施记录（G2 进程/信号）

**实现路径裁定**（AskUserQuestion，用户授权）：tgkill / waitid / set+get_robust_list / prctl(PR_SET_NAME/PR_GET_NAME) = 相对完整设计实装；capget/capset / arch_prctl = **ENOSYS 保留**（无凭证能力模型 / 无 arch 相关用户态需求，登记不实装）。
> **2026-09-26 改判（分册 9 批次 3，见 B-10.6）**：`capget` / `capset` / `arch_prctl` 三项经用户裁定改为**实装**（capset 走 privileged 受约束子集 setter；arch_prctl 最小实装 `ARCH_SET_FS`/`ARCH_GET_FS`）⇒ 下段原「ENOSYS 保留」判定**作废**。

**实装项**（5 项 + privileged 退出路径机制，functions 0 unsafe）：

| 项 | functions 落点 | 机制/要点 |
|---|---|---|
| tgkill | `functions/proc/signal.rs`（`tgkill_syscall`） | 校验链：tgid<=0/tid<=0 → EINVAL；sig 范围 0..=63（0=存在性探测）→ EINVAL；目标不存在或 `target.pid != tgid` → ESRCH；委托 `api::sys_kill`。SIMPLIFIED：tgid==tid 等价校验替代"tid ∈ tgid 线程组"判定（当前 tid≡pid 无线程组模型；K-06 引入线程组后改查 tid 所属进程 tgid） |
| waitid | `functions/proc/wait4.rs`（`waitid_syscall`） | idtype P_ALL/P_PID/P_PGID（P_PGID 转负值复用统一收集）；options 合法位校验 + 必须含 WEXITED/WSTOPPED/WCONTINUED 之一；委托 privileged `wait_reap`；Reaped 且 infop 非 0 → 组装 128B `SiginfoChld`（si_signo=SIGCHLD、si_code=CLD_EXITED）经 `api::write_struct_to_user` 写回；Running → Ok(0)（WNOHANG）；NoChild → ECHILD。SIMPLIFIED：拒绝 WSTOPPED/WCONTINUED（无 stop/continue 状态跟踪）、si_code 恒 CLD_EXITED、si_uid 恒 0 |
| set_robust_list | `functions/proc/clone.rs`（`set_robust_list_syscall`） | len != 24（`struct robust_list_head` ABI 大小）→ EINVAL；`process_with` 登记 robust_head/robust_len |
| get_robust_list | 同上（`get_robust_list_syscall`） | pid==0 → 当前进程；head/len 指针非 0 时经 `api::write_struct_to_user` 写回（失败 EFAULT）。SIMPLIFIED：不做 PTRACE_MODE_READ 权限校验（无 ptrace/uid 模型） |
| prctl(PR_SET_NAME/PR_GET_NAME) | `functions/proc/seccomp.rs`（prctl_syscall 补两 arm） | SET_NAME：`copy_string_from_user` 读 16B → truncate(15)+NUL → UTF-8 lossy 写 comm；GET_NAME：comm 16B NUL 结尾写回用户 `[u8;16]` |

**privileged 机制扩展**：

- **Process 机制字段**（process.rs）：`clear_child_tid: AtomicU64`（CLONE_CHILD_CLEARTID 清除地址）、`robust_head: AtomicU64` + `robust_len: AtomicU32`（robust list 登记项）。
- **`privileged/proc/robust.rs`（新建，退出清理机制）**：`exit_cleanup(pid)` 退出路径执行——clear_child_tid 非 0 → 写 0 + `futex_wake` + 清字段；robust_head 非 0 → 遍历 robust list（先读 next 再处理当前，防链表破坏；`ROBUST_LIST_LIMIT=2048` 防环形链表），futex 字 `(word & FUTEX_TID_MASK) == pid` 时置 `FUTEX_OWNER_DIED` 并唤醒。SIMPLIFIED：list_op_pending 不区分 get_robust_list 与 set_robust_list 两种 pending 语义（直接按 uaddr 处理）。内核测试：`robust::futex_word_masks` / `robust::robust_head_layout`（24B ABI 验证）。
- **退出路径挂钩**（proc_ops.rs `process_exit`）：flock 释放之后、切换内核页表/销毁用户地址空间之前调用 `robust::exit_cleanup`（用户内存仍可访问）。
- **clone.rs 消费**：CLEARTID 登记（fork 与 CLONE_VM 双路径均登记 child_tidptr）；CHILD_SETTID 仅 CLONE_VM 路径写 child tid（fork 路径父上下文写入落父地址空间，COW 后子不可见——Linux 同语义）；`_child_tidptr` 改名 `child_tidptr`。
- **wait4.rs 统一收集机制重构**：新增 `WaitInfo{pid, exit_code}` + `WaitOutcome{Reaped/Running/NoChild}` + `wait_reap(target_pid, non_blocking, keep_zombie)`，wait4 与 waitid 共用；`keep_zombie` 实现 WNOWAIT；sys_wait4 重写为 `wait_reap` 委托（行为等价）；wait4 的 target_pid < -1（P_PGID 匹配）从 ENOSYS 桩实装（匹配 Process.pgid）。
- **futex.rs**：`futex_wake` pub 化（robust 退出路径唤醒消费端）。

**ENOSYS 保留登记**：capget/capset（无凭证能力模型）、arch_prctl（无 arch 相关用户态需求）——不实装，保留回退层 ENOSYS 哨兵。
> **已于 2026-09-26 作废**（分册 9 批次 3，B-10.6）：三项均已实装并接线，不再走 ENOSYS 回退。

**验证**：build.sh all 5/5、clippy 3 维 0 warning、核心审计通过、host-tests 全量、QEMU boot（Ring 3/init）通过、QEMU kernel_test 470/470（较前批 +2：robust 两测试）。

### T1 实施记录（G3 网络）

**实现路径裁定**（AskUserQuestion，用户授权）：socketpair / sendmmsg / recvmmsg = **B 相对完整设计实装**——socketpair 支持 AF_UNIX Stream+Dgram 双类型，recvmmsg/sendmmsg 循环复用既有收发原语，recvmmsg 完整 timeout 语义（get_ticks deadline + scheduler_yield 重试），mmsghdr msg_flags 逐条透传。

**实装项**（3 项，functions 0 unsafe）：

| 项 | functions 落点 | 机制/要点 |
|---|---|---|
| socketpair | `functions/net/syscall.rs`（`socketpair_syscall`） | sv 缓冲校验（EFAULT）；domain≠AF_UNIX → ENOTSUP、protocol≠0 → EPROTONOSUPPORT、type 非 Stream/Dgram → ENOTSUP；委托 `uds_socketpair`；sv 写回 packed u64（低 32 位 fd0 / 高 32 位 fd1，低 4 字节落 sv[0]），写回失败 close 两端 → EFAULT |
| sendmmsg | 同上（`sendmmsg_syscall`） | vlen==0 → Ok(0)、vlen>UIO_MAXIOV(1024) → EINVAL；逐 64B mmsghdr entry 校验（EFAULT）；UDS 分流 `sendmmsg_uds_entry`（`gather_entry_iov` 收集 iov；msg_name 非空走路径发送（与 sendto 一致）、空走 `uds_send_connected`，超 UNIX_DGRAM_MAX → EMSGSIZE）/ 非 UDS 委托 `sendmsg_syscall`；msg_len 经 `raw_copy_out(entry+56, 4, …)` 写回（4 字节粒度避免越界下条 entry）；出错且已发送 ≥1 条 → break 返回已发条数（Linux 语义） |
| recvmmsg | 同上（`recvmmsg_syscall`） | timeout timespec 解析（sec<0 或 nsec 越界 → EINVAL；rel_ms checked_mul/add 溢出饱和 u64::MAX）；blocking = timeout_ptr≠0 且无 MSG_DONTWAIT；EAGAIN 时满足（非 blocking / deadline 已过 / MSG_WAITFORONE 且 received>0）其一 → break（received==0 的 EAGAIN 终态返 EAGAIN），否则 `scheduler_yield()` 重试；成功逐条补写 msg_len（recvmsg_syscall 不写 msg_len，mmsghdr 语义须补）；UDS 分流 `recvmmsg_uds_entry`（`uds_sock_type` 分流 Stream recv / Dgram recvfrom，单缓冲 cap=min(iov 总容量, 类型缓冲上限)，逐段 `copy_out_to_entry_iov` 写回，`cap>0 && n>=cap` → MSG_TRUNC 透传 msg_flags） |

**unix.rs 机制扩展**（functions 内纯策略）：

- `uds_socketpair(sock_type)`：双端 `uds_create` + UDS_STATE 互设 peer + 双方 Connected（参照 uds_connect Dgram 双向 peer 模式）；第二端创建失败回滚 close 第一端，槽位关联异常回滚两端。
- `uds_send_connected(fd, data)`：Stream 委托 `uds_send`；Dgram 校验 Connected/peer/peer_closed（→ NotFound）+ len 超 `UNIX_DGRAM_MAX`（→ Invalid）+ 对端在途数据报（→ Again），写入对端 dgram 缓冲 + passcred 12B 凭据（同 uds_sendto）。SIMPLIFIED：Dgram 已连接发送为单条在途非排队（Linux 为可靠排队缓冲），dgram 缓冲队列化时改写。
- `uds_sock_type(fd)`：套接字类型查询（recvmmsg 分流接收原语）。

**syscall.rs 常量区**：`MMSGHDR_SIZE=64` + msghdr 字段偏移（msg_name@0 / msg_namelen@8 / msg_iov@16 / msg_iovlen@24 / msg_flags@48 / msg_len@56）+ `UIO_MAXIOV=1024` + MSG_TRUNC=0x20 / MSG_DONTWAIT=0x40 / MSG_WAITFORONE=0x10000 + NSEC_PER_SEC。

**dispatch 接线**（`functions/syscall/dispatch.rs` dispatch_net）：SYS_socketpair / SYS_sendmmsg / SYS_recvmmsg 3 分支（sendmmsg 4 参、recvmmsg 5 参含 timeout 指针）。

**kernel_test 扩展**（privileged/tests/test_uds.rs，批实施 +3 / 审查处置 +2）：socketpair_stream（双向收发）/ socketpair_dgram（Connected 发送 + 二次发送 Again 断言）/ socketpair_rollback（资源耗尽回滚验证）/ close_releases_bitmap（close 位图回收回归）/ sendto_oversize（超限拒绝回归）——后两项见批审查处置；`release_all_uds_bitmap_bits()` 保留为失败用例兜底清理（位图泄漏修复后正常路径位图应自空）。

**批审查处置**（AskUserQuestion 用户裁决，3 项预存）：

- **uds_close 补 free_fd（本批修复）**：close 回收 fd_alloc 位图位（对齐 pidfd close 释放模式；含 listener 关闭时 pending client 位同步回收）；回归测试 close_releases_bitmap（17 次 create+close 循环，泄漏行为下第 17 次即 NoMem）。
- **uds_sendto 长度校验（本批修复）**：`data.len() > UNIX_DGRAM_MAX → Invalid`（与 uds_send_connected 同款防护）；回归测试 sendto_oversize（超限拒绝 + 正常路径不受影响）。
- **recvmsg/sendmsg_syscall UDS 数据面分流缺失（专项登记）**：登记下方预存表，后续工程处置。

**验证**：双架构 check 0w0e、clippy 3 维 0 warning、核心审计通过、build.sh all 5/5、host-tests 全量、QEMU boot（Ring 3/init）通过、QEMU kernel_test 475/475（批实施 +3 / 处置 +2）。

### T1 实施记录（G4 内存）

**实现路径裁定**（AskUserQuestion，用户授权）：mbind = **B 路径**（VMA 增策略字段 + 两级查询，不伪造 PMM per-node 分配）；userfaultfd = **B 真阻塞语义**（相对完整），且 #PF 阻塞落点采 **B′ 标记阻塞 + tick 抢占**（#PF 走 IST 栈不能就地切换上下文）。

**实装项**（2 项，functions 0 unsafe）：

| 项 | functions 落点 | 机制/要点 |
|---|---|---|
| mbind | `functions/mm/numa.rs`（`mbind_syscall`） | 参数校验：`len == 0` / 非页对齐 → EINVAL；`flags` 保留位（`MPOL_MF_VALID` = 0xF）→ EINVAL；`MPOL_*` → `NumaPolicy` 映射（`from_linux_mode`，ABI 编码与枚举判别值不同）失败 → EINVAL；`addr + len` 溢出 → ENOMEM；`MPOL_DEFAULT` 要求 `nodemask == NULL`、Bind/Interleave 要求非空位掩码；委托 `MmStruct::set_numa_policy_range` 写 VMA 级策略 |
| userfaultfd | `functions/mm/uffd.rs`（`userfaultfd_syscall` / `ioctl_uffd` / `read_event`） | `userfaultfd(flags)`：仅接受 `O_CLOEXEC`/`O_NONBLOCK`，创建实例（失败 EMFILE）；`UFFDIO_*`：结构体 read/write_struct_to_user + 参数校验（COPY 校验 `len == PAGE_SIZE` + `check_user_buf` + `copy_from_user` 到内核暂存；ZEROPAGE 校验页对齐）；`read()`：无事件则 `scheduler_block` + `scheduler_yield_ex` 等待，`fault_notify` 入队后由 privileged 唤醒 |

**privileged 机制扩展**：

- **`privileged/mm/uffd.rs`（新建，ABI 常量 + 实例表 + #PF 拦截 + 页填充）**：Linux 真实 ABI 值（`UFFDIO_API` = 0xC018AA3F / `REGISTER` = 0xC020AA00 / `UNREGISTER` / `WAKE` / `COPY` / `ZEROPAGE`、`UFFD_API` = 0xAA、`UFFD_EVENT_PAGEFAULT` = 0x12、`struct uffd_msg` = 32B）；实例表 `UFFD_TABLE: IrqSpinLock<[UffdInstance; 16]>`（中断安全，`#PF` 路径读取）；`fault_notify(page, flags)` → `NotRegistered` / `Waiting`（入队事件 + 唤醒 `read()` 线程）/ `Ready`；`fill_provided_page` 在 `#PF` 重入路径把暂存数据写入 PMM 新页；`create`/`release`（唤醒等待者）/`api_negotiate`/`register`/`unregister`/`wake`/`provide_page`/`pop_event`/`set_reader`/`clear_reader`/`is_open`/`is_active`。
- **`privileged/mm/page_fault.rs`**：`PfResult` 增 `UffdWait = 5`；`handle_vma_fault_with_mm` 在匿名页缺页前拦截（仅 `user && !present`）——`Waiting` → `PfResult::UffdWait`，`Ready` → 新增 `handle_uffd_provided`（alloc_page → 填充暂存 → 映射）。
- **`privileged/idt/handlers.rs`**：`PageFaultHandler` 对 `UffdWait` 仅 `scheduler_block(WaitingForIo)` + 返回 `Recovered`（iretq 回用户态，由 tick 抢占切走；`#PF` 走 IST=4 专用栈，禁止就地切换上下文）。
- **`privileged/mm/vma.rs`**：`range_is_mapped(start, end)`（注册区间全覆盖校验，允许 VMA 真子集/跨相邻 VMA，无空洞）；`Vma.numa` 字段 + `numa_policy_at` + `set_numa_policy_range`（G4①）。
- **`privileged/mm/numa.rs`**：`MPOL_*` 常量 + `NumaPolicy::from_linux_mode` + `NumaRangePolicy` + `effective_policy_for`（G4①）。
- **`privileged/proc/fd_alloc.rs`**：`FdSubsystem::UserFaultFd = 7`（`COUNT` 7 → 8）+ `FdPlan::USERFAULT_FD`（1200, 16）。
- **服务侧接线**：`functions/fs/file_ops.rs`（ioctl 路由）、`functions/fs/io.rs`（read 路由）、`functions/fs/open.rs`（close 回收）、`functions/syscall/dispatch.rs`（`SYS_mbind` + `SYS_userfaultfd`）。
- **kernel_test 扩展**（`privileged/tests/test_mm.rs`，+3 并注册为 `mm::uffd` 组）：`lifecycle`（创建/握手/版本校验/ioctls 位图/释放）、`arg_validation`（对齐/模式/fd 校验分支）、`fault_flow`（注册 → 缺页入队 → 事件字段 → 提供页 → Ready → 物理页填充校验 → 注销回落 demand paging；临时安装测试地址空间）。

**SIMPLIFIED 清单**：

| 位置 | 简化点 / 影响面 / 扩展时机 |
|---|---|
| uffd 单挂起页模型 | 一个实例同时只跟踪一页；COPY/ZEROPAGE 必须命中该页否则 EINVAL；改 per-page 挂起表后可支持多页并发 |
| uffd 事件队列定长（16）| 队列满则丢弃拦截（`NotRegistered`，交回内核 demand paging），不永久挂起缺页进程；扩容时可改动态队列 |
| uffd 仅 MISSING 模式 | WP/MINOR 与 fork/remap/remove 事件未实装（`UFFDIO_API.features` 回填 0）|
| uffd `O_NONBLOCK` | 接受但无行为差异（`read()` 无事件即阻塞）；引入 fd 级状态标志后按 `O_NONBLOCK` 返 EAGAIN |
| mbind `MPOL_MF_MOVE` | 不迁移已触达页（无页迁移机制），不报错；PMM per-node 分区 + 页迁移后由分配路径消费策略 |
| mbind 范围粒度 | 要求范围被 VMA 整体对齐覆盖（无 split_vma → 部分生效会被整体拒绝，见 `set_numa_policy_range` 文档）|

**验证**：双架构 check 0w0e、build.sh all 5/5、clippy 3 维 0 warning、11 个核心/扩展审计脚本通过（`audit_comment_language` 修 1 处纯英文段落注释）、host-tests 全量、QEMU kernel_test 480/480（G4 新增 5 项：`mm::numa` 的 `vma_policy`/`linux_mode` + `mm::uffd` 的 `lifecycle`/`arg_validation`/`fault_flow`，475 → 480）。

**批审查处置**（静态契约测试同步，本批修复）：`FdSubsystem::COUNT` 新增 UserFaultFd 后，`td15_fd_idx_of_test::test_timerfd_subsystem_in_fd_plan`（断言 COUNT == 7）与 `fd_allocator_unified_test::test_subsystem_count_is_five`（仅统计 0..4 变体，PidFd 起已过时）同步为 COUNT == 8 / 8 个变体，并对 `test_mm.rs` 早前遗留的未命中 `#[expect(unreadable_literal)]` 清理。

### T1 实施记录（G5 多路复用）

**实装项**（3 项，functions 0 unsafe）：

| 项 | 落点 | 机制/要点 |
|---|---|---|
| inotify_init | `functions/syscall/dispatch.rs`（`SYS_inotify_init` 分支） | Linux 遗留接口 = `inotify_init1(0)`，直接委托既有 `sys_inotify_init1`（无新增实现，仅接线） |
| ppoll | `functions/fs/file_ops.rs`（`ppoll_syscall`） | `struct timespec` 解析（`tv_sec`/`tv_nsec` 范围校验 → EINVAL，越界指针 → EFAULT）+ 临时信号屏蔽字 + 委托 `poll_syscall` |
| epoll_pwait | `functions/sync/epoll.rs`（`epoll_pwait_syscall`） | 临时信号屏蔽字 + 委托 `epoll_wait_syscall`（校验与阻塞语义继承，timeout == -1 真阻塞） |

**privileged/functions 机制扩展**：

- **`privileged/proc/signal.rs`**：新增 `SIGKILL`/`SIGSTOP` 编号常量 + `sanitize_blocked_mask(mask)`（剔除不可屏蔽信号位的**单点权威**实现）。`signal_pick_next` 判据为 `pending & !blocked`，屏蔽字含 SIGKILL/SIGSTOP 位会导致进程永久不可终止 — 任何写 `blocked_mask` 的路径必须过此函数。
- **`privileged/syscall/dispatch.rs`**：`sys_rt_sigprocmask` 原内联位运算 `& !((1u64 << 9) | (1u64 << 19))` 改为调用 `sanitize_blocked_mask`（消除与 functions 新调用点的并行实现，符合"内核内部并行实现必须收敛为单一权威"硬约束）。
- **`functions/proc/signal.rs`**：新增 `with_temporary_sigmask(ptr, sigsetsize, f)`（ppoll / epoll_pwait 共用）——`sigmask == NULL` 透传；否则校验 `sigsetsize == 8`（EINVAL）、读用户掩码（EFAULT）、记录旧掩码 → `set_blocked_mask`（经 sanitize）→ 执行闭包 → 恢复旧掩码（错误路径同样恢复）。
- **`functions/syscall/dispatch.rs`**：`SYS_ppoll`/`SYS_inotify_init` 接入 `dispatch_fs`，`SYS_epoll_pwait` 接入 `dispatch_sync`（`dispatch_sync` 形参 `_a5` → `a5` 以取 `sigsetsize`）。

**SIMPLIFIED 清单**：

| 位置 | 简化点 / 影响面 / 扩展时机 |
|---|---|
| `ppoll_syscall` 超时 | 复用 `poll_syscall` 的单次扫描语义（既有 `SYS_poll` 现状：事件就绪即返回，否则立即 0，timeout 不生效）；解析出的毫秒仅下传。影响：无事件时不阻塞（与 `epoll_wait` 的 timeout > 0 同缺口）。扩展：hrtimer 定时唤醒接入 poll 等待队列后由 `poll_syscall` 消费 |
| `with_temporary_sigmask` 原子性 | "设置 → 执行 → 恢复"顺序等效（非 Linux 的原子 `set_user_sigmask`）；等待期间屏蔽字保持替换值，故唤醒判定按新掩码。扩展：引入信号等待队列原子重挂时改为真原子替换 |

**验证**：双架构 check 0w0e（x86_64/aarch64，含 kernel_test 维度）、clippy 3 维 0 warning、11 个核心审计脚本 rc=0（含 `audit_reverse_deps` 生产反向依赖 0/0）、host-tests 全量、`./ci/build.sh all` 5/5、QEMU kernel_test 484/484（G5 新增 `fs::multiplex` 4 项：`inotify_init_legacy`/`temporary_sigmask_swap`/`ppoll_arg_validation`/`epoll_pwait_validation`，480 → 484）。

### T1 实施记录（G6 时间）

**实装项**（3 项，functions 0 unsafe）：

| 项 | 落点 | 机制/要点 |
|---|---|---|
| settimeofday | `functions/timer/clock.rs`（`settimeofday_syscall` + 策略核心 `apply_settimeofday`） | `tv == NULL` 按 Linux 语义返回 0（只设时区，本实装忽略时区）；`tv` 读取失败 / `tz` 非空不可读 → EFAULT；`tv_sec < 0` 或 `tv_usec` 越界 → EINVAL；euid != 0 → EPERM；委托 `privileged::timer::TimeSyncSubsystem::set_time` 写墙钟基准 |
| adjtimex | `functions/timer/clock.rs`（`adjtimex_syscall` + 策略核心 `apply_adjtimex`） | 读入 `struct timex`（x86_64 ABI 208B，host gcc 实测 `sizeof`/`offsetof` 定为权威布局）；支持 `ADJ_OFFSET`/`ADJ_FREQUENCY`/`ADJ_SETOFFSET`/`ADJ_NANO`，其余 mode 位 → EINVAL；非特权 → EPERM；恒回填状态快照（`status` 按 `synced` 置 `STA_UNSYNC`）并返回 `TIME_OK` |
| clock_nanosleep | `functions/timer/clock.rs`（`clock_nanosleep_syscall` + 策略核心 `clock_nanosleep_wait_ns`） | `flags` 仅 `TIMER_ABSTIME`（否则 EINVAL）；`req` 为空 → EFAULT；`TIMER_ABSTIME` 下目标已过则等待 0；`rem` 按 Linux 语义不回写；睡眠委托 `privileged::timer::sleep_ns` |

**privileged/functions 机制扩展**：

- **`privileged/timer/sleep.rs`**：新增 `sleep_ns(total_ns)` — 睡眠策略的**单点权威**机制（`< 1ms` 走 hrtimer 时钟源忙等；`>= 1ms` 委托 `timer_sleep`，毫秒换算由原截断改为 `div_ceil` 向上取整以符合 POSIX「不低于请求时长」）。`privileged/syscall/dispatch.rs::sys_nanosleep` 原内联的忙等/换算分支改为调用本函数（消除与 `SYS_clock_nanosleep` 的并行实现）。
- **`privileged/timer/time_sync.rs`**：`set_time` / `adj_freq` 补 `last_sync_time` 基准重置 — 原实装不更新基准，`get_adjusted_time_ns` 的 `elapsed` 会以启动时刻起算，频率/跳变调整后墙钟按全部 uptime 累积补偿（秒级偏离）。本批使该路径经 `settimeofday`/`adjtimex` 可达，故属必须修复项（回归 `time::clock::freq_baseline` 以 500ppm 上界覆盖）。
- **`privileged/timer/tick.rs`**：`on_timer_interrupt` 末尾接线 `timesync_subsystem().tick_adjust()` — 原 `tick_adjust` 全项目零调用 → `ADJ_OFFSET` 登记的渐进偏移（`offset_remaining`）永不被消耗，`adjtimex` 会是半成品。仅原子操作，无锁/无分配，可在 hardirq 上下文调用。
- **`privileged/syscall/info.rs`**：删除 `sys_gettimeofday` — 墙钟查询改为 functions 策略后失去唯一调用方（避免死代码）。
- **`functions/timer/clock.rs`**：读路径统一为「原始 tick + timesync 机制偏移」（偏移初值 0，行为与旧实现一致）；`functions/syscall/dispatch.rs` 的 `dispatch_proc` 接入 `SYS_settimeofday` / `SYS_adjtimex` / `SYS_clock_nanosleep` 三个分支。

**SIMPLIFIED 清单**：

| 位置 | 简化点 / 影响面 / 扩展时机 |
|---|---|
| `settimeofday_syscall` 的 `tz` | 仅做可读性校验后忽略（Linux 已废弃 timezone 语义，仅首次设置时消费）。影响：依赖时区偏移反推本地时间的旧程序拿不到时区信息。扩展：引入时区表后按 Linux 首次设置语义消费 |
| `adjtimex_syscall` 字段面 | 不支持 `ADJ_MAXERROR`/`ADJ_ESTERROR`/`ADJ_STATUS`/`ADJ_TIMECONST`/`ADJ_TICK`（→ EINVAL），误差估计类回填字段恒 0，`precision` 固定 1us。影响：ntpd/chronyd 类调优与误差统计不可用。扩展：引入时钟误差估计（PLL/FLL 二阶环路）后按 Linux 语义补齐 |
| `clock_nanosleep_syscall` 中断语义 | 不支持信号中断提前返回 `EINTR`（与 `SYS_nanosleep` 基线一致）。影响：等待期间收到信号不提前唤醒。扩展：睡眠机制接入可中断等待（信号投递唤醒阻塞队列）后统一补充 |
| 特权判定 | `has_time_privilege()` 用 `euid == 0` 等价 `CAP_SYS_TIME`（与 `sethostname` 的 PWM 能力判定路径不同）。影响：细粒度 capability 场景下语义偏粗。扩展：PWM capability 全覆盖后改判 `CAP_SYS_TIME` |

**验证**：双架构 check 0w0e（x86_64/aarch64，含 kernel_test 维度）、clippy 3 维 0 warning、13 个核心/扩展审计脚本 rc=0、host-tests 全量（99 个 test bin 全 ok）、`./ci/build.sh all` 5/5、QEMU kernel_test 490/490（G6 新增 `time::clock` 6 项：`gettimeofday_validation`/`settimeofday_apply`/`clock_nanosleep_policy`/`adjtimex_policy`/`freq_baseline`/`gradual_adjust`，484 → 490）。

> 测试策略：kernel_test/host 双端无用户可写内存（低地址为内核恒等映射，真解引用会踩内核数据），故成功路径测**策略核心函数**（`apply_settimeofday`/`apply_adjtimex`/`clock_nanosleep_wait_ns`）+ **机制往返**（`set_time`/`adj_freq`/`adj_time`/`tick_adjust`），用户指针路径以越界地址（`>= USER_ADDR_MAX`）触发确定性 EFAULT。

### T1 实施记录（G7 文件系统）

**实现路径裁定**（AskUserQuestion，用户授权）：

- **chroot / pivot_root = A 完整**——VFS 路径解析引入根前缀 + 单一权威路径归一化；`chroot` 校验目录/特权后设根；`pivot_root` 校验 `new_root`/`put_old` 关系后切根；**默认根 "/" 时行为与改造前逐字节等价**。
- **setdomainname = 统一路线**——`sethostname` / `gethostname` / `uname` / `setdomainname` 四处主机名/域名全部收敛到 privileged `UtsNamespace`（单一权威），不再各自硬编码。

**实装项**（4 项，functions 0 unsafe）：

| 项 | functions 落点 | 机制/要点 |
|---|---|---|
| chroot | `functions/fs/path.rs`（`chroot_syscall`） | 指针校验（EFAULT）→ `CAP_SYS_ADMIN`（EACCES，SYSTEM 域 bit0，与 `mount`/`umount2` 先例一致）→ `vfs_stat_safe` 校验存在且为目录（ENOENT/ENOTDIR）→ `resolve_user_path` 归一化（超长 ENAMETOOLONG）→ `VFS_MANAGER.set_root(real_root)`（切根 + cwd 重置为视图根 "/"） |
| pivot_root | 同上（`pivot_root_syscall` + `pivot_root_plan` 纯逻辑） | 两路径同前置校验；关系校验：`new_root` == 当前根 → EBUSY；`put_old` 非严格位于 `new_root` 之下 → EINVAL（`is_strictly_under` 路径边界感知，`"/"` 情形单独处理）；通过后 `set_root(new_root)`。`pivot_root_plan` 抽为纯函数便于直接验证 |
| setdomainname | `functions/proc/sysinfo.rs`（`setdomainname_syscall` + 共用核心 `set_uts_name_syscall`） | 与 sethostname 同构：`len == 0 || len > 63` → EINVAL；SYSTEM 域 UTS 名称设置位 → EACCES；读入 64B 缓冲（EFAULT）→ `uts_current().set_domainname(...)` |
| execveat | `functions/proc/exec.rs`（`execveat_syscall`） | `flags` 白名单（`AT_EMPTY_PATH` / `AT_SYMLINK_NOFOLLOW`，其余 EINVAL）；`dirfd != AT_FDCWD` → ENOTSUP；`AT_EMPTY_PATH` + 空 pathname → ENOTSUP（`fexecve` 语义未支持）；其余委托 `execve_syscall`（ABI 与执行语义单点复用） |

**privileged/functions 机制扩展**：

- **`privileged/fs/vfs/vfs.rs`**：`VfsManager.root` 根前缀字段（`IrqSpinLock<[u8; VFS_MAX_PATH]>`，默认 "/"）+ `get_root`/`set_root`（切根并重置 cwd）+ **`normalize_view_path_into`（单一权威路径归一化）**——绝对路径以视图根为起点、相对路径以视图 cwd 为起点、`.` 忽略、`..` 上溯一级但**钳制在视图根内**（chroot 逃逸防护关键）、结果恒以 '/' 开头且无尾随 '/'；零堆分配（栈上 `[u8; VFS_MAX_PATH]`）；+ `resolve_view_path`（仅归一化, 供 `chdir` 保存视图路径 cwd）+ `resolve_user_path`（归一化 + 根前缀拼接, 默认根时逐字节等价旧行为）+ `truncate_to_parent`；`VfsSnapshot.root` 纳入快照。
- **`privileged/fs/vfs/path.rs`**：16 个接受用户路径的 VFS 入口统一改经 `resolve_user_path`；`vfs_set_cwd_internal` 改用 `resolve_view_path`（cwd 语义为视图路径——相对解析须以视图为基准, 且 `..` 已在此钳制）。
- **`privileged/fs/vfs/handle.rs`**：`vfs_open_internal` 入口归一化。
- **`functions/fs/file_handle.rs`**：`name_to_handle_at` 先 `resolve_user_path` 归一化再 `resolve_mount_fs`（host 源检查回归 `name_to_handle_at_uses_path_resolution` 同步断言两步入径）。
- **`privileged/proc/namespace.rs`**：`UtsNamespace.set_domainname`/`get_domainname`（65B 缓冲，未设置为空串）+ `UTS_DEFAULT_NODENAME` 常量（与 `UtsNamespace::new` 初值同源）+ `uts_current()`（**单一权威读取入口**：进程表 → `NamespaceSet` → uts；仅暂持 namespaces 锁取 Arc，UTS 字段锁在调用方按需获取以避免锁嵌套）。
- **`privileged/syscall/info.rs`**：`sys_uname` 的 nodename/domainname 改取 `uts_current()`（无进程上下文回退 `default_nodename()`），删除原硬编码 `"edgine-node"` / `"(none)"`。
- **`functions/proc/sysinfo.rs`**：`gethostname_syscall` 由恒返回字面量 `"localhost"` 改为读 UTS nodename（≤64B + NUL，按 `size` 截断）；`sethostname_syscall` 由"仅校验不存储"改为写入 UTS nodename。
- **`privileged/sgeg/capability.rs`**：新增命名常量 `SYSTEM_CAP_UTS_SETNAME = 1 << 9`（提取自原 sethostname 字面量 `9`）+ `sgeg::mod` 顶层 re-export；`sysinfo.rs` 特权判定由 `(pwm, 0, 9)` 改为 `(pwm, CAP_DOMAIN_SYSTEM, SYSTEM_CAP_UTS_SETNAME)`。
- **`functions/syscall/dispatch.rs`**：`SYS_chroot` / `SYS_pivot_root`（dispatch_fs）、`SYS_setdomainname` / `SYS_execveat`（dispatch_proc）接线。

**SIMPLIFIED 清单**：

| 位置 | 简化点 / 影响面 / 扩展时机 |
|---|---|
| `VfsManager.root` 为全局单例状态（非 per-process root） | 影响面：`chroot` 影响全系统视图（其他进程亦受新根约束）。扩展：引入 per-process root 或 mount namespace 时改进程级根字段 |
| `pivot_root_syscall` 仅切根前缀 | 影响面：不摘除旧根挂载点（无 per-process mount namespace 可摘），`put_old` 仅参与关系校验、旧根经绝对路径仍可达。扩展：需要 Linux"旧根不可达"强语义时在 `VfsManager` 摘除旧根挂载点 |
| `vfs_set_cwd_internal` 归一化失败静默保持原 cwd | 影响面：FFI 无返回值可上报，`chdir` 失败（超长/非 UTF-8）对用户不可见。扩展：需要 ENAMETOOLONG 上报时改签名为 `i32` |
| `execveat_syscall` 不支持目录 fd 相对解析与空路径执行 | 影响面：依赖 `AT_EMPTY_PATH` 的程序（部分动态加载器 / 容器运行时）不可用。扩展：VFS 提供"目录 fd + 相对路径"解析机制后按 Linux 语义补齐 |

**kernel_test 扩展**（+8，490 → 498）：`privileged/tests/test_vfs.rs` 4 项（`test_resolve_default_root` / `test_resolve_dot_components` / `test_resolve_relative_to_cwd` / `test_resolve_with_root_prefix`，经 `resolve_is` 辅助断言归一化结果）+ `privileged/tests/sys.rs` 4 项（`execveat_validation` / `setdomainname_validation` / `chroot_validation` / `pivot_root_validation`，注册于 `register_fs_tests`）。

**host 链接修复（本批引入，非预存）**：

- **现象**：`host-tests` 的 E-04 共享测试集（`e04_shared_runner_test`，debug 与 release 皆然）链接失败 —— `rust-lld: error: undefined symbol: _kernel_text_start / _kernel_text_end / USER_CR3_SAVE`。
- **根因定位**：HEAD（88382323）release 链接正常 → 本批引入。逐文件回退二分：`privileged/tests/` 三个文件单独回退均正常，回拷 `sys.rs` 即失败。`ar x` + `nm -C` 分析 release rlib 各 CGU：`cgu.05` 定义 `setrlimit_syscall` 且带 `U create_user_page_table` / `U USER_CR3_SAVE` / `U _kernel_text_*`（与 `user_proc::raw::*`、`mm::kpti`、`mm::vmm_x86_64` **CGU 共置**），`cgu.09` 定义 `execveat_syscall` 且引用 `kpti::KPTI_READY`/`kpti_init`。即：新增测试引用 `functions::proc::sysinfo` / `functions::proc::exec` 处理器 → rustc 把 arch 目标文件合并进同一 CGU → host 测试二进制需解析仅由链接脚本（`x86_64.ld`）与汇编（`isr.asm`）提供的符号。
- **修复方案（用户裁定「还有更优方案吗」后选定）**：在 host-only 壳 crate `src/rust/src/lib.rs` 以 `#[cfg(feature = "host-test")] #[unsafe(no_mangle)]` 提供 4 个零值占位符号（`_kernel_text_start` / `_kpti_trampoline_end` / `_kernel_text_end`: `u8 = 0`；`USER_CR3_SAVE`: `AtomicU64::new(0)`）。**TCB（privileged）零改动**、无需新增构建配置、对全部 host 测试二进制一次性生效，与"壳仅供 host 链接（裸机直接走 kernel crate + 链接脚本）"的既有设计意图一致。
- **候选对比**：候选 A（privileged 内 `#[cfg(feature = "host-test")]` 占位）需在 TCB 内混入 host 分支；候选 B（`host-tests` 用 `--defsym`）需逐测试目标配置链接参数、且 `--defsym` 无法表达 `AtomicU64` 类型的原子变量；候选 C（相关测试降级 QEMU-only）以损失 host 回归覆盖为代价。三者均劣于所选方案。**2026-09-17 审查修正**：对候选 A 的排除理由**不成立**——`privileged/arch/x86_64/mod.rs:49-67`（E-04 `cpu_id`）早有同类 host 桩分支，且这正是"同源双编译"的实现方式；本批选占位壳的理由是"TCB 零改动 + 对全部 host 二进制一次生效"的**成本考量**，而非架构约束。
- **残留风险（非结构根治）**：本方案在**符号级**确定性收敛（不再依赖 CGU 布局），但**类级根因未消除**——host 仍编译引用汇编/链接脚本符号的裸机 arch 模块、两侧无同步守卫、且"链接期硬失败"退化为"运行期静默错值"。三项残留 + 候选处置（审计守卫 / 结构根治 / 维持现状）见下方「预存登记（host 链接占位符号残留风险）」，**2026-09-17 审查裁定：结构根治（符号使用点级 host 桩化），壳占位随之删除**。
- **附带影响**：`host-tests/tests/plan_b_inode_test.rs::name_to_handle_at_uses_path_resolution` 的源文本断言随 `file_handle.rs` 入径变化同步更新（`VFS_MANAGER.resolve_mount_fs(path)` → `resolve_mount_fs(` + 新增 `resolve_user_path(` 断言，契约强度不降）。

**验证**：双架构 check 0w0e（x86_64/aarch64，含 `kernel_test` 维度）、clippy 3 维（pedantic lib / kernel_test / host-test）0 warning、`./ci/audit.sh quick` 全绿（含注释中文化 100%、functions 0 unsafe、边界黑名单、双子树 deadlock 矩阵等；`audit_implicit_deps` 151 → 159，详见下方预存登记）、`./ci/build.sh all` 5/5、host-tests 全量（99 个 test bin 全 `ok` / 0 failed，含 E-04 共享测试集 371 项）、QEMU kernel_test 498/498（0 skipped）。

### T4 实施记录（2026-09-18）

**核实结论**（工具 `scripts/audit_unwired_pub_fn.py` R3，7 项逐一核实）：

| 模块 | trait | 包装器 | 生产引用 | 内联测试 | host-tests 平行实现 |
|---|---|---|---|---|---|
| `dmu_trait` | `DmuManager` | `StandardDmu` | 0 | 11 个，门禁中从不编译 | `HostDmuManager` / `StandardHostDmu` |
| `raidz_trait` | `RaidzEngine` | `StandardRaidz` | 0 | 11 个 | `HostRaidzEngine` / `StandardHostRaidz` |
| `spa_trait` | `SpaManager` | `StandardSpa` | 0 | 11 个 | `HostSpaManager` / `StandardHostSpa` |
| `txg_trait` | `TxgManager` | `StandardTxg` | 0 | 9 个 | `HostTxgManager` / `StandardHostTxg` |
| `zap_trait` | `ZapStore` | `StandardZap` | 0 | 11 个 | `HostZapStore` / `StandardHostZap` |
| `zil_trait` | `ZilLog` | `StandardZil` | 0 | 13 个 | `HostZilLog` / `StandardHostZil` |
| `zil_persist_trait` | `ZilPersist` | `StandardZilPersist` | 0 | 11 个 | `HostZilPersist` / `StandardHostZilPersist` |

7 项**全部为纯预留抽象**（零生产引用）。判据：

1. trait 自述目的「便于单元测试注入 mock」未落地——全仓无任何生产/测试调用方依赖该抽象。
2. 具体实现的测试由 `privileged/tests/test_unkfs.rs` 直接针对 `UnkfsZap` / `UnkfsZil` / `UnkfsSpa` / `UnkfsObjSet` / `UnkfsTxgGroup` 等**具体类型**完成，未走 trait。
3. 7 文件内的 `#[cfg(test)] mod tests` 在 `make test-unit`（构造为 `--features kernel_test`，非 `cargo test`）与 `make test-host`（依赖编译不含 `cfg(test)`）下**均不编译**——从不执行。本仓 `cargo test` 入口仅为 host-tests（Makefile L419 / ci/build.sh L58）。
4. host-tests 已走「直接引用内核真实源码」的源共享路线（DECISION-052 路线 C），取代 trait 注入式 mock。
5. 项目既有教训明载：「Trait injection for empty forwarding logic creates unnecessary abstraction overhead」。

**ZFS 归属性判定**：这层 trait 壳**不承载任何 ZFS 语义**——纯方法签名镜像 + 1:1 转发（如 `StandardZap::insert` 仅 `self.0.insert(...)`；`StandardSpa::name()` 是返回 `""` 的 stub）。类 ZFS 关键在具体实现子系统（`spa.rs` / `dmu.rs` / `zap.rs` / `txg.rs` / `zil.rs` / `zil_persist.rs` / `raidz.rs` / `arc.rs` / `vdev.rs` / `metaslab.rs` / `bp.rs` / `dedup.rs` / `snapshot.rs` / `dataset.rs` 等，约 7100 行），与本次删除零交集。

**处置裁定**（AskUserQuestion，用户授权）：**删除 7 模块**。

**改动清单**：

| 文件 | 改动 |
|---|---|
| `functions/fs/unkfs/{dmu,raidz,spa,txg,zap,zil,zil_persist}_trait.rs` | 删除 7 文件（含从不运行的内联测试） |
| `functions/fs/unkfs/mod.rs` | 删除 7 行 `pub mod` 声明 |
| `scripts/audit_invariants.py` | I2 检测 docstring 中指向已删文件 `raidz_trait.rs:304` 的历史注释改写为通用表述（本批改动直接导致的 stale 引用，§9.3 本轮修复） |

**验证**：

- `audit_unwired_pub_fn.py` R3：**7 → 0**；
- `./ci/build.sh all` 5/5（双架构 0w0e + host-tests + link）；
- `./ci/audit.sh quick` rc=0（双架构 check、clippy 3 维 0 warning、functions 0 unsafe、注释中文化 100% 全绿）；
- `make test-host` 全量 0 failed；
- 未跑 QEMU（未触及 boot/架构路径）。

**附带观测（归 T5）**：R1 由 438 → 447（+9）——删除包装器后，原先仅被 `Standard*` 一次性转发调用而「看似被引用」的具体方法失去该跨文件引用，真实零引用状态随之暴露。属审计信号变得更诚实（非本批引入的缺陷），纳入 T5 甄别一并处置。

### T5 实施记录（批 1，2026-09-18）

**路径裁定（用户）**：R1 甄别采用**方案 A「分批只删确证无用」**——仅对「**重复能力 / 有等价公共入口 / 确证废弃**」三类施工删除；「**能力预留 / 转正式**」类**不改代码**，仅登记入台账（沿用 DECISION-052 第三层 + 分册 9 D-4 判据）。T5 声明**必须分批**。

**调研发现（关键，改变 T5 预期）**：R1 447 项经逐文件抽查后确认**主体为有意保留的 API 面，而非缺陷**：

| 类别 | 实例 | 为何零调用但保留 |
|---|---|---|
| functions 的 privileged 安全代理壳 | `driver/acpi.rs`（`get_lapic_base`/`get_ioapic_*`/`hpet_info`）、`sgeg/secure_boot.rs`（8 项）、`sgeg/crypto.rs`、`driver/char/serial.rs`、`driver/storage/ahci.rs`、`fs/unkfs/dedup.rs` | functions 经顶层 re-export 暴露 safe API（F2 边界要求）；「零调用」≠「无用」 |
| 文件系统 mount/umount API 面 | `fs/{sysfs,cgroupfs,configfs,virtiofs,systree,devpts}.rs` 的 `mount_*`/`umount_*` | 完整实现，待 VFS mount 集成接线 |
| 调试/统计查询面 | `idt/statistics.rs`、`idt/handlers.rs`、`mm/slab.rs`、`mm/page_fault.rs`、`sync/atomic.rs` 计数、`net/route.rs`、`net/netfilter.rs` | 诊断/可观测能力预留 |
| 同步/跨架构原语预留 | `sync/{rwlock,seqlock,pi_mutex,atomic}`、`arch/aarch64/{mmu,gic,psci,vmm_aarch64}`、`arch/x86_64/{apic,ioapic}` | D-4 已登记「能力预留」 |
| 驱动硬件操作 | `driver/{display,xhci,e1000,dma,uefi,power}` | D-4 已登记「部分应接线，部分转正式」 |

→ 结论：**T5 的可删面远小于 R1 计数**；粗暴删除会移除有意保留的 API 面（D-4 明载「转正式比接线更合理，避免为消除而接线」）。

**批 1 删除清单（9 项 R1 项，均为「重复能力 / 等价公共入口 / 废弃兼容壳」）**：

| # | 文件 | 项 | 判据 |
|---|---|---|---|
| 1 | `privileged/error.rs` | `io_error()` / `out_of_memory()` / `read_only()` | 「向后兼容别名（fs 层旧变体名 → 统一变体名）」块内三项，全仓 0 调用 → 废弃兼容壳 |
| 1b | 同上 | `not_found()` | 同块第四项，`.not_found()` 全仓 0 调用（审计 `rg -w` 把测试局部变量 `not_found` 计入引用，故未进 R1 计数）→ 连同 #1 整块 impl 删除 |
| 2 | `privileged/config/boot_image.rs` | `read_boot_image()` | doc 自述「供测试/调试」，全仓（含 host-tests）0 引用；同文件 `encoded_len()` 有调用方 |
| 3 | `privileged/driver/bus/pci.rs` | `pci_device_count()` | 纯转发 `crate::privileged::pci::device_count()`，后者已被 `pci_get_device_count` FFI 使用 → 等价公共入口 |
| 4 | `privileged/mm/vma.rs` | `mm_struct_new()` | 纯别名 `MmStruct::new()`（测试已在用），且未进 `mm/mod.rs` re-export |
| 5 | `privileged/mm/swap.rs` | `pte_to_swap_entry()` | 薄包装 `SwapEntry::from_pte()`，后者已有调用方；未进 `mm/mod.rs` re-export |
| 6 | `functions/fs/inode.rs` | `new_legacy_inode()` | 与 `LegacyInode::from_fs_result()`（`fs/file_handle.rs:203` 在用）重复的构造入口 |
| 7 | `functions/proc/table.rs` | `allocate_reserved_pid()` | 函数体即 `allocate_pid()`（doc 亦述「普通进程用 allocate_pid」）→ 重复入口且名实不符 |

**批 1 保留（登记「待裁」，非删除）**：

| 项 | 保留理由 | 建议 |
|---|---|---|
| `privileged/driver/bus/pci.rs::pci_scan()` | 能力与 `pci::scan_all_buses()`（已被 `functions/driver/storage/mod.rs:182` 等使用）重叠，但含设备日志输出，属驱动**诊断面** | 待裁（接线 vs 删） |
| `fs/{sysfs,cgroupfs,configfs,virtiofs}::umount_*` 等占位实现 | `umount_sysfs` 恒 `Ok(())`、`umount_devpts` 误调 `mount_devpts` —— 属 FS API 面**半成品**，删除会移除 API 面 | 登记预存缺陷，待 VFS mount 集成时修实装 |
| `sync/atomic.rs` `record_*` 四项 | 计数本应被原子操作调用（`dump_stats` 已在用），当前计数恒 0 —— 属**半接线缺陷** | **已决（本批）＝删除 feature**（按职责判定：非内核所需 + 零履行 + 已有 `*_dump_stats` 等价诊断 idiom，见 B-5.2 / B-5.3 G2） |

**R1 台账（批 1 后 438 项，按子系统分类）**：

| 项数 | 子系统 | 类别 |
|---|---|---|
| 60 | `privileged/` 其他（idt/timer/net/cpu/dma/pci/io/debug/console/egdf/config/ipc/sgeg/error/vmspace/page_table/irqline） | 预留（查询/诊断/原语） |
| 58 | `privileged/arch/*`（apic/ioapic/gic/mmu/psci/gdt/uart/acpi/timer/shadow_stack） | 预留（跨架构原语，D-4） |
| 43 | `privileged/proc/*`（进程/调度/命名空间/信号/fd_table/rlimit/cgroup/session/cfs） | 预留（策略查询面） |
| 43 | `functions/driver/*`（usb/acpi/char/storage/display/virtio/firmware/uefi） | 预留（safe 代理壳 + 硬件面） |
| 42 | `privileged/driver/*`（display/usb/net/power/uefi/hotplug/pci/input） | 预留（D-4） |
| 41 | `functions/fs/*`（systree/devpts/exfat/ext2/tmpfs/virtiofs/sysfs/cgroupfs/configfs/ramfs/inode） | 预留（mount API 面）+ 少量待裁 |
| 34 | `functions/fs/unkfs/*`（dedup/bp/arc/raidz/txg/spa/zil/dmu…） | 预留（UNKFS 子系统） |
| 30 | `functions/` 其他（ipc/proc/mm/net/wasm/freg/timer/config） | 预留 + 少量待裁 |
| 22 | `privileged/mm/*`（vmm_aarch64/kpti/numa/swap/slab/vma/page_fault/frame） | 预留（D-4） |
| 21 | `functions/sgeg/*`（identity/secure_boot/crypto） | 预留（D-4） |
| 17 | `privileged/sync/*`（rwlock/seqlock/pi_mutex/atomic/mutex/rcu/spinlock） | 预留（D-4） |
| 14 | `privileged/freg/*`（domain/recovery/reset/recoverable/snapshot） | 预留（BCB 策略面） |
| 13 | `privileged/fs/vfs/*`（flock/handle/inotify/dcache/vfs） | 预留（VFS 内部） |

**验证**：`audit_unwired_pub_fn.py` R1 **447 → 438**（−9，与删除项精确对应；R3 仍 0）；`./ci/build.sh all` 5/5（双架构 0w0e + host-tests + link）；`./ci/audit.sh quick` rc=0；`make test-host` 全量 0 failed；未跑 QEMU（未触及 boot/架构路径）。

**后续批次判据（沿用批 1）**：先查「是否存在等价公共入口 / 是否有使用者」，再看是否属 D-4 预留类别；仅前者成立才删，后者一律登记入台账。

### T5 全量甄别台账（批 2，2026-09-18）

**性质**：批 2 为**只读扫描 + 登记**，未改任何源码（用户裁定「主项扫，登记后我和审核进行讨论」）。扫描后逐项四分类结果如下，供用户与 reviewer 讨论后决定「删 / 接线 / 预留 / 待裁」的最终处置。

**扫描区间**（批 1 后 R1 余 438 项，按区间切三块）：

| 区间 | 覆盖 | 项数 |
|---|---|---|
| g1 | `privileged/{arch,mm,sync,freg}` | 111 |
| g2 | `privileged/` 其余（driver/fs/proc/idt/timer/net/cpu/dma/pci/io/debug/console/egdf/sgeg/ipc/…） | 158 |
| g3 | `functions/` 全量 | 169 |

**方法与限制（复核前置，reviewer 反馈后补）**：

**① 判定工具**：`scripts/audit_unwired_pub_fn.py` 的 **R1 规则**（`pub fn` 跨文件零引用 WARN）。引用计数实现为 `rg -c -w <name> src/ host-tests/`（[脚本 L247-255](file:///home/anfer/Code/Edgine/scripts/audit_unwired_pub_fn.py#L247-L255)），**纯文本词边界计数**；脚本自述「基于 .rs 文件源码 AST 分析, **不依赖 cargo build**」（[L69](file:///home/anfer/Code/Edgine/scripts/audit_unwired_pub_fn.py#L69)）。**声明侧无 cfg 感知**——`EXEMPT_CFG_ATTR` 只豁免「引用侧被 cfg 门控的引用」，不识别「声明本身被 cfg 门控」。

**② 判定构建维**：**不存在单一构建维**。名义上是「`src/` + `host-tests/` 文本并集」，既非某一构建维，也未做「逐维取交集」。⇒ 台账中的「零引用」**不等于**「任何构建维下都无调用者」。

**③ 已知系统性误判源**（计数由此失真，非个案）：

| 误判源 | 实例 | 后果 |
|---|---|---|
| 声明侧 `#[cfg(target_arch = "…")]` | [mm/mod.rs:51-53](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/mod.rs#L51-L53) 以 `#[path = "vmm_aarch64.rs"]` 门控 `pub mod vmm`；`arch/aarch64/**` 同理 | 非本维编译的模块被判「无调用者」= **构造性结果**，非死代码证据 |
| 声明侧 `#[cfg(feature = "…")]` | 原实例 `sync/atomic.rs` `#[cfg(feature = "atomic_stats")] mod stats`（原链接已随本批 feature 删除失效） | feature 门控代码被判「死」，实为「默认维未启用」；**该实例本批已消解**（feature 删除，见 B-5.2 / B-5.3 G2）。**类别本身仍适用**（现存同类如 `#[cfg(feature = "kernel_test")]` / `#[cfg(feature = "alloc")]`） |
| `#[cfg(feature = "kernel_test")]` 测试模块 | `freg/snapshot.rs` / `freg/reset/layered.rs` 的 `pub mod tests` | 测试用例被判「未接线」 |

③ 表补充：**单维甄别的结果仅在该维有效**，跨维完备性需逐维复核（遗留 1 裁定：属**漏项风险**，非误删风险——甄别所在维中 x86_64 专属项*有*引用，不会被误列删候选；误删方向已由「aarch64 门控」属性隔离）。

**④ 复核判据（正确口径）**：**逐构建维取交集**——x86_64 裸机 / aarch64 裸机 / kernel_test / host-test **四维皆零引用**，才可判「真无引用」；且**无调用者 ≠ 死代码**（F9 要求「硬件规范常量须通过实现使用路径消除」，但其**反向不成立**：汇编/外部调用、硬件规范要求、跨架构对称性都会使「无 Rust 调用者」合法）。**删候选判据进一步升格为三合一**（reviewer 第二轮）：零引用 ＋ 等价公共入口 ＋ 非 API/FFI/feature/硬件原语面，见「分类语义」。

**⑤ 本台账定位**：以下全部计数为**暂定值**，**不得作为施工依据**（§12.4）；处置路径见 ⑥（试删流程），不另造静态分析器。

**⑥ 施工落地方式（遗留 2 裁定：不立项新工具）**：删候选**不用静态分析器**，直接**逐项试删 → 跑既有五条门槛 → 任一维硬失败即回退并归入「硬件原语保留 / 待裁」**。四维判据由既有门槛原地覆盖，零新增基础设施：

| 维 | 既有门槛 |
|---|---|
| x86_64 裸机 | `./ci/build.sh x86_64`（`./ci/build.sh all` 第一阶段） |
| aarch64 裸机 | `./ci/build.sh all` 第二阶段 |
| `kernel_test` | clippy `kernel_test` 维 + QEMU `kernel_test` |
| `host-test` | clippy `host-test` 维 + `make test-host` + QEMU boot |

> 理由：**编译/链接器即权威判据**（工具是近似、编译器是真相）；且与上一轮「host 链接守卫交还链接器、否决审计守卫脚本」的裁定同构——近似的静态分析恰是「黑名单不全 → 虚假信心」那类危险机制（§12.3 简约准则）。低风险项可批量，`kernel_test` 与 QEMU boot 为硬闸门。若日后仍要立项该工具，应**单开任务且低优先级**，并明确**不是 T5 的前置**。

**⑦ 试删的原理性盲区（reviewer 第二轮，必须记住）**：`试删 + 五条门槛` **在原理上发现不了「安全校验被移除」**——删掉一个零调用的校验函数后，编译/链接全过；测试若未覆盖该路径，也全绿，但校验已经没了。⇒ 安全敏感项（**路径校验 / 常数时间比较 / CET-SSP / CR4 权限谓词**）**不能靠试删兜底**，必须先经安全面确认（见 A-3 与「分类语义」安全面行）。

**⑧ 顺序约束与解锁条件（reviewer 第二 / 三轮裁定）**：

- **不得并行 T1 收尾与 T5 施工**：两批都动 `privileged`，QEMU 一旦挂掉无法归因。**文件级冲突已确认**：`pcid_is_enabled` 位于 `mm/kpti.rs`，正是 T1 P2′ 声明收拢所改文件。T5 的**文档修订**不占代码，可与 T1 收尾并行。
- **A-2 开工解锁条件（五条，缺一不可；reviewer 第三轮已逐条核销）**：

| 条件 | 状态 | 依据 |
|---|---|---|
| ① 7 项「零消费」判据补齐证据或退桶 | ✅ 完成 | 7 项全退（3 项同文件整组统一、1 项已登记路线图项、3 项统计/诊断 API 面） ⇒ A-4 退桶 8 |
| ② 安全敏感 4 项完成安全面二次分桶 | ✅ 本次裁定完成 | `ct_eq_salt`/`ct_eq_password` ⇒ 族残缺入 A-1；`split_path`/`validate_path` ⇒ B-4 待 T3 结论（A-3） |
| ③ `gdt.rs` 注释原文核实 | ✅ 已完成 | `get_gdt_table` 注释原文＝`/// 获取 GDT 表的引用 (调试用途)` ⇒ 自述调试用途，非死代码，退桶（A-4） |
| ④ 两组同文件分裂按路线图统一 | ✅ 已完成 | `shadow_stack.rs`（`is_ssp_valid` 随 4 项 CET 项）/ `numa.rs`（`contains_cpu` `all_nodes` 随 3 项 NUMA 项）整组归入待裁（B-3 注） |
| ⑤ T1 收尾完成（串行闸门） | ✅ 已解除 | T1 收尾三门槛复跑全 RC=0（QEMU boot 1/1 命中 `VFS ready`） |

- **A-2 开工执行约束（reviewer 第三轮，区别于原方案）**：① **逐项试删**，每项**独立提交、可单独回退**（便于归因）；② 每项试删后跑**五条门槛全量**（非只编译），其中 **QEMU boot 为硬闸门**；③ **B-4 的 ramfs 2 项不在本次试删范围内**（等 T3 结论）；④ **`pcid_is_enabled` 已移出删候选**（属已登记路线图地基，A-4），确认**不在 11 项内**——A-2 11 项清单为 `get_ap` / `tss_64bit` / `consume_quota_tick` / `is_quota_exceeded` / `write_log_line` / `vfs_close_safe` / `vfs_seek_safe` / `vfs_readdir_safe` / `get_fs_name` / `pipe_exists` / `format_duration`。

**分类语义（修订版，登记口径）**：

| 分类 | 含义（可复核标准） | 处置路径 |
|---|---|---|
| 删候选 | **收窄为三合一判据（reviewer 第二轮升格）**：① **该构建维零引用** ＋ ② **存在能力等价的公共入口**（或重复实现）＋ ③ **非 API/FFI/feature/硬件原语面**。三者**同时成立**方可判删——「有等价入口」单独不成立（该函数可能正属 API 面 / FFI 面 / feature 面，判删后即删掉对外能力） | 须先过「硬件原语完整性」二次分类 + **安全面确认** + ⑥ 试删 |
| 安全面待确认 | 安全敏感面：路径校验 / 常数时间比较 / CET-SSP / CR4 权限谓词 等。**试删在原理上发现不了「安全校验被移除」**（删掉零调用校验函数后编译链接全过、测试若无该路径覆盖亦全绿） | **不走试删**；**产出规格＝三档（冗余 / 缺陷 / 族残缺，见 B-0）**，逐项定型后分别入 A-1 / B / C-1。本轮 4 项已分流完毕 |
| 硬件原语完整性保留 | 硬件原语/规范常量（内省、兼容包装、常量 getter 属其典型形态）；「无 Rust 调用者」可由汇编/规范/对称性解释 | 记预留，**不删** |
| 接线 | **仅限**「现有调用链缺此半」（小改即可挂上） | 可施工子清单 |
| 未来功能 | 所属**子系统未集成** + 原「预留」API 面（DECISION-052 第三层） | 记录，不接线、不删除 |
| 待裁 | 半接线缺陷 / 依赖路线图 / 受构建维或 feature 门控影响 | 需用户 + reviewer 裁定 |

**分组统计（原始扫描输出，保留可追溯；口径已作废）**：

| 区间 | 项数 | 删 | 接线 | 预留 | 待裁 |
|---|---|---|---|---|---|
| g1 `privileged/{arch,mm,sync,freg}` | 111 | 59 | 34 | 8 | 10 |
| g2 `privileged/` 其余 | 158 | 7 | 102 | 47 | 2 |
| g3 `functions/` | 169 | 4 | 6 | 150 | 9 |
| **合计** | **438** | **70** | **142** | **205** | **21** |

**修订后桶数（六次修订 + reviewer 第四轮**范围裁定**：接线 / 未来功能移出 T5；不得作为施工依据）**：

| 桶 | 项数 | 包含 | 是否在 T5 内 |
|---|---|---|---|
| 删候选 | **0** | 四次修订 2 − 试删删除 1（`write_log_line`）− 判据待补退桶 1（`format_duration`，A-6）⇒ **清零（裁定四.1 达成）** | ✅ 是 |
| 完整性保留（原「硬件原语完整性保留」） | **75** | 43（41 硬件原语＝x86_64 侧 31 + aarch64 侧 10，优先级裁定见 A-1；+ 安全面「族残缺」档 2 `ct_eq_salt`/`ct_eq_password`）+ **32**（**六次修订**：B-5.2 判据待补补齐后定桶，G1-G8 明细见 B-5.2） | ✅ 是（**仅登记**，不施工） |
| 待裁 | **0** | **七次修订**：六次修订后余 **2**（`split_path`/`validate_path`，安全面待 T3）⇒ **已裁决删除**（裁定七证据链定型「冗余」+ reviewer 授权解除裁定六阻塞，见 B-5.3 / B-10.8）；**三字段齐备，无裸待裁（裁定四.2 达成）** | ✅ 是（**裁定四.2 验收**） |
| **T5 内合计** | **75** | **算术闭合**：0 + 75 + 0 = 75 ＝ 436 − 2（**七次修订**：`split_path`/`validate_path` 已删除, 退出甄别池）− 359（未来功能）；六次修订前为 90（＝438−1−8−339）；**甲批 C-1 后为 436 − 0 − 359**（见 B-8） | — |
| — 接线 | **0** | 8 项**已全部处置**（**甲批 C-1**，见 B-8）：1 项接线落地（`set_fd`）+ 7 项「仅缺此半」判据不成立 ⇒ 转「未来功能」 | ❌ **否**——已完结（原「功能接线批次」，与 T3 联动） |
| — 未来功能（DECISION-052 第三层） | **359** | 339（来源合成：原「接线」剩余 134 ＝142−8，排除法所得，非实测 + 原「预留」205）+ **13**（**六次修订**：B-5.1 等路线图转入——CET 5 / NUMA 5 / PCID 1 / IOMMU-DMAR 2）+ **7**（**甲批 C-1**：判据不成立退桶，见 B-8.2） | ❌ **否**——移出，归 **DECISION-052 第三层** |

> **裁定一（reviewer 第四轮，T5 范围）**：删候选＝逐项试删（**仅冗余档**）+ 五门槛全量 + 独立提交可回退；完整性保留＝**不施工**，登记为「已分类」供审计引用；待裁＝逐项补「等待原因 + 解锁条件 + 责任方」，**禁裸待裁**；**接线 / 未来功能移出 T5**（接线是「加功能」、甄别是「清死代码」，混桶会让 T5 永远无法关闭且归因困难；339 项属未集成子系统，本不属甄别范畴）。

> **裁定二（reviewer 第四轮，待裁 47 的归零路径，禁裸待裁）**：每一项必须落入下列**三态**之一，否则不许留在待裁；且每项须带**三字段＝等待原因 + 解锁条件 + 责任方**。

| 态 | 判据 | 去向 |
|---|---|---|
| **等路线图** | 能点名具体路线图（CET / NUMA / PCID…） | 转「未来功能」，注明**路线图名与登记处** |
| **判据待补** | 族残缺 / 事实待核 | **补齐后定桶**；补不出即转「**完整性保留**」 |
| **安全面待 T3** | 路径校验 / 常数时间比较等 | **留在待裁**，挂 T3 依赖并标注 |

> **裁定四（T5 关闭验收标准，五条缺一不可）**：① **删候选桶清零**（试删通过提交 / 失败退桶）——**已达成**；② **待裁 47 项全部带三字段，无裸待裁**（**T5-B 已达成**：B-5 归零表，47 → 13 等路线图 + 32 判据待补定桶 + 2 安全面待 T3，逐项三字段齐备）；③ **桶数算术闭合且桶间转移路径可在台账明细逐项追**（438 → **437**；本轮 11 → 2 的 −9 逐项列于 A-5、2 → 0 的 −2 列于 A-2 / A-6、**47 → 2 的 −45 逐项列于 B-5.1 / B-5.2 / B-5.3**，桶效应见 B-5「桶数六次修订」）；④ **审计噪音治理落地**（裁定五，单开任务，**当前未落地** ⇒ 见 T5-C）；⑤ **文档同步**：R1 状态行 + 与 [audit-fix-09 D-4](archive/audit-fix-09-hard-rules-deadcode.md) 的**双向引用**（**已达成**，见两处六次修订状态注）。

> **裁定五（审计噪音治理——单开任务，最小形态）**：`scripts/audit_unwired_pub_fn.py`（R1）在 CI 反复报零引用 pub fn，T5 关闭后若不治理则**成果无法固化、清单必漂移**。裁定：给 R1 增加「已分类清单」数据源，**不新建独立文件**，清单以**台账内机器可读区块**承载——以 `audit-classified-begin` / `audit-classified-end` 两个 HTML 注释标记圈定（**实际区块见 B-6**）；审计脚本读该区块作为「已知分类」集合，**仅对未分类的零引用 pub fn 报 HIGH**；**fail-closed**＝区块缺失 / 解析失败视同未分类（仍报）；**只降噪不豁免**＝不改变「零引用」事实判定，只改变报告分级。定位＝**单开任务**，**不作为 T5 前置**（T5 可先完成甄别），但**是 T5 关闭条件之一**。

> **裁定六（授权边界）**：委托人**可自主处置**＝判据复核 / 三档分档 / 试删 / 退桶 / 桶数修订 / 台账事实订正 / 已确认删候选的试删全流程（含提交、回退）。**必须上报（不自主决定）**＝① **新增删候选**（本轮 23 → 2 的教训：**宁保守**）；② 删除涉及**安全面**（sgeg / 权限 / 路径 / 内存保护）；③ 删除涉及 **TCB 核心**（GDT / IDT / MMU / 中断 / 调度）；④ 删除涉及**已登记路线图的地基**（如 `pcid_is_enabled`）。

> **裁定七（待裁 47 的优先次序）**：**安全面待 T3（2 项，ramfs `split_path` / `validate_path`）先解**——它**阻塞一个可能的安全缺陷判定**；其余两态按文档工作量平行推进。

> 已识别 **aarch64 专属 31 项**（`arch/aarch64/**` 8+9+3+2+1 与 `mm/vmm_aarch64.rs` 6、`mm/kpti_aarch64.rs` 2），**x86_64 专属项面未量化**（`arch/x86_64/**`、`mm/kpti.rs`、`idt/**`、`cpu/**`、`arch/shadow_stack.rs` 等），两者均须按 ⑥ 试删复核。

#### A. 删候选（五次修订：70 → 11 → 2 → 0）+ 硬件原语完整性保留（三次修订：41 → 43）

**A-1 硬件原语完整性保留（43 项；记预留，不删）**

| 文件 | 项 | 理由 |
|---|---|---|
| `privileged/arch/x86_64/apic.rs` | 15：`get_version` `get_timer_count` `is_timer_calibrated` `configure_lint0` `configure_lint1` `apic_read_isr` `apic_read_tmr` `apic_read_irr` `apic_is_in_isr` `apic_is_in_irr` `apic_is_level_triggered` `send_ipi_level` `broadcast_ipi_level` `icr_level` `icr_broadcast` | APIC 原语完整性；须在**裸机 + kernel_test 维**逐一验证是否被中断/启动路径调用——**误删直接破坏真机中断** |
| `privileged/arch/x86_64/ioapic.rs` | 6：`get_max_irq` `set_irq_level` `set_id` `get_arbitration_id` `delivery_lowest` `delivery_init` | IOAPIC 原语完整性，同上 |
| `privileged/sync/pi_mutex.rs` | 2：`get_ceiling` `get_protocol` | 同步原语族完整性（PI 协议查询属规范形态） |
| `privileged/sync/rwlock.rs` | 5：`raw_read_unlock` `raw_write_unlock` `read_irqsave` `write_irqsave` `pending_writer_count` | 原语层入口完整性 |
| `privileged/sync/seqlock.rs` | 2：`current_sequence` `get_valid` | 原语层入口完整性 |
| `privileged/sync/spinlock.rs` | 1：`lock_irq` | 规范要求的原语形态 |
| `privileged/arch/aarch64/gic.rs` | 3：`is_ppi` `is_valid_irq` `is_spi_pending` | 架构规范范围谓词 + GIC 寄存器状态查询（aarch64 原语面） |
| `privileged/arch/aarch64/mmu.rs` | 1：`allows_el0_access` | 页表描述符权限谓词（硬件内省） |
| `privileged/arch/aarch64/timer.rs` | 1：`read_control` | 定时器控制寄存器读取（硬件内省） |
| `privileged/mm/vmm_aarch64.rs` | 5：`is_desc_table` `is_desc_block` `is_desc_page` `is_desc_device_memory` `is_desc_non_cacheable` | 页表描述符类型谓词（硬件内省） |
| `functions/sgeg/crypto.rs` | 2：`ct_eq_salt`(210) `ct_eq_password`(216) | **族残缺档（reviewer 第三轮裁定一）**：与 `ct_eq_hash`(204) 形状完全相同，同为 `Salt` / `PasswordHash` / `Sha256Hash` 的**类型化比较同族**；`ct_eq_hash` 有调用者、未入删候选 ⇒ 删另两者则**族残缺且不对称**。真正作用不是「提供能力」（`ct_eq` 已提供），而是**阻止调用方拆字段**（写 `ct_eq(&a.0, &b.0)`）——删掉等于**诱导密码学代码绕过类型包装**、降低抽象层级。与 A-1 保留 `sync/*` 原语族入口**判据完全同构** |

> **安全面「族残缺」档并入（reviewer 第三轮裁定一：41 → 43）**：`ct_eq_salt` / `ct_eq_password` 由「安全面待确认」定型为**族残缺** ⇒ 按裁定只保留、不删、不停留于待裁。**附核实（裁定一要求）**：`grep -rn` 全仓 `src/` + `host-tests/` 实测两者**零引用**（仅自身定义行，无任何调用）；二者为 functions 侧普通 `pub fn`，**无 `#[no_mangle]` / 无 `extern "C"` / 无 FFI 导出**，且零引用即排除跨 crate / 用户态 API 面按名调用 ⇒ **保留不引入新暴露面**。

> **桶间优先级裁定（三处核对 ①）**：aarch64 12 项中 **10 项**同时符合「硬件原语」与「aarch64 门控」两种属性 ⇒ 按 **硬件原语保留 > aarch64 门控 > 删候选** 归入本桶（本桶 31 → **41**）；余 **2 项**（`arch/aarch64/mmu.rs::diagnose_permission`、`mm/vmm_aarch64.rs::diagnose_descriptor`）为**诊断输出**非原语，留「待裁」并保留 aarch64 门控标记。

**A-2 删候选（0 项；五次修订后清零）**

| 文件 | 项（行号） | 判据与结果 |
|---|---|---|
| `privileged/console/gfx_console.rs` | `write_log_line`(286) | ② 与在用 `write_str`(280) **逐字节相同** ⇒ 纯重复实现；**试删五门槛 5/5 全过 ⇒ 已删除**（commit `23681a14`） |
| `privileged/timer/tick.rs` | `format_duration`(355) | **退桶（A-6）**：② 判据待补——`core::fmt` 是通用设施而**非能力等价公共入口**（不能产出 `1h23m45s678ms`，需自行重写同一逻辑），全仓无等价格式化入口 ⇒ 不属冗余档 |

> **本桶三次修订的 11 项中：9 项于试删前复核退桶**（A-5）、**1 项试删删除**（`write_log_line`）、**1 项判据待补退桶**（`format_duration`，A-6）⇒ **删候选清零**。

**A-6 `format_duration` 判据待补退桶（1 项，reviewer 第四轮裁定三「第 2 项自判」）**

| 文件 | 项（行号） | 自判过程 | 结论 |
|---|---|---|---|
| `privileged/timer/tick.rs` | `format_duration`(355) | 裁定三给出的自判规则＝「**凡属冗余档（逐字节 / 能力等价复本 ＋ 族完整）即试删，否则退桶**」。逐条核：**逐字节复本 ✗**（非重复实现）；**能力等价复本 ✗**（全仓 `grep` 无等价时长格式化公共入口，`core::fmt` 为通用设施）；**族完整**——`tick.rs` 的 `pub fn` 族为 tick↔时间单位换算（`ticks_to_ms` / `ms_to_ticks` / `get_uptime_*` / `get_time_info`），`format_duration` **不属该族**（格式化 vs 换算，形态不同） | **退桶**（判据待补）⇒ 入 B-5，非冗余档 |

> **为何不按「③ 非对外 API 面」直接删**：三合一判据须**三者同时成立**；② 已落空，故不构成删候选（reviewer 第二轮「有等价入口单独不成立」的反向同理＝**无等价入口即不可删**）。此外 `#[cfg(feature = "alloc")]` 门控属性亦须经 feature 维复核。

**A-3 安全面二次分桶（reviewer 第三轮裁定，4 项已全部分流完毕；本桶清零）**

| 文件 | 项（行号） | 定型档 | 去向 |
|---|---|---|---|
| `functions/sgeg/crypto.rs` | `ct_eq_salt`(210) `ct_eq_password`(216) | **族残缺** | ⇒ **A-1 硬件原语完整性保留（43）**。与 `ct_eq_hash`(204) 同族，删则族残缺且不对称，且诱导调用方绕过类型包装（裁定一） |
| `functions/fs/ramfs.rs` | `split_path`(524) `validate_path`(544) | **待定型（冗余 / 缺陷 二选一）** | ⇒ **B-4 待裁**。**不得进试删队列**，须先由 T3 安全面给出「VFS 是否已提供等价校验」的结论（裁定二） |

> **事实更正（裁定二，威胁模型修正）**：上轮 A-3 记述的「路径穿越防护」**不成立**——实读 [ramfs.rs:544-562](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/ramfs.rs#L544-L562) `validate_path` 只做**空 / 长度(`VFS_MAX_PATH`) / NUL** 三项检查，**不含 `..` 穿越检查**；穿越防护由 VFS 侧 `resolve_path`（[ramfs.rs:515](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/ramfs.rs#L515) `global().resolve_path(path)`）负责。故「删掉即移除穿越防护」的担心不成立。
>
> **零调用的两种含义（裁定二给出，必须二选一）**：① **VFS 层已做等价（空/长度/NUL）校验** ⇒ 这两项**冗余**，可删，但须**登记契约**「ramfs 依赖 VFS 前置校验」（否则将来 VFS 改动会无声破坏 ramfs 的假设）；② **VFS 未做等价校验** ⇒ 零调用说明 ramfs 路径入口**缺基础校验**，是**安全缺陷**而非死代码 ⇒ **不得删**，登记缺陷移 T3 并按 C-1 接线（ramfs 路径入口补调用）。此项待 T3 结论后定型。

**A-4 退桶（8 项；判据不成立 ⇒ 并入「待裁」桶）**

| 文件 | 项（行号） | 判据不成立之处 |
|---|---|---|
| `privileged/arch/shadow_stack.rs` | `is_ssp_valid`(129) | **同文件整组统一**（`set_ssp` 等 4 项在待裁，取决于 CET 路线图）；且属安全敏感面（CET/SSP）。② 未取得等价入口证据 |
| `privileged/mm/numa.rs` | `contains_cpu`(195) `all_nodes`(334) | **同文件整组统一**（`set_distance` 等 3 项在待裁，取决于 NUMA 路线图）。② 未取得等价入口证据 |
| `privileged/mm/kpti.rs` | `pcid_is_enabled`(149) | **已登记的未完成路线图项**：文件头 doc（`kpti.rs:23-31`）明载「未完成 … **PCID/INVPCID 优化**：当前每次切换 CR3 都 TLB 全清，高频 syscall 性能损失 5-15%」⇒ 该函数是这条已登记项的地基，删它＝删掉登记过的路线图基础设施 |
| `privileged/freg/reset/audit.rs` | `count_by_result`(101) | ② 未取得等价入口（同文件 `count_by_layer` 是另一维度，非等价）；③ 属诊断查询 API 面 |
| `privileged/mm/page_fault.rs` | `page_fault_count`(497) | ② 未取得等价**封装**入口（`PAGE_FAULT_COUNT` 为 `pub static`，是并行读法而非等价替代）；③ 属统计 API 面 |
| `privileged/mm/slab.rs` | `utilization`(921) | ② 仅部分等价（`get_stats() -> CacheStats{total_objects, active_objects}` 可推导，但为使能等价能力需调用方自行除法）；③ 属统计 API 面 ⇒ 证据不足以支撑删除 |
| `privileged/arch/x86_64/gdt.rs` | `get_gdt_table`(701) | **注释原文核对**：`/// 获取 GDT 表的引用 (调试用途)` ⇒ 自述调试用途，**非死代码**，退桶 |

> **退 A-1 还是 B**：上表 8 项**均非硬件原语 / 规范常量**（A-1 的准入条件），故按 reviewer「补不出即退 A-1 或 B」统一退 **B（待裁）**，不走 A-1。

**A-5 试删前逐项复核退桶（9 项；reviewer 第四轮裁定 + 本轮实测；退入「待裁」）**

> **触发**：A-2 11 项试删开工后，逐项复核「零调用 ＝ 冗余 / 缺陷 / 族残缺」定型（B-0 三档规格）时，实测发现 9 项判据不成立。**判定依据为全仓 `grep -rn` 实测引用计数**，非静态推断。

**A-5.1 族残缺档（6 项；同族兄弟在用 ⇒ 不删，保对称）**

| 文件 | 项（行号） | 同族兄弟实测引用 | 族残缺证据 |
|---|---|---|---|
| `privileged/arch/x86_64/gdt.rs` | `tss_64bit`(173) | `code_64bit` **4** / `data_32bit` **4** / `tss_64bit` **0** | `Granularity` 构造器族 **2/3 在用**。字段为 `Granularity(pub(crate) u8)` ⇒ 删命名构造器后调用方只能写 `Granularity(Granularity::LONG_MODE)` **绕过构造器族**（与裁定一 `ct_eq(&a.0, &b.0)` **判据同构**） |
| `privileged/fs/vfs/handle.rs` | `vfs_close_safe`(542) `vfs_seek_safe`(547) `vfs_readdir_safe`(556) | `vfs_*_safe` 全族 **17 员 / 14 员在用** | 在用者：`vfs_open_safe` `vfs_read_safe` `vfs_write_safe` `vfs_fstat_safe` `vfs_stat_safe` `vfs_mount_safe` + `path.rs` 8 员 + `vfs_utimensat_safe` ⇒ 14/17 在用，删 3 员则族残缺不对称 |
| `privileged/arch/x86_64/acpi.rs` | `get_ap`(457) | `get_ap_list` **1** / `get_ap_count` **3** / `has_madt` **6** / `parse_madt` **5** | AP 信息访问器族在用 |
| `privileged/fs/vfs/vfs.rs` | `get_fs_name`(75) | `get_fs_type` **2** / `get_fs` **13** / `set_fs` **1** | VFS 挂载点元信息访问器族在用 |

> **`vfs_*_safe` 三项的档位争议（如实登记）**：实测 [functions/fs/dir_ops.rs:14-28](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/dir_ops.rs#L14-L28) 已**直接调用裸 `extern "C"`** `vfs_seek` / `vfs_readdir`（不经 `_safe` 壳）⇒ 该族**本就不是封装边界**，删 3 员**不改变抽象层级**（弱于 `tss_64bit` 的族残缺强度）。仍按「同族部分在用 ⇒ 保对称」退回待裁，档位由 reviewer 复核确定。

> **`get_ap` 的试删门槛已实测通过（如实登记）**：该项试删后 `./ci/build.sh all` / `./ci/audit.sh quick` / `make test-host` / `make test-unit` / `./scripts/qemu_boot_test.sh x86_64` **5/5 全过**（QEMU boot 硬闸门通过）⇒ **门槛在原理上无法识别族残缺**（与 ⑦ 试删盲区同源），故按裁定退回待裁，代码已 `git checkout` 回退。

**A-5.2 台账 ② 判据事实错误 / 无等价入口档（3 项）**

| 文件 | 项（行号） | 原台账记述 | 实测更正 |
|---|---|---|---|
| `privileged/freg/domain.rs` | `consume_quota_tick`(213) `is_quota_exceeded`(307) | 「② 与**在用** `check_quota` 语义重复」 | **`check_quota` 自身零引用**（仅 [domain.rs:278](file:///home/anfer/Code/Edgine/src/kernel/privileged/freg/domain.rs#L278) 定义行；全仓其余命中仅在 `docs/` 与 `build/*.map`）⇒「在用」**不成立**；且 `check_quota` 本身已列在同台账待裁表（B-2）。② 判据落空 ⇒ 退桶 |
| `privileged/ipc/dynamic.rs` | `pipe_exists`(118) | 「② 可由**在用** `get_pipe` / `pipe_count` 等价判定」 | 全仓**无 `get_pipe` 符号**；`pipe_count()` 只返回数量、**不判定指定 `IpcId` 是否存在** ⇒ **无能力等价的公共入口**，② 判据落空 ⇒ 按三合一判据它本不该入删候选，退桶 |

> **负债如实登记**：上表 2 处错误系「二次修订」时未经全仓实测即写入 ② 判据所致——已于本轮就地订正，并据此退桶。

#### B. 待裁（四次修订：21 → 37 → 46 → 47；**六次修订后归零为 2 项**——13 等路线图 + 32 判据待补定桶 + 2 安全面待 T3，逐项见 **B-5 归零表**）

**B-0 二次分桶产出规格（三档形态；reviewer 第三轮裁定三，本桶的定型口径）**

> 「安全面二次分桶」的产物**固定为三档**，不再以「三合一判据」代偿——三合一判据只判「**是否依赖等价入口**」，**不判「零调用是冗余还是缺陷」**，故不足以区分下列三档：

| 定型档 | 含义 | 处置 |
|---|---|---|
| **冗余** | 能力已被**上层 / 等价入口**覆盖（零调用＝上层已代劳） | **可删**，但须**登记契约**（写明依赖哪一层的前置能力），否则上层将来改动会无声破坏本层假设 |
| **缺陷** | 本该被调用却零调用（零调用＝**校验/能力缺失**） | **不能删**；登记为缺陷移 T3，并按 C-1 **接线**（补调用） |
| **族残缺** | 同族中**部分在用**（删之则族残缺且不对称） | **不删**，保对称 ⇒ 并入 A-1 完整性保留 |

> 三档与包装层的关系：**可删的前提是「零调用＝冗余」**，而非「零调用」本身；**零调用本身不构成删的判据**（第二轮已裁定）。本轮 4 项安全面按此规格分流结果见 A-3。

**B-1 新增（构建维 / feature 门控，reviewer 反馈）**

| 文件 | 项 | 待裁点 |
|---|---|---|
| `privileged/arch/aarch64/mmu.rs` + `privileged/mm/vmm_aarch64.rs` | 2：`diagnose_permission` / `diagnose_descriptor` | **aarch64 专属 + 诊断类**（[mm/mod.rs:51-53](file:///home/anfer/Code/Edgine/src/kernel/privileged/mm/mod.rs#L51-L53) 门控）⇒ x86_64 维下整模块不编译，「零引用」为**构造性结果**；另 10 项 aarch64 项因「硬件原语」优先级已归 A-1（见该桶优先级裁定） |
| `privileged/sync/atomic.rs` | 4：`record_inc` `record_dec` `record_cmpxchg_success` `record_cmpxchg_fail` | ~~原位于 `#[cfg(feature = "atomic_stats")]`~~，**feature 门控代码非死代码**；既有登记 [subsystem-sync.md §9.3 [P2]](archive/audit-2026-08-14/subsystem-sync.md#L1075-L1092)「`atomic_stats` 引用 `println!` — no_std 不支持」⇒ **本批处置＝删除 feature**（按职责判定：非内核所需 + 零履行 + 已有 `*_dump_stats` 等价诊断 idiom，见 B-5.2 / B-5.3 G2），4 项随 feature 消失 |

**B-2 原有（21 项）**

| 文件 | 项（行号） | 待裁点 |
|---|---|---|
| `privileged/arch/shadow_stack.rs` | `set_ssp`(124) `alloc_kernel_shadow_stack`(302) `configure_user_cet_msr`(394) `configure_interrupt_ssp_table`(445) | 去留取决于 CET 子系统路线图 |
| `privileged/arch/x86_64/acpi.rs` | `get_dmar_drhd_list`(878) `get_dmar_host_addr_width`(883) | 取决于 IOMMU/DMAR 路线图 |
| `privileged/freg/recoverable.rs` | `lock_fast`(75) | 免 checkpoint 快速路径，属设计决策 |
| `privileged/mm/numa.rs` | `set_distance`(291) `best_alloc_node`(339) `nearest_free_node`(346) | 取决于 NUMA 子系统路线图 |
| `privileged/cpu/cpuid.rs` | `cpuid_checked`(60) | 叶范围校验变体，保留安全变体 vs 删 |
| `privileged/driver/bus/pci.rs` | `pci_scan`(75) | 与在用 `scan_all_buses` 能力重叠且带日志（批 1 已登记） |
| `functions/fs/sysfs.rs` / `cgroupfs.rs` / `configfs.rs` / `virtiofs.rs` / `systree.rs` | `umount_sysfs`(189) `umount_cgroupfs`(389) `umount_configfs`(361) `umount_virtiofs`(314) `umount_systree`(505) | 占位半成品（恒 `Ok(())` / 常量清零）；删则移除 FS API 面 |
| `functions/fs/devpts.rs` | `umount_devpts`(241) | 误调 `mount_devpts` 的半成品 |
| `functions/fs/process_fd_table.rs` | `new_default`(55) `get_fd`(96) `close_cloexec_fds`(174) `clear_non_cloexec`(186) | Plan B 并行 FD 表整体未采用，删/接线待裁 |

**B-3 由 A-2 退桶并入（8 项；reviewer 第二轮裁定）**

| 文件 | 项 | 待裁点 |
|---|---|---|
| `privileged/arch/shadow_stack.rs` | `is_ssp_valid`(129) | 与同文件 4 项（`set_ssp` 等）**整组统一**，取决于 CET 路线图；安全敏感（CET/SSP） |
| `privileged/mm/numa.rs` | `contains_cpu`(195) `all_nodes`(334) | 与同文件 3 项（`set_distance` 等）**整组统一**，取决于 NUMA 路线图 |
| `privileged/mm/kpti.rs` | `pcid_is_enabled`(149) | 已登记路线图项（`kpti.rs:23-31` PCID/INVPCID 优化）的地基 |
| `privileged/freg/reset/audit.rs` | `count_by_result`(101) | 诊断查询 API 面；② 未取得等价入口 |
| `privileged/mm/page_fault.rs` | `page_fault_count`(497) | 统计 API 面；② 未取得等价封装入口 |
| `privileged/mm/slab.rs` | `utilization`(921) | 统计 API 面；② 仅部分等价（`CacheStats` 可推导） |
| `privileged/arch/x86_64/gdt.rs` | `get_gdt_table`(701) | 注释自述调试用途 ⇒ 非死代码 |

> **同文件分裂已收敛（reviewer 第二轮结构性问题 1）**：`shadow_stack.rs` / `numa.rs` 两处「同文件内部分裂成两桶」已按**路线图整组统一**处理——判删理由（零消费）对同文件「待裁」项同样成立，反证「零消费」不足以作为删的判据。故该 3 项随同文件整组归入待裁，不再单列删候选。

**B-4 由安全面分流并入（2 项；reviewer 第三轮裁定二）**

| 文件 | 项（行号） | 待裁点 |
|---|---|---|
| `functions/fs/ramfs.rs` | `split_path`(524) `validate_path`(544) | **待定型（冗余 / 缺陷 二选一）**：须先由 **T3 安全面**给出「VFS 是否已提供等价（空/长度/NUL）校验」的结论。**结论出来前不进试删队列**。若定型为**冗余** ⇒ 可删，但须登记契约「ramfs 依赖 VFS 前置校验」；若定型为**缺陷** ⇒ 不得删，登记缺陷移 T3 并按 C-1 接线 |

> **本批唯一挂起项**：B-4 是 A-2 之外唯一的"待 T3"项——与 C-1 接线子清单联动（若定型为缺陷，C-1 新增 1 项「ramfs 路径入口补 `validate_path` 调用」；若定型为冗余，则 C-1 不动、本项转为可删候选）。

> **裁定七已解（本轮实读证据链；reviewer 第四轮「先解」要求）**：逐条核对 `validate_path` 的三项检查在**下层**是否已等价提供：
>
> | `validate_path` 检查项 | 下层实测 | 是否等价 |
> |---|---|---|
> | **空** | privileged [`RamFsData::open`](file:///home/anfer/Code/Edgine/src/kernel/privileged/fs/ramfs/ramfs_data.rs#L481-L484)（L482 `if path.is_empty() { return None }`）＋ [`resolve_path`](file:///home/anfer/Code/Edgine/src/kernel/privileged/fs/ramfs/ramfs_data.rs#L354-L365)（去前导 `/`、跳过空段） | ✅ **等价** |
> | **长度**（`VFS_MAX_PATH`） | 路径以 `[u8; VFS_MAX_PATH]`（[vfs/types.rs:16](file:///home/anfer/Code/Edgine/src/kernel/privileged/fs/vfs/types.rs#L16) ＝ 128）承载，转换处 `bytes.len().min(VFS_MAX_PATH - 1)`（[vfs.rs:53](file:///home/anfer/Code/Edgine/src/kernel/privileged/fs/vfs/vfs.rs#L53)/137/546/574）**按构造截断** | ⚠️ **不等价**（截断 vs `NameTooLong` 报错；语义差异） |
> | **NUL** | 路径以 **NUL 结尾语义**承载（`position(\|&b\| b == 0)`，[vfs.rs:553](file:///home/anfer/Code/Edgine/src/kernel/privileged/fs/vfs/vfs.rs#L553)）⇒ 嵌入式 NUL 在边界被截断 | ✅ 等效处理（非显式报错） |
>
> **调用面实测**：functions `SafeRamFs`（`GLOBAL_RAMFS` / `global()`）**在 `src/` 下无生产调用者**（全仓仅 host-tests [td18_fs_kernel_error_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/td18_fs_kernel_error_test.rs) 对该文件做**源文本断言**，非调用）；`split_path` / `validate_path` 自身更是全仓零引用 ⇒ 其形态是「**未被调用的辅助**」，**不是**「被调用却缺失校验」。
>
> **结论**：**非安全缺陷**（活路径的空校验由 privileged 提供；长度由 VFS 缓冲区按构造约束；NUL 由缓冲区语义处理）⇒ 上轮「阻塞一个可能的安全缺陷判定」**已解除**。定型**倾向「冗余」**（附两条保留：① 长度校验**语义不等价**（截断 vs 报错）⇒ 将来若接线 functions 代理入口须补长度校验并登记契约；② 删除属**安全面**，按**裁定六**「必须上报」⇒ **不自主删**，**待 reviewer 授权**）。本项保留在待裁（三态＝安全面待 T3），**三字段见 B-5**。

#### B-5. 待裁 47 归零表（六次修订：裁定二逐项落三态 + 三字段，禁裸待裁）

> **口径**：三态定义与去向按**裁定二**（等路线图 ⇒ 转「未来功能」并注明路线图名 + 登记处；判据待补 ⇒ 补齐后定桶，补不出即转「完整性保留」；安全面待 T3 ⇒ 留待裁并挂 T3 依赖）。**三字段＝等待原因 + 解锁条件 + 责任方**，本节 47 项逐项具备（同组同因者组内共享，组表逐项列出成员，**无裸待裁**）。
>
> **裁定四.3 转移可追**：47 ＝ **13**（等路线图 ⇒ 未来功能）+ **32**（判据待补 ⇒ 完整性保留）+ **2**（安全面待 T3 ⇒ 留待裁）；桶效应见本节末「桶数六次修订」。

**B-5.1 等路线图（13 项 ⇒ 转「未来功能」）**

| 路线图 | 登记处（核实结论） | 项（文件::符号） | 等待原因 | 解锁条件 | 责任方 |
|---|---|---|---|---|---|
| **CET 子系统**（Shadow Stack / IBT / PAC-BTI） | ① `src/kernel/privileged/arch/shadow_stack.rs:26-31`——文件头「当前实现状态」自述未完成面（IBT 仅定义未启用、PAC/BTI 仅定义未启用）；② [audit-fix-09](archive/audit-fix-09-hard-rules-deadcode.md) B09-10「16 处转 plan」含 shadow_stack PMM 物理页（TRACK-4C9A12）+ CR4 #GP 检测（TRACK-6E7C34）；③ [unresolved-issues-2026-08-09.md](unresolved-issues-2026-08-09.md) ISSUE-SRC-001 / ISSUE-SRC-018 | `arch/shadow_stack.rs::set_ssp` / `alloc_kernel_shadow_stack` / `configure_user_cet_msr` / `configure_interrupt_ssp_table` / `is_ssp_valid`（**5 项整组统一**） | CET 硬件面未完成：影子栈物理页分配、#GP 安全检测、中断 SSP 表 IDT 集成都未落地（函数族存在但无消费链路） | CET 路线图排期（ISSUE-SRC-001 + ISSUE-SRC-018 实装并接入 `cet_init`/IDT） | 用户（路线图排期） |
| **NUMA 子系统** | ① `src/kernel/privileged/mm/numa.rs:1-10`——DECISION-J 归属反转记录（`NumaMempolicy` 被 privileged proc/process 持有、`numa_init` 被 privileged mm 调用 ⇒ 机制已实装）；② [archive/subsystem-bootstrap-sequence-2026-06.md](archive/subsystem-bootstrap-sequence-2026-06.md) D3（NumaNode/NumaTopology + 距离矩阵，**已归档快照**）；③ 本台账 T1 G4 记录（`mbind` / `set_mempolicy` 已接线 + `NumaPolicy::from_linux_mode`） | `mm/numa.rs::set_distance` / `best_alloc_node` / `nearest_free_node` / `contains_cpu` / `all_nodes`（**5 项整组统一**） | 拓扑/距离矩阵与跨节点分配策略**未接入 PMM 分配路径**（策略面已实装，消费面缺） | NUMA 分配策略集成排期（PMM 按节点分配 + SLIT 距离矩阵消费） | 用户（路线图排期） |
| **PCID / INVPCID 优化** | ① `src/kernel/privileged/mm/kpti.rs:23-31`——文件头「未完成」清单明载「**PCID/INVPCID 优化**：当前每次切换 CR3 都 TLB 全清，高频 syscall 性能损失 5-15%」；② [kpti-complete-project.md](kpti-complete-project.md)（活跃 KPTI 完整化工程 Phase 0-3） | `mm/kpti.rs::pcid_is_enabled`（1） | KPTI 完整化工程 Phase 2/3 未启动（PCID 化须待 `.text` 收窄与 USER_PML4 高半区复制移除后实施） | KPTI 完整化工程 Phase 2 排期（KPTI-07/KPTI-08） | 用户（工程排期） |
| **IOMMU / VT-d（DMAR 消费）** | ① `AGENTS.md` §4.2 **I6**「外设 DMA 不可写入内核内存」＝全项目硬不变式（未落地即为欠账）；② [archive/framekernel-compliance.md](archive/framekernel-compliance.md) E8「IOMMU 不变式强制」；③ `src/kernel/privileged/arch/x86_64/acpi.rs:783-884`——DMAR 表**已解析**入 `DMAR_DRHD_LIST` / `DMAR_HOST_ADDR_WIDTH`，**无任何消费方** | `arch/x86_64/acpi.rs::get_dmar_drhd_list` / `get_dmar_host_addr_width`（2） | DMAR 已解析但 IOMMU 重映射未实装 ⇒ DRHD 数据无消费链路（I6 强制未落地） | IOMMU/VT-d 集成排期（DRHD 消费 + DMA 重映射域，落地 I6） | 用户（路线图排期） |

> **台账事实订正（裁定六授权范围内；本项核实结论）**：`kpti.rs:23` 所引「在 `engineering-progress.md` §五 + roadmap Backlog 登记」**已失效**——`docs/plan/engineering-progress.md` **不存在**（全仓仅历史文档引用该文件名，`src/` 内唯一命中即 `kpti.rs:23` 自身），`kernel-roadmap.md` 亦已归档至 `archive/2026-07-08-kernel-roadmap.md` 且**无 PCID 条目**。故本项「登记处」**改以 `kpti.rs:23-31` 现状清单 + 活跃的 [kpti-complete-project.md](kpti-complete-project.md) 为准**，路线图成立性不受影响。另：按**裁定六**，`pcid_is_enabled` 属「已登记路线图的地基」⇒ **不得删**（本表已剔除出任何删候选路径）。

**B-5.2 判据待补（32 项 ⇒ 补齐后定桶「完整性保留」）**

| 定型组 | 项（文件::符号） | 补齐判据（② 等价入口 / ③ 面属性 / 族） | 定桶 |
|---|---|---|---|
| **G1 aarch64 门控诊断**（2） | `arch/aarch64/mmu.rs::diagnose_permission`、`mm/vmm_aarch64.rs::diagnose_descriptor` | ② **不成立**——aarch64 专属诊断输出（非原语），x86_64 维下 `mm/mod.rs:51-53` 整模块门控，**「零引用」为构造性结果**；③ aarch64 架构门控诊断面；族（aarch64 诊断/内省族）完整 | 完整性保留（aarch64 门控诊断面） |
| **G2 feature 门控**（4 ⇒ **0**） | `sync/atomic.rs::record_inc` / `record_dec` / `record_cmpxchg_success` / `record_cmpxchg_fail` | ② **不适用**（feature 门控代码非死代码）；③ feature 面。**本批已按职责判定删除 feature**（非内核所需 + 零履行 + 已有 `*_dump_stats` 等价诊断 idiom）⇒ 4 项随 feature 消失 | **已删（feature 去留裁定：删），4 项归零** |
| **G3 机制原语 / 安全加固 / TCB**（3） | `freg/recoverable.rs::lock_fast`、`cpu/cpuid.rs::cpuid_checked`、`arch/x86_64/gdt.rs::get_gdt_table` | ② **不成立**——`lock_fast` 为免 checkpoint 快速路径（与常规 `lock` 路径能力不同）；`cpuid_checked` 为**叶范围校验安全变体**（裸 `cpuid` 无校验，非等价）；`get_gdt_table` 注释自述 `/// 获取 GDT 表的引用 (调试用途)`（退桶时已引原文核对）；③ 机制原语 / 安全加固 / **TCB 核心**（GDT，裁定六）面 | 完整性保留 |
| **G4 驱动 / 统计 / 诊断 API 面**（4） | `driver/bus/pci.rs::pci_scan`、`freg/reset/audit.rs::count_by_result`、`mm/page_fault.rs::page_fault_count`、`mm/slab.rs::utilization` | ② **不成立（能力重叠但非等价）**——`pci_scan`（`driver/bus/pci.rs:75-90`）唯一实现体调用在用 `pci::scan_all_buses`（`pci/mod.rs:518`，3 处在用），**差异能力＝逐设备 `klog_info!` 输出**（与 A-6 `format_duration` 的「`core::fmt` 非等价」同款判据）；`count_by_result` 与同文件 `count_by_layer` 为**不同维度**；`page_fault_count` 的 `PAGE_FAULT_COUNT` 为 `pub static`（并行读法非等价封装）；`utilization` 需调用方自行除法（部分等价）；③ 驱动 / 统计 / 诊断 API 面 | 完整性保留 |
| **G5 FS mount-unmount 面 + Plan B FD 表**（9） | `functions/fs/{sysfs,cgroupfs,configfs,virtiofs,systree}.rs::umount_*`（5）、`functions/fs/devpts.rs::umount_devpts`、`functions/fs/process_fd_table.rs::get_fd` / `close_cloexec_fds` / `clear_non_cloexec`（3） | ② **不成立**——各 FS 的 unmount 面唯一（无等价入口），与 C-2 `mount_*` 5 项为同批「待 VFS mount 集成」；`umount_devpts`（`devpts.rs:237-243`）实现体**误调 `mount_devpts`**、注释自述「当前实现恒返回 `Ok(())`」⇒ **已知缺陷形态（缺陷档）**，但**无调用链且非安全面** ⇒ 不删；`process_fd_table.rs` 3 项属**未采用的 Plan B 并行 FD 表**（`functions/fs/mod.rs:43` 模块已注册）；③ FS / 进程 API 面 | 完整性保留 |
| **G6 族残缺**（6） | `arch/x86_64/gdt.rs::tss_64bit`、`fs/vfs/handle.rs::vfs_close_safe` / `vfs_seek_safe` / `vfs_readdir_safe`、`arch/x86_64/acpi.rs::get_ap`、`fs/vfs/vfs.rs::get_fs_name` | ② **不成立**；**族残缺**（实测引用计数：`Granularity` 构造器族 `code_64bit` 4 / `data_32bit` 4 / `tss_64bit` 0；`vfs_*_safe` 全族 17 员 / 14 员在用；AP 访问器族 `get_ap_list` 1 / `get_ap_count` 3 / `has_madt` 6 / `parse_madt` 5 在用；VFS 元信息访问器族 `get_fs_type` 2 / `get_fs` 13 / `set_fs` 1 在用）⇒ 删则**族残缺且不对称**、诱导调用方绕过类型包装 | 完整性保留（**B-0 三档规定：族残缺 ⇒ 并入 A-1 完整性保留**） |
| **G7 无等价入口（计数 / IPC 查询）**（3） | `freg/domain.rs::consume_quota_tick` / `is_quota_exceeded`、`ipc/dynamic.rs::pipe_exists` | ② **不成立**（实测订正：`check_quota` 自身零引用且已列 B-2 ⇒ 原「与在用 `check_quota` 重复」不成立；全仓**无 `get_pipe` 符号**、`pipe_count()` 不判定指定 `IpcId` 是否存在）；③ 计数 / IPC 查询 API 面 | 完整性保留 |
| **G8 无等价格式化入口**（1） | `timer/tick.rs::format_duration` | ② **不成立**（全仓无等价时长格式化公共入口；`core::fmt` 为通用设施，**不能产出 `1h23m45s678ms`** ⇒ 非能力等价）；族（`tick.rs` 的 tick↔单位换算族）**外**成员；③ `#[cfg(feature = "alloc")]` feature 面 | 完整性保留 |

**B-5.2 三字段（组内同因同责，逐组登记）**

| 定型组 | 等待原因 | 解锁条件 | 责任方 |
|---|---|---|---|
| G1 aarch64 门控诊断 | x86_64 维零引用为**构造性结果**，不可作为删据；且未取得等价入口证据 | aarch64 专项复核（若裁定 aarch64 诊断面无保留价值 ⇒ **新增删候选，按裁定六须授权**） | 用户（aarch64 面裁定） |
| G2 feature 门控 | `atomic_stats` feature **已裁定**——既有登记 [subsystem-sync.md §9.3 [P2]](archive/audit-2026-08-14/subsystem-sync.md#L1075-L1092)「`atomic_stats` 引用 `println!` — no_std 不支持」；本批经**职责四维核验**（是否内核所需 / 履行度 / 是否已有等价承担者 / 保留代价）判定＝**删除** | ✅ **已解锁（本批删除）**——`mod stats` 私有且 `atomic.rs` 内无调用点（即便启用 feature 亦触发 `dead_code`）＋ `dump_stats` 引用 no_std 不可用的 `println!` 本就无法编译 ⇒ 4 项随 feature 消失 | 用户（feature 决策，**已决**） |
| G3 机制 / 安全 / TCB | 属机制设计决策（`lock_fast` 优化路径）与安全加固变体（`cpuid_checked` 叶范围校验）；`get_gdt_table` 属 **TCB 核心**（GDT）⇒ 裁定六 | 若 reviewer 判非保留 ⇒ 须**先经安全面 / TCB 复核**并授权（不得走试删兜底，见 ⑦ 原理性盲区） | 用户 |
| G4 驱动 / 统计 / 诊断面 | ② 仅「能力重叠」而非「能力等价」（差异能力＝日志/维度/封装层级），零引用不构成删据 | reviewer 复核 `pci_scan` 是否属冗余档（**若判冗余 ⇒ 新增删候选，须授权**）；其余 3 项保留无附加依赖 | 用户 |
| G5 FS 面 + Plan B FD 表 | unmount 面待 **VFS mount 集成**（与 C-2 `mount_*` 同批）；`umount_devpts` 的误调缺陷待该集成统一修正；Plan B FD 表属**「消除并行实现」议程**（项目规则：内核内并行实现须归一到唯一权威实现），**非 T5 甄别范畴** | VFS mount/unmount 集成排期（`umount_devpts` 修正随该集成）；Plan B FD 表归属由 [eliminate-parallel-implementations.md](eliminate-parallel-implementations.md) 工程裁定 | 用户（VFS 集成排期 + 并行实现议程） |
| G6 族残缺 | 族残缺（删则不对称且诱导绕过类型/构造器族） | reviewer 复核 `vfs_*_safe` 3 项**档位**——实测 `functions/fs/dir_ops.rs:14-28` 已直接调用裸 `extern "C"` `vfs_seek`/`vfs_readdir`（不经 `_safe` 壳）⇒ 该族**本就不是封装边界**，档位**弱于** `tss_64bit`（最终档位由 reviewer 定） | 用户（档位复核） |
| G7 无等价入口 | 三合一判据第 ② 条**落空**（无能力等价公共入口）⇒ 结构上**不构成删候选**；零引用亦未证明属「冗余」 | 若后续接入调用链则为**接线项**，归「功能接线批次」（与 T3 联动），不再回 T5 | 用户（接线批次排期） |
| G8 无等价格式化入口 | 同上（② 落空）；另 `#[cfg(feature = "alloc")]` 门控须经 feature 维复核 | reviewer 若认定 `core::fmt` 等价成立 ⇒ 须先复核 feature 维并授权（**新增删候选须上报**） | 用户 |

**B-5.3 安全面待 T3（2 项 ⇒ 留待裁，挂 T3 依赖并标注）**

| 项（文件::符号） | 三态 | 定桶 | 等待原因 | 解锁条件 | 责任方 |
|---|---|---|---|---|---|
| `functions/fs/ramfs.rs::split_path`(524) / `validate_path`(544) | **安全面待 T3** | **倾向「冗余」**（附两条保留：① 长度校验**语义不等价**——VFS 侧为缓冲区按构造截断 vs `NameTooLong` 报错 ⇒ 将来若接线 functions 代理入口须补长度校验并**登记契约**；② 删除属**安全面**） | **裁定七已解**（commit `3173b5b9` 证据链）：空校验由 privileged `RamFsData::open`（`ramfs_data.rs:481-484` `path.is_empty()`）等价提供 / 长度由 VFS `[u8; VFS_MAX_PATH]`（`vfs/types.rs:16` ＝ 128）转换处 `min(VFS_MAX_PATH-1)` 构造截断 / NUL 由缓冲区 NUL 结尾语义（`vfs.rs:553`）处理；`SafeRamFs`/`GLOBAL_RAMFS` 在 `src/` **无生产调用者** ⇒ **非安全缺陷**。**剩余唯一阻塞＝删除涉及安全面（路径校验），按裁定六必须上报** | **reviewer 授权**（授权后走删候选流程：试删 → 五门槛全量 → 独立提交可回退） | 用户（reviewer 授权） |

> **裁定四.3 桶数六次修订（本轮，B-5 归零后定型）**

| 桶 | 项数 | 是否在 T5 内 | 本轮变化与追溯 |
|---|---|---|---|
| 删候选 | **0** | ✅ 是 | 无变化（A-2 已清零，裁定四.1 达成） |
| 完整性保留（原「硬件原语完整性保留」） | **75** | ✅ 是（**仅登记**，不施工） | 43（A-1：41 硬件原语 + 2 族残缺）+ **32**（B-5.2 判据待补定桶，G1-G8 明细见上） |
| 待裁 | **2** | ✅ 是 | 47 − **13**（B-5.1 等路线图 ⇒ 未来功能）− **32**（B-5.2 ⇒ 完整性保留）= **2**（B-5.3 ramfs，**三字段齐备，无裸待裁**） |
| **T5 内合计** | **77** | — | 0 + 75 + 2 ＝ 436 − 0（**甲批 C-1 后接线清零**）− 359（未来功能，352 + 甲批退桶 7）；六次修订前为 90 ＝ 437 − 8 − 352 |
| — 接线 | **0** | ❌ 否（**甲批 C-1 处置完毕**，见 B-8） | 8 → **0**（1 接线落地 `set_fd` + 7 判据不成立退未来功能） |
| — 未来功能（DECISION-052 第三层） | **359** | ❌ 否（移出） | 352 + **7**（甲批 C-1 判据不成立退桶，B-8.2） |
| 合计 | **436** | — | ＝ 0 + 75 + 2 + 0 + 359（**算术闭合**；438 − 1（`write_log_line` 已删除）− 1（`set_fd` 已接线 ⇒ 不再零引用）） |

> **B-5 结论**：**待裁 47 已全部归零为三态**（13 等路线图 / 32 判据待补已定桶 / 2 安全面待 T3）⇒ **裁定四.2「无裸待裁」达成**、**裁定四.3「桶数算术闭合 + 转移可逐项追」达成**。**剩余阻塞（不改判据、只待授权 / 排期）**：① ramfs 2 项**待 reviewer 授权**方可进删候选（裁定六安全面）；② 潜在新增删候选（`pci_scan` / aarch64 诊断 2 项 / `format_duration` 若 `core::fmt` 判等价）**已按裁定六上报，未自主处置**；③ `umount_devpts` 误调 `mount_devpts` 缺陷已登记（G5），修正随 VFS mount 集成。
>
> **本批变更（`atomic_stats` feature 删除）**：B-5.2 **G2** 4 项 `record_inc` / `record_dec` / `record_cmpxchg_success` / `record_cmpxchg_fail` 随 `atomic_stats` feature 删除而消失（处置依据＝按职责判定「非内核所需 + 零履行 + 已有 `*_dump_stats` 等价诊断 idiom」，详见 **B-5.2 / B-5.3 G2**）⇒ 上表 **完整性保留 75 → 71**（B-5.2 判据待补 32 → 28）、**T5 内合计 77 → 73**、**合计 436 → 432**；审计侧 **B-6 区块 466 → 462**（见 B-6 维护说明），脚本 `INFO` 相应降 4。

#### B-6. R1 已分类清单（机器可读区块；B09-21 数据源）

> 口径（裁定五）：下列 `<repo-relative path>::<pub fn 名>` 为**已分类**的零引用 pub fn 全集（**440 项**）。[audit_unwired_pub_fn.py](../../scripts/audit_unwired_pub_fn.py) 读本区块，**仅对未分类的零引用 pub fn 报 HIGH**；**fail-closed**＝区块缺失 / 解析失败 ⇒ **视同未分类（仍报）**；**只降噪不豁免**＝**不改变「零引用」这一事实判定**，仅将其报告分级降为 INFO。
>
> 维护：清单随台账桶数修订同步（新增 / 删除零引用 pub fn 时更新本区块）。**最近一次同步＝MIG-004 安全代理批次**（本轮 R1 复核检出 **6 项既有条目失效**并逐项移除：4 项因本工程而失效 —— 3 项 `privileged/egdf/user_driver.rs::{egdf_forward_irq, devtree_map_user_device, devtree_unmap_user_device}` 因 functions `egdf::user_driver` 强类型封装接线后不再零引用、1 项 `functions/egdf/mod.rs::find_net_device` 因函数下沉至 `functions/egdf/proto.rs` 并被 host-tests 调用；另 2 项**预存失效** `privileged/arch/aarch64/gic.rs::{configure_spi_level, enable_spi}`（实为同文件私有 `configure_and_enable_device_spi` 内部调用 ⇒ refs=2）经用户裁决本轮一并清理 ⇒ **446 → 440**；同步改写 `functions/egdf/proto.rs` 头注释、去除与 `char_read`/`char_write` 同名的字面量，使该两项恢复「真零引用」并被本区块正确命中；修复后实测区块 440 ＝ matched 440、stale 0、HIGH 18 不变）。**上一轮同步＝B-6 区块路径漂移清理（**B-13.7**）**（R1 实测检出 43 项**失效条目**并逐项处置：31 项路径更新 `privileged/...`→`functions/...`（文件随框内核迁移下沉、函数仍零引用 ⇒ 原 privileged 路径失效、现报 HIGH），12 项移除（已接线 / 已删 / 已豁免：`privileged/driver/net/e1000_io.rs::install_rings`（已删）、`::set_ctrl` / `::set_ipg` / `::set_rx_ctl` / `::set_tx_ctl`（已被 `functions/driver/net/e1000.rs` 调用）、`privileged/driver/usb/xhci.rs::init_command_ring` / `::recover_endpoint`（已删）、`privileged/timer/tickless.rs::enter_tickless` / `::exit_tickless`（内联测试引用）、`functions/egdf/mod.rs::find_by_proto`（host 测试引用）、`functions/driver/usb/xhci.rs::port_connected` / `::port_status`（内部调用））⇒ **458 → 446**；修复后实测区块 446 ＝ matched 446、stale 0、HIGH 18）。**上一轮同步＝B-12.8 遗留清理批次（**B-13**）**（本批改动**未改变** R1 零引用集合，以 HEAD 独立 worktree 对跑逐项一致 ⇒ 本区块计数**不变（458）**，见 **B-13.6**）。**上一轮同步＝热插拔运行时接线批次（**B-12**）**（4 项因本工程「接线 / 删除」而不再零引用 ⇒ 本区块移除：`privileged/driver/hotplug.rs::hotplug_poll`（函数已不存在，由 `hotplug_wakeup` + softirq 轮询替代）、`functions/driver/usb/xhci.rs::ack_port_change` / `::has_port_change`（已被 `usb_port_poll` 调用）、`privileged/driver/block.rs::mark_removed`（函数已不存在，由 EGDF 墓碑协议 `egdf_blk_is_removed` 替代）⇒ **462 → 458**）。**上一轮同步＝`atomic_stats` feature 删除**（`privileged/sync/atomic.rs` 4 项 `record_*` 随 feature 删除而消失 ⇒ 本区块移除 4 条目，**466 → 462**；处置依据＝按职责判定「非内核所需 + 零履行 + 已有 `*_dump_stats` 等价诊断 idiom」，见 **B-5.2 / B-5.3 G2**）。**更早同步＝per-process fd 表全量下沉（**B-11**）**（10 项因本工程接线 / 删除而不再零引用 ⇒ 本区块移除：`privileged/fs/vfs/handle.rs::vfs_get_fd_handle`（poll 改源）、`privileged/proc/fd_table.rs` 4 项（`get_handle_id` / `is_cloexec` / `set_cloexec` / `get_cloexec_fds` 接线）、`privileged/proc/scheduler.rs::get_current_process`（`with_current_fd_table` 取当前进程）、`functions/fs/process_fd_table.rs` 4 项（Plan B 表文件删除）⇒ **476 → 466**；编辑前实测区块为 **476** 行，区块说明原记 475（差 1，前批计数笔误，本次以实测为准）。另有 2 条**既有偏差**（`privileged/timer/tickless.rs::enter_tickless` / `::exit_tickless`：脚本按名计数，其内联测试即计入引用 ⇒ 恒不在 INFO 集，见上述口径说明②）经本批复核确认，**非本工程引入，不属本批处置面**）。**更早同步＝项 5 批 C「67 项逐项分流」**（21 项判定为可删并已删除 ⇒ 本区块移除对应 21 条目，496 → 475；其余 46 项保留 `pub` 并留块登记，见 **B-10.11**；上一轮为项 2「`kmalloc_slab.rs` 孤岛删除」，被删 2 项原为 HIGH / **未入块** ⇒ 计数不变，见 **B-10.10**；更早为项 5 批 B「豁免面收窄」新增 67 项，429 → 496，见 **B-10.9**）。
>
> **口径说明（2026-09-26 订正）**：本区块与 `audit_unwired_pub_fn.py` 的「零引用」判定均为**按名计数**（`rg -c -w`），因而存在两类已知偏差，本区块**不承诺**与脚本输出逐项等同：① **同名遮蔽**（文档注释 / 局部变量出现同名字符串即计入引用 ⇒ 真零引用项可能**漏报**，实例见 **B-10.3** 的 `slab_init` / `functions/driver/acpi.rs::lapic_base`）；② **已失效条目**（被接线或删除后不再零引用，需人工同步移除，本期移除 6 项见 B-10.4）。

<!-- audit-classified-begin -->
src/kernel/privileged/arch/aarch64/gic.rs::configure_spi_edge
src/kernel/privileged/arch/aarch64/gic.rs::disable_spi
src/kernel/privileged/arch/aarch64/gic.rs::is_ppi
src/kernel/privileged/arch/aarch64/gic.rs::is_spi_pending
src/kernel/privileged/arch/aarch64/gic.rs::is_valid_irq
src/kernel/privileged/arch/aarch64/gic.rs::set_spi_pending
src/kernel/privileged/arch/aarch64/mmu.rs::alloc_user_page_table
src/kernel/privileged/arch/aarch64/mmu.rs::allows_el0_access
src/kernel/privileged/arch/aarch64/mmu.rs::diagnose_permission
src/kernel/privileged/arch/aarch64/mmu.rs::make_kernel_rw_entry
src/kernel/privileged/arch/aarch64/mmu.rs::make_user_ro_entry
src/kernel/privileged/arch/aarch64/mmu.rs::make_user_rw_entry
src/kernel/privileged/arch/aarch64/mmu.rs::tlbi_vaae1
src/kernel/privileged/arch/aarch64/mmu.rs::tlbi_vmalle1
src/kernel/privileged/arch/aarch64/mmu.rs::write_ttbr0
src/kernel/privileged/arch/aarch64/psci.rs::system_off
src/kernel/privileged/arch/aarch64/psci.rs::system_reset
src/kernel/privileged/arch/aarch64/timer.rs::read_control
src/kernel/privileged/arch/aarch64/timer.rs::set_compare
src/kernel/privileged/arch/aarch64/timer.rs::set_timeout_ms
src/kernel/privileged/arch/shadow_stack.rs::alloc_kernel_shadow_stack
src/kernel/privileged/arch/shadow_stack.rs::configure_interrupt_ssp_table
src/kernel/privileged/arch/shadow_stack.rs::configure_user_cet_msr
src/kernel/privileged/arch/shadow_stack.rs::is_ssp_valid
src/kernel/privileged/arch/shadow_stack.rs::set_ssp
src/kernel/privileged/arch/x86_64/acpi.rs::get_ap
src/kernel/privileged/arch/x86_64/acpi.rs::get_dmar_drhd_list
src/kernel/privileged/arch/x86_64/acpi.rs::get_dmar_host_addr_width
src/kernel/privileged/arch/x86_64/apic.rs::apic_is_in_irr
src/kernel/privileged/arch/x86_64/apic.rs::apic_is_in_isr
src/kernel/privileged/arch/x86_64/apic.rs::apic_is_level_triggered
src/kernel/privileged/arch/x86_64/apic.rs::apic_read_irr
src/kernel/privileged/arch/x86_64/apic.rs::apic_read_isr
src/kernel/privileged/arch/x86_64/apic.rs::apic_read_tmr
src/kernel/privileged/arch/x86_64/apic.rs::broadcast_ipi_level
src/kernel/privileged/arch/x86_64/apic.rs::configure_lint0
src/kernel/privileged/arch/x86_64/apic.rs::configure_lint1
src/kernel/privileged/arch/x86_64/apic.rs::get_timer_count
src/kernel/privileged/arch/x86_64/apic.rs::get_version
src/kernel/privileged/arch/x86_64/apic.rs::icr_broadcast
src/kernel/privileged/arch/x86_64/apic.rs::icr_level
src/kernel/privileged/arch/x86_64/apic.rs::is_timer_calibrated
src/kernel/privileged/arch/x86_64/apic.rs::mask_lint0
src/kernel/privileged/arch/x86_64/apic.rs::mask_lint1
src/kernel/privileged/arch/x86_64/apic.rs::send_ipi_level
src/kernel/privileged/arch/x86_64/apic.rs::unmask_lint0
src/kernel/privileged/arch/x86_64/apic.rs::unmask_lint1
src/kernel/privileged/arch/x86_64/gdt.rs::get_gdt_table
src/kernel/privileged/arch/x86_64/gdt.rs::tss_64bit
src/kernel/privileged/arch/x86_64/ioapic.rs::delivery_init
src/kernel/privileged/arch/x86_64/ioapic.rs::delivery_lowest
src/kernel/privileged/arch/x86_64/ioapic.rs::get_arbitration_id
src/kernel/privileged/arch/x86_64/ioapic.rs::get_max_irq
src/kernel/privileged/arch/x86_64/ioapic.rs::set_id
src/kernel/privileged/arch/x86_64/ioapic.rs::set_irq_level
src/kernel/privileged/freg/domain.rs::check_proc_limit
src/kernel/privileged/freg/domain.rs::check_quota
src/kernel/privileged/freg/domain.rs::consume_quota_tick
src/kernel/privileged/freg/domain.rs::is_quota_exceeded
src/kernel/privileged/freg/recoverable.rs::lock_fast
src/kernel/privileged/freg/recovery.rs::cascade_recover
src/kernel/privileged/freg/recovery.rs::hard_reset_domain
src/kernel/privileged/freg/recovery.rs::recovery_registry_init
src/kernel/privileged/freg/recovery.rs::recovery_subdomain_save_checkpoint
src/kernel/privileged/freg/reset/audit.rs::count_by_result
src/kernel/privileged/freg/snapshot.rs::test_registry_priority_order
src/kernel/privileged/freg/snapshot.rs::test_registry_register
src/kernel/privileged/freg/snapshot.rs::test_snapshot_basic
src/kernel/privileged/egdf/composite.rs::compatible_str
src/kernel/privileged/egdf/devtree.rs::as_bool
src/kernel/privileged/egdf/mod.rs::egdf_with_device_map
src/kernel/privileged/egdf/mod.rs::driver_as_mut
src/kernel/privileged/console/gfx_console.rs::set_colors
src/kernel/privileged/console/gfx_console.rs::set_margin
src/kernel/privileged/cpu/cpuid.rs::cpuid_checked
src/kernel/privileged/cpu/feature.rs::is_amd_style
src/kernel/privileged/cpu/feature.rs::is_intel_style
src/kernel/privileged/cpu/feature.rs::supports_64bit
src/kernel/privileged/cpu/feature.rs::supports_avx
src/kernel/privileged/cpu/feature.rs::supports_simd
src/kernel/privileged/cpu/feature.rs::supports_virtualization
src/kernel/privileged/cpu/tsc.rs::nanoseconds_to_cycles
src/kernel/privileged/sgeg/api.rs::umask_get
src/kernel/privileged/sgeg/audit.rs::get_entries
src/kernel/privileged/sgeg/identity.rs::find_mut
src/kernel/privileged/sgeg/secure_boot.rs::add_trust_entry
src/kernel/privileged/sgeg/types.rs::get_creator_pwm
src/kernel/privileged/sgeg/types.rs::to_uid
src/kernel/privileged/debug/api.rs::kgdb_break_now
src/kernel/privileged/debug/ebpf.rs::get_map
src/kernel/privileged/debug/ebpf.rs::get_prog
src/kernel/privileged/debug/ebpf.rs::prog_run
src/kernel/privileged/dma/engine.rs::sg_add_entry
src/kernel/privileged/dma/engine.rs::sg_init
src/kernel/privileged/dma/engine.rs::sg_total_length
src/kernel/privileged/dma/engine.rs::submit_transfer_async
src/kernel/privileged/dma/engine.rs::sync_both
src/kernel/privileged/dma/engine.rs::unmap_single
src/kernel/privileged/driver/bus/pci.rs::pci_scan
src/kernel/privileged/driver/display/framebuffer.rs::intersection
src/kernel/privileged/driver/infra.rs::inw
src/kernel/privileged/driver/infra.rs::outw
src/kernel/privileged/driver/input/keyboard.rs::get_modifiers
src/kernel/privileged/driver/mod.rs::list_devices
src/kernel/privileged/driver/power.rs::latency_us
src/kernel/privileged/driver/power.rs::ondemand_check
src/kernel/privileged/driver/power.rs::pm_is_initialized
src/kernel/privileged/driver/power.rs::pm_subsystem
src/kernel/privileged/driver/power.rs::power_saving
src/kernel/privileged/driver/power.rs::register_notifier
src/kernel/privileged/driver/storage/mod.rs::xhci_read_trb
src/kernel/privileged/driver/uefi.rs::get_memory_map
src/kernel/privileged/driver/uefi.rs::set_gop_mode
src/kernel/privileged/driver/uefi.rs::set_memory_map
src/kernel/privileged/driver/uefi.rs::variable_count
src/kernel/privileged/driver/virtio/mod.rs::read_config64
src/kernel/privileged/driver/virtio/mod.rs::set_status
src/kernel/privileged/frame.rs::set_meta
src/kernel/privileged/idt/handlers.rs::category_count
src/kernel/privileged/idt/idt.rs::set_exception_handler
src/kernel/privileged/idt/idt.rs::spurious_irq_count
src/kernel/privileged/idt/safety.rs::memory_fence
src/kernel/privileged/idt/safety.rs::rdtsc_fence
src/kernel/privileged/idt/safety.rs::save_frame_pointer
src/kernel/privileged/idt/safety.rs::store_fence
src/kernel/privileged/idt/statistics.rs::get_recent_events
src/kernel/privileged/idt/types.rs::dump_registers
src/kernel/privileged/idt/types.rs::error_code_flags
src/kernel/privileged/idt/types.rs::set_handler
src/kernel/privileged/io/iouring.rs::io_uring_destroy
src/kernel/privileged/io/iouring.rs::io_uring_reap
src/kernel/privileged/ipc/dynamic.rs::pipe_exists
src/kernel/privileged/irq/mod.rs::register_tasklet
src/kernel/privileged/irq/mod.rs::schedule_tasklet
src/kernel/privileged/irqline.rs::is_registered
src/kernel/privileged/klog/mod.rs::klog_get_level
src/kernel/privileged/klog/mod.rs::klog_set_level
src/kernel/privileged/klog/mod.rs::log_crit
src/kernel/privileged/klog/mod.rs::log_debug
src/kernel/privileged/klog/mod.rs::log_warn
src/kernel/privileged/mm/kpti.rs::invpcid_flush_single
src/kernel/privileged/mm/kpti.rs::kpti_kernel_pml4
src/kernel/privileged/mm/kpti.rs::kpti_user_pml4_or_kernel
src/kernel/privileged/mm/kpti.rs::pcid_is_enabled
src/kernel/privileged/mm/numa.rs::all_nodes
src/kernel/privileged/mm/numa.rs::best_alloc_node
src/kernel/privileged/mm/numa.rs::contains_cpu
src/kernel/privileged/mm/numa.rs::nearest_free_node
src/kernel/privileged/mm/numa.rs::set_distance
src/kernel/privileged/mm/page_fault.rs::page_fault_count
src/kernel/privileged/mm/slab.rs::utilization
src/kernel/privileged/mm/swap.rs::swap_deinit
src/kernel/privileged/mm/swap.rs::swap_free
src/kernel/privileged/mm/vma.rs::with_offset
src/kernel/privileged/mm/vmm_aarch64.rs::diagnose_descriptor
src/kernel/privileged/mm/vmm_aarch64.rs::is_desc_block
src/kernel/privileged/mm/vmm_aarch64.rs::is_desc_device_memory
src/kernel/privileged/mm/vmm_aarch64.rs::is_desc_non_cacheable
src/kernel/privileged/mm/vmm_aarch64.rs::is_desc_page
src/kernel/privileged/mm/vmm_aarch64.rs::is_desc_table
src/kernel/privileged/net/api.rs::init_network_now
src/kernel/privileged/net/api.rs::status_snapshot
src/kernel/privileged/net/netfilter.rs::hook_count
src/kernel/privileged/net/netfilter.rs::list_rules
src/kernel/privileged/net/route.rs::default_route
src/kernel/privileged/net/route.rs::route_list
src/kernel/privileged/page_table.rs::verify_kernel_code_protection
src/kernel/privileged/pci/api.rs::register_scanner
src/kernel/privileged/pci/mod.rs::find_by_vendor
src/kernel/privileged/pci/mod.rs::find_device
src/kernel/privileged/pci/mod.rs::get_device_list
src/kernel/privileged/pci/mod.rs::get_ecam_base
src/kernel/privileged/pci/mod.rs::set_ecam_base
src/kernel/privileged/pci/msi.rs::msi_disable
src/kernel/privileged/pci/msi.rs::msi_enable
src/kernel/privileged/pci/msi.rs::msix_disable
src/kernel/privileged/pci/msi.rs::msix_mask_vector
src/kernel/privileged/pci/msi.rs::msix_unmask_vector
src/kernel/privileged/proc/canary.rs::set_per_proc_seed
src/kernel/privileged/proc/cfs.rs::get_load
src/kernel/privileged/proc/cfs.rs::get_weighted_load
src/kernel/privileged/proc/cfs.rs::steal_highest_vruntime
src/kernel/privileged/proc/cgroup.rs::account_read
src/kernel/privileged/proc/cgroup.rs::account_write
src/kernel/privileged/proc/cgroup.rs::cgroup_of
src/kernel/privileged/proc/cgroup.rs::check_budget
src/kernel/privileged/proc/cgroup.rs::is_over_limit
src/kernel/privileged/proc/cgroup.rs::period_reset
src/kernel/privileged/proc/cgroup.rs::try_charge
src/kernel/privileged/proc/cgroup.rs::uncharge
src/kernel/privileged/proc/cpu_queue.rs::register_sched_softirq
src/kernel/privileged/proc/namespace.rs::map_gid
src/kernel/privileged/proc/namespace.rs::map_uid
src/kernel/privileged/proc/namespace.rs::to_clone_flag
src/kernel/privileged/proc/posix_timer.rs::posix_timer_release_pid
src/kernel/privileged/proc/process.rs::allocate_user_space
src/kernel/privileged/proc/process.rs::kernel_stack_check_canary
src/kernel/privileged/proc/rlimit.rs::check_as_exceeded
src/kernel/privileged/proc/rlimit.rs::check_nofile_exceeded
src/kernel/privileged/proc/rlimit.rs::check_nproc_exceeded
src/kernel/privileged/proc/rlimit.rs::get_nofile_limit
src/kernel/privileged/proc/rlimit.rs::get_stack_limit
src/kernel/privileged/proc/scheduler.rs::set_deadline_params
src/kernel/privileged/proc/scheduler_ex.rs::exit_thread
src/kernel/privileged/proc/scheduler_ex.rs::freeze_all
src/kernel/privileged/proc/scheduler_ex.rs::thaw_all
src/kernel/privileged/proc/scheduler_ex.rs::thread_dump_info
src/kernel/privileged/proc/seccomp.rs::from_linux
src/kernel/privileged/proc/seccomp.rs::to_linux
src/kernel/privileged/proc/session.rs::get_session
src/kernel/privileged/proc/session.rs::signal_foreground_pgid
src/kernel/privileged/proc/session.rs::sys_tiocsctty
src/kernel/privileged/proc/signal.rs::has_deliverable_signal
src/kernel/privileged/proc/thread.rs::get_thread
src/kernel/privileged/proc/types.rs::set_user_mode
src/kernel/privileged/proc/types.rs::thaw_target_state
src/kernel/privileged/proc/user_proc.rs::create_from_binary
src/kernel/privileged/smp/mod.rs::broadcast_reschedule
src/kernel/privileged/sync/mutex.rs::wait_timeout
src/kernel/privileged/sync/pi_mutex.rs::get_ceiling
src/kernel/privileged/sync/pi_mutex.rs::get_protocol
src/kernel/privileged/sync/pi_mutex.rs::set_ceiling
src/kernel/privileged/sync/rcu.rs::rcu_process_all_callbacks
src/kernel/privileged/sync/rwlock.rs::pending_writer_count
src/kernel/privileged/sync/rwlock.rs::raw_read_unlock
src/kernel/privileged/sync/rwlock.rs::raw_write_unlock
src/kernel/privileged/sync/rwlock.rs::read_irqsave
src/kernel/privileged/sync/rwlock.rs::write_irqsave
src/kernel/privileged/sync/seqlock.rs::current_sequence
src/kernel/privileged/sync/seqlock.rs::get_valid
src/kernel/privileged/sync/spinlock.rs::lock_irq
src/kernel/privileged/syscall/epoll.rs::epoll_destroy
src/kernel/privileged/timer/hrtimer.rs::hrtimer_ns_to_cycles
src/kernel/privileged/timer/tick.rs::format_duration
src/kernel/privileged/timer/tick.rs::get_uptime_tsc
src/kernel/privileged/timer/tick.rs::reset_ticks
src/kernel/privileged/timer/time_sync.rs::client_request
src/kernel/privileged/vmspace.rs::map_huge
src/kernel/functions/freg/audit_export.rs::count_failure
src/kernel/functions/freg/audit_export.rs::count_success
src/kernel/functions/egdf/mod.rs::char_read
src/kernel/functions/egdf/mod.rs::char_write
src/kernel/functions/config/sysctl.rs::write_to
src/kernel/functions/sgeg/crypto.rs::as_bytes_mut
src/kernel/functions/sgeg/crypto.rs::ct_eq_password
src/kernel/functions/sgeg/crypto.rs::ct_eq_salt
src/kernel/functions/sgeg/crypto.rs::generate
src/kernel/functions/sgeg/identity.rs::create_first_identity
src/kernel/functions/sgeg/identity.rs::current_gid
src/kernel/functions/sgeg/identity.rs::current_uid
src/kernel/functions/sgeg/identity.rs::get_capability_raw
src/kernel/functions/sgeg/identity.rs::get_fs_capability
src/kernel/functions/sgeg/identity.rs::load_from_disk
src/kernel/functions/sgeg/identity.rs::save_to_disk
src/kernel/functions/sgeg/identity.rs::try_genesis
src/kernel/functions/sgeg/identity.rs::try_load
src/kernel/functions/sgeg/secure_boot.rs::hash_sha256
src/kernel/functions/sgeg/secure_boot.rs::hash_sha256_extend
src/kernel/functions/sgeg/secure_boot.rs::init_secure_boot
src/kernel/functions/sgeg/secure_boot.rs::init_tpm
src/kernel/functions/sgeg/secure_boot.rs::is_secure_boot_initialized
src/kernel/functions/sgeg/secure_boot.rs::is_tpm_initialized
src/kernel/functions/sgeg/secure_boot.rs::secure_boot_syscall
src/kernel/functions/sgeg/secure_boot.rs::tpm_syscall
src/kernel/functions/debug/mod.rs::kgdb_is_active
src/kernel/functions/driver/acpi.rs::hpet_info
src/kernel/functions/driver/acpi.rs::ioapic_addr
src/kernel/functions/driver/acpi.rs::ioapic_count
src/kernel/functions/driver/acpi.rs::ioapic_gsib
src/kernel/functions/driver/acpi.rs::ioapic_list
src/kernel/functions/driver/char/serial.rs::available_bytes
src/kernel/functions/driver/char/serial.rs::clear_tx_buffer
src/kernel/functions/driver/char/serial.rs::enqueue_tx
src/kernel/functions/driver/char/serial.rs::read_from_buffer
src/kernel/functions/driver/char/serial.rs::send_str
src/kernel/functions/driver/char/serial.rs::tx_available
src/kernel/functions/driver/char/vga.rs::clear_row
src/kernel/functions/driver/char/vga.rs::read_cell
src/kernel/functions/driver/char/vga.rs::with_blink
src/kernel/functions/driver/char/vga.rs::write_string_at
src/kernel/functions/driver/display/controller.rs::connected_count
src/kernel/functions/driver/display/controller.rs::disable_monitor
src/kernel/functions/driver/display/controller.rs::enabled_count
src/kernel/functions/driver/display/controller.rs::get_active_monitor
src/kernel/functions/driver/display/controller.rs::get_best_mode
src/kernel/functions/driver/display/controller.rs::get_monitor_mut
src/kernel/functions/driver/display/controller.rs::get_primary_monitor
src/kernel/functions/driver/display/controller.rs::remove_monitor
src/kernel/functions/driver/display/controller.rs::set_active_monitor
src/kernel/functions/driver/display/controller.rs::set_display_mode
src/kernel/functions/driver/display/controller.rs::set_primary_monitor
src/kernel/functions/driver/display/dp.rs::from_iomem
src/kernel/functions/driver/display/dp.rs::read16
src/kernel/functions/driver/firmware.rs::firmware_name_hash
src/kernel/functions/driver/firmware.rs::firmware_request
src/kernel/functions/driver/storage/ahci.rs::cmd_list_base
src/kernel/functions/driver/storage/ahci.rs::fis_base
src/kernel/functions/driver/storage/ahci.rs::implemented_ports
src/kernel/functions/driver/storage/ahci.rs::port_cmd_issue
src/kernel/functions/driver/storage/ahci.rs::set_interrupt_enable
src/kernel/functions/driver/storage/nvme.rs::ring_admin_sq
src/kernel/functions/driver/storage/nvme.rs::ring_cq_head
src/kernel/functions/driver/storage/nvme.rs::set_admin_cq_phase
src/kernel/functions/driver/uefi.rs::uefi_syscall
src/kernel/functions/driver/usb/mass_storage.rs::build_read_capacity_10_cbw
src/kernel/functions/driver/usb/mass_storage.rs::build_request_sense_cbw
src/kernel/functions/driver/usb/ring.rs::dequeue_pointer
src/kernel/functions/driver/usb/ring.rs::enqueue_pointer
src/kernel/functions/driver/usb/usb_core.rs::find_device_by_class
src/kernel/functions/driver/usb/usb_core.rs::find_device_by_vid_pid
src/kernel/functions/driver/usb/usb_core.rs::register_controller
src/kernel/functions/driver/usb/xhci.rs::crcr
src/kernel/functions/driver/usb/xhci.rs::enqueue_offset
src/kernel/functions/driver/usb/xhci.rs::is_halted
src/kernel/functions/driver/usb/xhci.rs::physical_address
src/kernel/functions/driver/usb/xhci.rs::port_enabled
src/kernel/functions/driver/usb/xhci.rs::push_control_transfer
src/kernel/functions/driver/usb/xhci.rs::push_interrupt_transfer
src/kernel/functions/driver/usb/xhci.rs::set_command_ring
src/kernel/functions/driver/usb/xhci.rs::set_config
src/kernel/functions/driver/usb/xhci.rs::set_dcbaa
src/kernel/functions/driver/virtio/blk.rs::geometry
src/kernel/functions/driver/virtio/net.rs::read_link_status
src/kernel/functions/fs/cgroupfs.rs::delete_group
src/kernel/functions/fs/cgroupfs.rs::mount_cgroupfs
src/kernel/functions/fs/cgroupfs.rs::umount_cgroupfs
src/kernel/functions/fs/configfs.rs::delete_dir
src/kernel/functions/fs/configfs.rs::mount_configfs
src/kernel/functions/fs/configfs.rs::umount_configfs
src/kernel/functions/fs/dcache.rs::icache_get_ref_count
src/kernel/functions/fs/devfs.rs::is_physical
src/kernel/functions/fs/devfs.rs::is_virtual
src/kernel/functions/fs/devpts.rs::alloc_pty
src/kernel/functions/fs/devpts.rs::free_pty
src/kernel/functions/fs/devpts.rs::get_pty
src/kernel/functions/fs/devpts.rs::pty_count
src/kernel/functions/fs/devpts.rs::pty_exists
src/kernel/functions/fs/devpts.rs::umount_devpts
src/kernel/functions/fs/exfat/alloc.rs::alloc_cluster
src/kernel/functions/fs/exfat/alloc.rs::free_cluster_chain
src/kernel/functions/fs/exfat/dir.rs::stream_length
src/kernel/functions/fs/exfat/dir.rs::valid_length
src/kernel/functions/fs/exfat/super_block.rs::data_start_sector
src/kernel/functions/fs/ext2/bitmap.rs::count_free
src/kernel/functions/fs/ext2/bitmap.rs::is_set
src/kernel/functions/fs/ext2/inode.rs::get_block
src/kernel/functions/fs/flock.rs::flock_count
src/kernel/functions/fs/flock.rs::flock_ops
src/kernel/functions/fs/flock.rs::posix_lock_count
src/kernel/functions/fs/flock.rs::posix_lock_ops
src/kernel/functions/fs/handle.rs::vfs_readdir_safe
src/kernel/functions/fs/handle.rs::vfs_seek_safe
src/kernel/functions/fs/inotify.rs::inotify_fd_readable
src/kernel/functions/fs/inotify.rs::inotify_stats
src/kernel/functions/fs/unkfs/arc.rs::is_referenced
src/kernel/functions/fs/unkfs/bp.rs::is_data
src/kernel/functions/fs/unkfs/bp.rs::is_encrypted
src/kernel/functions/fs/unkfs/bp.rs::is_gang
src/kernel/functions/fs/unkfs/bp.rs::is_metadata
src/kernel/functions/fs/unkfs/bp.rs::set_encrypted
src/kernel/functions/fs/unkfs/bp.rs::with_gang
src/kernel/functions/fs/unkfs/dataset.rs::list_entries
src/kernel/functions/fs/unkfs/dedup.rs::cas_aware_free
src/kernel/functions/fs/unkfs/dedup.rs::cas_aware_write
src/kernel/functions/fs/unkfs/dedup.rs::cas_init
src/kernel/functions/fs/unkfs/dedup.rs::cas_insert
src/kernel/functions/fs/unkfs/dedup.rs::cas_is_known
src/kernel/functions/fs/unkfs/dedup.rs::cas_lookup
src/kernel/functions/fs/unkfs/dedup.rs::cas_ref_count
src/kernel/functions/fs/unkfs/dedup.rs::cas_ref_inc
src/kernel/functions/fs/unkfs/dedup.rs::cas_stats
src/kernel/functions/fs/unkfs/dedup.rs::sha256_matches
src/kernel/functions/fs/unkfs/dmu.rs::is_zap
src/kernel/functions/fs/unkfs/metaslab.rs::fragmentation
src/kernel/functions/fs/unkfs/raidz.rs::create_stripe
src/kernel/functions/fs/unkfs/raidz.rs::generate_parity
src/kernel/functions/fs/unkfs/raidz.rs::reconstruct_data
src/kernel/functions/fs/unkfs/raidz.rs::scrub_block
src/kernel/functions/fs/unkfs/spa.rs::is_disk_present
src/kernel/functions/fs/unkfs/spa.rs::is_formatted
src/kernel/functions/fs/unkfs/spa.rs::sync_uberblock
src/kernel/functions/fs/unkfs/txg.rs::add_free_to_open
src/kernel/functions/fs/unkfs/txg.rs::add_io_to_open
src/kernel/functions/fs/unkfs/txg.rs::drain_free
src/kernel/functions/fs/unkfs/txg.rs::drain_io
src/kernel/functions/fs/unkfs/txg.rs::get_open_txg_mut
src/kernel/functions/fs/unkfs/zil.rs::new_dedup_unref
src/kernel/functions/fs/unkfs/zil.rs::new_setattr
src/kernel/functions/fs/unkfs/zil_persist.rs::as_static_str
src/kernel/functions/fs/ramfs.rs::is_read_only
src/kernel/functions/fs/sysfs.rs::has_node
src/kernel/functions/fs/sysfs.rs::mount_sysfs
src/kernel/functions/fs/sysfs.rs::umount_sysfs
src/kernel/functions/fs/sysfs.rs::write_node_value
src/kernel/functions/fs/systree.rs::add_attr
src/kernel/functions/fs/systree.rs::delete_attr
src/kernel/functions/fs/systree.rs::find_attr_mut
src/kernel/functions/fs/systree.rs::mount_systree
src/kernel/functions/fs/systree.rs::read_int_attr
src/kernel/functions/fs/systree.rs::umount_systree
src/kernel/functions/fs/systree.rs::write_int_attr
src/kernel/functions/fs/tmpfs.rs::free_size
src/kernel/functions/fs/tmpfs.rs::sub_used
src/kernel/functions/fs/vfs_manager.rs::get_fs_name
src/kernel/functions/fs/vfs_types.rs::inode_arc
src/kernel/functions/fs/virtiofs.rs::mount_virtiofs
src/kernel/functions/fs/virtiofs.rs::umount_virtiofs
src/kernel/functions/ipc/async_ipc.rs::filter_by_type
src/kernel/functions/ipc/async_ipc.rs::wait_for_condition
src/kernel/functions/ipc/async_ipc.rs::with_buffer
src/kernel/functions/ipc/sem.rs::ipc_sem_create
src/kernel/functions/ipc/sem.rs::ipc_sem_destroy
src/kernel/functions/ipc/sem.rs::ipc_sem_post
src/kernel/functions/ipc/sem.rs::ipc_sem_wait
src/kernel/functions/ipc/signal.rs::ipc_signal_block
src/kernel/functions/ipc/signal.rs::ipc_signal_dispatch
src/kernel/functions/ipc/signal.rs::ipc_signal_register
src/kernel/functions/ipc/signal.rs::ipc_signal_send
src/kernel/functions/ipc/signal.rs::ipc_signal_unblock
src/kernel/functions/mm/memory_pressure.rs::is_pressure_critical
src/kernel/functions/mm/memory_pressure.rs::is_pressure_emergency
src/kernel/functions/mm/swap.rs::usage_ratio
src/kernel/functions/net/mod.rs::reset_state
src/kernel/functions/net/mod.rs::start_dhcp
src/kernel/functions/net/mod.rs::static_ip
src/kernel/functions/net/unix.rs::uds_parse_path
src/kernel/functions/net/unix.rs::uds_recv_with_creds
src/kernel/functions/proc/canary.rs::get_canary_u64
src/kernel/functions/proc/elf.rs::is_executable
src/kernel/functions/proc/mod.rs::priority_from_u32
src/kernel/functions/proc/shadow_stack.rs::cet_syscall
src/kernel/functions/proc/signal.rs::cont
src/kernel/functions/syscall/mod.rs::dispatch_from_ctx_typed
src/kernel/functions/timer/tickless.rs::tickless_syscall
src/kernel/functions/timer/time_sync.rs::timesync_syscall
src/kernel/functions/wasm/interpreter.rs::instantiate
src/kernel/functions/wasm/interpreter.rs::register_host_function
src/kernel/functions/wasm/types.rs::as_i64
src/kernel/functions/wasm/wasi/errno.rs::from_kernel_error
src/kernel/functions/wasm/wasi/mod.rs::wasi_function_table
<!-- audit-classified-end -->

#### B-7. T5-C 关闭记录（裁定四「五条验收」核销）

> 本段为 [audit-fix-09-hard-rules-deadcode.md](archive/audit-fix-09-hard-rules-deadcode.md) B09-06 / B09-21 所引 **B-7** / **B-7.2** 的落点（此前该锚点缺失 ⇒ 已就地补建）。

| # | 验收条 | 状态 | 证据 |
|---|---|---|---|
| ① | 删候选桶清零 | ✅ | `write_log_line` 已试删删除（commit `23681a14`）；`format_duration` 判据待补退桶；ramfs 2 项试删**门槛失败**退桶 |
| ② | 待裁 47 项全部带三字段、无裸待裁 | ✅ | **B-5** 归零表（13 等路线图 / 32 判据待补定桶 / 2 安全面待 T3） |
| ③ | 桶数算术闭合 + 转移可逐项追 | ✅ | 6 次修订后 **437**（见 B-5）；**甲批 C-1 后 436**（见 **B-8.4**） |
| ④ | 审计噪音治理落地 | ✅ | 见 **B-7.2** |
| ⑤ | 文档同步：R1 状态行 + 与 audit-fix-09 **D-4** 双向引用 | ✅ | 本台账状态行 + **B-6** / **B-8** 与 [audit-fix-09](archive/audit-fix-09-hard-rules-deadcode.md) B09-06 / B09-21 互引 |

**B-7.1 T5 置 `[X]`**：五条全部达成 ⇒ 状态行 `- [X] T5`。**QEMU boot 通过**为该判定的硬闸门（本项改动为文档 + `set_fd` 接线，QEMU boot 与 kernel_test 均须实跑，见 B-8.5）。

**B-7.2 审计噪音治理（B09-21）落地**：`scripts/audit_unwired_pub_fn.py` 新增 `load_classified_set()` —— 读本台账 **B-6** 区块作「已知分类」集合，**未分类 ⇒ HIGH / 已分类 ⇒ INFO**（零引用**事实**保留），退出码仅计未分类；**fail-closed 六条失败路径**（区块缺失 / 标记缺失 / 解析失败等）实测**全部回落空集 ⇒ 全量报 HIGH**。正向实测（甲批后）：**HIGH = 0 / INFO = 436（＝ R1 435 + R4 1）/ CRITICAL = 7（R2 预存未接线 syscall，与本批无关）**。

#### B-8. 甲批 C-1 接线批次记录（8 项：1 落地 + 7 退桶）

> 来源：reviewer **甲批开工单｜C-1 接线 8 项**（委托批次内自主执行，只报批次结果）。
> 核心约束：每项须**指明具体调用点**——「仅缺此半」是**待验证断言**；**找不到调用点即退「未来功能」，禁止为接线造无意义调用**。

**B-8.1 接线落地（1 项）**

| 项 | 调用点（函数 / 路径） | 改动 | 生效证据（测试） |
|---|---|---|---|
| `privileged/fs/vfs/vfs.rs::set_fd` | `vfs_open_internal` **两分支**（`fs_open` 命中 / `CREAT` 命中）；live 路径 `open_syscall` / `openat_syscall` → `vfs_open_safe` → `vfs_open_internal` | [handle.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/fs/vfs/handle.rs#L70-L76)：`set_fd_handle` 之后补 `VFS_MANAGER.set_fd(fd_idx, node_id, 0, flags, pwm, file_type, path)`；CREAT 分支同款（[L101-L103](file:///home/anfer/Code/Edgine/src/kernel/privileged/fs/vfs/handle.rs#L101-L103)） | kernel_test `vfs::backend::open_populates_fd_metadata`（[test_vfs.rs](file:///home/anfer/Code/Edgine/src/kernel/privileged/tests/test_vfs.rs#L227-L266)） |

**「缺此半」的具体后果（逐条可在源码定位，故调用点成立）**：

| 受害调用点 | 未接线时的行为 |
|---|---|
| [file_ops.rs:298-305](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/file_ops.rs#L298-L305) `flock_syscall`（以 `fd_table[fd].node_id` 作 ino） | 所有 fd 的 ino 恒 **0** ⇒ flock 全部落到同一 ino |
| [mmap.rs:44-51](file:///home/anfer/Code/Edgine/src/kernel/functions/mm/mmap.rs#L44-L51) `fd_to_inode_id` | 恒得 **0** ⇒ 文件映射分支恒 `EBADF` |
| [mmap.rs:54-59](file:///home/anfer/Code/Edgine/src/kernel/functions/mm/mmap.rs#L54-L59) `fd_to_mount_idx` | `path` 为空 ⇒ `find_mount` 反查失败 ⇒ VMA 无 `mount_idx` |
| [handle.rs:143-180](file:///home/anfer/Code/Edgine/src/kernel/privileged/fs/vfs/handle.rs#L143-L180) `vfs_close_internal` | pcache 失效按 `node_id = 0` 执行 ⇒ 关联 inode 缓存页不释放 |

**B-8.2 判据不成立 ⇒ 转「未来功能」（7 项；三字段）——其中 5 项已于 2026-09-27 兑现（见 **B-11**），余 2 项仍为未来功能**

| 文件 / 项 | 等待原因（判据不成立的事实） | 解锁条件 | 责任方 |
|---|---|---|---|
| `privileged/fs/vfs/handle.rs::vfs_get_fd_handle` | ~~同能力 `VFS_MANAGER.get_fd_handle` 的调用点全在同模块内直呼 manager，无独立调用点~~ ⇒ **2026-09-27 已接线**：functions [file_ops.rs `poll_syscall`](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/file_ops.rs) 改源为 `vfs_get_fd_handle`（原直查已退役的 `VFS_MANAGER.fd_table` / `VFS_MAX_FDS` 全局表）⇒ 出现**跨模块** FD→句柄访问需求（如 functions 侧 fd 查询代理）**已成立** | **已兑现**（见 **B-11**） | — |
| `privileged/irqline.rs::is_registered` | `IrqLine` 类型全库 **0 构造点**（在用者仅 `idt.rs::dispatch_irq`）⇒ 无对象可查 | 中断处理切到 `IrqLine` 抽象（`functions/driver/mod.rs:21` 已登记的路线图项） | 路线图（driver 中断抽象） |
| `privileged/frame.rs::set_meta` | 读侧 `Frame::meta()` 同样**零引用**，无消费方；文档自述「预留元数据槽位」 | functions 侧需给页帧挂自定义状态（回写 / 迁移标记） | reviewer 排期 |
| `privileged/proc/fd_table.rs::get_handle_id` | ~~`Process::fd_table` 从未被填充（`alloc_fd` 全库零引用）⇒ 恒 `None`~~ ⇒ **2026-09-27 全量兑现**：`Process.fd_table(FdTable)` 权威化，`alloc_fd` 经 `vfs_open_internal` 两分支接线，本函数成为 `vfs_get_fd_handle` / `vfs_dup` / `vfs_dup2` / `fcntl(F_GETFD)` 的统一取句柄入口 | **已兑现**（见 **B-11**） | — |
| 同上 `::is_cloexec` | ~~`cloexec[]` 恒 `false`~~ ⇒ **2026-09-27 全量兑现**：`fcntl(F_GETFD)` 经此查询 `FD_CLOEXEC` | **已兑现**（见 **B-11**） | — |
| 同上 `::set_cloexec` | ~~写入无任何消费方的死结构~~ ⇒ **2026-09-27 全量兑现**：`fcntl(F_SETFD)` 经此置位（本项为 `cloexec[]` 唯一写入者） | **已兑现**（见 **B-11**） | — |
| 同上 `::get_cloexec_fds` | ~~恒空集~~ ⇒ **2026-09-27 全量兑现**：`vfs_close_cloexec_fds` 经此收集 exec 时需关闭的本地 fd | **已兑现**（见 **B-11**） | — |

> 结论（本批复核更新）：原判定「补调用＝**往死结构写值**」的前提是 `Process.fd_table` **无权威数据源**；本专项将该表升为权威后，下列 5 项（`vfs_get_fd_handle` + `fd_table.rs` 4 项）**均有真实消费方**（poll 查询 / open 分配 / dup·dup2·fcntl 读写 / exec 关闭）⇒ 从「未来功能」转**接线兑现**，其区块条目已随之移除（见 **B-6** 维护说明）。余下 `irqline.rs::is_registered` / `frame.rs::set_meta` **2 项维持未来功能**（判据仍不成立，未动）。

**B-8.3 安全面单列：FD_CLOEXEC 全链路缺失（fd 跨 exec 可见性）**

> reviewer 第二节要求：先判定 `execve` 路径的**实际行为**，再决定是「补接线」还是「缺陷修复」。**实测结论＝「没接于是不生效」，且不是单点漏接，而是整条语义链缺失**。

| 链路环节 | 实测（源码定位） | 缺口后果 |
|---|---|---|
| 标记来源 `fcntl(F_SETFD, FD_CLOEXEC)` | [privileged/syscall/io.rs:108-124](file:///home/anfer/Code/Edgine/src/kernel/privileged/syscall/io.rs#L108-L124)：`F_GETFD => 0`、`F_SETFD => 0` **静默返回成功** | 用户设 CLOEXEC **无任何存储**，且**不报错**（虚假成功） ⇒ **2026-09-27 全量兑现**：`F_GETFD`/`F_SETFD` 接 `FdTable::is_cloexec`/`set_cloexec` |
| 打开时携带 `O_CLOEXEC` | `functions/fs/open.rs:44 pub const O_CLOEXEC` 全库**零引用** | open 无法携带 cloexec ⇒ **2026-09-27 全量兑现**：`vfs_open_internal` 消费 `O_CLOEXEC`（剥离后经 `alloc_fd(handle_id, cloexec)` 入表） |
| 存储位 | live fd 表 `VFS_MANAGER.fd_table: [VfsFile; VFS_MAX_FDS]` 的 `VfsFile` **无 cloexec 字段** | 无处存放标记 ⇒ **2026-09-27 全量兑现**：标记位改存 per-process `FdTable.cloexec`（原 `VfsManager` 全局表版 `VfsFile.cloexec` 最小件随之退役） |
| exec 时关闭 | `functions/proc/exec.rs` `execve_syscall` / `execveat_syscall` → `proc_ops.rs::proc_exec_replace`（transactional：加载 ELF → `replace_user_space` → argv → 信号复位）**全程不触碰 fd 表** | exec 后**所有 fd 原样保留** ⇒ **2026-09-27 全量兑现**：exec 经 `vfs_close_cloexec_fds`→`FdTable::get_cloexec_fds` 关闭已标记本地 fd |
| fd 命名空间 | fd 表为**全局**（`VFS_MANAGER` 单例 + 全局 `next_fd` 计数器） | fd 跨进程可见（POSIX per-process fd 语义未隔离） ⇒ **2026-09-27 全量兑现**：fd 命名空间改 per-process（`Process.fd_table`）；全局 `VfsManager.fd_table` 退役删除 |

**判定**：⇒ **不是「C-1 少接一项」，而是 FD_CLOEXEC / per-process fd 语义整体未落地**（安全面）。
**为何不由本批修复**：修复须先有 per-process fd 表（**B-11**）+ `fcntl` F_SETFD 真实实现 + `VfsFile` 增字段 + exec 关闭遍历 —— **跨 4 个模块的架构改动**，属 **TCB / VFS 核心面**，**超出批次授权**；且此处亦**无「能力等价的公共入口」**可依（三合一判据不成立）。
**回归测试说明（如实登记）**：reviewer 要求「exec 后 cloexec fd 不可见」的回归测试 —— 因标记**当前无处可设**（`F_SETFD` 不存储、无 `cloexec` 字段），该测试**在现有结构上无法构造**（无 API 能产生一个「已标记 CLOEXEC 的 fd」）；若强行写「未标记 fd 仍可见」的断言，通过也不构成对缺陷的覆盖。故本项**按缺陷单列上报，不附伪测试**。
**上报**：按批次「涉安全面」条款单列于此，**未自主施工**。
**解锁条件**：per-process fd 表（**B-11**）落地后，`FdTable` 的 cloexec 4 项即为该表的**自然消费方**（届时从「未来功能」转「接线」），并可在该结构上补 exec 关闭回归测试。
**责任方**：reviewer（是否立项 per-process fd 表 + FD_CLOEXEC 语义）。
**2026-09-27 全量兑现**：本表 5 行缺口已由 **B-11**「per-process fd 表全量下沉 VFS 管理面」专项全部闭合 —— 标记来源接 `fcntl(F_GETFD/F_SETFD)` → `FdTable::is_cloexec`/`set_cloexec`；打开时 `vfs_open_internal` 消费 `O_CLOEXEC` 经 `alloc_fd(handle_id, cloexec)` 入表；存储位改 per-process `FdTable.cloexec`（早期在**全局** fd 表落地的 `VfsFile.cloexec` 最小件已随全局表退役删除）；exec 经 `vfs_close_cloexec_fds`→`FdTable::get_cloexec_fds` 关闭；fd 命名空间改 per-process，`VfsManager.fd_table` 退役。上文「判定 / 为何不由本批修复 / 回归测试说明 / 解锁条件 / 责任方」保留为**历史判定快照**（当时前提成立），本条为其兑现记录。

**B-8.4 R1 清单数与算术闭合**

- `set_fd` 接线后**不再零引用** ⇒ 从 **B-6** 区块移除 1 行 ⇒ **R1 437 → 436**（N = 1）。
- 实测 `python3 scripts/audit_unwired_pub_fn.py`：`已分类清单 436 项` / `[HIGH] R1 未分类零引用 pub fn: 0 项` / 汇总 `CRITICAL=7（R2 预存未接线 syscall，与本批无关）/ HIGH=0 / WARN=0 / INFO=436`。
- 桶数：`0 + 75 + 2 = 77 ＝ 436 − 0（接线清零）− 359（未来功能 352 + 甲批退桶 7）`（**算术闭合**）。

**B-8.5 门槛（五条全量）**

| # | 门槛 | 命令 | 结果 |
|---|---|---|---|
| 1 | 双架构构建 + host-tests + link | `./ci/build.sh all` | ✅ `x86_64: build passed` / `aarch64: build passed` / `Host tests: passed` / `x86_64: link passed` |
| 2 | 三审计（含 clippy 三维） | `./ci/audit.sh quick` | ✅ 全绿 — `TCB 边界: functions/ 零 unsafe` / `I1-I6` 全 PASS / `SAFETY 注释覆盖 1924/1924 (100%)` 缺漏 0 / `I-43` ✓ / `I-16` ✓ / `I-07 PASSED: 0 C 风格残留` / `TD-22 注释中文化 100% (0 违规)` / 双架构 check passed / `clippy pedantic (lib)` + `kernel_test 维` + `host-test 维` 三维 passed / `━━━ audit (quick) 完成 ━━━` |
| 3 | host-tests | `make test-host` | ✅ `RESULT: ALL 364 TESTS PASSED (8 skipped)`（E-04 共享运行器；含新增 `vfs::backend::open_populates_fd_metadata`） |
| 4 | QEMU `kernel_test` | `make test-unit` | ✅ `ALL TESTS PASSED (QEMU exit: 33)` |
| 5 | QEMU boot（硬闸门） | `./scripts/qemu_boot_test.sh x86_64` | ✅ `找到里程碑: 'VFS ready'` / `完整启动成功! 进入 Ring 3 启动 init 进程` / `QEMU 真实启动测试: 1/1 通过` |

> 日志：`build/log/jia_batch_c1_gates_final.log`（**最终树全量**，`RC=0`；mtime 20:53 晚于全部改动文件 20:41）。

#### B-9. 乙批收尾记录（5 项：证据补齐 + 命题定性）

**B-9.1 `handle.rs` diff 澄清（结论：非误改，不回退）**

- `b34d6b9b` 改 `handle.rs` 的**唯一原因**＝C-1 **第 2 组**（`vfs.rs::set_fd`）的**调用点在该文件内**：`vfs_open_internal` 的两个分支（`fs_open` 成功 / `CREAT` 创建）。
- 实读 `git show b34d6b9b -- src/kernel/privileged/fs/vfs/handle.rs`：diff 中**不含一行 `vfs_get_fd_handle`** —— 第 1 组判「判据不成立 ⇒ 代码一行未动」，与该文件被改**不矛盾**（同文件、不同函数）。两处改动仅为新增 `let node_id = inode.node_id();` + 两处 `VFS_MANAGER.set_fd(...)`。
- 处置：**保留现状，不回退**。

**B-9.2 `node_id` 端到端下游验证（新增 1 条链路测试）**

- 测试名：`vfs::backend::fd_to_inode_id_downstream`（[test_vfs.rs](../../src/kernel/privileged/tests/test_vfs.rs)）。真实 open 后经**下游消费者**取值，而非只读元数据：
  - `functions::mm::mmap::fd_to_inode_id(fd)` ＝真实 inode —— 它是 `mmap_syscall` 文件映射的**唯一** inode 来源，取 0 即**直接返回 `EBADF`**（[mmap.rs:134-137](../../src/kernel/functions/mm/mmap.rs#L134-L137)）；
  - `functions::mm::mmap::fd_to_mount_idx(fd)` 为 `Some` —— 同一 `mmap_syscall` 的挂载点来源。
- 结果：**PASS**。⇒ 证明 `set_fd` 接线修的是**下游行为**（mmap 文件映射不再因 `inode_id` 恒 0 而恒 `EBADF`），**非仅元数据填充**。
- host 共享运行器计数：`ALL 365 TESTS PASSED (8 skipped)`（甲批 364 → 乙批 **+1**）。

**B-9.3 fail-closed 负向实证（由「声明」补为「实证」）**

脚本 `scripts/audit_unwired_pub_fn.py` 的降噪依赖台账 **B-6** 区块；此前台账仅陈述「fail-closed」逻辑，无负向输出。本轮实做（临时破坏 → 实测 → 复原）：

| # | 台账状态 | 脚本实测 `HIGH` | `INFO` | 清单 | fail-closed |
|---|---|---|---|---|---|
| ① | 基线（正常） | **0** | 435 | 436 | — |
| ② | 结束标记改名（缺失） | **435** | 0 | 区块未加载 | ✅ 回落「全部未分类」 |
| ③ | 区块内插入非法行 | **435** | 0 | 区块未加载 | ✅ 同上 |
| ④ | 区块清空 | **435** | 0 | 区块未加载 | ✅ 同上 |
| ⑤ | 复原后 | **0** | 435 | 436 | — |

- 复原校验：`sha256` 与破坏前**逐字节一致**（`True`）。
- 结论：**fail-closed 三态（标记缺失 / 行格式非法 / 区块为空）全部实证触发全量 HIGH**，降噪**不可被「静默失效」** ⇒ 上一轮 T5 的「降噪落地」证据缺口**已补齐**。
- 附注：四次运行的 `rc=1` 均来自预存 `CRITICAL=7`（R2 未接线 syscall），与 fail-closed 无关。

**B-9.4 ramfs 2 项定型：冗余（但删除受双重阻塞）**

自读证据（活路径＝`vfs_open_internal` → `RamFsData`，functions `SafeRamFs` 无生产调用者）逐条核对 `validate_path` 三项检查：

| 检查项 | 下层等价实测 | 判定 |
|---|---|---|
| **空** | `resolve_user_path` 把 `""` 归一为 cwd/`/`（[vfs.rs:593-655](../../src/kernel/privileged/fs/vfs/vfs.rs#L593-L655)）⇒ ramfs 侧**收不到空串**；且 privileged [`RamFsData::open`](../../src/kernel/privileged/fs/ramfs/ramfs_data.rs#L481-L484) L482 `if path.is_empty() { return None }` | ✅ **等价（双重）** |
| **长度** | [`normalize_view_path_into`](../../src/kernel/privileged/fs/vfs/vfs.rs#L641-L648) 溢出即 `return None`（L641/L647）⇒ `resolve_user_path` None ⇒ `vfs_open_internal` 返回 -1，**失败语义**；另 VFS_MAX_PATH ＝128（[types.rs:16](../../src/kernel/privileged/fs/vfs/types.rs#L16)） | ✅ **等价**（且为**报错**，强于 `NameTooLong`） |
| **NUL** | FFI 入口 `ptr_to_str` → [`as_kstr()`](../../src/kernel/privileged/lib/cstr.rs#L92-L110) 按 NUL 终止扫描（`while n < MAX_CSTR_LEN && *ptr.add(n) != 0`）⇒ 用户路径**按构造不含内嵌 NUL** | ✅ **等价（不可达）** |

- **定型**：**冗余**（`split_path`/`validate_path` 自身全库零引用，且 `SafeRamFs`/`GLOBAL_RAMFS` 在 `src/` 无生产调用者）⇒ 形态是「**未被调用的辅助**」，**不是**「被调用却缺失校验」。
- **删除的双重阻塞（⇒ 不自主删）**：
  1. **涉安全面**（裁定六「必须上报」）；
  2. **会破坏 host-tests 源文本断言**：[td18 `usages_all_use_kernel_wrapper`](../../host-tests/tests/td18_fs_kernel_error_test.rs#L147-L164) 断言 `min("FsError::Kernel(", "KernelError::") >= 10`，而这两个函数正文恰含 3 处 `KernelError::`（`InvalidArgument`×2 + `NameTooLong`）⇒ 删除后计数跌破下界 10。**与 B-7 记录的「试删门槛失败」同源**。
- ⇒ 本项由「待裁」转为**可删候选 + 需同步调整 host-tests 断言**，**待 reviewer 授权**（不并入本批）。
- **桶数影响（授权后生效，本批不自行变更）**：若 reviewer 批准转删候选，则 `删候选 0 → 2`、`待裁 2 → 0`，合计仍 **436**（`2 + 75 + 0 + 0 + 359`）。本批**不改桶数**——「新增删候选」属四类上报事项。

**B-9.5 cloexec 独立项登记确认（发现引用错配 + 能力冗余线索）**

- **现状核实**：**未**存在以 per-process fd 表／FD_CLOEXEC 为题的**独立工程项**。仅有两处登记：
  - [unresolved-issues-2026-08-09.md](./unresolved-issues-2026-08-09.md) 的 `ISSUE-SRC-024`（memfd per-process fd 表）与 `ISSUE-SRC-028`（vfs/api.rs per-process fd 表）——**2026-08-09 快照 backlog**，非活看板；
  - 本台账 **B-8.3** 的「解锁条件 / 责任方」段。
- **引用错配（须 reviewer 裁定）**：B-8.3 与本文件多处以「**B09-10（per-process fd 表）**」指代该路线图，但 [audit-fix-09](./archive/audit-fix-09-hard-rules-deadcode.md) 的 **B09-10 实为「28 处 TODO(TRACK-...) 注释」治理任务（已 `[X]`）**，非 per-process fd 表工程；`handle.rs:61` 注释同样挂在 B09-10 名下 ⇒ **标签错配**，三项引用需统一改指新编号。
- **能力冗余线索（影响成本评估）**：[`functions/fs/process_fd_table.rs`](../../src/kernel/functions/fs/process_fd_table.rs) **已完整实装** Plan B per-process fd 表（`FdEntry.cloexec` 字段 + `alloc_fd` / `alloc_fd_at` / [`close_cloexec_fds`](../../src/kernel/functions/fs/process_fd_table.rs#L174-L183) / [`clear_non_cloexec`](../../src/kernel/functions/fs/process_fd_table.rs#L186-L195)），全库**零引用**（台账 B-5 记「Plan B 并行 FD 表整体未采用，删/接线待裁」）⇒ 解锁路径可能是「**启用既有 Plan B 表 + 接线**」而非「从零新建」，B-8.3 的修复成本评估应据此下修。
- **本条即本项的独立登记条目**（编号待 reviewer 分配，命名建议「per-process fd 表 + FD_CLOEXEC 语义」；绑定关系＝B-8.3 解锁条件）。**未自行在 audit-fix-09 新增 B09-xx 编号**（属已登记路线图地基，按四类上报）。
- **2026-09-27 实施记录（本项已被用户裁定立项并落地）**：用户裁定「相对完整」「全量下沉 VFS 管理面」⇒ 本项由 reviewer 待裁转**实施完成**，详见 **B-11**。三处遗留随之收口：
  - **标签错配**：B-8.3 及本文件多处的「B09-10」指代错误，现统一改指本专项编号 **B-11**（不再回填 audit-fix-09 的 B09-10，后者确为 TODO 注释治理任务）；
  - **能力冗余线索（Plan B `process_fd_table.rs`）**：该文件（`FdEntry.cloexec` + `alloc_fd`/`alloc_fd_at`/`close_cloexec_fds`/`clear_non_cloexec`）经复核**未采用**，用户裁定以 `privileged/proc/fd_table.rs::FdTable` 为权威实装（新建而非启用 Plan B 表）⇒ 该文件**已删除**，能力冗余线索终结（台账 B-5 的「删/接线待裁」同步核销）；
  - **本项命名建议「per-process fd 表 + FD_CLOEXEC 语义」**：实装后 B-8.3 五环全部闭合（见 **B-11**）。

**B-9.7 CLOEXEC 最小件实装（2026-09-27，本项部分兑现；per-process fd 表部分转本条专项）**

- **范围裁定**：B-8.3 的修复面跨「per-process fd 表 + `fcntl` F_SETFD + `VfsFile` 增字段 + exec 关闭遍历」四环。本批**只拆最小件**——在**现有全局 fd 表**（`VFS_MANAGER.fd_table`）上落地 CLOEXEC 语义（fd 表版本位 + 标记/查询 API + exec 关闭 + memfd 置位）；**per-process fd 表命名空间部分本批不改**（仍为全局 fd 命名空间），**留驻本条 B-9.5 专项**。
- **已实装（4 处）**：
  - **存储位**：[`VfsFile`](../../src/kernel/privileged/fs/vfs/vfs.rs) 末字段 `pub cloexec: bool`，`Clone` / `const fn new()` / `free_fd` 重置三处同步；
  - **标记/查询 API**：`VfsManager::set_fd_cloexec(idx, bool)` / `get_fd_cloexec(idx) -> bool`（越界/未分配安全空操作）；
  - **exec 关闭**：新增 `vfs_close_cloexec_fds()`（[handle.rs](../../src/kernel/privileged/fs/vfs/handle.rs)，**先收集再逐个 `vfs_close_internal`**，避免持 `fd_table` 锁递归自锁死），经 `privileged/fs` 顶层 re-export（F2 合规），在 [`proc_exec_replace`](../../src/kernel/privileged/proc/proc_ops.rs) 的 `reset_signal_state_on_exec` 之后接线；
  - **标记来源（最小）**：[`memfd_create_syscall`](../../src/kernel/functions/proc/memfd.rs) 依 `MFD_CLOEXEC` 置位（原「fd CLOEXEC 标记待实现」占位消除）。**`fcntl(F_SETFD, FD_CLOEXEC)` / `open(O_CLOEXEC)` / `dup3` / `pipe2` 的 CLOEXEC 仍为待扩展注释**（[privileged/syscall/io.rs](../../src/kernel/privileged/syscall/io.rs) / [functions/fs/io.rs](../../src/kernel/functions/fs/io.rs)），未在本批范围。
- **测试**：内联单测 2 项（`fd_cloexec` 模块：置位/查询/清零/free 后清零/first-fit 复用不残留 + 未分配/越界安全空操作，函数内 `static VfsManager` 独立实例规避全局污染与栈溢出）；host 契约测试 4 项（[fd_cloexec_test.rs](../../host-tests/tests/fd_cloexec_test.rs)：先收集后关闭的顺序、顶层 re-export、exec 接线、memfd 置位）。
- **仍留本条专项（per-process fd 表命名空间）**：全局 `VFS_MANAGER` 单例 + `next_fd` 计数器不改 ⇒ **fd 跨进程仍可见**（POSIX per-process 语义未隔离）；B-8.3 表「fd 命名空间」行与 B-8.2 的 `FdTable` 4 项（`get_handle_id`/`is_cloexec`/`set_cloexec`/`get_cloexec_fds`）仍待该专项落地；上文「能力冗余线索」的 Plan B `process_fd_table.rs` 仍为零引用（启用/接线待裁）。
- **门槛**：`./ci/build.sh all` Passed 5 / Failed 0；clippy `-D warnings` 0 warning；`./ci/audit.sh quick` 全绿（经 `build.sh aarch64` 收尾以避 FP-06）；`make test-host` 全过；`make test-kernel-host` 828 passed / 0 failed；`audit_unwired_pub_fn.py` **HIGH=0**（CRITICAL=2 为既有 process_vm 待裁项；**该 2 项已于 B-10.12 实装核销 ⇒ 未接线 syscall 清零**）。
- **2026-09-27 全局表版已退役（本最小件被 per-process 表取代）**：本条在**全局 `VfsManager.fd_table`** 上落地的 CLOEXEC 最小件（`VfsFile.cloexec` 字段 + `VfsManager::set_fd_cloexec`/`get_fd_cloexec`）已随**全局 fd 表退役删除**——per-process 专项 **B-11** 将存储位迁至 `FdTable.cloexec`，`fcntl`/`open`/`dup3`/`pipe2` 的 CLOEXEC 标记来源全部接线，B-8.3「fd 命名空间」行亦随 per-process 命名空间闭合。本条保留为**分阶段实施的历史快照**（全局表最小件 → per-process 全量），现网唯一权威为 `Process.fd_table(FdTable)`。

**B-9.6 门槛（五条全量；日志 `build/log/yi_batch_c1_gates.log`，`RC=0`）**

| # | 门槛 | 结果 |
|---|---|---|
| 1 | `./ci/build.sh all` | ✅ `x86_64: build passed` / `aarch64: build passed` / `Host tests: passed` / `x86_64: link passed` |
| 2 | `./ci/audit.sh quick`（三审计） | ✅ 全绿 — `functions/ 零 unsafe` / `I1-I6` 全 PASS / `SAFETY 1924/1924 (100%)` 缺漏 0 / `I-43` ✓ / `I-16` ✓ / `I-07: 0 C 风格残留` / `TD-22: 0 违规` / 双架构 check passed / clippy `lib + kernel_test 维 + host-test 维` passed |
| 3 | `make test-host` | ✅ `RESULT: ALL 365 TESTS PASSED (8 skipped)` |
| 4 | `make test-unit`（QEMU `kernel_test`） | ✅ `ALL TESTS PASSED (QEMU exit: 33)` |
| 5 | `./scripts/qemu_boot_test.sh x86_64`（硬闸门） | ✅ `找到里程碑: 'VFS ready'` / `完整启动成功! 进入 Ring 3 启动 init 进程` / `QEMU 真实启动测试: 1/1 通过` |

#### B-10. 分册 9 批次 2：R1 复查 8 项未分类甄别（4 项登记 + 4 项 TCB 上报）

> 来源：[audit-fix-09-hard-rules-deadcode.md](archive/audit-fix-09-hard-rules-deadcode.md) **B09-18** 2026-09-26 全量实测列出的「新增未分类 HIGH 8 项」（B-6 区块外）。
> 本批**只做甄别与登记，不删任何代码**；涉 TCB 的 4 项按**裁定六**上报待裁、不自主处置。

**B-10.1 逐项三档定性（引用计数口径同 R1 ＝ `rg -c -w`）**

| 项（文件::符号） | 面 | 实测 | 三档定性 | 本批处置 |
|---|---|---|---|---|
| `privileged/mm/kpti_aarch64.rs::kpti_kernel_ttbr0`(118) | TCB（MMU / KPTI） | total=1（仅声明） | **族残缺**：同族 `kpti_kernel_ttbr1`(112) / `kpti_trampoline_ttbr1`(106) **已在 B-6 区块分类**（硬件原语完整性保留档）；异常入口/出口汇编**按固定字节偏移直读 `KPTI_GLOBALS`**（偏移由 `offset_of!` 静态断言锁定，见 `kpti_aarch64.rs:85-91`），不经 getter ⇒ 该族 getter 是「Rust 侧诊断 / 兜底」对称面 | **上报待裁** |
| `::kpti_user_ttbr0`(130) | 同上 | total=1 | 同上：写侧 `kpti_set_user_ttbr0` 有 1 处调用点（`arch/aarch64/mod.rs:326` `enter_user`），**读侧 getter 零调用者** ⇒ 与 ttbr1 族同构 | **上报待裁** |
| `privileged/mm/kmalloc_slab.rs::slab_kmalloc`(76) | TCB（内存分配器） | total=1 | **未接线（整模块孤岛）**：本模块 3 个 `pub fn`（`slab_init` / `slab_kmalloc` / `slab_kfree`）**全部零调用者** ⇒ `SLAB_READY` 永为 `false`，Slab 路径永不生效（`slab_kmalloc` 恒走 `kmalloc` 回退）。**与既有登记同源**：[archive/audit-2026-08-14/subsystem-mm.md](archive/audit-2026-08-14/subsystem-mm.md) 记「内部用 SLAB 但未注册到 kmalloc → 死代码」（P2） | **上报待裁**（接线 / 删除二选一） |
| `::slab_kfree`(90) | 同上 | total=1 | 同上 | **上报待裁** |
| `functions/fs/unkfs/txg.rs::add_free_to_open`(243) | functions | total=1（底层 `add_free` 2 ＝ 声明 + 本项调用） | **族残缺**：同族 `add_dirty_to_open` **7 引用在用**（且额外递增 group 级 `total_dirty`）；free / io 两条并行路径（`add_free` / `add_io` / `drain_free` / `drain_io`）**只有本项与 `add_io_to_open` 作为唯一送入口** ⇒ 删则族残缺、且 `add_free` / `add_io` 一并沦为孤儿 | **完整性保留**（登记） |
| `::add_io_to_open`(249) | functions | total=1（底层 `add_io` 2） | 同上 | **完整性保留**（登记） |
| `functions/driver/char/serial.rs::send_str`(468) | functions | total=1（同族 `send_all` 2 在用） | **能力等价复本**（实现体 ＝ `self.send_all(s.as_bytes())`）；但**三合一判据第三项不成立**——属 **driver 公共 API 面**（同文件已有 5 项同类 API 在 B-6 区块）⇒ 不构成删候选 | **完整性保留**（登记） |
| `functions/driver/char/vga.rs::write_string_at`(370) | functions | total=1（同族 `write_char` 3 在用） | 同上（实现体 ＝ 逐格 `write_char` 的定位写串封装；同文件已有 3 项同类 API 在区块） | **完整性保留**（登记） |

**B-10.2 新增登记的 4 项（附裁定二三字段）**

| 项 | 等待原因（零引用的事实成因） | 解锁条件 | 责任方 |
|---|---|---|---|
| `txg.rs::add_free_to_open` / `add_io_to_open` | UNKFS 写路径当前**只走 dirty 累积**（`add_dirty_to_open` 已在用）；free / io 累积器**无送入口**——本两项即其唯一入口，故 `free_bps` / `io_list` 在 live 路径**结构性为空**，`drain_free` / `drain_io` 恒得空集 | UNKFS 写路径接入 free / io 累积（届时本两项即为自然接线点；一并核对 `total_dirty` 同类 group 计数是否需补 `total_free`） | 路线图（UNKFS 写路径 / TXG 完整语义） |
| `char/serial.rs::send_str` | driver 公共 API 面：同能力 `send_all` 已在用，字符串便捷入口无调用者 | 出现「按 `&str` 写串口」的上层需求（用户态串口服务 / 内核日志旁路） | 路线图（driver API 收敛） |
| `char/vga.rs::write_string_at` | driver 公共 API 面：同能力 `write_char` 已在用，定位写串入口无调用者 | 出现屏幕**绝对定位**写串需求（状态栏 / 日志定区刷新） | 路线图（driver API 收敛） |

**B-10.3 上报 TCB 4 项（裁定六：不自主处置）**

- **`kpti_aarch64.rs` 2 项**：与**已在 B-6 区块分类**的 ttbr1 同族、同文件、同形态（`pub fn` getter 读 `KPTI_GLOBALS` 原子量、`#[inline(always)]`、供诊断 / 兜底）。**候选处置**：① 按同族先例登记为「硬件原语完整性保留」（与 ttbr1 对称，零代码改动）；② 删除（TCB 删除，须先证无诊断用途）。**属 TCB 核心（MMU / KPTI）⇒ 请裁定。**
- **`kmalloc_slab.rs` 2 项（+ 整模块）**：**新发现（已含于事实陈述）**——该模块 **3 个 `pub fn` 全部零调用者**，即整模块为孤岛；`slab_init` 因本文件第 15 行**文档注释出现同名串** `\`slab_init()\``，被 `rg -c -w` 计入引用而**未进 R1 报告**（R1 的按名计数已知偏差，见 B-6 口径说明）。**候选处置**：① 接线（在 kmalloc 初始化点调用 `slab_init`，并把 `slab_kmalloc` / `slab_kfree` 接为 kmalloc 的 size≤2048 路径）；② 删模块（含 host-tests 源文本断言 `kmalloc_irq_save_test` 需同步）。**属 TCB 内存分配器面 ⇒ 请裁定。**
- **附带发现（同源偏差，不在本批 8 项内，未处置）**：`functions/driver/acpi.rs::lapic_base` 亦为真零引用，但因 `privileged/arch/x86_64/acpi.rs` 存在**同名局部变量** `lapic_base`（本次 D-9-6 修复引入）而被计入引用 ⇒ **从 R1 报告消失**。本次仅在 B-6 区块同步中移除其失效行（B-10.4），**其零引用事实另需 reviewer 定夺是否单列**（functions 层、非 TCB，可直接接线或登记）。

**B-10.4 B-6 区块同步（新增 4 / 移除 6；`434` 项）**

| 动作 | 项 | 依据 |
|---|---|---|
| 新增 | `char/serial.rs::send_str`、`char/vga.rs::write_string_at`、`fs/unkfs/txg.rs::add_free_to_open`、`::add_io_to_open` | B-10.1 / B-10.2（非 TCB，完整性保留） |
| 移除 | `arch/aarch64/uart.rs::switch_to_high_half` | **已接线**：`arch/aarch64/mod.rs:323` 调用 |
| 移除 | `freg/reset/layered.rs::test_recovery_status` | **已删除**（UT-07 孤儿清理，`layered.rs:121` 注释残迹） |
| 移除 | `fs/vfs/handle.rs::vfs_close_safe` | **已接线**：`privileged/tests/test_vfs.rs:191` / `:239` 调用（UT-07 收敛后生效） |
| 移除 | `proc/thread.rs::create_thread` | **被同名串遮蔽**：`proc/scheduler.rs:725` 注释出现 `ThreadManager::create_thread` ⇒ 不再计数为零引用（真零引用仍成立，另见 B-10.3 附带发现） |
| 移除 | `functions/driver/acpi.rs::lapic_base` | 同名局部变量遮蔽（见 B-10.3 附带发现） |
| 移除 | `fs/unkfs/txg.rs::get_syncing_txg` | **已被引用**：`host-tests/src/framekernel_bench.rs:1759` 调用 |

**B-10.5 实测计数（脚本正向复跑）**

- 脚本：`python3 scripts/audit_unwired_pub_fn.py` ⇒ `已分类清单 434 项` / `[HIGH] R1 未分类零引用 pub fn: **4 项**`（＝ B-10.3 的 kpti ×2 + kmalloc_slab ×2）/ 汇总 **`CRITICAL=7 / HIGH=4 / WARN=0 / INFO=435`**（`rc=0`；CRITICAL 7 为 R2 预存未接线 syscall，与本批无关）。

**B-10.6 R2 接线（分册 9 批次 3；R2 7 → 2 / B-6 移除 1 项 ⇒ `433`）**

> 来源：[audit-fix-09-hard-rules-deadcode.md](archive/audit-fix-09-hard-rules-deadcode.md) **B09-05**。R2 口径为「`types.rs` 声明 `SYS_*` 但 `functions/syscall/dispatch.rs` / `privileged/syscall/dispatch.rs` 文本未出现该常量名」⇒ 本轮 5 项以**接线 / 实装**消解（用户裁定：全部通过实现功能或修改代码解决）。

| 项 | 类别 | 处置 | 落点 |
|---|---|---|---|
| `SYS_sigaltstack` | 半实装未接线 | **接线** | `privileged/syscall/dispatch.rs::sys_sigaltstack` 早已存在（含 `SS_DISABLE`/`SS_ONSTACK` 状态机），仅缺 functions 分发臂 ⇒ `dispatch_proc` 加 `SYS_sigaltstack => signal::sigaltstack_syscall` |
| `SYS_fdatasync` | 未实装（同族近似） | **接线（复用）** | 与 `fsync` 同语义复用 `fs::misc::fsync_syscall`（VFS 整体同步，无数据/元数据区分，与 `fsync` 的 SIMPLIFIED 口径一致） |
| `SYS_arch_prctl` | **原裁定 ENOSYS 保留，本轮改判实装** | **最小实装** | privileged 新增 `sys_arch_prctl`（`ARCH_SET_FS` 归档 `Process.tls_base` + 当前进程立即写 `MSR_FS_BASE`；`ARCH_GET_FS` 经 `raw::write_u64_to_user` 回读）；functions `proc::clone::arch_prctl_syscall` 代理 |
| `SYS_capget` / `SYS_capset` | **原裁定 ENOSYS 保留，本轮改判实装** | **ABI 映射层实装** | functions `sgeg::auth::{capget,capset}_syscall` 建 sgeg↔Linux cap ABI 映射（`_LINUX_CAPABILITY_VERSION_3`，SYSTEM 域 64 位 ↔ 2×`cap_data`）；capset 经 privileged 新增受约束 setter `sgeg::api::pwm_set_current_capability_raw`（**子集 + 不破 `VIABLE_FLOOR`**，不接受 pwm 参数 ⇒ 无法跨进程篡改） |
| `SYS_process_vm_readv` / `SYS_process_vm_writev` | **原裁定「不实装 · 上报」，本轮改判实装** | **相对完整实装** | privileged 新增跨进程用户内存 safe 代理（`mm::cross_process`：`copy_{from,to}_user_in_mm` 逐页翻译目标 CR3 + 写权限校验 + SAFETY）；functions 新建 `proc::process_vm`（iovec 解析 + 权限判定 + 分块拷贝 + 全错误分支，0 unsafe）；dispatch 接线；详见 **B-10.12** |

- **实测（脚本正向复跑）**：`python3 scripts/audit_unwired_pub_fn.py` ⇒ 汇总 **`CRITICAL=2 / HIGH=4 / WARN=0 / INFO=434`**（`rc=1`，仅 CRITICAL 非零）。R2 **7 → 2**（余 `process_vm_readv` / `process_vm_writev`；**余 2 项已于 B-10.12 实装核销**）。
- **B-6 区块同步**：`functions/proc/signal.rs::sigaltstack_syscall` 接线后不再零引用 ⇒ 移除该行 ⇒ 清单 **434 → 433**（脚本 INFO 同期 435 → 433，差额 1 项为已接线移出 + 1 项为 HIGH/INFO 分级口径差）。
- **新增 pub fn 未进 R1**：本轮新增 `proc::clone::arch_prctl_syscall` / `sgeg::auth::{capget,capset}_syscall` 均**已被 dispatch 引用**，非零引用，无需登记 B-6。
- **原裁定溯源**：`capget`/`capset`/`arch_prctl` 此前按「无凭证能力模型 / 无 arch 相关用户态需求」判 **ENOSYS 保留**（见本台账「实现路径裁定」段）；批次 3 经用户裁定改为实装，**原判定作废**，相关 ENOSYS 回退说明同步失效。

**B-10.7 R4 实装（分册 9 批次 4；R4 1 → 0 / B-6 区块不变 ⇒ `433`）**

> 来源：[audit-fix-09-hard-rules-deadcode.md](archive/audit-fix-09-hard-rules-deadcode.md) **B09-08**。R4 口径为「`mod.rs` / `api.rs` / `types.rs` 中 pub struct/enum 零跨文件引用且总引用 ≤ 1」⇒ 唯一项 `DomainFlags` 经用户裁定**实装**（非删除）消解。

| 项 | 类别 | 处置 | 落点 |
|---|---|---|---|
| `DomainFlags`（`privileged/sgeg/types.rs`） | R4 零引用核心类型 | **实装为域级行为门控** | privileged 新建 `proc/domain.rs`（6 门控位判定表 + `domain_gate_check` 咽喉点 + `domain_flags_get`/`set` 状态读写）；`Process` 新增 `domain_flags: AtomicU32`（fork 全量继承）；`syscall_dispatch_impl` 在 seccomp 之后调用门控；functions 新建 `sgeg/domain.rs` 策略（set 需新增 `SYSTEM_CAP_SET_DOMAIN_FLAGS` 鉴权）；新增 syscall `SYS_SGEG_GET_DOMAIN_FLAGS(414)` / `SYS_SGEG_SET_DOMAIN_FLAGS(415)` 并接线 |

- **新增 syscall 编号记录**：`SYS_SGEG_GET_DOMAIN_FLAGS = 414` / `SYS_SGEG_SET_DOMAIN_FLAGS = 415`（**EG 独有功能** —— Linux 无对应 syscall，沿用 `SYS_SGEG_*` 族既有编号段；`416-419` 仍为保留号）。二者**均已 dispatch**，非 R2 范畴；编译期 `PRIVATE_NUMS` 断言同步追加。
- **实测（脚本正向复跑）**：`python3 scripts/audit_unwired_pub_fn.py` ⇒ 汇总 **`CRITICAL=2 / HIGH=4 / WARN=0 / INFO=433`**。R4 **1 → 0**；`扫描 SYS_/EG_ 编号 231`（批次 3 为 229，+2 ＝ 本轮新增）且 `已 dispatch 229 → 231` 全部命中。
- **B-6 区块同步**：**无需同步**（清单仍 `433` 项）——本轮新增的 `proc/domain.rs::{domain_gate_check,domain_flags_get,domain_flags_set}` 与 `functions/sgeg/domain.rs::{domain_flags_get_syscall,domain_flags_set_syscall}` **均已被引用**（前者被 privileged dispatch / functions 策略消费），非零引用，不入 B-6。

**B-10.8 裁定落地（七次修订：TCB 4 项 + 安全面 2 项）**

| 项 | 裁定 | 落地 |
|---|---|---|
| `kpti_aarch64.rs::kpti_kernel_ttbr0` / `::kpti_user_ttbr0` | B-10.3 候选 ② **删除** | **扩至整族删除**：4 getter（+ 同族已在 B-6 区块的 `kpti_kernel_ttbr1` / `kpti_trampoline_ttbr1`）+ `kpti_enter_kernel` / `kpti_exit_to_user` 对。**机制本身保留**（`KPTI_GLOBALS` 由异常入口/出口汇编按 `offset_of!` 锁定偏移直读；`kpti_is_active` / `kpti_set_user_ttbr0` / `kpti_init` / `kpti_trampoline_ttbr1_or_kernel` 全在用）。**B 条件（符号级零消费）已核实**：全仓无 `.S` / `.ld` / `global_asm!` 按名引用上述各项 |
| `kmalloc_slab.rs::slab_kmalloc` / `::slab_kfree` | B-10.3 候选 ② **删除**（分册 9 项 2 档 1 裁定） | **整文件删除**（含同族 `slab_init` —— 三者全仓均零调用者）。原「3 文件 / 5 入口」口径经只读复核**收窄为 1 文件**：`slab.rs` 的 `slab_system_init` / `slab_alloc` / `slab_free` 为 `#[unsafe(no_mangle)] pub extern "C"` ⇒ **FFI 面**，按「三合一」判据**非删候选**（且删除会连带 `SLAB_INITIALIZED` / `find_general_cache_index` 级联，并使 procfs slab 统计来源结构性空置）；`alloc/slab_alloc.rs` 唯一引用为 `prelude.rs` re-export ⇒ **API 面**，亦不删。逐项见 **B-10.10** |
| `functions/fs/ramfs.rs::split_path` / `::validate_path` | 定型「冗余」+ reviewer 授权 ⇒ **删除** | 三合一判据全过（零引用 + 下层 VFS 等价校验 + functions 层非 API/FFI/硬件原语面）；连带 `VFS_MAX_PATH` / `alloc::string::String` 去接线、host-tests `td18` 源文本下界 10 → 8 |

- **B-6 区块同步**：移除 4 行（`kpti_aarch64` ttbr1 ×2 + `ramfs` ×2）⇒ 已分类清单 433 → **429** 项（脚本实测口径）。
- **门槛**：双架构 0w0e / fmt / clippy pedantic（lib + `kernel_test` + `host-test` 三维）/ 核心审计（SAFETY 1937 → **1933**，覆盖仍 100%）/ host-tests 全绿 / kernel-host **817 passed 0 failed** / QEMU `kernel_test` exit 33 —— 全过。
- **R1 实测（脚本正向复跑）**：`已分类清单 429 项` / 汇总 **`CRITICAL=2 / HIGH=2 / WARN=0 / INFO=429`**（`rc=1`，CRITICAL 2 为 R2 预存 `process_vm_*`，与本批无关；HIGH 4 → 2，余 2 项为 `kmalloc_slab.rs` 孤岛）。**零引用事实随删除同步核销**，非仅改分级。（该 CRITICAL 2 已于 **B-10.12** 实装核销）
- 对 B-9.3 的时序说明：B-9.3 记录的「HIGH 0 / INFO 435 / 清单 436」为**甲批后基线**；其后 UT-07 收敛、批次 3/4 接线实装与本次 D-9-6 改动使若干条目接线 / 遮蔽 / 消失，故本期基线为「HIGH 2 / INFO 429 / 清单 429」。

**B-10.9 项 5 批 B — B09-21 豁免面收窄（按文件名 → 按路径；+67 项）**

> 来源：[audit-fix-09-hard-rules-deadcode.md](archive/audit-fix-09-hard-rules-deadcode.md) **B09-21**。原 `EXEMPT_FILENAMES` **按文件名**豁免 ⇒ 全仓**任意同名文件**（`mod.rs` / `api.rs` / `types.rs` / `lib.rs` / `prelude.rs` / `dispatch.rs`）内的一切 `pub fn` 均被静默豁免，构成**结构性漏报面**。

| 阶段 | 动作 | 实测 |
|---|---|---|
| 收窄前 | 豁免面 = 108 文件（上述六种基名），内含 **902 个 `pub fn` 定义**，B-6 区块内该类条目数 **0**（从未甄别） | HIGH = 2 / INFO = 429 / 清单 429 |
| 收窄 | `EXEMPT_FILENAMES`（按文件名）→ `EXEMPT_PATHS`（按**具体路径**，仅保留 `privileged/prelude.rs` + 两个 `syscall/dispatch.rs`）；`is_exempt_function` 改按 `relative_to(ROOT)` 比对；脚本头部豁免清单同步改写 | HIGH = **69**（＝ 2 + 67） |
| 登记 | 暴露的 67 项按排序并入 **B-6** 区块（429 → **496**，无重复、保持字典序） | HIGH = **2** / INFO = **496** / 清单 **496** |

- **67 项分布**：`mod.rs` 53 / `types.rs` 9 / `api.rs` 5（`prelude.rs` 与 `dispatch.rs` 无命中；smoltcp 子树已排除）。
- **口径更正（本次实测）**：首次度量误用 `cross_file == 0` 得 115 项 —— R1 真实口径为 `actual_callers == 0`（即 `total == 1`，仅声明自身）⇒ 正确值为 **67 项**。差值 48 项为「同文件内有多处引用但跨文件为 0」者（如 `privileged/sync/mod.rs` 内 `pub(crate) mod raw` 的 helper，由同文件调用，非零引用）。
- **代表性条目**：`privileged/klog/mod.rs::{log_warn,log_debug,log_crit,klog_set_level,klog_get_level}`、`privileged/mm/mod.rs::{is_dirty,set_dirty,is_accessed,set_accessed,is_nx,set_nx}`、`privileged/pci/mod.rs::{set_ecam_base,get_ecam_base,get_device_list,find_by_vendor,find_device}`、`functions/proc/mod.rs::{pid_new,pid_raw,tid_new,tid_raw,...}`、`functions/egdf/mod.rs` 10 项、`privileged/driver/virtio/mod.rs` 4 项、`privileged/idt/types.rs::{error_code_flags,dump_registers,set_handler}`。
- **失败关闭不变**：`load_classified_set()` 的 fail-closed 六条路径未改动；区块行格式校验（`^src/[^\s:]+\.rs::\w+$`）与唯一性校验保持。
- **待办（批 C）**：67 项逐项收敛 —— 原估三条路径（内部实现改 `pub(crate)` / 无价值者删除 / API 面保留登记）中的**「改 `pub(crate)`」经批 C 实测判定为不可行**（触发 `dead_code` ⇒ 违 F5；补 allow 又违 F9），仅余**删除**与**保留 `pub` 并登记**两类；**已于批 C 执行完毕**（21 删 / 46 留），见 **B-10.11**。

**B-10.10 项 2 档 1 — `kmalloc_slab.rs` 零引用孤岛整文件删除**

> 来源：[audit-fix-09-hard-rules-deadcode.md](archive/audit-fix-09-hard-rules-deadcode.md) **B09-21**（B-10.3 表中同项条目）。本轮经只读复核后按「三合一」判据**收窄删除面**，仅处置真孤岛。

| 阶段 | 内容 | 实测 |
|---|---|---|
| 删除前 | `privileged/mm/kmalloc_slab.rs`（126 行；3 个 `pub fn` 全零调用者：`slab_init` / `slab_kmalloc` / `slab_kfree`；1 个 `#[cfg(test)]` 用例 `test_cache_index_selection`） | HIGH = **2** / INFO = 496 / 清单 496 |
| 删除后 | 整文件删除 + `mm/mod.rs` 模块声明移除 + host-tests 源文本用例与注释连带 + 审计脚本名单/守卫清理 | HIGH = **0** / INFO = 496 / 清单 496 |

- **删除面只读复核结论（推翻原估）**：
  - `slab_init` 之所以**未**被脚本报为 HIGH —— 同文件文档注释含该名，脚本按 `rg -c` 计**行数** ⇒ `total=2` ⇒ `actual_callers=1`，构成**同名遮蔽漏报**（与 B-10.3 记录的 `functions/driver/acpi.rs::lapic_base` 同类；该盲区机制本批未改，仍是已知残余风险）。
  - `slab.rs` 的 `slab_system_init` / `slab_alloc` / `slab_free`：`#[unsafe(no_mangle)] pub extern "C"` ⇒ **FFI 边界导出**，脚本本就豁免；按「三合一」判据（须「非 API/FFI/feature/硬件原语面」）**非删候选**。若删，将连带 `SLAB_INITIALIZED`（唯一写入点）与 `find_general_cache_index`（唯一 Rust 调用点，仅余源侧用例）沦为死代码，并使 `/proc/slabinfo`（`functions/fs/procfs_core.rs`）与 `/proc/meminfo` Slab 行的唯一数据来源**结构性空置**（`GENERAL_CACHES` 目前仅由零调用的 `slab_system_init` 写入，运行期已恒为空）⇒ 属 FFI 面与能力面变更，需另立裁定（裁定六）。
  - `alloc/slab_alloc.rs` 的 `SlabAlloc` trait 与 `KmallocSlabAlloc`：全仓唯一引用为 `privileged/prelude.rs` re-export ⇒ **API 面**，不删。
- **连带清理**（均为本次删除直接导致，非工程外）：`privileged/mm/mod.rs` 去 `pub mod kmalloc_slab;`；`host-tests/tests/kmalloc_irq_save_test.rs` 删 `kmalloc_slab_source_uses_irq_save_flags_signature`（`include_str!` 该文件，否则编译失败）+ 头部条目与注释改写；`scripts/audit_c_naming.py` 去 `LEGACY_KMALLOC_NAMES` 的 `slab_kmalloc` / `slab_kfree` 与 `mm/kmalloc_slab` 路径判据；`scripts/audit_coupling.py` 去 `privileged::mm::kmalloc_slab` 守卫模式；`privileged/tests/test_new_features.rs` 顶部 UT-07 注记追加后续处置。
- **B-6 区块同步**：**无需同步**（清单仍 **496** 项）—— 本轮删除的 2 项原为 **HIGH（未入块）**，故区块计数不变；裁定询问稿中「清单 496 → 494」系误估，已按脚本实测订正。
- **门槛**：双架构 0w0e（`Passed 5 / Failed 0`）/ fmt `--check` 0 差异 / clippy pedantic（lib + `kernel_test` + `host-test` 三维）/ 核心审计 quick exit 0（0 处 `✗`）/ host-tests 全绿 / kernel-host **817 → 816 passed 0 failed**（核销 1 个源侧用例）/ QEMU `make` + `make test-unit` **exit 33（ALL TESTS PASSED）** —— 全过。
- **R1 实测（脚本正向复跑）**：`已分类清单 496 项` / 汇总 **`CRITICAL=2 / HIGH=0 / WARN=0 / INFO=496`**（`rc=1`，CRITICAL 2 为 R2 预存 `process_vm_*`，与本批无关）—— **HIGH 首次清零**。（该 CRITICAL 2 已于 **B-10.12** 实装核销）

**B-10.11 项 5 批 C — 67 项逐项分流（21 项删除 / 46 项保留登记；496 → 475）**

> 来源：[audit-fix-09-hard-rules-deadcode.md](archive/audit-fix-09-hard-rules-deadcode.md) **B09-21**。批 B「豁免按文件名 → 按路径」收窄后，新暴露 67 项「零引用 pub fn」（批 B 前被文件名豁免静默吞掉，见 **B-10.9**）。本批按裁定「逐项分流」路径处置：先出逐项判定表，再施工。

**B-10.11.1 决定性约束 — 「内部化」路径技术上不可行（实测）**

| 路径 | 实测结果 | 结论 |
|---|---|---|
| `pub` → `pub(crate)`（零引用项） | 以 `privileged/klog/mod.rs::klog_get_level` 试改后 `cargo check --release` 报 `warning: function klog_get_level is never used`（`#[warn(dead_code)]` 默认开启） | 违反 **F5（0 warning）**；实验已回退（仓库 clean） |
| 补 `#[allow(dead_code)]` 消警告 | **F9** 零容忍禁止任何类型死代码 allow（无豁免、无例外） | 不可用 |
| 结论 | 批 C 原定「可见性收窄/内部化」**无合法路径**，仅余**删除**与**保留 `pub` 并登记**两类处置 | 本批据此分流 |

- **旁证**：`audit_unwired_pub_fn.py` 的 `collect_pub_fns` 正则本身**会**匹配 `pub(crate) fn`（`pub(?:\([^)]+\))?`），故「改窄可见性以退出 R1」亦不成立——名字仍会被计入 R1；B-6 区块 496 项实测亦**全为 plain `pub`**（`pub(crate)` 0 项）。

**B-10.11.2 三桶分流（67 项；判据＝「三合一」：①该构建维零引用 ②存在能力等价的公共入口 ③非 API/FFI/feature/硬件原语面）**

| 桶 | 项数 | 处置 | 构成 |
|---|---|---|---|
| A 可删 | **21** | 删除 | 见 B-10.11.3 |
| B1 保留（删之生孤儿） | 10 | 保留 `pub` + 留块登记 | `functions/egdf::{find_by_proto, find_net_device, char_write, char_read}`；`functions/proc::priority_from_u32`；`functions/syscall::dispatch_from_ctx_typed`；`functions/debug::kgdb_is_active`；`functions/net::{start_dhcp, static_ip, reset_state}` —— 均为其 privileged 对应项的**唯一调用方**，删除将使 privileged 侧入口失去全部调用链（新 R1 孤儿） |
| B2 保留（无等价活入口 / API·机制面） | 27 | 保留 `pub` + 留块登记 | `klog::{log_warn, log_debug, log_crit, klog_set_level, klog_get_level}`（`MIN_LEVEL` 私有，无等价入口）；`egdf::{driver_as_mut, egdf_with_device_map}`；`sgeg::{umask_get, to_uid, get_creator_pwm}`；`debug::kgdb_break_now`；`driver::list_devices`；`irq::{register_tasklet, schedule_tasklet}`（`TASKLETS` 私有）；`proc/types::{thaw_target_state, set_user_mode}`；`smp::broadcast_reschedule`；`pci::{register_scanner, set_ecam_base, get_ecam_base, get_device_list, find_by_vendor, find_device}`；`net/api::{init_network_now, status_snapshot}`；`wasm::{as_i64, wasi_function_table}` |
| B3 保留（硬件原语面，判据③不满足） | 9 | 保留 `pub` + 留块登记 | `virtio::{set_status, read_config64}`；`idt/types::{error_code_flags, dump_registers, set_handler}`；`fs/vfs/types::inode_arc`；`driver/storage::xhci_read_trb`；`fs/devfs::{is_virtual, is_physical}` |

**B-10.11.3 桶 A 已删除 21 项（含必删连带）**

| 组 | 项 | 等能力活入口（证据） |
|---|---|---|
| A1（6） | `privileged/mm/mod.rs`：`is_dirty` / `set_dirty` / `is_accessed` / `set_accessed` / `is_nx` / `set_nx` | 同文件 `flags()`（L701）+ `set_flags()` + `PageFlags::{ACCESSED, DIRTY, NX}` —— 能力完全重叠；活调用点 20 处（`vmm_x86_64.rs`） |
| A2（2） | `privileged/driver/virtio/mod.rs`：`setup_vq` / `setup_vq_legacy` | 细粒度组合 `select_queue` + `setup_queue_addrs` / `setup_queue_legacy` + `set_queue_ready`（`functions/driver/virtio/{net,blk}.rs` 生产路径已用后者） |
| A3（1） | `privileged/egdf/mod.rs::egdf_device_list` | 纯别名转调 `egdf_list`（`privileged/driver/mod.rs` 活） |
| A4（6） | `functions/egdf/mod.rs`：`find_by_id` / `count_by_proto` / `blk_name` / `blk_info` / `input_read` / `input_has_data` | 其 privileged 对应项**另有独立活调用点**（`composite.rs` / `lib.rs` / `block.rs` / `keyboard.rs`）⇒ 删 wrapper 不产孤儿 |
| A5（6） | `functions/proc/mod.rs`：`init_per_cpu` / `scheduler_ready` / `pid_new` / `pid_raw` / `tid_new` / `tid_raw` | privileged 侧另有活入口（`init_per_cpu_sched` / `SCHEDULER_READY` 为 pub static）；`ProcessId(pub Pid)` / `ThreadId(pub Tid)` 字段公开 ⇒ `ProcessId(x)` / `x.0` 直接可用 |

- **必删连带（本次删除直接导致的死代码，非工程外）**：`privileged/driver/virtio/mod.rs` 的 `const QUEUE_NUM: usize = 0x038` —— 唯一使用点为 `setup_vq` / `setup_vq_legacy` 两函数体，删函数后成死常量（`dead_code` ⇒ F5），按 §9.3「硬件规范常量须通过实现使用路径消除」随之删除；同时移除两函数各自的 `#[expect(clippy::unnecessary_wraps)]`（避免 `unfulfilled_lint_expectations`）。
- **未涉及**：`QUEUE_PFN`（`setup_queue_legacy` 仍用）、`QUEUE_READY`（`set_queue_ready` 仍用）、`pub mod queue`（host-tests `framekernel_bench.rs` 消费 `VirtQueue` 等，仍被引用 ⇒ 无 R3 WARN）。
- **后续修订（MIG-004 边界治理批次）**：A4 中删除的 `functions/egdf::{input_read, input_has_data}`「能力等价公共入口」结论**不变**（privileged `egdf_input_read` / `egdf_input_has_data` 仍由 `privileged/driver/input/keyboard.rs` 独立调用）。MIG-004 在 `functions/egdf/proto.rs` **新建同名强类型安全代理**（与 `char`/`net`/`block` 协议族统一 `functions::egdf` 出口、去裸指针、含 host-tests 覆盖），属**新增 API 面**而非恢复旧 wrapper；本区块删除结论不受影响，详见 `docs/plan/unresolved-issues-2026-08-09.md` 的 MIG-004 结案条目。

**B-10.11.4 B-6 区块同步**

- 本批删除的 21 项**原已在块内**（批 B 新暴露并登记，`r1_classified_downgraded: 67`）⇒ 区块**逐项移除 21 条**：**496 → 475**（保持字典序、无重复、格式合法，已校验）。
- 其余 46 项**留块不动**（保留 `pub`；登记口径＝「零引用事实保留 + 未来功能 / API 面」）。

**B-10.11.5 门槛与 R1 实测**

- **门槛**：双架构 0w0e（`./ci/build.sh all` → `Passed 5 / Failed 0`）/ fmt `--check` 0 差异 / clippy pedantic（lib + `kernel_test` + `host-test` 三维）/ `./ci/audit.sh quick` **exit 0（0 处 `✗`）** / `make test-host` 全绿 / `make test-kernel-host` **816 passed 0 failed**（与上批持平，本批未增删源侧用例）/ QEMU `make` + `make test-unit` **exit 33（ALL TESTS PASSED）** —— 全过。
- **R1 实测（脚本正向复跑）**：`已分类清单 475 项` / 汇总 **`CRITICAL=2 / HIGH=0 / WARN=0 / INFO=475`**（INFO 496 → 475，**恰为 −21**）/ `R4 = 0` / `R3 = 0` —— 删除未引入任何新零引用项（HIGH / WARN / R4 均保持 0）。（该 CRITICAL 2 已于 **B-10.12** 实装核销）

**B-10.12 process_vm_readv / process_vm_writev 实装（R2 2 → 0，未接线 syscall 首次清零；B-6 区块不变 ⇒ `475`）**

> 来源：[audit-fix-09-hard-rules-deadcode.md](archive/audit-fix-09-hard-rules-deadcode.md) **D-2**（列于下方「未实装 syscall 处置」）。**原裁定「不实装 · 上报」**（须 ptrace 级权限模型，属裁定六安全面），本轮经用户裁定改判**实装**，取**相对完整**路径（路径/边界/错误分支闭合 + 单测覆盖）。**本条核销**上列 B-9.7 / B-10.6 / B-10.8 / B-10.10 / B-10.11.5 各实测记录中「CRITICAL=2 为 R2 预存 `process_vm_*` 待裁项」的悬挂标记。

- **架构方向**：**不切 CR3**（KPTI 下以用户 CR3 执行内核代码不安全），改为逐页经 `translate_in_pml4` 把目标地址翻译为 `PhysAddr`，再取 **HHDM 别名**访问；HHDM 对用户页 PTE 的 U/S=0，故 SMAP 不拦截，无需 `smap_begin` 或异常恢复点。
- **归属（§4.1 判据）**：机制（跨 CR3 页翻译 + 写权限位）留 privileged，新增 safe 代理 [privileged/mm/cross_process.rs](../../src/kernel/privileged/mm/cross_process.rs)（`copy_from_user_in_mm` / `copy_to_user_in_mm`；逐页翻译 + `PageTranslation.writable` 写校验 + 3 处 `// SAFETY:` + 4 内联单测；经 [mm/api.rs](../../src/kernel/privileged/mm/api.rs) 与 [mm/mod.rs](../../src/kernel/privileged/mm/mod.rs) 顶层 re-export）；功能（iovec 解析 / 权限判定 / 分块拷贝 / 错误分支，**0 unsafe**）落 functions 新建 [functions/proc/process_vm.rs](../../src/kernel/functions/proc/process_vm.rs)。
- **写权限校验**：`PageTranslation.writable`（[vmm_x86_64.rs](../../src/kernel/privileged/mm/vmm_x86_64.rs) 取 `PAGE_WRITABLE`；[vmm_aarch64.rs](../../src/kernel/privileged/mm/vmm_aarch64.rs) 取 AP[2] `(entry & (1 << 7)) == 0`）；写目标页不可写即 EFAULT（`copy_in_mm` 内 `write_to_target && !t.writable` 判定）。
- **权限判定**：`check_access` 三档——自身（pid == 当前）/ `pwm_check_privilege(current, target)` 特权 / uid 相等（`pwm_get_uid`）。**SIMPLIFIED 注释已标**（未实现 `/proc/pid/mem` 式 `PTRACE_MODE_ATTACH_FSCREDS` 完整判据；后续引入 LSM / 能力模型时在 `check_access` 处接入）。
- **复用**：local iovec 复用 [functions/fs/io.rs](../../src/kernel/functions/fs/io.rs) 的 `read_iovecs`（`pub(crate)` 化，`IOV_MAX` 同步 `pub(crate)`）；remote iovec 自建读取器（先读 `[u8;16]` 再按字节重组 u64，避免 functions 用 unsafe）。
- **错误面（全闭合）**：`flags != 0` → EINVAL；`iovcnt > IOV_MAX` → EINVAL；`iovcnt == 0` → `Ok(0)`；`pid <= 0` / 进程不存在（`process_try_inc_ref` 失败 / 无 pwm / 无 cr3）→ ESRCH；权限不足 → EPERM；局部失败已传字节 → 返回已传部分，否则 EFAULT（`partial_or`）；`drop` 经 `ProcRef` RAII 减引用。
- **接线**：[dispatch.rs](../../src/kernel/functions/syscall/dispatch.rs) use 列表 + `SYS_process_vm_readv` / `SYS_process_vm_writev` 分派臂（arm 消费第 6 参数 `a5` ⇒ `let [a0, a1, a2, a3, a4, a5] = args;`）。
- **测试**：functions 内联单测 4 项（`flags != 0` / `iovcnt > IOV_MAX` / 零 iovcnt / 无效 pid，均在触碰页表 / 调度器前失败）；privileged `cross_process` 内联单测 4 项；host 契约测试 6 项（[process_vm_wiring_test.rs](../../host-tests/tests/process_vm_wiring_test.rs)：mm 顶层 re-export / 写校验 + SAFETY / functions 0 unsafe + 4 拷贝原语 / 模块注册 / dispatch 接线 / iovec 助手可见性）。
- **门槛**：双架构 0w0e（`./ci/build.sh all` → `Passed 5 / Failed 0`）/ fmt `--check` FMT_OK / clippy pedantic 0 warning（x86_64）/ `./ci/audit.sh quick` exit 0（FP-06 通过）/ `make test-host` 全绿（含新 6 项）/ `make test-kernel-host` **836 passed 0 failed**（828 → 836，+8 = 4 process_vm + 4 cross_process）—— 全过。
- **R1 实测（脚本正向复跑）**：汇总 **`CRITICAL=0 / HIGH=0 / WARN=0 / INFO=474`**（`rc=0`，**R2 未接线 syscall 首次清零**）/ `R3 = 0` / `R4 = 0`。

#### B-11. per-process fd 表全量下沉 VFS 管理面（2026-09-27；B-9.5 专项落地）

> 来源：**B-8.3** 解锁条件 + **B-9.5** 独立登记条目（per-process fd 表 + FD_CLOEXEC 语义）+ **B-8.2** 的 `FdTable` 4 项 + **B-9.7** 全局表版最小件。用户裁定：**相对完整**路径 / **全量下沉 VFS 管理面** / CLOEXEC 标记来源**接线** / `OpenFileTable.next_id` 隐患**随本工程修正**。本条为上述各登记项的**兑现记录**（原判定快照保留于各条，不删）。

**B-11.1 范围（「全量下沉 VFS 管理面」边界）**

- **权威化**：`Process.fd_table(FdTable)` 成为 fd 表唯一权威（[_fd_table.rs_](../../src/kernel/privileged/proc/fd_table.rs)：`MAX_FDS_PER_PROCESS = 64`；`entries: IrqSpinLock<[u32; 64]>` 存 handle_id（`u32::MAX` 空闲）+ `cloexec: IrqSpinLock<[bool; 64]>`）。
- **获取路径**：新增 [`with_current_fd_table<F, R>`](../../src/kernel/privileged/proc/proc_ops.rs)（取当前进程 fd 表并施加闭包），`privileged/fs` 侧经此访问，**不新增 `fs → proc` 内部 use**（F2 合规）。
- **落地面**：open / close / dup / dup2 / fcntl / read / write / lseek / readdir / truncate / fstat / fchmod / fchown / sendfile / epoll / poll / close_range / mmap / memfd 全接线 + fork / exec / exit 接线。
- **退役**：全局 `VfsManager.fd_table`（含 `VfsFile.cloexec` 最小件、`set_fd_cloexec`/`get_fd_cloexec`）**删除**。
- **排除面（用户裁定「只登记不处置」）**：另 6 套并行子系统 fd 表、`cwd`/`root`/`umask`、`CLONE_FILES` —— 登记入 [audit-fix-09](./archive/audit-fix-09-hard-rules-deadcode.md) **D-10**，本批不动。

**B-11.2 CLOEXEC 全链路兑现（B-8.3 五环闭合）**

- **标记来源**：`fcntl` `F_GETFD` → `FdTable::is_cloexec`、`F_SETFD` → `FdTable::set_cloexec`（[privileged/syscall/io.rs](../../src/kernel/privileged/syscall/io.rs)；`F_GETFL` 改源 `OpenFile::get_flags`）。
- **打开时携带**：[`vfs_open_internal`](../../src/kernel/privileged/fs/vfs/handle.rs) 两分支解析并**剥离** `VfsOpenFlags::CLOEXEC`，经 `with_current_fd_table(|t| t.alloc_fd(handle_id, cloexec))` 入表。
- **存储位**：`FdTable.cloexec`（替代原全局 `VfsFile.cloexec`）。
- **exec 时关闭**：[`vfs_close_cloexec_fds`](../../src/kernel/privileged/fs/vfs/handle.rs) 经 `FdTable::get_cloexec_fds` **先收集再逐个 `vfs_close_internal`**（避免持 `fd_table` 锁递归自锁死），在 [`proc_exec_replace`](../../src/kernel/privileged/proc/proc_ops.rs) 的 `reset_signal_state_on_exec` 之后接线。
- **fd 命名空间**：改 per-process；**fd 跨进程不再可见**（POSIX per-process 语义隔离）。
- **memfd**：[`memfd_create_syscall`](../../src/kernel/functions/proc/memfd.rs) 依 `MFD_CLOEXEC` 置位经同一 `alloc_fd(handle_id, cloexec)` 路径。

**B-11.3 顺带修正（用户裁定「随本工程」）**

- **dup2 上限**：[`vfs_dup2`](../../src/kernel/privileged/fs/vfs/handle.rs#L815-L820) 的 `new_usize >= MAX_FDS_PER_PROCESS` 检查，**256 → 64**（原 `256` 硬编码与 per-process 表容量不符，会误放行 `64..256` 的越界 newfd）。
- **`OpenFileTable` 句柄泄漏**：[open_file_table.rs](../../src/kernel/privileged/fs/vfs/open_file_table.rs) `alloc` 由原 `next_id` 单调递增改 **first-fit 复用空闲槽位**（原实现单调递增不回收 ⇒ 系统级 `OpenFile` 槽位泄漏；`MAX_OPEN_FILES = 256` 不变）。
- **`VFS_MAX_FDS` 退役删除**：原全局 fd 表尺寸常量随之删除，上限唯一由 `MAX_FDS_PER_PROCESS` 承载（[vfs.rs](../../src/kernel/privileged/fs/vfs/vfs.rs) 头注释已声明）。
- **`functions/fs/process_fd_table.rs` 删除**：Plan B 并行 fd 表（B-9.5「能力冗余线索」）经复核**未采用**，以 `FdTable` 为权威实装 ⇒ 文件删除，该线索终结（台账 B-5「删/接线待裁」同步核销）。

**B-11.4 门槛（§2.3 六条 + fmt + 审计，全绿）**

| # | 门槛 | 结果 |
|---|---|---|
| 1 | `./ci/build.sh all` | ✅ `Passed: 5 / Failed: 0`（x86_64 + aarch64 + host-tests + link） |
| 2 | clippy pedantic `-D warnings` | ✅ 0 warning（lib + `kernel_test` + `host-test` 三维） |
| 3 | `./ci/audit.sh quick` | ✅ `EXIT=0`（经 `./ci/build.sh aarch64` 收尾以避 FP-06） |
| 4 | `make test-host` | ✅ 退出 0 |
| 5 | `make test-kernel-host` | ✅ **834 passed / 0 failed** |
| 6 | `./scripts/qemu_boot_test.sh x86_64` | ✅ `找到里程碑: 'VFS ready'` / `1/1 通过` |
| 附 | `cargo fmt --check`（kernel manifest） | ✅ `EXIT=0` |

**B-11.5 R1 实测与 B-6 区块同步**

- **B-6 区块（B09-21 数据源）**：移除 10 条因本工程「接线 / 删除」而**不再零引用**的条目（`vfs_get_fd_handle` 1 条 + `fd_table.rs` 4 条 + `get_current_process` 1 条 + Plan B `process_fd_table.rs` 4 条）⇒ 计数 **476 → 466**（区块头部原记 475 为前批计数笔误，已订正）。
- **脚本正向复跑**：`已分类清单 (B09-21 数据源): 466 项` / `[HIGH] R1 未分类零引用 pub fn: 0 项` / `[INFO] R1 已分类: 464 项` / 汇总 **`CRITICAL=0 / HIGH=0 / WARN=0 / INFO=464`**（466 − 2 = 464；余 2 条**既有偏差** `privileged/timer/tickless.rs::enter_tickless` / `::exit_tickless` 因内联测试即为引用方而恒不入 INFO 集，**非本工程引入**，不属本批处置面）。
- **R3 / R4**：`0` / `0`。

**B-11.6 测试覆盖**

- **内联单测**：`FdTable` first-fit 分配 / `get_entry` / `set_fd_at` / `copy_from` / `close_fd`（原子 claim-and-clear）/ `is_cloexec` / `set_cloexec` / `get_all_fds` / `get_cloexec_fds`；`OpenFileTable` first-fit 复用。
- **host 契约测试**：[fd_cloexec_test.rs](../../host-tests/tests/fd_cloexec_test.rs)（先收集后关闭顺序 / 顶层 re-export / exec 接线 / memfd 置位）、[fs_permissions_regression_test.rs](../../host-tests/tests/fs_permissions_regression_test.rs)（新增静态契约 `poll_syscall_validates_fd_via_per_process_fd_table`）、td03_atomic_close_test.rs、plan_b_inode_test.rs。
- **fmt 噪音处置**：96 个与本工程无关的 `host-tests/` 纯 fmt 文件按用户裁定**还原至 HEAD**（其中 3 个含本工程语义改动的文件已按新架构重写语义断言，非盲还原）。

#### B-12. 热插拔运行时接线批次（方案 B：相对完整实现）

> 来源：遗留调研结论「热插拔**基础设施已就位但运行时链路未接通**」（8 处断点）。用户裁定：**方案 B（相对完整实现）**；结构决策 **D1＝块设备移除协议迁移到 EGDF**、**D2＝检测 + 分发 + 日志**。本条为兑现记录。

**B-12.1 背景（8 处断点）**

`HotplugManager`（`slots`/`listeners`/`initialized` 三锁 + `HotplugEvent`/`HotplugListener`）与 PCIe Slot Capability/`PcieHotplugSlot::probe` 早已存在，但无运行时链路：① 无 softirq 唤醒；② 无调度 tick 触发；③ 无 functions 侧总线重枚举回调契约；④ 无 EGDF 块设备移除协议；⑤ UNKFS 监听器以端口 `location.slot` 误当 drive 号（语义错位）；⑥ storage 侧无重扫描；⑦ USB 端口变化无检测；⑧ USB 控制器无自持注册。本批接线使之端到端可用。

**B-12.2 架构关键决策（用户裁定）**

- **D1：块设备移除协议迁移到 EGDF**（[privileged/egdf/mod.rs](../../src/kernel/privileged/egdf/mod.rs)）。新增 `BlockDevice` trait（**定义在 egdf 而非 driver 模块，避免循环依赖 F3**）+ `egdf_register_block_dev` / `egdf_unregister_block` / `egdf_blk_read` / `egdf_blk_write` / `egdf_blk_is_present` / `egdf_blk_drives` / `egdf_blk_is_removed`。**墓碑（tombstone）语义**：移除**不物理删除** `EGDF_DEVICES` 槽位，改置 `state = Removed` + `block_dev = None`，保证已下发 `drive` 句柄索引稳定（`AHCI_CONTROLLERS`/`NVME_CONTROLLERS` 为 `Vec` 且 `AhciBlockDevice`/`NvmeBlockDevice` 缓存 `controller_index: usize`，**不得从中间删除**）。`block_device_state` 迁移至 [privileged/driver/block.rs](../../src/kernel/privileged/driver/block.rs)（`(hdd_is_present, egdf_blk_is_removed, 0)`）。
- **D2：检测 + 分发 + 日志**。USB 端口变化扫描 → 构造 `DeviceLocation` → `HOTPLUG_MANAGER.dispatch()`；storage 侧重扫描 + 分发（`drives_for_location`）；UNKFS 监听器逐 drive 认领/移除 + `slog_info!` 记录。

**B-12.3 privileged 侧接线**

- **softirq（kswapd 先例模式，Edgine 无 kthread 抽象）**：`SoftirqVec::Hotplug = 8`（[irq/mod.rs](../../src/kernel/privileged/irq/mod.rs)；`MAX_SOFTIRQS = 10`）。`hotplug_wakeup()`（pending 标志去重 + `raise_softirq`）**替代原 `hotplug_poll`**（原函数已不存在）；`hotplug_softirq_handler` = 清 pending → `HOTPLUG_MANAGER.poll()` → `dispatch_aux_poll()`；调度 tick 周期 `HOTPLUG_TICK_INTERVAL = 100` 调 `hotplug_wakeup()`（[proc/scheduler.rs](../../src/kernel/privileged/proc/scheduler.rs#L1176-L1177)）。
- **DECISION-K 回调契约**（functions → privileged 单向注册无捕获函数指针，未注册 fail-quiet）：`register_reenum_hook`（总线重枚举，**先于**监听器分发）+ `register_aux_poll`（辅助轮询）。
- **`HotplugManager::dispatch`**（[driver/hotplug.rs](../../src/kernel/privileged/driver/hotplug.rs#L166)）：先 `dispatch_reenum(event)`，再 `listeners` 锁内按变体调 `on_device_added`（认领返回 `bool`）/ `on_device_removed`。

**B-12.4 functions 侧接线**

- **storage**（[functions/driver/storage/mod.rs](../../src/kernel/functions/driver/storage/mod.rs)）：注册 `storage_reenum_hook`（L631）重扫描控制器，重插经 `install_controller(..., slot_hint, ...)` **复用槽位**；新增 `drives_for_location(location) -> Vec<u8>`（位置 → 已注册块设备，x86_64 实装 + aarch64 恒空 stub）；移除路径 `remove_stale_controllers`（L409）经 `egdf_unregister_block` 墓碑化。
- **UNKFS**（[functions/fs/unkfs/unkfs.rs](../../src/kernel/functions/fs/unkfs/unkfs.rs)）：`on_device_added` 仅认领 `DeviceAdded`，逐 `drives_for_location(location)` 调 `hotplug_add_disk`；`on_device_removed` 处理 `DeviceRemoved | SurpriseRemoval` 调 `hotplug_remove_disk`（**原 `location.slot` 直用为语义错位，本批修正**）。
- **USB**（[functions/driver/usb/mod.rs](../../src/kernel/functions/driver/usb/mod.rs)）：`static USB_CONTROLLERS: Mutex<Vec<&'static mut XhciController>>`；`usb_init` 改为 `Box::leak` 自持控制器 + `egdf_register("xhci", EGDFProto::Bus, ...)` + `register_aux_poll(usb_port_poll)`；`usb_port_poll` 扫描 PORTSC 变化位（`CSC|PEC|OCC|RC`）→ `ack_port_change`（读-改-写屏蔽 RW1CS 位，`PORTSC_RWS`，参照 Linux `xhci_port_state_to_neutral`，[usb/xhci.rs](../../src/kernel/functions/driver/usb/xhci.rs#L625)）→ **先释放 USB 锁再 `dispatch`**（避免持锁跨界）。

**B-12.5 测试**

- host 契约测试 [hotplug_reenum_contract_test.rs](../../host-tests/tests/hotplug_reenum_contract_test.rs)（2 passed / 0 failed）：① **EGDF 墓碑索引稳定**——交错注册（块/非块）→ 墓碑化块设备 → 读回存活设备数据 → 越界/非块/已移除边界；新注册**不回收墓碑槽位**；② **分发契约**——reenum hook 先于 listener；`DeviceAdded`/`DeviceRemoved`/`SurpriseRemoval` 分发计数正确。

**B-12.6 门槛（§2.3 六条，全绿）**

| # | 门槛 | 结果 |
|---|---|---|
| 1 | `./ci/build.sh all` | ✅ `Passed: 5 / Failed: 0`（x86_64 + aarch64 + host-tests + link） |
| 2 | clippy pedantic `-D warnings` | ✅ 0 warning（lib + `kernel_test` + `host-test` 三维） |
| 3 | `./ci/audit.sh quick`（经 `./ci/build.sh aarch64` 收尾以避 FP-06） | ✅ `Passed: 2 / Failed: 0` |
| 4 | `make test-host` | ✅ 退出 0（含新契约测试 2 passed） |
| 5 | `make test-kernel-host` | ✅ **949 passed / 0 failed** |
| 6 | `./scripts/qemu_boot_test.sh x86_64` | ✅ `1/1 通过`（VFS ready / e1000 / Ring 3 init / KPTI 断言） |

**B-12.7 R1 台账同步**

- 移除 **4 条**因本工程「接线 / 删除」而不再零引用的条目：`privileged/driver/hotplug.rs::hotplug_poll`（**函数已不存在**，由 `hotplug_wakeup` + softirq 轮询替代）、`functions/driver/usb/xhci.rs::ack_port_change`、`::has_port_change`（**已被 `usb_port_poll` 调用**）、`privileged/driver/block.rs::mark_removed`（**函数已不存在**，由 EGDF 墓碑协议 `egdf_blk_is_removed` 替代）⇒ **B-6 区块 462 → 458 项**（脚本正向复跑：`已分类清单: 458 项` / `HIGH 未分类: 49 项`，HIGH 均为**预存项**，本批新增/改动函数（`dispatch`/`register_reenum_hook`/`register_aux_poll`/`egdf_unregister_block`/`egdf_blk_drives`/`egdf_blk_is_removed`/`drives_for_location`/`ack_port_change`/`has_port_change`）**均已接线，未落 HIGH**）。

**B-12.8 SIMPLIFIED 登记（§12.3）**

- `is_present` 只比对 BDF, 不校验类码（保留）；移除后控制器槽位与 MMIO 映射**保留至重插复用**（不主动释放，保留）。
- SATA 盘插拔原「无 PCIe 事件 ⇒ 不追踪」**已兑现**：由 **B-13.3** 新增 AHCI 端口级轮询（`ahci_port_poll` + `BusType::Sata` 事件分发）消除。

#### B-13. B-12.8 遗留清理批次（用户裁定：全三项 ①②③ + 方案 B 相对完整）

> 来源：B-12.8 SIMPLIFIED 登记遗留两项 + 一处索引 bug。用户裁定 **全部三项一并处置**，实现深度 **方案 B（相对完整）**。本条为兑现记录。

**B-13.1 ① legacy `REGISTRY` 死路径清理**

- **新增 EGDF 多扇区 API**（[privileged/egdf/mod.rs](../../src/kernel/privileged/egdf/mod.rs)）：`egdf_blk_read_sectors(drive, start, count, buf)` / `egdf_blk_write_sectors(drive, start, count, buf)`——单次持 `EGDF_DEVICES` 锁完成整段读写；`buf` 不足 ⇒ `InvalidArgument`；越界 / 非块 / 缺实现 ⇒ `Io`；非 `Ready`（已墓碑化）⇒ `Busy`。补齐 UNKFS 之外 ext2/exFAT 的**多扇区**读写面。
- **迁移 ext2 / exFAT 全部块读写调用点**：ext2（[read.rs](../../src/kernel/functions/fs/ext2/read.rs) 4 处、[alloc.rs](../../src/kernel/functions/fs/ext2/alloc.rs) 10 处）与 exFAT（[read.rs](../../src/kernel/functions/fs/exfat/read.rs) 1、[dir.rs](../../src/kernel/functions/fs/exfat/dir.rs) 1、[fat.rs](../../src/kernel/functions/fs/exfat/fat.rs) 3、[alloc.rs](../../src/kernel/functions/fs/exfat/alloc.rs) 3）由 legacy `block::with_device` + `read_sectors`/`write_sectors` 改走 `egdf_blk_read_sectors`/`egdf_blk_write_sectors`。
- **删除 [block.rs](../../src/kernel/privileged/driver/block.rs) legacy 死路径**：`REGISTRY` / `DEVICE_NAMES` 两个 static + 7 个 pub fn（`register_named` / `register` / `with_device` / `registry` / `count` / `read_sectors` / `write_sectors`）及随之无用的 import（`KernelError` / `KernelResult` / `IrqSpinLock` / `Box`）。文件仅保留 EGDF 代理面（`hdd_*` / `block_device_*`）。

**B-13.2 ② `block_device_list()` 索引 bug 修复**

- [block.rs:52](../../src/kernel/privileged/driver/block.rs#L52) 由 `.filter(...).enumerate()` 改为 `.enumerate().filter(...)`，`map` 取**全局下标 `i`**（= EGDF 槽位 = `drive` 号）。原实现返回的是**过滤后序列的位置**，与 EGDF 下标错位 ⇒ 多块设备 / 存在非块设备时列表 drive 号错误。调用点：[privileged/driver/mod.rs:270](../../src/kernel/privileged/driver/mod.rs#L270)。

**B-13.3 ③ SATA 端口级热插拔（新功能）**

- **总线变体**：`BusType::Sata`（[hotplug.rs:37](../../src/kernel/privileged/driver/hotplug.rs#L37)）。
- **辅助轮询多槽化**：`HOTPLUG_AUX_POLL: Mutex<Vec<fn()>>`（[hotplug.rs:303](../../src/kernel/privileged/driver/hotplug.rs#L303)）+ `register_aux_poll(hook: fn())` 去掉返回值（原单槽 `OnceLock` 只能注册一条总线，与 USB 冲突）；去重 push；`dispatch_aux_poll()` 持锁只读遍历（[hotplug.rs:320](../../src/kernel/privileged/driver/hotplug.rs#L320)）。
- **AHCI 端口固定槽位**（[functions/driver/storage/ahci.rs](../../src/kernel/functions/driver/storage/ahci.rs)）：`init_controller` 无条件 push 全部已实现端口 ⇒ `ports` 下标恒等于 SATA 硬件端口号（不因在位状态改变槽位）；`AhciPort::drive()` / `set_drive(Option<u8>)` 维护端口 → EGDF `drive` 映射；`port_index_of(port_num)` / `bring_up_port` / `scan_ports() -> PortChanges { added, removed }`。
- **三相位轮询** `ahci_port_poll()`（[storage/mod.rs:552](../../src/kernel/functions/driver/storage/mod.rs#L552)，`#[cfg(target_arch = "x86_64")]`）：Phase 1 摘取 `PROBED` 在线 AHCI 控制器 `(slot, bus, device, function)`；Phase 2 持 `AHCI_CONTROLLERS` 逐控制器 `scan_ports()` 收集变化并 `set_drive` 回写；Phase 3 **锁外**分发——新增走 `register_block_device`（`dispatch(DeviceAdded)`），移除走 `egdf_unregister_block`（`dispatch(SurpriseRemoval)`）。`storage_init` 注册 `register_aux_poll(ahci_port_poll)`（[storage/mod.rs:779](../../src/kernel/functions/driver/storage/mod.rs#L779)）。
- **位置解析**：`drives_for_location` 新增 `BusType::Sata` 分支（[storage/mod.rs:511](../../src/kernel/functions/driver/storage/mod.rs#L511)）——经 `PROBED` 定位控制器槽位 → `AHCI_CONTROLLERS` → `port_index_of(location.slot)` → 端口 `drive`；`Usb | Virtio` 恒空；aarch64 恒空 stub（[storage/mod.rs:648](../../src/kernel/functions/driver/storage/mod.rs#L648)）。
- **锁顺序**（F8 合规）：合法链 `PROBED → EGDF_DEVICES → AHCI_CONTROLLERS`；禁止持 `AHCI_CONTROLLERS` 时锁 EGDF / PROBED（Phase 3 严格锁外分发）。

**B-13.4 测试**

- 内核单元：`test_ahci_port_drive_mapping` / `test_port_changes_default_empty`。
- host 契约 [ahci_port_hotplug_contract_test.rs](../../host-tests/tests/ahci_port_hotplug_contract_test.rs)（3 passed / 0 failed）：`sata_hotplug_event_routes_through_manager`（`BusType::Sata` 的 `DeviceAdded` / `SurpriseRemoval` + `DeviceRemoved` 分发计数）、`drives_for_location_empty_without_controller`（Sata / Usb / Virtio 无控制器返回空；**故意不触 Pcie 分支**——host 侧会执行真实 port I/O）、`ahci_port_hotplug_wiring_present`（静态源码断言）。

**B-13.5 门槛（§2.3 六条，全绿）**

| # | 门槛 | 结果 |
|---|---|---|
| 1 | `./ci/build.sh all` | ✅ `Passed: 5 / Failed: 0` |
| 2 | clippy pedantic `-D warnings` | ✅ 0 warning（lib + `kernel_test` + `host-test` 三维） |
| 3 | `./ci/build.sh aarch64` + `./ci/audit.sh quick` | ✅ `Passed: 2 / Failed: 0`；`AUDIT_EXIT=0`（I1–I6 全 PASS，SAFETY 1824/1824）；另单跑 deadlock/coupling 均 exit 0（各含预存项，非本批引入） |
| 4 | `make test-host` | ✅ 退出 0 |
| 5 | `make test-kernel-host` | ✅ **951 passed / 0 failed** |
| 6 | `./scripts/qemu_boot_test.sh x86_64` | ✅ `1/1 通过` |

**B-13.6 R1 台账同步**

- 实测 `已分类清单: 458 项` / `[INFO] R1 已分类 415 项` / `[HIGH] 未分类 49 项`。以 HEAD 独立 worktree 对跑同一脚本得**逐项一致**结果 ⇒ 本次改动**未改变 R1 零引用集合**（删除的 7 个 pub fn 均不在 B-6 区块内；新增函数均已接线）⇒ **B-6 区块计数不变（458）**。
- 458 声明 vs 415 实测交集 的 43 项偏差属**预存漂移**（历史累积），非本批引入，已登记待裁定（§12.5）。

**B-13.7 R1 台账漂移修复（B-13.6 登记项处置）**

- 依用户裁定「规划方案并修复」，对 B-13.6 登记的 **43 项预存漂移**逐项定位处置，根因＝**文件随框内核迁移从 `privileged/...` 下沉至 `functions/...`，区块仍记旧 privileged 路径 ⇒ R1 按路径比对失配**（函数仍零引用，故报 HIGH）：
  - **31 项路径更新**（`privileged/...`→`functions/...`）：`driver/display/controller.rs`(11)、`driver/usb/mass_storage.rs`(2) / `ring.rs`(2) / `usb_core.rs`(3)、`fs/devfs/mod.rs`(2)→`fs/devfs.rs`、`fs/vfs/flock.rs`(4)→`fs/flock.rs`、`fs/vfs/dcache.rs`(1)→`fs/dcache.rs`、`fs/vfs/handle.rs`(2)→`fs/handle.rs`、`fs/vfs/inotify.rs`(2)→`fs/inotify.rs`、`fs/vfs/types.rs`(1)→`fs/vfs_types.rs`、`fs/vfs/vfs.rs`(1)→`fs/vfs_manager.rs`。
  - **12 项移除**（已接线 / 已删 / 已豁免）：`privileged/driver/net/e1000_io.rs::install_rings`（已删）、`::set_ctrl` / `::set_ipg` / `::set_rx_ctl` / `::set_tx_ctl`（已被 `functions/driver/net/e1000.rs` 调用）、`privileged/driver/usb/xhci.rs::init_command_ring` / `::recover_endpoint`（已删）、`privileged/timer/tickless.rs::enter_tickless` / `::exit_tickless`（内联测试引用）、`functions/egdf/mod.rs::find_by_proto`（host 测试引用）、`functions/driver/usb/xhci.rs::port_connected` / `::port_status`（内部调用）。
- 复核：修复后 R1 实测 `已分类清单: 446 项` ＝ `[INFO] R1 已分类 446 项`（matched）、**stale 0**、`[HIGH] 未分类 18 项`（18 项均为**预存、不在 B-6 区块、与本批无关**：`privileged/driver/net/e1000_io.rs` 5 项、`functions/driver/usb/xhci.rs` 6 项、`functions/fs/vfs_mount.rs` 5 项、`functions/proc/coredump.rs` 2 项），脚本 EXIT=0。

#### C. 原「接线」142 项（重划：仅 8 项留「接线」，其余 134 项入「未来功能」）

**C-1 接线（8 项；判据＝同族入口已在调用链中使用，仅缺此半 —— 可施工子清单）**

> **甲批 C-1 施工结果（逐项见 **B-8**）**：8 项**全部处置完毕 ⇒ 接线桶清零**（1 项接线落地 + 7 项转「未来功能」）。
> 判据「仅缺此半」经源码复核，**7 项不成立**：同族入口本身也未接线（`IrqLine` 全库 0 构造点）/ 读侧同样零引用（`Frame::meta`）/ 底层结构从未被填充（`Process::fd_table`）/ 无独立调用点（同模块内直呼 manager）。
> 按批次规则「**找不到调用点 ⇒ 退未来功能，禁止为接线造无意义调用**」处置。

| 文件 | 项 | 缺的半 | 甲批评定（调用点核定） | 去向 |
|---|---|---|---|---|
| `privileged/fs/vfs/vfs.rs` | `set_fd` | FD 表写入口，与在用 `get_fd` 成对 | **成立**：调用点＝`vfs_open_internal` 两分支（live 路径 `open/openat_syscall → vfs_open → vfs_open_internal`）。缺它则 `get_fd_info`（flock 的 ino 识别 / `fd_to_inode_id` mmap-by-fd）恒得 node_id 0，`get_fd_mount_idx` 因 path 空而反查失败，close 时 pcache 按 node 0 失效 | ✅ **已接线**（B-8.1） |
| `privileged/fs/vfs/handle.rs` | `vfs_get_fd_handle` | FD→句柄访问器，同文件 `vfs_*` 入口已在用 | 不成立：同能力（`VFS_MANAGER.get_fd_handle`）全部调用点在同模块内直呼 manager，无独立调用点；非 FFI 面（无 `no_mangle`） | 未来功能（B-8.2） |
| `privileged/irqline.rs` | `is_registered` | IRQ 线注册状态查询，与 `register_irq` 成对 | 不成立：`IrqLine` 类型全库 **0 构造点**（在用者仅 `dispatch_irq`），查询状态无对象可查 | 未来功能（B-8.2） |
| `privileged/frame.rs` | `set_meta` | 页帧元数据写入口，与 `get_meta` 成对 | 不成立：读侧 `Frame::meta()` 同样**零引用**，无消费方；文档自述为「预留元数据槽位」 | 未来功能（B-8.2） |
| `privileged/proc/fd_table.rs` | `get_handle_id` `is_cloexec` `set_cloexec` `get_cloexec_fds`（4） | FD 表 cloexec 面，exec 路径需用 | 不成立：`Process::fd_table` **从未被填充**（`alloc_fd` 零引用，全库仅 `init()` + procfs `get_all_fds()`）⇒ `cloexec[]` 恒 false、`get_cloexec_fds()` 恒空集；且 live fd 表（`VFS_MANAGER.fd_table`）无 cloexec 字段。接线＝往死结构写值 | 未来功能 + **安全面单列**（B-8.3） |

**C-1 施工记录（甲批）**：见 **B-8**。

**C-2 原 142 项全量（逐项保留可追溯；除 C-1 外均归「未来功能」）**

| 文件 | 数 | 项 |
|---|---|---|
| `privileged/arch/aarch64/mmu.rs` | 6 | `write_ttbr0` `tlbi_vaae1` `tlbi_vmalle1` `make_user_rw_entry` `make_kernel_rw_entry` `make_user_ro_entry` |
| `privileged/arch/aarch64/psci.rs` | 2 | `system_off` `system_reset` |
| `privileged/arch/aarch64/timer.rs` | 1 | `set_timeout_ms` |
| `privileged/arch/aarch64/uart.rs` | 1 | `switch_to_high_half` |
| `privileged/arch/x86_64/apic.rs` | 4 | `mask_lint0` `mask_lint1` `unmask_lint0` `unmask_lint1` |
| `privileged/freg/domain.rs` | 2 | `check_quota` `check_proc_limit` |
| `privileged/freg/recovery.rs` | 4 | `recovery_registry_init` `recovery_subdomain_save_checkpoint` `cascade_recover` `hard_reset_domain` |
| `privileged/freg/{reset/layered,snapshot}.rs` | 4 | `test_recovery_status` `test_snapshot_basic` `test_registry_register` `test_registry_priority_order`（kernel_test 用例未挂测试运行器） |
| `privileged/mm/kpti.rs` | 3 | `invpcid_flush_single` `kpti_kernel_pml4` `kpti_user_pml4_or_kernel` |
| `privileged/mm/kpti_aarch64.rs` | 2 | `kpti_trampoline_ttbr1` `kpti_kernel_ttbr1` |
| `privileged/mm/swap.rs` | 1 | `swap_free` |
| `privileged/mm/vma.rs` | 1 | `with_offset` |
| `privileged/sync/mutex.rs` / `pi_mutex.rs` / `rcu.rs` | 3 | `wait_timeout` / `set_ceiling` / `rcu_process_all_callbacks` |
| `privileged/egdf/user_driver.rs` | 3 | `devtree_map_user_device` `devtree_unmap_user_device` `egdf_forward_irq` |
| `privileged/cpu/tsc.rs` | 1 | `nanoseconds_to_cycles` |
| `privileged/sgeg/{audit,identity,secure_boot}.rs` | 3 | `get_entries` `find_mut` `add_trust_entry` |
| `privileged/debug/ebpf.rs` | 3 | `prog_run` `get_map` `get_prog` |
| `privileged/dma/engine.rs` | 5 | `unmap_single` `sync_both` `sg_init` `sg_add_entry` `sg_total_length` |
| `privileged/driver/block.rs` | 1 | `mark_removed`（已由 EGDF 墓碑协议 `egdf_blk_is_removed` 替代，原函数不存在；见 B-12） |
| `privileged/driver/hotplug.rs` | 1 | `hotplug_poll`（已由 `hotplug_wakeup` + softirq 轮询替代，原函数不存在；见 B-12） |
| `privileged/driver/input/keyboard.rs` | 1 | `get_modifiers` |
| `privileged/driver/net/e1000_io.rs` | 5 | `set_ctrl` `set_rx_ctl` `set_tx_ctl` `set_ipg` `install_rings` |
| `privileged/driver/power.rs` | 4 | `ondemand_check` `register_notifier` `pm_subsystem` `pm_is_initialized` |
| `privileged/driver/usb/mass_storage.rs` | 2 | `build_read_capacity_10_cbw` `build_request_sense_cbw` |
| `privileged/driver/usb/usb_core.rs` | 3 | `register_controller` `find_device_by_class` `find_device_by_vid_pid` |
| `privileged/driver/usb/xhci.rs` | 2 | `init_command_ring` `recover_endpoint` |
| `privileged/frame.rs` | 1 | `set_meta` |
| `privileged/fs/vfs/handle.rs` | 1 | `vfs_get_fd_handle` |
| `privileged/fs/vfs/inotify.rs` | 1 | `inotify_fd_readable` |
| `privileged/fs/vfs/vfs.rs` | 1 | `set_fd` |
| `privileged/idt/idt.rs` | 1 | `set_exception_handler` |
| `privileged/io/iouring.rs` | 2 | `io_uring_destroy` `io_uring_reap` |
| `privileged/irqline.rs` | 1 | `is_registered` |
| `privileged/net/netfilter.rs` | 2 | `hook_count` `list_rules` |
| `privileged/net/route.rs` | 2 | `route_list` `default_route` |
| `privileged/page_table.rs` | 1 | `verify_kernel_code_protection` |
| `privileged/pci/msi.rs` | 5 | `msi_enable` `msi_disable` `msix_disable` `msix_mask_vector` `msix_unmask_vector` |
| `privileged/proc/canary.rs` | 1 | `set_per_proc_seed` |
| `privileged/proc/cfs.rs` | 3 | `get_weighted_load` `steal_highest_vruntime` `get_load` |
| `privileged/proc/cgroup.rs` | 8 | `check_budget` `period_reset` `try_charge` `uncharge` `is_over_limit` `account_read` `account_write` `cgroup_of` |
| `privileged/proc/cpu_queue.rs` | 1 | `register_sched_softirq` |
| `privileged/proc/fd_table.rs` | 4 | `get_handle_id` `is_cloexec` `set_cloexec` `get_cloexec_fds` |
| `privileged/proc/namespace.rs` | 3 | `to_clone_flag` `map_uid` `map_gid` |
| `privileged/proc/posix_timer.rs` | 1 | `posix_timer_release_pid` |
| `privileged/proc/process.rs` | 2 | `kernel_stack_check_canary` `allocate_user_space` |
| `privileged/proc/rlimit.rs` | 5 | `check_nofile_exceeded` `check_as_exceeded` `check_nproc_exceeded` `get_stack_limit` `get_nofile_limit` |
| `privileged/proc/scheduler.rs` | 2 | `set_deadline_params` `get_current_process` |
| `privileged/proc/scheduler_ex.rs` | 3 | `freeze_all` `thaw_all` `exit_thread` |
| `privileged/proc/seccomp.rs` | 2 | `from_linux` `to_linux` |
| `privileged/proc/session.rs` | 3 | `get_session` `sys_tiocsctty` `signal_foreground_pgid` |
| `privileged/proc/signal.rs` | 1 | `has_deliverable_signal` |
| `privileged/proc/thread.rs` | 2 | `create_thread` `get_thread` |
| `privileged/proc/user_proc.rs` | 1 | `create_from_binary` |
| `privileged/syscall/epoll.rs` | 1 | `epoll_destroy` |
| `privileged/timer/hrtimer.rs` | 1 | `hrtimer_ns_to_cycles` |
| `privileged/timer/tick.rs` | 2 | `reset_ticks` `get_uptime_tsc` |
| `privileged/timer/tickless.rs` | 2 | `enter_tickless` `exit_tickless` |
| `privileged/timer/time_sync.rs` | 1 | `client_request` |
| `privileged/vmspace.rs` | 1 | `map_huge` |
| `functions/fs/{sysfs,cgroupfs,configfs,virtiofs,systree}.rs` | 5 | `mount_sysfs` `mount_cgroupfs` `mount_configfs` `mount_virtiofs` `mount_systree`（实现完整，待 VFS mount 集成） |
| `functions/net/unix.rs` | 1 | `uds_recv_with_creds`（recvmsg UDS 凭据分流点缺失） |

#### D. 原「预留」205 项（按子系统计数；已并入「未来功能」桶）

| 子系统/文件 | 数 | 预留性质 |
|---|---|---|
| `privileged/arch/aarch64/gic.rs` | 5 | SPI 使能/禁用/pending/触发配置（SPI bring-up 前预留） |
| `privileged/arch/aarch64/mmu.rs` | 1 | `alloc_user_page_table`（Phase 6 每进程独立页表） |
| `privileged/arch/aarch64/timer.rs` | 1 | `set_compare`（oneshot 高精度定时） |
| `privileged/mm/swap.rs` | 1 | `swap_deinit`（host-tests 配对 + 将来热卸载） |
| `privileged/egdf/{composite,devtree}.rs` | 2 | 设备树匹配/属性解析 API 面 |
| `privileged/console/gfx_console.rs` | 2 | `set_margin` `set_colors`（控制台配置面） |
| `privileged/cpu/feature.rs` | 6 | CPU 能力/厂商查询面（D-4） |
| `privileged/dma/engine.rs` | 1 | `submit_transfer_async`（异步 DMA 面） |
| `privileged/driver/display/controller.rs` | 11 | 多显示器管理 API 面（热拔管理未启用） |
| `privileged/driver/display/framebuffer.rs` | 1 | `intersection`（绘制辅助） |
| `privileged/driver/infra.rs` | 2 | `outw` `inw`（16 位 PIO 原语族） |
| `privileged/driver/power.rs` | 2 | `latency_us` `power_saving`（电源约束查询面） |
| `privileged/driver/uefi.rs` | 4 | UEFI 运行期服务 API 面 |
| `privileged/driver/usb/ring.rs` | 2 | 环入队/出队指针访问器 |
| `privileged/fs/vfs/dcache.rs` / `flock.rs` / `inotify.rs` | 6 | VFS 内部诊断面 + flock 子系统查询面 + inotify 统计 |
| `privileged/idt/{handlers,idt,safety,statistics}.rs` | 7 | 诊断计数/历史查询面 + 屏障原语（D-4） |
| `privileged/proc/scheduler_ex.rs` | 1 | `thread_dump_info`（线程信息转储） |
| `functions/sgeg/{identity,secure_boot,crypto}.rs` | 19 | 身份/凭据/安全启动能力预留（D-4）+ safe 代理壳 |
| `functions/driver/{acpi,char,display,firmware,storage,uefi,usb,virtio}` | 41 | safe 代理壳 + 硬件操作面（D-4） |
| `functions/fs/unkfs/*` | 34 | UNKFS 子系统预留（D-4） |
| `functions/fs/{exfat,ext2,tmpfs,devpts,sysfs,systree,ramfs,cgroupfs,configfs}` | 21 | FS 内部访问器 / mount-配套面 / 空间统计面 |
| `functions/ipc/{async_ipc,sem,signal}.rs` | 12 | IPC API/FFI 导出面（待用户态接线） |
| `functions/mm/{memory_pressure,swap}.rs` | 3 | 内存压力策略 + SwapInfo 查询面 |
| `functions/freg/audit_export.rs` / `config/sysctl.rs` | 3 | 审计导出统计 + sysctl 序列化面 |
| `functions/proc/{canary,elf,shadow_stack,signal}.rs` / `timer/*` / `net/unix.rs` / `wasm/*` | 12 | 查询面 + safe 代理壳 + wasm 运行时 API 面 |

#### 登记结论（修订版：E.1-E.4 + 第二 / 三 / 四轮裁定 + 甲批 + 本批 feature 去留；**第 15 条为最新口径**）

1. **元信息已补（E.1）**：① 判定工具＝`scripts/audit_unwired_pub_fn.py` R1（`rg -c -w` 文本并集，声明侧无 cfg 感知）；② 判定构建维＝**无单一构建维**，未做逐维交集 ⇒ 原「零引用」口径不可复核。③④⑤ 见上「方法与限制」。
2. **桶边界已重划并重算（E.2 + 三处核对① + 第二轮二次修订 + 第三轮三次修订 + 第四轮四次修订，暂定值）**：删候选 70 → 23 → **11** → **2**（二次修订：安全面 4 转 A-3、判据不成立 8 退桶入 B-3；**四次修订：试删前逐项复核退桶 9 入待裁，见 A-5 / 12**）；**硬件原语完整性保留 41 → 43**（x86_64 侧 31 + aarch64 侧 10，优先级＝**硬件原语保留 > aarch64 门控 > 删候选**；三次修订并入安全面**族残缺**档 2 ＝`ct_eq_salt`/`ct_eq_password`）；接线 142 → **8**（逐项独立判定）；未来功能 **339**（**来源合成，非实测**：原接线剩余 134 ＝142−8 排除法 + 原预留 205）；待裁 21 → 27 → 35 → **37** → **46**（三次修订并入安全面**待 T3 结论**档 2 ＝`split_path`/`validate_path`，B-4；**四次修订并入 A-5 退桶 9**）。**原「安全面待确认 4」桶三次修订后清零**（三档分流完毕）。合计 438（算术已核对闭合：2+43+8+339+46）。三处核对 ②③ 的来源标记已就地标注。**五次修订（删候选清零）与六次修订（B-5 待裁归零）为最新口径，见 12 / 13**：删候选 **0** / 完整性保留 **75** / 待裁 **2** / 接线 8 / 未来功能 **352**，合计 **437**（`write_log_line` 已删除）——**以 13 为准，本条保留历次修订轨迹**。
3. **`atomic_stats` feature 已裁定删除并结项（E.3 结项）**：`sync/atomic.rs` 4 项 `record_*` 原属 `#[cfg(feature = "atomic_stats")]` 门控（**非死代码**，援引既有登记 [subsystem-sync.md §9.3 [P2]](archive/audit-2026-08-14/subsystem-sync.md#L1075-L1092)）；本批按**职责判定**处置＝**删除 feature**（依据＝① 非内核所需；② 零履行——`mod stats` 为私有且 `atomic.rs` 内无调用点，即便启用 feature 亦触发 `dead_code`；③ 已有 `*_dump_stats` 等价诊断 idiom；另 `dump_stats` 引用 no_std 不可用的 `println!`，本就无法编译）⇒ 4 项随 feature 删除而消失，B-6 区块 466 → 462；明细见 **B-5.2 / B-5.3 G2**。
4. **删候选施工＝试删（E.4 + 遗留 2 裁定，不立项新工具）**：二次分类已移出 43 项硬件原语（含族残缺 2）、**且已移出安全面 4 项（不走试删）**；余 **11 项**（A-2；**四次修订后为 2 项**，见 12）按 ⑥「**逐项试删 → 跑既有五条门槛 → 任一维硬失败即回退**」推进（`build.sh all` / clippy `kernel_test` 维 / clippy `host-test` 维 / `make test-host` / QEMU `kernel_test`+boot），**编译/链接器即权威判据**，不新建调用图分析器。**第三轮已授予开工**（解锁五条逐条核销见 ⑧），执行约束：逐项独立提交 / 每项全量五门槛 / QEMU boot 硬闸门 / ramfs 2 项不入本批。
5. **遗留 1 已降级为方法学注释（不专项量化）**：x86_64 专属项面未量化属**漏项风险（完备性）而非误删风险（正确性）**——已在 ③ 补注「单维甄别的结果仅在该维有效，跨维完备性需逐维复核」。
6. **第二轮复核：A-2 已退回重做（reviewer，12/23 项判据站不住）**——原 23 项按四档处置，逐档有据：① **判据仅「零消费」7 项** → 补齐证据或退桶，结果 **7 项全退**（3 项同文件整组统一、1 项已登记路线图项、3 项属统计/诊断 API 面且未取得等价入口证据）；② **安全敏感 4 项** → 转 A-3 安全面二次分桶，**不走试删**；③ **注释待核 1 项**（`get_gdt_table`）→ 引注释原文核对＝`/// 获取 GDT 表的引用 (调试用途)`，**非死代码**，退桶；④ **判据较强 11 项** → 保留为 A-2。算术：23 − 4 − 8 = **11**。
7. **判据升格为三合一（reviewer 第二轮，根本性修正）**：「存在等价公共入口」**单独不成立**——有等价入口 ≠ 该函数无人用（可能正属 API 面 / FFI 面 / feature 面）。删候选判据＝**该构建维零引用 ＋ 存在能力等价的公共入口 ＋ 非 API/FFI/feature/硬件原语面**，三者须同时成立。
8. **试删的原理性盲区 + 顺序约束（reviewer 第二轮）**：`试删 + 五条门槛`**在原理上发现不了「安全校验被移除」**（删零调用校验后编译链接全过、无覆盖即全绿）⇒ 安全敏感项必须先经安全面确认；**不得并行 T1 收尾与 T5 施工**（同动 `privileged`，且 `pcid_is_enabled` 与 T1 P2′ 所改 `kpti.rs` **文件级冲突**），T5 文档修订可并行。A-2 开工解锁条件五条见 ⑧。
9. **第三轮裁定一：`ct_eq_salt` / `ct_eq_password` → 不删，移入完整性保留（族残缺档）**——实读 [crypto.rs:198-217](file:///home/anfer/Code/Edgine/src/kernel/functions/sgeg/crypto.rs#L198-L217)，四者（`ct_eq` / `ct_eq_hash` / `ct_eq_salt` / `ct_eq_password`）**形状完全相同**，是 `Salt` / `PasswordHash` / `Sha256Hash` 的**类型化比较同族**；`ct_eq_hash` 有调用者、未入删候选 ⇒ 删另两者则**族残缺且不对称**。真正作用不是「提供能力」（`ct_eq` 已提供），而是**阻止调用方拆字段**（写 `ct_eq(&a.0, &b.0)`）——删掉等于**诱导密码学代码绕过类型包装**、降低抽象层级，与 A-1 保留 `sync/*` 原语族入口**判据同构**。**附核实已完成**：全仓 `src/` + `host-tests/` 零引用、无 `#[no_mangle]` / 无 `extern "C"` / 无 FFI、超出零引用即无跨 crate 与用户态 API 面按名调用 ⇒ 保留不引入新暴露面。桶效应：A-1 41 → **43**。
10. **第三轮裁定二 + 裁定三：ramfs 2 项挂起待 T3；二次分桶产出规格固定为三档**——（二）`split_path` / `validate_path` **不得按「零调用」删**，须先由 T3 给出「VFS 是否已提供等价（空/长度/NUL）校验」结论再二选一（冗余 ⇒ 可删 + 登记契约；缺陷 ⇒ 不删 + 登记缺陷 + 按 C-1 接线）；**威胁模型已更正**：`validate_path` **不含 `..` 穿越检查**，穿越由 VFS `resolve_path` 负责，上轮「删即移除穿越防护」不成立（实况见 A-3）。桶效应：并入 B-4，待裁 35 → **37**。（三）**「二次分桶」产物固定为三档＝冗余 / 缺陷 / 族残缺**，写入 **B-0** 作为本桶定型口径——**三合一判据不足以区分三档**（它只判「是否依赖等价入口」，不判「零调用是冗余还是缺陷」）。
11. **第三轮开工裁定：A-2 11 项试删已授予开工**——解锁五条逐条核销（①②③④⑤ 全 ✅，见 ⑧）；执行约束：**逐项试删 / 每项独立提交可单独回退 / 每项跑五条门槛全量 / QEMU boot 为硬闸门 / ramfs 2 项不入本批 / `pcid_is_enabled` 已确认不在 11 项内**。**QEMU 日志已留存归档**：boot 日志 `build/log/qemu_boot_x86_64_t1wrap_20260918.log`（md5 `ad749d876db2808d053e219b5f91523a`，240 行）、kernel_test 日志 `tests/reports/unit_test_20260918_165355.log`（md5 `b1cf091bf44d3e6419fcd8f6c75f4d17`）。注：二者所在目录（`build/` / `tests/reports/`）**按 B08-10 既有裁定为 gitignore**（禁止日志入仓），故**可追溯性＝时间戳/固定名归档文件 + 关键行逐字入档**，非 git 跟踪。
12. **第四轮（试删开工后逐项复核）：A-2 退桶 9 项入待裁 ⇒ 删候选 11 → 2**——试删启动后按 **B-0 三档规格**逐项复核「零调用 ＝ 冗余 / 缺陷 / 族残缺」，以**全仓 `grep -rn` 实测引用计数**为判据，发现 9 项判据不成立（明细见 **A-5**）：
    - **族残缺 6 项**（同族兄弟在用 ⇒ 不删，保对称）：`tss_64bit`（`Granularity` 构造器族 `code_64bit` **4** / `data_32bit` **4** / `tss_64bit` **0** ⇒ 2/3 在用；字段 `Granularity(pub(crate) u8)` ⇒ 删后调用方须写 `Granularity(Granularity::LONG_MODE)` **绕过构造器族**，**与裁定一 `ct_eq(&a.0, &b.0)` 判据同构**）、`vfs_close_safe` / `vfs_seek_safe` / `vfs_readdir_safe`（`vfs_*_safe` 全族 **17 员 / 14 员在用**）、`get_ap`（`get_ap_list`/`get_ap_count`/`has_madt`/`parse_madt` 在用）、`get_fs_name`（`get_fs_type`/`get_fs`/`set_fs` 在用）。
    - **台账 ② 判据事实错误 / 无等价入口 3 项**：`consume_quota_tick` / `is_quota_exceeded`（原记「与**在用** `check_quota` 语义重复」——实测 `check_quota` **自身零引用**且已列 B-2 待裁 ⇒「在用」不成立）、`pipe_exists`（原记「可由**在用** `get_pipe` / `pipe_count` 等价判定」——全仓**无 `get_pipe` 符号**，`pipe_count()` 不判定指定 `IpcId` 是否存在 ⇒ **无等价公共入口**）。两处错误已就地订正。
    - **新增原理性证据（重要）**：`get_ap` 试删后五门槛 **5/5 全过**（含 QEMU boot 硬闸门）却仍被判族残缺退回 ⇒ **试删 + 五条门槛在原理上无法识别族残缺**，与 ⑧「发现不了安全校验被移除」**同源**——**门槛通过 ≠ 判据成立**，试删只证「无构建/链接/回归破坏」，不证「零调用属冗余」。
    - **档位争议如实登记**：`vfs_*_safe` 三项**弱于** `tss_64bit`——实测 [dir_ops.rs:14-28](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/dir_ops.rs#L14-L28) 已直接调用裸 `extern "C"` `vfs_seek` / `vfs_readdir`（不经 `_safe` 壳）⇒ 该族**本就不是封装边界**，删 3 员不改变抽象层级。仍按「同族部分在用 ⇒ 保对称」退桶，最终档位由 reviewer 复核确定。
    - **桶效应**：删候选 11 → **2**（仅 `write_log_line` / `format_duration`，为**唯一进入试删者**）；待裁 37 → **46**。合计 438（算术已核对闭合：2+43+8+339+46）。

13. **六次修订（B-5 待裁 47 归零；裁定二 / 裁定四.2）**：每项均按**裁定二**落**三态**并补**三字段**（等待原因 + 解锁条件 + 责任方），**47 项逐项可追**：
    - **等路线图 13 项 ⇒ 转「未来功能」**（路线图 + 登记处均经核实，见 B-5.1）：CET 5（`set_ssp`/`alloc_kernel_shadow_stack`/`configure_user_cet_msr`/`configure_interrupt_ssp_table`/`is_ssp_valid`）、NUMA 5（`set_distance`/`best_alloc_node`/`nearest_free_node`/`contains_cpu`/`all_nodes`）、PCID 1（`pcid_is_enabled`）、IOMMU-DMAR 2（`get_dmar_drhd_list`/`get_dmar_host_addr_width`）。**台账事实订正**：`kpti.rs:23` 所引 `engineering-progress.md` §五 **已失效**（该文件在现行 `docs/plan/` 不存在；`kernel-roadmap.md` 已归档且无 PCID 条目）⇒ PCID 登记处改以 `kpti.rs:23-31` 现状清单 + [kpti-complete-project.md](kpti-complete-project.md) 为准。
    - **判据待补 32 项 ⇒ 补齐后定桶「完整性保留」**（G1-G8 八组，判据 + 三字段见 B-5.2）：G1 aarch64 门控诊断 2 / G2 feature 门控 4（`atomic_stats`）/ G3 机制·安全·TCB 3 / G4 驱动·统计·诊断 API 面 4 / G5 FS mount-unmount 面 + Plan B FD 表 9 / G6 族残缺 6 / G7 无等价入口 3 / G8 无等价格式化入口 1。**两条新事实**：① `pci_scan`（`driver/bus/pci.rs:75-90`）唯一实现体调用在用 `pci::scan_all_buses`（`pci/mod.rs:518`）但**差异能力＝逐设备 `klog_info!`** ⇒ 「能力重叠 ≠ 能力等价」，同 A-6 `format_duration` 判据；② `umount_devpts`（`devpts.rs:237-243`）**实现体误调 `mount_devpts`**（注释自述恒返回 `Ok(())`）⇒ 已登记为**已知缺陷**（缺陷档形态，无调用链且非安全面 ⇒ 不删），修正随 VFS mount 集成。
    - **安全面待 T3 2 项 ⇒ 已裁决删除（七次修订）**（B-5.3 / B-10.8）：`functions/fs/ramfs.rs::split_path`/`validate_path`——裁定七证据链（commit `3173b5b9`）已判**非安全缺陷**、定型「**冗余**」；**唯一剩余阻塞（裁定六：删除涉安全面须 reviewer 授权）已解除** ⇒ 本轮删除，`VFS_MAX_PATH` / `alloc::string::String` 去接线，host-tests `td18` 源文本下界 10 → 8。
    - **桶效应（六次修订）**：完整性保留 43 → **75**（+32）；待裁 47 → **2**；未来功能 339 → **352**（+13）；**T5 内合计 90 → 77**；合计 **437**（0 + 75 + 2 + 8 + 352，算术闭合）。**裁定四.2 / 四.3 达成**；**四.1 已于五次修订达成**；**四.4（审计噪音治理）未达成** ⇒ T5 关闭挂起（见 T5-C）。**已按裁定六上报、未自主处置的潜在新增删候选**：`pci_scan`、aarch64 诊断 2 项、`format_duration`（若 reviewer 认定 `core::fmt` 等价成立）。

14. **甲批 C-1 接线批次（reviewer 甲批开工单，8 项；最新口径）**：核心约束＝**每项须指明具体调用点**，「仅缺此半」是待验证断言，**找不到调用点即退「未来功能」，禁止为接线造无意义调用**。逐项核定结果（明细见 **B-8**）：
    - **接线落地 1 项**：`privileged/fs/vfs/vfs.rs::set_fd`——调用点＝`vfs_open_internal` 两分支（live 路径 `open_syscall` → `vfs_open_safe` → `vfs_open_internal`）；补调用后 `get_fd_info`（flock 的 ino / mmap-by-fd 的 `fd_to_inode_id`）、`get_fd_mount_idx`、close 时 pcache 失效才按真实 inode 生效。生效证据＝kernel_test `vfs::backend::open_populates_fd_metadata`（断言 fd 表 `node_id` ＝真实 inode、path 可反查挂载点）。
    - **判据不成立 7 项 ⇒ 转「未来功能」**（三字段见 B-8.2）：`vfs_get_fd_handle`（同能力调用点全在同模块直呼 manager，无独立调用点）/ `irqline.rs::is_registered`（`IrqLine` 全库 0 构造点）/ `frame.rs::set_meta`（读侧 `meta()` 同样零引用）/ `fd_table.rs` cloexec 4 项（`Process::fd_table` 从未被填充 ⇒ `cloexec[]` 恒 false，接线＝往死结构写值）。
    - **安全面单列（B-8.3）**：`fd_table.rs` cloexec 4 项的「exec 路径需用」判据经实测**不是「漏接一项」而是整条语义链缺失**——`fcntl` F_SETFD **静默返回 0 且无存储**、`O_CLOEXEC` 常量全库零引用、live fd 表 `VfsFile` **无 cloexec 字段**、`execve`/`execveat` → `proc_exec_replace` **全程不触碰 fd 表**、fd 表为**全局命名空间**。⇒ 修需 per-process fd 表 + `fcntl` 真实实现 + `VfsFile` 增字段 + exec 关闭遍历（跨 4 模块架构改动，属 TCB/VFS 核心面），**超出批次授权，未自主施工**；解锁＝**B-11** per-process fd 表；责任方＝reviewer。
    - **桶效应（甲批）**：接线 8 → **0**；未来功能 352 → **359**（+7）；**R1 437 → 436**（`set_fd` 不再零引用）；合计 **436**（0 + 75 + 2 + 0 + 359，算术闭合）。**五门槛全量见 B-8.5**。

15. **`lock_stats` feature 已裁定删除并结项（本批）**：`privileged/sync/types.rs::LockStatistics`（`#[cfg(feature = "lock_stats")]` 门控结构体 + `Default` impl）及其在 `functions/sync/mod.rs` 的 re-export 属**账外项**（原未登记进 B-5.2 / B-5.3 G2 / B-6 区块）。按**职责判定**处置＝**删除 feature**（依据＝① **非内核所需**：全仓零实例化 / 零引用、无任何 `record_*` / `dump_*` 集成，无人递增计数器；② **零履行**：纯数据结构、无行为方法、无调用点；③ **已有等价承担者**：`privileged/sync/lockdep.rs` 已完整接线（spinlock / rwlock / mutex / pi_mutex / irq_spinlock 均调用），提供锁类跟踪 + 违规 / 递归 / AB-BA 死锁检测 + State Dump，覆盖其「统计」职责）。与 `atomic_stats` 的差异：本项**可编译**（无 no_std 不可用 import / `println!`），且为 `pub` 项不触发 `dead_code` lint（F9 不适用）。⇒ 删除 `privileged/sync/types.rs` 结构体 + `Default`、`functions/sync/mod.rs` re-export、两处 `Cargo.toml` feature 声明 / 转发；**桶数与 B-6 区块 462 不变**（账外项，非 R1 pub fn 分类面）。

## 详情

### 预存登记（T1 G2 审查处置，2026-09-16）

| 项 | 裁决 | 说明 |
|---|---|---|
| CLONE_SETTLS 切换恢复实装 | 待办（用户态线程库出现时） | `Process.tls_base` 保留不删（POSIX 线程兼容面预留）；x86_64 需在切换时写 MSR_FS_BASE、aarch64 写 tpidr_el0。注释已如实修正（clone.rs / process.rs 字段文档），原"上下文切换时恢复"陈述与事实不符 |
| sched_ops.rs 整批 FFI 面 0 引用核实 | 待办（后续专项） | scheduler_set_quota/remove_quota/set_proc_limit/proc_get_current_pid_internal/proc_yield_internal 全仓（Rust+C+asm）0 引用；`no_mangle` 屏蔽编译器告警。同批已删 proc_exit_internal（同性质，exit 实际走 proc_ops.rs `SCHEDULER.exit` 直调），整批删除待专项核实确认 |

### 预存登记（T1 G3 批报告，裁决处置）

| 项 | 裁决 | 说明 |
|---|---|---|
| uds_close 不释放 fd_alloc 位图 | 已修（本批） | 生产级 FD 泄漏：uds_close 仅清 UDS socket 槽位，从未调用 `fd_alloc::free_fd(Uds, fd)`（对照 pidfd close 释放模式）；UDS 容量 16，累计创建 16 次后位图永久耗尽 → uds_create 恒 NoMem。修复：uds_close 补 free_fd + listener 关闭时 pending client 位同步回收；kernel_test 回归 close_releases_bitmap，socketpair_rollback 显式补偿逻辑随修复移除 |
| recvmsg/sendmsg_syscall 无 UDS 数据面分流 | 专项登记（后续工程） | 旧 recvmsg/sendmsg_syscall 处理完 cmsg 后直调 fw::recvmsg/sendmsg，fw 层 fd_type 只认 smoltcp 1/2 → UDS fd 返 EBADF（当前仅 cmsg 凭据回传逻辑可达 UDS）。G3 recvmmsg/sendmmsg 新路径已内建 UDS 分流；旧路径分流待单独工程处置（本批 §12.2 未顺手扩大） |
| uds_sendto 无消息长度校验 | 已修（本批） | `uds_sendto` 未校验 `data.len() > UNIX_DGRAM_MAX`，超限直接 copy_from_slice 写 dgram_buf → 越界 panic 隐患。修复：入口补 `data.len() > UNIX_DGRAM_MAX → Invalid` 防护（与 uds_send_connected 同款）+ kernel_test 回归 sendto_oversize（超限拒绝 + 正常路径不受影响） |

### 预存登记（T1 G4 批报告）

| 项 | 裁决 | 说明 |
|---|---|---|
| `sys_set_mempolicy` 直接强转 Linux MPOL 编码 | 登记（后续专项，本批 §12.2 未扩大） | `privileged/mm/numa.rs` 的 `sys_set_mempolicy(mode, nodemask)` 用 `NumaPolicy::from_u8(mode as u8)` 直接强转，而 Linux MPOL 编码与枚举判别值**不同**（`MPOL_PREFERRED = 1` vs `NumaPolicy::Bind = 1`、`MPOL_BIND = 2` vs `Interleave = 2`、`MPOL_INTERLEAVE = 3` vs `Preferred = 3`）→ 用户态传 1/2/3 全部错位。同批新增的 mbind 已走 `from_linux_mode` 正确映射（`NumaPolicy::from_u8` 文档已标注"禁止直接 from_u8"），两根路径编码处理不一致。同时 `sys_set_mempolicy` 的 `nodemask` 按值接收（Linux ABI 为指针 + maxnode），且无 `flags` 参数。修复需连同 G4 一词核对该 syscall 的 dispatch 调用形态（当前回退层调用点）。 |
| `range_is_mapped` 允许 VMA 真子集（本批语义修正） | 已修（本批） | 原实现要求目标区间被**单个** VMA 完整包含（`vma.start > start` 或 `vma.end < end` 即 false）→ uffd 注册落在 VMA 内部的区间被拒（kernel_test `mm::uffd::fault_flow` 实测暴露）。修正为交叠长度累加（允许真子集 / 跨相邻 VMA，仅拒绝空洞），与 `set_numa_policy_range` 的"整 VMA 对齐"要求差异在文档注明（uffd 注册区间不携带 VMA 策略字段，无策略越界问题）。 |

### 预存登记（T1 G7 批报告）

| 项 | 裁决 | 说明 |
|---|---|---|
| `uname` 默认 nodename `"edgine-node"` → `"Edgine"`，domainname `"(none)"` → 空串 | 已随本批变更（行为变化登记） | UTS 收敛的直接后果：`uname`（硬编码 `"edgine-node"`）/ `gethostname`（硬编码 `"localhost"`）/ `sethostname`（仅校验不存储）三处各说各话，统一后以 `UtsNamespace` 初值 `UTS_DEFAULT_NODENAME = b"Edgine"` 为准。原 `"(none)"` 为 Linux 未设域名的显示占位，本批按"未设置即空串"处理。回退路径：需对齐 Linux `(none)` 字面量时在 `sys_uname` 回填占位串 |
| `VfsManager.root` 全局单例根 + `pivot_root` 不摘除旧根 + `vfs_set_cwd_internal` 静默失败 | 登记（本批 SIMPLIFIED，见实施记录 SIMPLIFIED 清单） | 三项均为随 chroot/pivot_root 完整路径解析引入的显式简化，已在代码内以 `// SIMPLIFIED:` 标注具体简化点/影响面/扩展时机；待 per-process root 或 mount namespace 机制出现时统一升级 |
| `audit_implicit_deps` 151 → 159（+8） | 登记（本批引入，扩展审计维度） | 增量全部来自用户路径归一化的必要调用点：`functions/fs/path.rs` +5（chroot/pivot_root 的 `VFS_MANAGER.set_root`/`get_root`/`resolve_user_path`）、`functions/fs/file_handle.rs` +2（`name_to_handle_at` 归一化）、`functions/fs/file_ops.rs` +1（`resolve_user_path`）。该维度为**扩展审计**（非 CI 硬门槛），HEAD 基线已有 151 处预存 backlog，本批未做清减（§12.2 不顺手扩大） |
| `audit_unwired_pub_fn` / `public_api_docs` / `implicit_deps` 大额预存 backlog | 登记（预存，非本批引入） | 经 HEAD worktree 对比确认与 T1 G4-G7 批次无关；待专项工程处置 |

### 预存登记（host 链接占位符号残留风险，T1 G7 批引入）

描述：T1 G7 批为修复 host 测试链接失败，在 host-only 壳 crate `src/rust/src/lib.rs` 提供 4 个零值占位符号（`_kernel_text_start` / `_kpti_trampoline_end` / `_kernel_text_end` / `USER_CR3_SAVE`）。该修复**不是结构根治**，残留风险登记如下，待与审查讨论处置。

方案（已定，2026-09-17 审查裁定）：**符号使用点级 host 桩化**——沿用 E-04 既有 idiom（`#[cfg(feature = "host-test")]` 桩分支 + `#[cfg(not(feature = "host-test"))]` 真机分支，先例见 `privileged/arch/x86_64/mod.rs:49-67` 的 `cpu_id`），把"引用链接脚本/汇编符号的语句"收进 cfg 分支，host 侧取常量中性返回或整段跳过；随后**删除壳 crate 的 4 个占位**。目标是**消灭 undefined symbol 这一类**，并把"违反即链接期硬失败"的响亮守卫交还给链接器——无需新增审计脚本，无需豁免表。

**否决的候选**：① 审计守卫（保留静默零值语义 + 长期维护门槛）；② 模块级 `cfg` 排除（整块排除会牵连 `vmm`/`proc` 的纯逻辑退出 host 编译，且 stub 面积膨胀）；③ 维持现状（保留对 CGU 布局的依赖，本批已实际付出一次逐文件二分排查成本）。原候选对比中"候选 A（privileged 内 `cfg(host-test)` 占位）需在 TCB 内混入 host 分支"的排除理由**不成立**——`privileged/arch/x86_64/mod.rs` 早有同类 host 桩分支，且这正是"同源双编译"的实现方式。

**可达性矩阵（2026-09-17 调研）**

调研口径：逐个符号访问点向上追溯调用链至入口，判定 host 测试能否到达；旁证为 `host-tests/` 全线零**代码**引用（`boot_install` / `SGEG_DISK_INSTALL` / `create_user_page_table` / `kpti_init` / `boot::init` 无任何命中；`handle_user_page_fault` / `read_user_cr3_asm` 仅命中文档注释，无调用）。

| 簇 | 符号 | 访问点 | 调用链顶端 | host 可达性 |
|---|---|---|---|---|
| 1 | `USER_CR3_SAVE` | ① `mm::read_user_cr3_asm`（`privileged/mm/mod.rs:26`）② `kpti::map_kpti_data_pages`（`privileged/mm/kpti.rs:718-720`） | ① `page_fault.rs:114` `handle_user_page_fault` ← `#PF` 入口 ② `kpti_init`（`kpti.rs:389`）/ `create_user_page_table`（`vmm_x86_64.rs:651`） | **不可达**（①`demand_paging_test.rs:10-13` 已载明"内核 mm 层 host 不可测，已移除 `handle_user_page_fault`/`handle_page_fault` 依赖"；②host 无 MMU，不可构造页表上下文） |
| 2 | `_kernel_text_start` / `_kernel_text_end` | ① `kpti::kpti_init`（`privileged/mm/kpti.rs:366-380` step 4.5）② `vmm::create_user_page_table`（`privileged/mm/vmm_x86_64.rs:630-635`） | `vmm_init` / 进程地址空间创建 | **不可达**（host 无 MMU，不可构造页表上下文） |
| 2b | `_kpti_trampoline_end` | **零引用**（仅 `kpti.rs:161` extern 声明 + `x86_64.ld:51` 定义） | — | 无引用即无 undefined symbol；G7 实际报错清单亦不含它 → **连声明一并删除** |
| 3 | `_kernel_end` | `boot::init`（`privileged/boot/mod.rs:285`） | `kernel_init`（`src/kernel/lib.rs:562` / `:654`） | **不可达**（裸机引导入口） |
| 4 | `_kernel_start` / `_kernel_end` | `raw::kernel_start_ptr`（`privileged/syscall/mod.rs:318`）/ `raw::kernel_end_phys`（`:331`） | `sys_boot_install`（`privileged/syscall/dispatch.rs:876`，gate 为 `all(not(kernel_test), x86_64)`） | **不可达**（host-tests 零引用 boot_install / SGEG_DISK_INSTALL） |
| 5 | `stack_bottom` | ① `proc::check_boot_stack_canary`（`privileged/proc/process.rs:50`）② `proc::write_boot_stack_canary`（`privileged/proc/process.rs:69`） | `kernel_init`（`src/kernel/lib.rs:528` / `:904`）+ aarch64 引导入口（`boot/aarch64/entry.rs:40` / `:52`） | **不可达**（裸机引导入口） |

**aarch64 侧顺带项**：`_kernel_end` 另有 `privileged/boot/aarch64/entry.rs:101` 访问点（该模块受 `target_arch = "aarch64"` 门控，host 构建不编译，不构成 host 链接风险）；列此仅为"符号语义桩化原则在双架构一致"的完整性记录。

**矩阵结论**：5 簇全部 host 不可达 → **全部桩化，无"必须保留占位"的残余项**。存量漏项（`_kernel_start` / `_kernel_end` / `stack_bottom`——G7 未覆盖、但已被 host 编译路径引用，只因 CGU 未共置而未爆）随本方案一并消灭，不需要"补占位"也不需要"豁免表"。净效果：链接脚本/汇编符号契约 **7 → 0**（P2′ 后**声明亦全部收拢**，host 下误引用为**编译期**失败——见裁决表「守卫两级化」条）。

**实施形态与纪律**

> **关键约束（实施前必读）**：cfg 必须落在**符号引用所在的语句/函数体内部**，**不能只 gate 调用点**。rustc 对 crate 内所有可达 item 一律生成代码，即使某函数在 host 构建下已无任何调用者，其函数体内的 `extern` 符号引用仍会进入目标文件并产生 undefined symbol。因此"gate 掉调用方"这种做法对消灭 undefined symbol **无效**（G7 的簇 4 判断即由此而来）。

| 簇 | 改法 | host 桩 |
|---|---|---|
| 1 | 两个访问点各自**函数体内** cfg 分叉：`read_user_cr3_asm`、`kpti::map_kpti_data_pages` | `return 0` / 整段跳过 |
| 2 | 2 处访问点各自把"符号取值 + 页表映射"收进 cfg 块（**函数本体保留编译，签名仍受 host 检查**） | 整段跳过 |
| 2 补注 | `kpti_init` / `create_user_page_table` 为**体内 cfg 分叉**（符合上表原文）；但其叶子函数 `map_text_region_in_user_pml4` / `map_text_page` 实施为**整函数 cfg**（实施后补注） | 理由：F9 禁 `#[allow(dead_code)]`，host 下调用点已整段 cfg 致**无调用者**，整函数消除是唯一合规路径；两者为 `pub(super)` 叶子、全为符号取值 + 页表操作，**无纯逻辑可分离**（不存在"保留编译有价值"的部分） |
| 2b | 删除 `kpti.rs` 的 `_kpti_trampoline_end` extern 声明（零引用） | — |
| 3 | `boot::init` 内符号取值收进 cfg 块 | `kernel_end = 0` |
| 4 | 两个访问器函数**内** cfg 分叉（`kernel_start_ptr` / `kernel_end_phys`；**调用链零改动**——若改为 gate 函数本体，`sys_boot_install` 与 functions 侧 `boot_install_syscall` 的 cfg 需同链上推，且按上条约束仍不解决问题） | `null` / `0` |
| 5 | 两个访问点各自**函数体内** cfg 分叉：`check_boot_stack_canary`、`write_boot_stack_canary` | `return true` / 整段跳过 |

**纪律（维持"host-test 不平行实现"的关键）**：桩体**只允许两种形态**——常量中性返回、或整段不执行；**禁止**在桩体内出现任何条件分支、状态读写或业务判断。桩内无"实现"即无"两份实现"，平行实现的滋生根被切断。

**门控语义推广**：真机路径门控由 `not(feature = "kernel_test")` 推广为 `not(any(feature = "kernel_test", feature = "host-test"))`（kt/ht 同属"测试环境"，与 `audit_feature_semantics.py` 既有 `any(kernel_test, host_test)` 用法闭合）。属**既有约定表述扩展**，实施时须同步该脚本规则文本并确认无既有用例被推翻。

**验证**

1. 删除壳 4 占位后 `./ci/build.sh all` 5/5（host-tests 99 bin 全链接成功）；
2. **负向验证（必做，P1 改写版）**：临时去掉一处 host 桩（使 host 可达路径引用真符号），确认构建**硬失败**——P2′ 全声明收拢后报错层级为**编译期** E0425（`cannot find value … in module`）；仅当符号声明未随引用收拢时才为链接期 `undefined symbol`。两级均为「违反即硬失败」守卫（不承诺"编译进对象即报错"：引用不可达时不报错），证明守卫未换处藏问题；
3. 双架构 check 0w0e + clippy 3 维（pedantic lib / kernel_test / host-test）0 warning；QEMU kernel_test 498/498 + QEMU boot（Ring 3/init）；
4. `audit_feature_semantics.py` 的 HIGH 清单与 HEAD 基线**逐项一致**（不得新增/推翻；当前 7 项 HIGH 全为预存，与本批文件集零交集，详见实施记录第 6 条）；
5. **覆盖无损失断言**：门控后 host-tests 通过数不得下降。

状态：[X]
详情：已于 2026-09-17 按上述方案实施完成（符号使用点级 host 桩化 + 删除壳 4 占位），5 簇全部落地；P1-P4 收尾专项（负向验证 / extern 声明一致性 / 文档同步 / 全门槛回归含 QEMU boot）+ P2′（裁定 2：三处残余声明一并收拢）已全部完成，验证实测见下方「实施记录（2026-09-17）」。符号契约 **7 → 0** 达成（含声明侧），守卫为**两级**（编译期 / 链接期，均 fail-closed，见裁决表条与 P1 条）。

| 项 | 裁决 | 说明 |
|---|---|---|
| 占位仅为**当前枚举子集**，新符号会再次炸 | **已裁定：随本方案消灭**（存量漏项 3 个实测确认） | 底层条件（host 编译引用汇编/链接脚本符号）由 5 簇使用点桩化直接消除；G7 未覆盖、已被 host 编译路径引用却因 CGU 未共置而未爆的 3 个存量漏项（`_kernel_start` / `_kernel_end` / `stack_bottom`）同批消灭。**新增符号不会再炸**——host 可达路径一旦引用此类符号即**硬失败**（具体层级见下行），不需要枚举清单与豁免表 |
| 两侧无同步机制 | **已裁定：守卫两级化（编译期 / 链接期，均硬失败）；原「链接器即为守卫」表述修正** | 删除壳占位 + P2′ 全声明收拢后，「privileged 引用的汇编/链接脚本符号 ↔ host 侧定义」这一原本无人校验的映射不再存在。守卫形态取决于该符号的**声明是否随引用一并 cfg 收拢**：**① 编译期**（声明已收拢 → item 在 host 下不存在，本项目现状：`USER_CR3_SAVE_ASM`、`stack_bottom`、syscall 的 `_kernel_start` / `_kernel_end`、`kpti` 的 `_kernel_text_*`、`boot` 的 `_kernel_end`）报 `error[E0425]: cannot find value … in module`；**② 链接期**（声明保留 → 无定义）报 `rust-lld: error: undefined symbol`。两级均为 fail-closed，且**编译期级更强**（更早暴露）。守卫从"需新写审计脚本枚举两侧"降级为"编译器/链接器内建"，无需维护成本 |
| 语义退化：链接期硬失败 → 运行期静默错值 | **已裁定：消除** | 静默错值的前提是"host 侧存在零值定义"；删除壳 4 占位后该前提不存在，退化路径随之消失，不变量回到"违反即**硬失败**"（编译期优先，层级见上行） |
| 可选收益（已发生，非风险） | **必要非充分（表述修正）** | 占位仅解除了链接障碍，**不解除**语义障碍（host 无 MMU / 无中断状态 / 无 CR3，不可构造页表上下文）。故 [demand_paging_test.rs](host-tests/tests/demand_paging_test.rs) 与 [td22_sigill_delivery_test.rs](host-tests/tests/td22_sigill_delivery_test.rs) 的源码扫描降级在本质因上仍然成立；本方案后 host 侧不再有 `USER_CR3_SAVE`，两处维持降级。恢复真实链接级覆盖属独立工程，**不据此扩大本方案范围** |

### 实施记录（host 链接占位符号残留，2026-09-17）

**改动清单**（与上方 5 簇改法逐条对应）

| 文件 | 改动 |
|---|---|
| [mm/mod.rs](src/kernel/privileged/mm/mod.rs) | 簇 1①：`USER_CR3_SAVE_ASM` extern 声明 `all(x86_64, not(host-test))` 门控（连声明排除）；`read_user_cr3_asm` 函数体内三分支（真机 `load` / host `0` / aarch64 回退） |
| [mm/kpti.rs](src/kernel/privileged/mm/kpti.rs) | 簇 1②+2+2b：`map_kpti_data_pages` 函数体内整段 cfg（host 分支消费参数）；`kpti_init` step 4.5 与 `map_text_region_in_user_pml4` / `map_text_page` 一并 `not(host-test)` 门控；删除 `_kpti_trampoline_end` 声明；`KERNEL_BASE` 导入按 host-test 分叉；P2′：`_kernel_text_start` / `_kernel_text_end` 声明补 `not(host-test)` 门控 |
| [mm/vmm_x86_64.rs](src/kernel/privileged/mm/vmm_x86_64.rs) | 簇 2②：`create_user_page_table` 内 KPTI 同步段整段 cfg（该段即 `_kernel_text_*` 的跨模块引用点，P1 第二处负向验证即临时去此 cfg） |
| [boot/mod.rs](src/kernel/privileged/boot/mod.rs) | 簇 3：`boot::init` 的 `kernel_end` 双分支（真机取符号地址 / host `0`）；P2′：`_kernel_end` 声明（块级，块内仅此一项）补 `not(host-test)` 门控 |
| [syscall/mod.rs](src/kernel/privileged/syscall/mod.rs) | 簇 4：`kernel_start_ptr` / `kernel_end_phys` 函数体内分叉（host `null` / `0`），调用链零改动；P2 收尾：`_kernel_start` / `_kernel_end` 声明补 `not(host-test)` 门控（cfg 加在**单个 item** 上，同块 `timer_get_ticks` 不受影响） |
| [proc/process.rs](src/kernel/privileged/proc/process.rs) | 簇 5：`stack_bottom` extern 声明门控；两个 canary 函数体内分叉（host `true` / 整段跳过） |
| [lib.rs](src/kernel/lib.rs) | 门控语义推广：两处真机引导块 `not(kernel_test)` → `not(any(kernel_test, host-test))`；`kernel_init` 的 `used_underscore_binding` / `unreadable_literal` expect 条件同步推广 |
| [src/rust/src/lib.rs](src/rust/src/lib.rs) | 删除壳 crate 4 个零值占位符号（回退到原 19 行内容，git diff 归零） |
| [scripts/audit_feature_semantics.py](scripts/audit_feature_semantics.py) | 规则文本同步（仅 docstring 追加约定推广说明；判定逻辑未动，检查面仍为 functions/ + privileged/tests/） |

**实施中发现的附带项（本批内一并处置，属桩化的直接后果）**：host-test 维 clippy 报 10 处 error —— 9 处 `unfulfilled_lint_expectations`（被 cfg 排除的代码不再触发 lint，但 item 级 `#[expect]` 仍注册）+ 1 处 `kpti.rs` 的 `KERNEL_BASE` unused import。修复手段与 J-01 先例一致：`#[expect(...)]` 收窄为 `#[cfg_attr(not(feature = "host-test"), expect(...))]`，import 按 host-test 分叉。**未使用任何 `#[allow(dead_code)]` / 豁免表**（F9）。

**验证实测（P1-P4 收尾专项 + P2′ 收拢后复跑）**

1. **P1 负向验证（改写版：全声明收拢后预期为编译期失败，报错逐字留存）**：
   - 做法：临时移除 [syscall/mod.rs](src/kernel/privileged/syscall/mod.rs) `kernel_start_ptr` 的 host 桩分支（真机分支转为无条件），使 host 构建重新引用已收拢的真符号 `_kernel_start`。
   - 实测：`cargo test --manifest-path host-tests/Cargo.toml --test e04_shared_runner_test --no-run` → **exit=101**，报错逐字：
     ```
     error[E0425]: cannot find value `_kernel_start` in this scope
        --> /home/anfer/Code/Edgine/src/kernel/privileged/syscall/mod.rs:338:19
         |
     338 |         unsafe { &_kernel_start as *const u8 }
         |                   ^^^^^^^^^^^^^ not found in this scope
     ```
   - **第二处（针对本批新收拢声明的定向验证，证明跨模块引用同样 fail-closed）**：临时移除 [vmm_x86_64.rs](src/kernel/privileged/mm/vmm_x86_64.rs) `create_user_page_table` KPTI 同步段的**块级 cfg**，使跨模块引用（`crate::privileged::mm::kpti::_kernel_text_*`）在 host 下转 live → **exit=101**，报错逐字（共 3 项，前两项即新收拢符号；rustc 并指认门控位置）：
     ```
     error[E0425]: cannot find value `_kernel_text_start` in module `crate::privileged::mm::kpti`
        --> /home/anfer/Code/Edgine/src/kernel/privileged/mm/vmm_x86_64.rs:633:69
     error[E0425]: cannot find value `_kernel_text_end` in module `crate::privileged::mm::kpti`
        --> /home/anfer/Code/Edgine/src/kernel/privileged/mm/vmm_x86_64.rs:636:69
     note: found an item that was configured out
        --> /home/anfer/Code/Edgine/src/kernel/privileged/mm/kpti.rs:516:22
         |
     493 | #[cfg(not(feature = "host-test"))]
         |          ----------------------- the item is gated here
     ```
   - **守卫性质（P2′ 后最终表述）**：**两级守卫**——声明随引用收拢的符号在 host 下误引用即**编译期** E0425（早于链接）；仅当存在未收拢声明时才退为链接期 undefined symbol。原 P1 预期（"仅去桩即链接失败"）默认了两个前提（对象必被拉入 + 声明保留），P2′ 后两前提均不成立，故 P1 验收标准同步改写为「**编译期失败**」。**保留的语义精确化**：引用不可达（承载对象未被拉入二进制）时**不报错**——守卫是「违反即硬失败」，不承诺"编译进对象即报错"（该结论来自 P2′ 前的 `nm` 实测：rlib 内 `U _kernel_start` 计 1 处而 e04 二进制零命中）。
   - 撤销：两处临时改动全部回退（`kernel_start_ptr` 恢复双分支、`vmm_x86_64.rs` 恢复块级 cfg）→ 复跑 e04 `--no-run` **exit=0，恢复绿**；全仓 `P1 临时负向验证` 临时标记零残留。
2. **P2 择一结论 + P2′（裁定 2）**：**采纳「加 cfg」并一并收拢三处残余** ——
   - `syscall/mod.rs` 的 `_kernel_start` / `_kernel_end` 声明补 `not(feature = "host-test")` 门控；cfg 加在**单个 item** 上，同块 `timer_get_ticks` 不受影响。
   - **门控判据：按"符号存在性"而非"引用侧函数 gate"**（收尾复核后修正）——这 6 个符号全部由 `x86_64.ld` / `aarch64.ld` 定义（`_kernel_text_*` 亦两架构皆有；`_kpti_trampoline_end` 为 x86_64 独有且已随声明删除），**唯一不存在这些符号的构建是 host-test**（无 ld 脚本产物），故统一取 `not(feature = "host-test")`。此前一度写的 `all(not(kernel_test), x86_64, not(host-test))`（镜像引用侧函数的 gate）已弃用：它把"引用侧函数恰好被 gate 掉"误当作"符号不存在"，方向是把声明收得比事实更窄——在 aarch64 / kernel_test 维该符号**真实存在**，未来若把引用放宽到这两维会得到一次**误报**。现写法与 `stack_bottom`（仅 `not(host-test)`）、`USER_CR3_SAVE_ASM`（arch 项编码"只此架构存在"的事实）同构，符合裁定 2 理由 1（一致性）与理由 2（语义自洽）。
   - P2′：`kpti.rs` 的 `_kernel_text_start` / `_kernel_text_end` 与 `boot/mod.rs` 的 `_kernel_end` 声明亦补 `not(feature = "host-test")` 门控（`kpti.rs` 整模块已受 `target_arch = "x86_64"` 门控，不重复 arch 条件；`boot/mod.rs` 唯一引用点在 `init` 真机分支）。
   - 裁定依据：一致性（与 `mm/mod.rs` / `process.rs` 对齐）+ 语义自洽（声明保留意味着 host 构建仍"宣称"依赖链接脚本符号，与「符号契约 7 → 0」相斥）+ 守卫前移（编译期早于链接期，且条件写错只会是编译期报错，无实质风险）。
   - 原「残余一致性观察（未改）」一项随之**关闭**：三处声明已全部收拢，本方案符号集合内**不再存在保留声明的链接期守卫样本**（此即 P1 验收标准改写的原因）。
   - 复跑结果（P2′ 落地 + 判据修正为"存在性"后）：E-04 `--no-run` **exit=0**；`./ci/audit.sh quick` **exit=0**（双架构 check + clippy lib / kernel_test / host-test 三维全 passed，F4 SAFETY 覆盖缺漏 0；**aarch64 / kernel_test 两维的"未引用声明"零告警已实测**）；`./ci/build.sh all` **Passed 5 / Failed 0**（含 aarch64 release 构建）；`make test-unit` **498/498**（kernel_test 维链接正常，未引用声明不引入符号引用）；`qemu_boot_test.sh x86_64` **1/1**。注：x86_64 真机维在两种判据下声明均存在，真机产物等价，故 boot 语义无差异。
3. **门控语义推广落地**：[lib.rs](src/kernel/lib.rs) **3 处**（`:505` `kernel_init` 的 `used_underscore_binding` cfg_attr 条件、`:661` Boot Info 真机块、`:861` UNKFS 挂载块）由 `not(kernel_test)` 推广为 `not(any(kernel_test, host-test))`；[audit_feature_semantics.py](scripts/audit_feature_semantics.py) 仅注释同步，**判定规则未动**（实测 HIGH 清单与基线一致，见第 6 条）。
4. `./ci/build.sh all` → **Passed 5 / Failed 0**（双架构 0w0e + host-tests + forbidden-patterns + x86_64 link）；`./ci/audit.sh quick` → **exit 0**，含 `privileged SAFETY 覆盖 (缺漏 0 ≤ 0 基线)`（F4 100%）、`audit_safety_coverage` 之外全部子审计绿。（**P2′ 收拢后复跑，结果一致**——声明收拢动了 TCB，故全门槛重跑。）
5. **覆盖无损失断言** → `make test-host` **99 个 test bin 全 ok、755 tests passed / 0 FAILED**（与门控前基线一致，无下降；**P2′ 后复跑同值**）。
6. **HIGH 清单与 HEAD 基线逐项一致（不得新增/推翻）**：当前基线 **7 项 HIGH**（`functions/sgeg/storage/disk.rs` ×2、`functions/fs/io.rs` ×1、`functions/net/unix.rs` ×1、`functions/syscall/dispatch.rs` ×3）+ 20 项 INFO（`privileged/tests/net.rs`、`privileged/tests/test_config.rs`、`functions/net/mod.rs`）——**与本批桩化文件集（`privileged/mm/*`、`privileged/boot/mod.rs`、`privileged/proc/process.rs`、`privileged/syscall/mod.rs`、`src/kernel/lib.rs`）零交集**；实测与 HEAD 影子树基线逐项一致，差异仅 `functions/syscall/dispatch.rs` 3 处行号平移（785/898/902 → 826/939/943）。原文「rc=0」系裁定方笔误（未核 HEAD 基线），已按实测修正。
7. **QEMU 三门槛**：`make test-unit` → **ALL 498 TESTS PASSED (0 skipped)**；`./scripts/qemu_boot_test.sh x86_64` → **1/1 通过**（进入 Ring 3 启动 init）；**真机路径证据（非仅编译通过）**——引导日志实测 `Boot stack canary verified`、`Boot info: mem=128 MB, kernel_end=0x3E60000`（非零 → 簇 3 真机分支生效）、`[KPTI] text region: start=0x12B000 end=0x2801B9 (342 pages)`、`[KPTI] map_text_region: …`、`[KPTI] data pages mapped: USER_CR3_SAVE=0x23FD000, …`、`[KPTI] kpti_init: kernel_pml4=0x102000, user_pml4_phys=0x3e60000, …` —— 簇 1/2/3/5 的真机分支均**实际执行**，桩化未侵蚀裸机语义。（**P2′ 后复跑同绿**：498/498 + boot 1/1，日志中 `_kernel_text_*` / `_kernel_end` 取值点均正常输出 → 声明收拢未影响真机构建。）
   - **日志留存归档（reviewer 第三轮要求：可追溯）**：boot 日志已从会被下次运行覆盖的 `build/log/qemu_boot_x86_64.log` 归档为固定名 **`build/log/qemu_boot_x86_64_t1wrap_20260918.log`**（240 行，md5 `ad749d876db2808d053e219b5f91523a`，与归档前逐字节一致）；kernel_test 日志 **`tests/reports/unit_test_20260918_165355.log`**（带时间戳，md5 `b1cf091bf44d3e6419fcd8f6c75f4d17`）。上列逐字证据行**均取自该归档文件**（原引用 `end=0x2805E9` 与本轮归档实况 `end=0x2801B9` 不符，已按归档更正）。注：两目录按 B08-10 既有裁定为 gitignore，日志不入仓 ⇒ 可追溯性＝归档文件 + 逐字行入档。
8. **本轮复核复跑（T1 收尾复核：纯复核，未改任何代码）**：
   - **P2′ 现状核实**：三处残余声明收拢在源码中在位——[mm/kpti.rs](src/kernel/privileged/mm/kpti.rs#L167-L171)（`_kernel_text_start` / `_kernel_text_end`，`not(feature = "host-test")`，模块已受 `target_arch = "x86_64"` 门控）、[boot/mod.rs](src/kernel/privileged/boot/mod.rs#L114-L117)（`_kernel_end`，块级）、[syscall/mod.rs](src/kernel/privileged/syscall/mod.rs#L124-L127)（`_kernel_start` / `_kernel_end`，单 item 级）；判据与 [mm/mod.rs](src/kernel/privileged/mm/mod.rs#L17-L22) / [proc/process.rs](src/kernel/privileged/proc/process.rs#L38-L41) 同构（符号存在性）。
   - **P1 负向验证复跑**：临时将 `syscall/mod.rs` 的 `kernel_start_ptr` 真机分支转为无条件（去掉 host 桩分支）→ `cargo test --manifest-path host-tests/Cargo.toml --test e04_shared_runner_test --no-run` **exit=101**，逐字报错：
     ```
     error[E0425]: cannot find value `_kernel_start` in this scope
        --> /home/anfer/Code/Edgine/src/kernel/privileged/syscall/mod.rs:336:23
     ```
     ⇒ **编译期**失败（非链接期 `undefined symbol`），与改写后的验收标准一致；改动已回退（`git diff --stat` 仅两份文档、临时标记全仓零残留）。
   - **P4 全门槛回归（本轮实测）**：`./ci/build.sh all` → **Passed 5 / Failed 0**（RC=0）；`./ci/audit.sh quick` → **RC=0**（**F4 SAFETY 覆盖 1924 / 1924 = 100%，缺 SAFETY 0**；clippy lib / `kernel_test` / `host-test` **三维全 passed**）；`make test-host` → **RC=0**（101 个测试二进制全 `ok`、合计 **766 passed / 0 FAILED**，对基线 755 **无下降**）；`make test-unit` → **RC=0**（QEMU `kernel_test` **ALL 498 TESTS PASSED**）；`./scripts/qemu_boot_test.sh x86_64` → **RC=0，1/1 通过**（串口 240 行，命中里程碑 `VFS ready`，进入 Ring 3 启动 init）。
   - **P3 文档同步**：本条即同步结果；「守卫两级化（编译期 / 链接期，均 fail-closed）」表述与本轮复跑实测一致，无待改项。

**审核已确认的四项静态/链接证据（裁定方复核结论，本批复核后保持一致）**

- E-04 共享测试集 `e04_shared_runner_test` 链接成功（壳 4 占位删除后仍成功）；
- 该二进制符号表 `_kernel_text*` / `_kpti_trampoline_end` / `USER_CR3_SAVE` / `stack_bottom` / `_kernel_start` **零命中**；
- clippy `kernel_test` 维 + `host-test` 维均 **RC=0**（`host-test` 维含本批新增的 10 处 expect 收窄 + `KERNEL_BASE` import 分叉）；
- **F9**：无 `#[allow(dead_code)]` / 任何死代码豁免；桩体仅三种形态（常量中性返回 / 整段跳过 / 参数消费），无逻辑承载。
- **P2′（声明收拢）复核**：六处符号声明（`USER_CR3_SAVE_ASM`、`stack_bottom`、syscall 的 `_kernel_start` / `_kernel_end`、`kpti` 的 `_kernel_text_*`、`boot` 的 `_kernel_end`）门控判据统一为**符号存在性**（唯一无 ld 脚本产物的 host-test 维排除；`USER_CR3_SAVE_ASM` 另含 `x86_64` 因为该 asm 符号仅 x86_64 有；`kpti.rs` 模块本身即 x86_64 门控）；收拢后 P1 第二处负向验证在**编译期**即失败（报错逐字见第 1 条），证明"声明随引用收拢"未造成声明/引用条件错位。

### 源码核实（2026-09-15）

- sendfile 已实装：`functions/fs/sendfile.rs:13`（包装 `privileged::syscall::sys_sendfile`）→ **D-2 清单含 sendfile 属过时**，实施前重扫。
- functions dispatch 无 read/write/execve/prctl 分支（privileged 回退层执行）→ B2 24 未迁移分类准确。
- 用户态暂不用 sendfile/readv/writev/preadv（src/user+userland grep 空）→ 高级项实装优先级可据用户态需求排后。
