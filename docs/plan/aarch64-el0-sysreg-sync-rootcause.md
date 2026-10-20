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
  - 状态：[X]
  - 实测（2026-10-19，桌面取证 + 3 轮样本）：前提推翻 — 首轮 EL0 SYNC（EC=0x24，
    ELR=0x4000D8）反汇编坐实为 init KPTI-09 探针的**预期行为**（探针子进程 ldrb
    读内核别名被终止，父进程存活至 `[tcp] Listening` 段——但该段整体被
    `#[cfg(target_arch = "x86_64")]` 门禁，aarch64 从未编译 echo 服务）：
    **`listen=0` 的直接原因是用户态门禁而非内核缺陷**，旧门禁的"aarch64 无 NIC
    环境"前提已随 P-31 翻绿失效。放行后又现**真预存 flaky 竞态**（3 样本：1 静默
    死寂 / 1 未达就绪 / 1 全链达成 listen=1+accept/echo/close），两形签名：
    ① 收割窗口（destroy_page_table/TLB shootdown 前后）全日志静默；
    ② `socket_egress+0xa0` 跳转表野分支 SYNC（ESR=0x96000004，ELR 在 kernel text
    内、FAR=0xFFFE000080532566 未映射，SocketSet 槽 state 字读出垃圾 → handler
    WFI 死循环整机静默）。EC=0x24 异于预期 0x25 的细节归入签名②一族待查。

- **E3: 根因裁定与修复**
  - 描述：主修复已落地；残余 flaky 竞态（E2 两形签名）已根治 — 根因锁定为
    **ERET-return-window 竞态**，非内存覆写族假说。
  - 方案：① 已完成 — `tcp_echo_server`/`delay_ms` 门禁改
    `cfg(any(x86_64, aarch64))`（实现本已 arch 中性），run3 样本证明 aarch64
    全链（Listening/accept/recv/send/close）可达成；其余 UDP/SLAAC/组播探针
    依赖 x86 宿主 tap/dnsmasq 设施，维持门禁不动。② 已完成 — flaky 竞态按
    P-31 范式扩样 + 埋点（批 2–11）逐层证伪内存假说，最终锁定系统寄存器竞态。
  - 状态：[X]
  - 详情（E3 根因裁定，2026-10-19，批 11 bracket 探针决定性取证）：
    **根因 = ERET-return-window 竞态**。`handle_el1h_irq` 出口序列
    `msr elr_el1,x0` → `msr spsr_el1,x1` → … → `ERET` 之间，若 `PSTATE.I` 处于
    放行态（`irq_handler` 子树中 `do_softirq` 开中断 / `run_pending_resched`
    上下文切换往返所致），一条异步异常可在此窗口被硬件取走，把刚写入的
    `ELR_EL1/SPSR_EL1` **系统寄存器**覆写成嵌套现场 ⇒ 外层 `ERET` 返回错误上下文
    → illegal ERET 级联（saved SPSR 被误判成 EL0/保留 EL1t → 核死寂或误杀服务进程）。
    - **证伪链**：批 11 bracket 探针在 `irq_handler` 入口/出口比对 `frame.elr/spsr`
      ⇒ `BRK DRIFT=0`（异常帧**内存**从未被改）→ 证伪 P-a 子树野写覆写活帧；
      `BRK E` 暴露 `ELR=0xffff000040206100`（×64，恰落 epilogue `eret` 前一条）
      → 证实 IRQ 在 epilogue 内被取走、此时 `PSTATE.I` 开着。H1 epoch / H2-kstack
      （上段 `KSM=none` 已证伪）/ H3 跨调度栈 / poll_network / virtio OOB /
      close UAF 全部排除。
    - **修复**：三处 epilogue（`handle_el1h_sync` / `handle_el1h_irq` / `el0_return`）
      在 `msr elr/spsr` 之前加 `msr daifset, #0xf`，返回序列全程屏蔽异步异常，
      挂起项留待 `ERET` 依 SPSR 恢复放行后投递。净改动 = 17 行（仅 daifset + 中文注释）。
    - **method-A 反转**：上段以为"softirq 延迟 poll 是卫生改进、保留"实为误判 —
      它把 tick 腿 `poll_network` 延迟到 softirq，违反已提交 E-IV 契约
      （`net_poll_at_hrtimer_test::eiv_tick_leg_untouched`），且正是 `echo=0` 成因。
      回退 method-A（恢复 tick 腿无条件 `poll_network`）后崩溃（靠 daifset）与
      echo（靠无条件 tick poll）同时解决。
    - **经验确证**：修复后 16 轮跑样 silent_dead 8→0/16、`BRK E@epilogue` 64→0、
      SYNC! 崩溃签名→0（残余 terminate 全为良性 KPTI-09 探针）。

- **E4: 回归测试**
  - 描述：锁死修复点防回归 — ERET 返回序列的异步异常屏蔽顺序契约。
  - 方案：host-tests 文本契约测试 `aarch64_eret_window_mask_test.rs`（3 test）：
    校验 `handle_el1h_irq` / `handle_el1h_sync` / `el0_return` 三处 epilogue 中
    `msr daifset, #0xf` 严格早于 `msr elr_el1, x0`（若后续重排把 daifset 挪到
    `msr elr/spsr` 之后即回归崩溃，测试阻断）。附带修正 `aarch64_el0_frame_symmetry_test`
    因注释小写 `eret` 令 `slice_between` 提前截断的既有文本冲突（注释改大写 `ERET`）。
  - 状态：[X]

- **E5: 实机验证 + 台账收口**
  - 描述：`./scripts/qemu_boot_test.sh aarch64` 软观测翻绿 + 双架构六门槛全过。
  - 方案：数据行期望 `listen=1`，`observe=pass`（accept/echo ≥ E2E_ROUNDS）。
  - 状态：[X]
  - 实测（2026-10-19，最终修复树）：aarch64 QEMU 完整启动至 EL0，KPTI 隔离断言
    通过、SMP 双核上线、EL0 中断可达、双核并发 EL0；**e2e 软观测 `3/3 rounds echoed`，
    数据行 `A64_E2E observe=pass accept=3 echo=3 addr=dhcp tx_timeout=0`** — E2 立案的
    flaky 竞态与 `echo=0` 一并根治。六门槛复核：`./ci/build.sh all` Passed 6/Failed 0、
    双架构 clippy（build-std 口径 + `-D warnings`）RC=0、`cargo fmt --check` 无漂移、
    核心审计（functions_boundary/safety_coverage/comment_language/coupling/invariants/
    deadlock_matrix）全过、host-tests 0 failed（含 `aarch64_eret_window_mask_test` 3/3）、
    `make test-kernel-host` 989 passed/0 failed、x86_64 QEMU 腿不受影响（本次仅改
    `arch/aarch64/exception.rs`，x86 二进制字节不变）。

## 验证门槛

AGENTS.md §2.3 全部 6 条 + E5 专项（a64_e2e 数据行）。

## 关联文档

- [aarch64-virtio-net-tx-rootcause.md](./aarch64-virtio-net-tx-rootcause.md) — P-31 R7 移交来源
- [net-feature-alignment.md](./net-feature-alignment.md) — T1 软观测载体（listen 判据）
- `src/kernel/privileged/arch/aarch64/`（SYNC handler / ERET 路径）、`src/kernel/privileged/proc/elf/`（装载入口）
