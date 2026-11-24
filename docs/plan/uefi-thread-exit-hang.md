# UEFI 引导下 init 线程探针挂死（独立工程）

> 立项依据: [userspace-decoupling.md](./userspace-decoupling.md) D4-i 丙的 UEFI 真跑腿归因轮发现,
> 用户裁定 **「登记独立工程 + 腿按可达面接线」** —— 立项批只登记与取证待授权, 当轮不施工;
> 解耦批按原计划收口, `scripts/uefi_boot_test.sh` 按实测可达面断言; 修复前挂死腿以**显式横幅**声明不判.
> **2026-11-24 进展**: 用户授权 U-1 进入修复队列（P1），已完成取证并采用「uaccess 写前预检」根治；
> 不改 5s 预算，`[THREAD] PASS` 已加回 UEFI 硬断言并删除横幅，另加 `[UACCESS] PASS` 钉住根因。
> U-2（单核更早静默挂死）保持对照，不随 U-1 修复自动关闭。
> 关联登记: [init-probe-fault-branches.md](./init-probe-fault-branches.md) F-1/F-2 (同属 init 探针故障家族,
> 但那是 aarch64 跳零 / x86_64 NULL 拷贝的**偶发**分支, 本册是 UEFI 引导下的**确定性**分支);
> [aarch64-reap-timeout-rootcause.md](./archive/aarch64-reap-timeout-rootcause.md) (H1/H2 已修, 签名同为
> `reap-timeout` 但**病因不同** —— 见下「与 H2 的区分」).
> 边界: 不放宽 `THREAD_WAIT_BUDGET_MS` / 不放宽任何断言 / 不为消红延长预算（「红先归因」口径）;
> 取证改装（gdbstub 任务态 dump、`[IDT]`/`[REAP]` 行扩字段）需用户授权后才可施工.

---

## 背景与已验证事实

D4-i 甲 修复了「MB2 信息块指针硬编码 0 + inline RSDP 被当指针读」两处预存缺陷后, UEFI 引导
**首次**拿到正确的 ACPI 表与双核（此前 `AP count=0`, 恒单核）。双核 UEFI 是一个**此前不可达**的
配置, 本册缺陷是它首次被观测到, 不是新引入。

### 对照读数（同一 `boot_module.iso`, 同一内核, 串行执行）

| 面 | BIOS (SeaBIOS) | UEFI (OVMF) |
|---|---|---|
| `[THREAD]` 探针 | **5/5 PASS** | **5/5 FAIL: `reap-timeout code=0`** |
| `[TLS]` / `[SIG]` / `[EXEC]` / `[WASM]` / `[SCHED]` / `[KPTI]` | PASS | PASS（逐项与 BIOS 同） |
| `[ACPI]` RSDP 来源 | Multiboot2 | Multiboot2 |
| MADT / FADT / HPET | 全解析 | 全解析 |
| `AP count` / `online CPUs` | 2 / 2 | 2 / 2 |
| `Timebase` (PIT measured) | 2095675222 Hz | 2095881036 Hz |
| firmware 预留区条目数 | 7 | 13 |
| `mb2-info` 区 | `base=0x12A3C8 size=1392` | `base=0x2000 size=6208` |
| `Boot info` | `mem=511 MB, kernel_end=0x3EA000` | 同 |
| `Buddy ready` | 131039 total / 125621 free / reserved 4074 | 130896 total / 124692 free / reserved 4074 |

时基读数几乎相同 ⇒ **不是定时器标定差异**。除 `[THREAD]` 外全部探针逐项同形 ⇒ 不是载体/文件系统/
exec/wasm/观测面的问题。

### 落点：`group_exit_leg`（探针最后一条腿）

[main.rs:858](../../src/user/init/src/main.rs#L858) `group_exit_leg` fork 子进程 →
[main.rs:896](../../src/user/init/src/main.rs#L896) `group_child_body` 起一个睡死在 `GROUP_HOLD`
上的 helper → 组长 `exit_group(7)` → 父进程按 `7 << 8` 收割。

| 读数 | BIOS | UEFI |
|---|---|---|
| 组长 / helper 退出 | `exit: pid=13 code=7` + `exit: pid=14 code=7` | **两条都缺席**（直到 25 s 后仍无） |
| `[REAP]` 取证行 | 无需（正常收割） | **缺席** |
| `[SCHED] TG tgid=13` | 快照时已收割, 不在表里 | `members=2 runnable=1`（仍活着） |
| `SNAP END` | `cpus=2 tasks=2 tg=5` | `cpus=2 tasks=3 tg=6` |
| cpu1 `wakeups` / `wait_n` | 37 / 42 | **4050 / 4049**（唤醒 churn） |

`[REAP]` 缺席是**有信息量的**: [process.rs:1219](../../src/kernel/privileged/proc/process.rs#L1219)
`diag_unreaped_group` 的首道判据是 `group.exit_requested()`
([process.rs:1220](../../src/kernel/privileged/proc/process.rs#L1220)), 为假即静默。故
**`exit_requested` 从未被置位** —— 组长根本没走到 `exit_group` 的组退出登记面, 而不是
「组已退出但成员收不齐」。这与 H2 的现场（`exit: pid=15 code=7` 已打印 + `[REAP] … state=Blocked`）
**形态相反**。

子进程侧三个可能出口（[main.rs:1040-1076](../../src/user/init/src/main.rs#L1040-L1076)）
`proc_exit(2)`（clone 失败）/ `proc_exit(3)`（helper 报到超时）/ `exit_group(7)` **一个都没出现**。

**2026-11-24 取证更正**: 旧读数里 `[SCHED] TG members=2` 只能证明 helper TCB **已登记进组**，
不能证明 clone 已成功返回 —— 取证已把落点继续前移到 **`clone_thread` 的 `vm_child_register` 内、
TID 回写处**：组长进程在 `clone_vm_path` 里完成 `alloc/inherit/join_group/process_insert/adopt`
后，卡在对 `parent_tidptr` 的 `write_struct_to_user`；helper 的 TCB 已插入进程表但**尚未入运行队列**
（`scheduler_add_to_run_queue` 在 TID 回写之后），因此 helper 一行打印也不会出现。
完整证据见下节「取证结果」。

### 取证结果（2026-11-24）：根因是 x86 内核写 COW 只读用户页触发核态 #PF 停机

**乙取证（用户态三出口打印）**，UEFI `-smp 2` 两轮：

1. 第一轮只在 `group_child_body` 加 clone 后打印：串口只有 `[GU-ATTR] leg start` 与
   `[GU-ATTR] parent child=1…`，子进程无任何行。
2. 第二轮补 `fork child branch` / `child body enter` / `child before clone`：串口出现
   `[GU-ATTR] fork child branch`、`[GU-ATTR] child body enter`、`[GU-ATTR] child before clone`，
   **没有** `child clone rc=`，也没有 `helper enter` / `helper reported`。

结论：fork 子进程已上核、已进入 `group_child_body`，但 `clone_thread` 系统调用**从未返回**。

**内核侧细分（`vm_child_register` 各步打印）**，同一 UEFI 轮：

```
[GU-K] clone_vm enter parent=13 flags=0x1390F00 stack=0x40E0E0
[GU-K] alloc ok child=14
[GU-K] inherit ok child=14
[GU-K] register begin child=14
[GU-K] reg join_thread_group begin child=14
[GU-K] reg insert begin child=14
[GU-K] reg adopt ok child=14
[GU-K] ptid write begin child=14 ptr=0x40D0D0
```

`ptid write begin` 之后没有 `ptid write ok`；同一轮 5 s 后父进程（pid=5）另起的 clone（child=15）
从 `ptid write begin ptr=0x40C06C` 到 `ctid write ok` / `cleartid store ok` / `reg enqueue ok`
全部正常。挂点因此钉死为 **`vm_child_register` 对 `CLONE_PARENT_SETTID` 指针的
`write_struct_to_user(parent_tidptr)`**。

**QEMU 异常直证**（干净镜像、不加载任何打印，`-d int` 抓 #PF）：

```
v=0e e=0003 i=0 cpl=0 IP=0008:0000000000141ed1 CR2=000000000040c0d0
```

`e=0003` 解码为 **present + write + supervisor**（P=1、W/R=1、U/S=0）：ring0 对
**存在的只读页**做写；`addr2line` 将该 IP 落在
`kernel_privileged::syscall::raw::write_struct`（`write_struct_to_user` 的 i32/u32 单态）内，
CR2 正是组长进程 `group_child_body` 的 BSS TID 槽。之后该 CPU 进 panic 停机循环，父进程
5 s 后 `[THREAD] FAIL: reap-timeout code=0`。

**为什么 BIOS 同镜像 PASS**：两腿 `CR0.WP` 实测都是 1（UEFI `0x80010033`、BIOS `0x80000011`），
所以不是 WP 位差异；BIOS 下同一 `parent_tidptr` 写成功，说明该用户页在 BIOS 侧不是
COW 只读（大页映射不参与 COW / 或已被先前写破 COW），而 UEFI 侧该页是 4K COW 只读。
x86_64 的 `#PF` 处理器目前只把核态 `ProtectionViolation` 直接 `Panic`
（[handlers.rs:274](../../src/kernel/privileged/idt/handlers.rs#L274)），没有 aarch64 那条
`break_cow_for_kernel_uaccess` 窄支路
（[page_fault.rs:694](../../src/kernel/privileged/mm/page_fault.rs#L694)），于是核态写 COW 页
= 停机，`clone_thread` 永不返回。

**修复（2026-11-24，已落地）**：采用“uaccess 写前预检”根治，不依赖跨函数恢复点：

1. `page_fault::validate_user_writable` 逐页确认用户页可写；只读但带 fork COW 标记时先
   在预检里 `cow_handle_fault` 破 COW，真只读/未映射直接返回 `false`。
2. `userptr::write_struct_to_user`、`syscall::raw::write_struct` / `write_u64` /
   `write_u64_pair`、`copy_user::copy_to_user` / `clear_user` 全部先过预检；
   读路径 (`copy_from_user` / `copy_string_from_user` / `strlen_user`) 逐页确认已映射。
3. x86 `#PF` 只保留 COW 竞态窄兜底（present + write + supervisor + 用户 leaf/只读/帧带
   COW 标记），命中破 COW 重试，不命中仍 panic。
4. `vm_child_register` 把 TID 回写移到入组/进程表登记之前；写失败释放 TCB、返回 `EFAULT`。

设计册：[`docs/design/uaccess-design.md`](../design/uaccess-design.md)。旧 `setup_recovery`
异常恢复点机制已删除（其标签在函数体内、无法安全跨函数返回，且此前无消费者）。

**复原**：本轮所有 `[GU-ATTR]` / `[GU-K]` 插桩已 `git checkout` 复原，工作树干净，HEAD 仍
`c9122f96`；证据日志 `/tmp/uefi_gu_attr_r{1..8}.log`、`/tmp/uefi_manual_serial.log`、
`/tmp/qemu_int.log` 为临时取证，不入库。

### 与 H2 的区分（不要误并归因）

H2（已修）的机理是「组长的 `unblock` 落在 helper 仍 Running 的窗口内被丢弃 + 入睡侧不查 pending
SIGKILL」, 现场必有 `exit: pid=<组长> code=7` 与 `[REAP] … state=Blocked`。本册现场**两者皆无**,
且 H2 修复面（入睡侧 SIGKILL 检查）覆盖的是 `process_block_finish` 收尾路径。故本册是**另一条分支**,
不得按 H2 已修结案。

## 归因：非 D4-i 乙 引入（负控制直证）

为排除「firmware 保留区进 PMM 早期预留」（D4-i 乙, [lib.rs:666-679](../../src/kernel/lib.rs#L666-L679)）
是本缺陷的成因, 做了**改装负控制**: 只注释掉 firmware 预留循环（保留 `mb2-info` 预留, 使 ACPI/SMP
仍成立）, 真构建 + 真启动 UEFI `-smp 2` 三轮:

| 轮 | `Early reserve 'firmware'` 行数 | 结局 |
|---|---|---|
| nc_r1 | 0 | **更早挂死** —— 用户态停在 `[TLS] PASS` 之后, `[THREAD]` 一行都不打印; 内核心跳仍走（tick=15000） |
| nc_r2 | 0 | 同 nc_r1 |
| nc_r3 | 0 | `[THREAD] FAIL: reap-timeout code=0`（与有预留时同签名） |

三轮 `RSDP found via Multiboot2` + `online CPUs: 2` 均在 ⇒ ACPI/SMP 面未受改装影响。
**结论: 去掉预留后 UEFI 从未通过, 且 2/3 轮挂得更早** ⇒ firmware 预留不是成因, 它反而**改善**了
UEFI 稳定性（有预留时 5/5 都能跑到 `group_exit_leg`）。

改装已复原: `src/kernel/lib.rs` 以 `cp` + `sha256` 对拍还原
（`e81768dacfbe085ac483f77a7cbd60d9de1a26fa993f2eca636197a06b3d5579`）, 重构建后 BIOS 复核
`[THREAD] PASS` + firmware 预留 7 条, 与改装前读数一致。

同时排除的两个候选:

- **MB2 信息块与 AP trampoline 重叠**: trampoline 固定在 `0x8000`
  ([smp_init.rs:18](../../src/kernel/privileged/arch/x86_64/smp_init.rs#L18)), UEFI 的 MB2 信息块占
  `0x2000–0x3840`, 不重叠; 且同轮 dump 出的 XSDT/FACP/APIC/HPET 字节完整, ACPI 表未被踩。
- **tag6 漏掉 EFI 必须保留的类型**: 同轮 guest-physical dump 对账, tag17 的 95 个描述符里
  13 个 Linux-must-reserve 类型区**全部**被 tag6 的 13 条非可用条目覆盖, 缺 0 ⇒ 无需解析 tag17。

## U-1 UEFI 双核下 `group_exit_leg` 确定性挂死

- **描述**: OVMF + GRUB2 multiboot2 + `-smp 2` 引导时, init 线程探针的最后一条腿 `group_exit_leg` 确定性失败（5/5）, 签名 `[THREAD] FAIL: reap-timeout code=0`; 组长与 helper 两个 TCB 永不退出, `exit_requested` 从未置位（`[REAP]` 缺席）, cpu1 出现 4050 次唤醒 churn。同镜像同内核在 SeaBIOS 下 5/5 通过, 其余探针逐项同形。**2026-11-24 取证已定位**: 挂点在 `clone_thread` 的 `vm_child_register` 对 `parent_tidptr` 做 `write_struct_to_user` 时；UEFI 下该页是 4K COW 只读, ring0 写触发 `#PF e=0003`, x86 `#PF` 无核态 uaccess 破 COW 支路 ⇒ panic 停机, clone 永不返回、helper 不入运行队列。
- **方案**: 已按“uaccess 写前预检”根治：`validate_user_writable` 逐页预检写权限并主动破 COW，所有写助手接入；`clone` TID 回写前置、失败返回 `EFAULT`；x86 `#PF` 仅保留 COW 竞态窄兜底。旧跨函数恢复点机制删除。**不改 5 s 预算**；修复后已把 `[THREAD] PASS` 加回 UEFI 硬断言并删除横幅。
- **状态**: [x] 已修复（P1，2026-11-24）。UEFI `-smp 2` 连续 5/5 `[THREAD] PASS`；新增 `[UACCESS] PASS: readonly clone EFAULT` 回归锁；BIOS 与 aarch64 同镜像 PASS。
- **详情**: 对照读数、负控制与完整取证链见上「背景与已验证事实」「归因」「取证结果」三节。修复验证日志 `/tmp/uefi_uaccess_r{1..5}.log`、`/tmp/bios_uaccess_fix.log`、`/tmp/aarch64_uaccess_fix.log`、`/tmp/uefi_uaccess_probe.log` 为临时取证, 不入库。历史证据日志 `/tmp/uefi_attr/`、`/tmp/uefi_gu_attr_r{1..8}.log`、`/tmp/uefi_manual_serial.log`、`/tmp/qemu_int.log` 同样不入库。

## U-2 UEFI 单核下更早的静默挂死

- **描述**: OVMF + `-smp 1` 时挂点比 U-1 更早: 用户态停在 `[VMM] kview build` 之后（探针 fork 子进程建内核视图处）, `[THREAD]` 一行不打印, **心跳 tick 也停**（25 s 窗口内只有 1 条 `HB] tick`）⇒ 属静默挂死形态, 不是「探针超时」。D4-i 甲 之前的三次 UEFI 真跑（`AP count=0`, 恒单核）日志尾部逐字同形（`kview build` @~1.13 s）, 说明该挂点**先于本轮改动存在**。去掉 firmware 预留的双核轮（nc_r1/r2）也退化到这一类更早挂死（停在 `[TLS] PASS` 后, 但心跳仍走）。
- **方案**: 与 U-1 合并取证还是分立, 待 U-1 甲/乙取证出数后按落点是否同源再裁; 本轮不预设。既有静默挂死装备（`scripts/qemu_liveness_probe.sh` 的 gdb 腿、`[HB]` 心跳区分读数）可复用, 不需新造。
- **状态**: []
- **详情**: `-smp 1` 不是出货配置（headline 腿恒 `-smp 2`）, 故本条不阻断门禁; 登记价值在于它是「UEFI 引导路径存在与 SMP 无关的用户态挂死」的直证, 可作 U-1 取证时的对照面。本条目只登记, 不施工。

## 门禁口径（本轮已落地部分）

`scripts/uefi_boot_test.sh`（x86_64-only）按**实测可达面**断言, 接 `ci/audit.sh` `full` 档步骤 4:

- **硬断言**: OVMF 真启动到 `Entering Ring 3`; `RSDP found via Multiboot2`; MADT `AP count=2` +
  FADT + HPET 三表; `[SMP] online CPUs: 2` + TLB probe PASS; `Boot firmware reserved regions: N`
  且 N > 0 + `Early reserve 'firmware'` 行数与 N 一致 + `Early reserve 'mb2-info'` 在;
  `mem=` 取自 tag6 而非 tag4 兜底; 载体链（`[INITRAMFS] Unpacked` + `init from /init`）;
  `[EXEC] PASS: cwd+root` / `[WASM] PASS:` / `[SCHED] SNAP END` + `conserve=ok` 且无 `conserve=BAD`;
  `[TLS] PASS` / `[SIG] PASS` / `[KPTI] EL0 … denied`。
- **U-1 修复后新增硬断言**: `[UACCESS] PASS: readonly clone EFAULT`（只读 TID 回写必须返回
  `EFAULT`，钉住本轮根因）与 `[THREAD] PASS:`（clone/futex/join/exit_group 全链路）。
  两者均由 host 侧接线形状锁把守：横幅残片出现即红，任一条被摘即红。
- **三态**: OVMF CODE/VARS 缺失时 `ci/audit.sh` 走 `skip_leg "uefi"`, 计入未执行腿数并打印
  「零读数, 不可当作已验证」, 不当绿。

U-1 已转 `[x]`；U-2 仍 `[]`，留作单核对照，未占修复队列。