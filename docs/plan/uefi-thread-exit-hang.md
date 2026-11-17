# UEFI 引导下 init 线程探针挂死（独立工程）

> 立项依据: [userspace-decoupling.md](./userspace-decoupling.md) D4-i 丙的 UEFI 真跑腿归因轮发现,
> 用户裁定 **「登记独立工程 + 腿按可达面接线」** —— 本册只登记与取证待授权, **本轮不施工**;
> 解耦批按原计划收口, `scripts/uefi_boot_test.sh` 按实测可达面断言, 挂死腿以**显式横幅**声明不判.
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

子进程侧三个可能出口（[main.rs:911-920](../../src/user/init/src/main.rs#L911-L920)）
`proc_exit(2)`（clone 失败）/ `proc_exit(3)`（helper 报到超时）/ `exit_group(7)` **一个都没出现**,
且 `members=2` 说明 helper TCB 已建 ⇒ clone 已成功。落点因此收窄到
**「clone 返回之后、`exit_group` 登记之前」**这一段, 其中 `futex_wait_until(&GROUP_HELPER_READY, …, deadline)`
带截止时刻却也没吐出 `code=3`, 是首要疑点。

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

- **描述**: OVMF + GRUB2 multiboot2 + `-smp 2` 引导时, init 线程探针的最后一条腿 `group_exit_leg` 确定性失败（5/5）, 签名 `[THREAD] FAIL: reap-timeout code=0`; 组长与 helper 两个 TCB 永不退出, `exit_requested` 从未置位（`[REAP]` 缺席）, cpu1 出现 4050 次唤醒 churn。同镜像同内核在 SeaBIOS 下 5/5 通过, 其余探针逐项同形。落点收窄在「clone 返回后、`exit_group` 登记前」, 首要疑点是带截止时刻的 `futex_wait_until(&GROUP_HELPER_READY, …)` 既不放行也不吐 `code=3`。
- **方案**: 待授权取证后再钉。候选取证面（均需改装授权）: 甲、QEMU gdbstub 挂死时 dump 两个 TCB 的 `ProcessState` / `BlockReason` / `sleep_until` 与用户态 RIP（判「睡死」还是「自旋」, 直接分开 `futex_wait_until` 与 `exit_group` 两个候选）; 乙、`group_child_body` 三个出口前各加一行无条件取证打印（clone 返回码 / READY 字面值 / 进入 `exit_group` 前）, 一次判死落点; 丙、`diag_unreaped_group` 的 `exit_requested` 判据旁增一条「组长非 Zombie 且超预算」的取证发射面（只取证, 不改收割语义）。**不放宽 5 s 预算、不改 `[THREAD]` 判据**。
- **状态**: []
- **详情**: 对照读数与负控制见上「背景与已验证事实」「归因」两节。证据日志留存 `/tmp/uefi_attr/`（`bios_r{1..5}.log` / `uefi_r{1..5}.log` / `nc_r{1..3}.log` / `bios_full.log`）与 `/tmp/uefi_d4i/boot.log`; 这些是临时取证目录, 不入库。本条目只登记, 不施工。

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
- **显式不判（非静默跳过）**: `[THREAD]` 腿。脚本无条件打印横幅指明本册 U-1/U-2 已登记、
  该腿在 UEFI 下为已知红、**本轮不判**, 并打印实测到的 `[THREAD]` 行原文供对账。
  横幅与不判口径由 host 侧接线形状锁钉住, 防止日后被静默删掉或悄悄改成「断言 PASS」。
- **三态**: OVMF CODE/VARS 缺失时 `ci/audit.sh` 走 `skip_leg "uefi"`, 计入未执行腿数并打印
  「零读数, 不可当作已验证」, 不当绿。

U-1 修复后应把 `[THREAD] PASS` 加回硬断言面并删掉横幅, 届时本册两条目转 `[X]`。