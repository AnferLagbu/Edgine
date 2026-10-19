# P-32: aarch64 EL0 用户程序启动 SYNC（sysreg trap）根因工程

> 输入依据：P-31 R6 实机取证副产物（[aarch64-virtio-net-tx-rootcause.md](./aarch64-virtio-net-tx-rootcause.md) R7 移交项，
> 用户 2026-10-19 裁定立独立工程）。网络数据面已翻绿（`tx_timeout=0`），本工程目标是打通 aarch64
> 用户态链路最后一环：init 存活 → `[tcp] Listening` 出现 → e2e 软观测 `listen=1` 翻绿。

---

## 背景与已知取证数据

P-31 修复后 aarch64 QEMU e2e 软观测数据行 `observe=fail reason=not_ready listen=0 addr=1 tx_timeout=0`：
内核侧网络栈已真实工作（DHCP 租约在位），但用户态 init 进程**启动即死**，`[tcp] Listening on 0.0.0.0:80`
永不出现。boot 里程碑日志实录：

- **SYNC 异常**：`ESR=0x92000006` — EC=0x24（Trapped MSR/MRS/System register 访问）、IL=1、ISS=0x6；
- `ELR=0x00000000004000D8`（user 低区，init 入口附近）、`FAR=0xFFFF000040080000`；
- modern 对照跑还见过 **EC=0x0E（Illegal exception return，非法 ERET）**——提示 ERET 前
  `SPSR_EL1/ELR_EL1` 组合或执行态设定存在第二处不一致。

x86_64 路径无此问题（e1000 + 用户态 echo 全链 1/1），故障域限定 aarch64 EL0 进入/异常返回协议。

## 目标

- aarch64 init 进程正常启动并常驻，`[tcp] Listening` 里程碑出现；
- `A64_E2E observe=pass`（或残余 fail 原因不再含 EL0 SYNC）；
- x86_64 腿零影响；§2.3 六门槛全过。

## 任务条目

- **E1: §10 源码调研（EL0 进入链 + 异常分发认知）**
  - 描述：摸清 aarch64 EL1→EL0 交接与 SYNC 分发全链，形成可追溯认知后再归因。
  - 方案：① 用户程序装载/入口设定（`privileged/proc/elf` + TCB 首返回路径）；② `privileged/arch/aarch64`
    SYNC handler 的 EC dispatch（0x24 分支现状：die or emulate）；③ 进入 EL0 前的 `SPSR_EL1` 构造
    （DAIF/SSBS/M 域）与 `TCR/SCTLR_EL1` 中影响 EL0 sysreg 可见性的位；④ 反汇编
    `other/build/aarch64/user/init` ELF 定位 `ELR=0x4000D8` 处具体指令（判读目标 sysreg）。
  - 状态：[X]
  - 详情（E1 实得认知，2026-10-19）：④ 已完成 — `ELR=0x4000D8` 非 sysreg 指令而是
    `ldrb w8, [x8]`，`x8=0xFFFF000040080000` = `src/user/init/src/main.rs` 的
    `KERNEL_IMAGE_ALIAS` KPTI-09 探针（故意在 **fork 子进程**里读内核高半区别名，
    预期内核终止探针子进程而非 init）；FAR 与之一致。日志中
    `EL0 sync fault: pid=8 -> terminate` 与探针子进程语义高度吻合（KPTI-09 预期行为），
    故首发现场本身可能不是缺陷。真正待归因的缺口：`[tcp] Listening` 属 init 后续段，
    boot 里程碑 KPTI/SMP/EL0-IRQ 全过但均为内核侧锚定，init 本体是否存活至 TCP 服务段
    尚未用用户态输出锚定。故障域收敛候选：① init 在探针之后的某段（如网络 syscall/后续
    fork）再次 EL0 异常被终止（查 terminate 日志是否仅此一条）；② 父进程 wait_pid 收割
    探针子进程时滞留/死锁（多核任务投送路径）；③ 内核 terminate 作用域错位连带伤父；
    ④ 异 EC 细节：数据读上报 EC=0x24（应 0x25/0x21）属 ESR 读取/解码链问题还是 QEMU
    行为——若为内核读错 ESR，terminate 后的处置分支也可能选错，归 E2 负样本。

- **E2: 复现取证（第一现场全寄存器）**
  - 描述：QEMU aarch64 稳定复现 EL0 SYNC，抓取 x0-x30/SPSR/ELR/FAR 全量 + 触发指令反汇编，
    坐实"哪个 sysreg、哪条访问指令、为何 trap"。
  - 方案：boot test 镜像复现 + 既有 SYNC 日志扩展（必要时临时诊断，用毕即撤，先例 P-31）；
    同步复现/判定 EC=0x0E 非法 ERET 路径。
  - 状态：[]

- **E3: 根因裁定与修复**
  - 描述：依 E1/E2 结论定修复——候选根因（不预设，逐项证实/证伪）：
    a) 编译器运行库/启动代码访问 EL0 被 trap 的 sysreg（如 CNTVCT_EL0/TPIDRRO_EL0 未初始化或未放行）；
    b) 内核进入 EL0 的 SPSR/上下文构造缺陷（对应 EC=0x0E 线索）；
    c) 用户 ELF 入口/链接基址与内核交接约定不一致。
  - 方案：待 E2 证据闭合后填（机制层放 privileged，策略层放 functions，遵归属决策树 §4.1）。
  - 状态：[]

- **E4: 回归测试**
  - 描述：锁死修复点防回归（sysreg trap 分发契约 / ERET 构造断言，按 E3 结论具体化）。
  - 方案：kernel 内联单测 + host-tests 契约；`make test-kernel-host` 覆盖。
  - 状态：[]

- **E5: 实机验证 + 台账收口**
  - 描述：`./scripts/qemu_boot_test.sh aarch64` 软观测翻绿 + P-31 R7/net-feature-alignment T1 台账回填。
  - 方案：数据行期望 `listen=1`，`observe=pass`（accept/echo ≥ E2E_ROUNDS）；双架构六门槛。
  - 状态：[]

## 验证门槛

AGENTS.md §2.3 全部 6 条 + E5 专项（a64_e2e 数据行）。

## 关联文档

- [aarch64-virtio-net-tx-rootcause.md](./aarch64-virtio-net-tx-rootcause.md) — P-31 R7 移交来源
- [net-feature-alignment.md](./net-feature-alignment.md) — T1 软观测载体（listen 判据）
- `src/kernel/privileged/arch/aarch64/`（SYNC handler / ERET 路径）、`src/kernel/privileged/proc/elf/`（装载入口）
