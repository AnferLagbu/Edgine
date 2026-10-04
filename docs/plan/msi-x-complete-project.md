# MSI-X 完整接入工程（原 B04-02，独立工程）

> 从 [audit-fix-04-framework-net-drivers.md](./archive/audit-fix-04-framework-net-drivers.md) B04-02 擢升为独立工程（2026-08-25 用户决策"规划接入工程"）。
> 来源：DECISION-061（用户 2026-08-23 裁决"一次性完整接入 NVMe/VirtIO"）+ TOP 20 #8 + [code-audit-final-summary.md](./code-audit-final-summary.md)。
> 现状基线：分册 4 中间态（commit `57e39057`）已含 IDT 128 项 + `register_msi_irq` + NVMe `enable_msix` 端到端接入 + `MSI_VECTOR_COUNT=96`（与 IDT stub 一致）。本工程负责**验证与完整化**剩余路径。

## 工程计划 A: 现状与目标

### 背景

- **MSIX-01. 已实装基线（2026-08-25 接手核验）**
  - 描述：IDT `irq_descriptors[128]` + `init_msi_idt` stub（vector 0x40-0x9F → irq32-irq127）；`register_msi_irq(vector, handler)` 校验 irq ∈ [32,128)；`handle_irq` MSI 分支（irq ≥ 32 走 LAPIC EOI + irq_descriptors 查表）；`msi_alloc_vector`/`msix_enable`；`NvmeController::enable_msix` + `storage_init` 注册 ISR。
  - 方案：以 commit `57e39057` 为基线，本工程只做验证与完整化，不重做已实装部分。
  - 状态：[X]

- **MSIX-02. 未验证/未完整路径**
  - 描述：(1) NVMe MSI-X 端到端**无 QEMU 实测**（仅编译通过）；(2) VirtIO 使用 virtio-mmio（非 PCI），无 MSI-X capability，未接入；(3) 多队列驱动（NVMe 多队列 / virtio-pci 多队列）接入时 vector 池 96 是否够用未评估；(4) LAPIC EOI 路径未在真实中断验证。
  - 方案：按 MSIX-03~06 顺序验证与完整化。
  - 状态：[]

### 待办

- **MSIX-03. QEMU 端到端验证（NVMe 中断路径）**
  - 描述：QEMU `-device nvme` 触发真实 MSI-X 中断，验证 `handle_irq` MSI 分支 → LAPIC EOI → `nvme_msix_irq_handler` → CQ 处理。
  - 方案：`-nic none` 隔离 ISSUE-RT-001；启动参数加 NVMe 设备；klog 观察 `[MSI] Allocated vector` 与中断处理计数；验证命令完成队列（CQ）尾指针推进。
  - 详情：2026-08-25 端到端打通。QEMU 启动参数：`qemu-system-x86_64 -serial file:/tmp/nvme.log -display none -no-reboot -m 512 -nic none -kernel build/kernel.flat -device nvme,serial=QM0001,id=nvme0 -drive file=/tmp/nvme_disk.img,if=none,id=nd0 -device nvme-ns,drive=nd0,bus=nvme0`。klog 关键序列：`[MSI-X] Enabled for 00:03.0 vectors=1/65 base_vec=64` → `[MSIX-03][diag] rflags=0x2 IF=false svr=0x1FF lapic_id=0 msix ctrl=0x8040 entry: addr=0xFEE00000 data=64 vec_ctrl=0x0` → `[NVMe] MSI-X IRQ 1 fired (total 1)` (admin CQ 完成中断) → `[NVMe] MSI-X IRQ 2 fired (total 2)` (I/O CQ 完成中断) → `[MSIX-03] ISR-driven io read: Ok(())`。修复三个预存 bug：(1) `db_stride = 4 << DSTRD` 而非 `1 << DSTRD`（NVMe 规范 4 字节粒度），原 stride=1 导致 SQ doorbell 写到错误偏移 → NVMe 不响应 I/O 命令；(2) `Create CQ cdw11[31:16]` 必须写入 MSI-X Table 数组索引（NVMe 设备把 vector 字段解释为 Table entry 索引而非 LAPIC vector），所以 enable_msix 后才能正确构造 Create CQ；(3) `msix_enable` 中 MASKALL (bit14) 必须清零，否则 QEMU `msix_function_masked=true` → `msix_notify` 直接丢弃中断。
  - 状态：[X]

- **MSIX-04. virtio-pci MSI-X 接入评估**
  - 描述：当前 virtio-blk 用 virtio-mmio（INTx IRQ 11），无 MSI-X。若后续接入 virtio-pci（PCI 传输层），则启用 MSI-X。
  - 方案：评估 virtio-pci 接入范围（新传输层实现 vs 现有 mmio 迁移）；若接入则复用 `msix_enable` + `register_msi_irq`。纯评估项，可输出结论"暂不接入"并关闭。
  - 状态：[]

- **MSIX-05. 多队列 vector 池评估**
  - 描述：`MSI_VECTOR_COUNT=96`（vector 0x40-0x9F，irq32-127）。多队列驱动（NVMe 多队列 / virtio-pci 多队列）每队列 1 vector，96 个可支撑 ~96 队列。超出需扩 IDT stub（isr.asm + init_msi_idt）至 vector 0xA0+。
  - 方案：评估当前驱动规模（NVMe 1 队列 + virtio 1 队列 = 2 vector），确认 96 富余；预留扩容点注释（IDT stub 扩展时同步上调 MSI_VECTOR_COUNT）。
  - 状态：[]

- **MSIX-06. LAPIC EOI 与 aarch64 路径**
  - 描述：x86_64 `handle_irq` MSI 分支的 `send_eoi`（LAPIC 优先）需在 MSIX-03 实测确认；aarch64 MSI（GIC ITS）当前未实现，需评估是否纳入本工程范围。
  - 方案：MSIX-03 中一并验证 x86 LAPIC EOI；aarch64 GIC ITS MSI 作为独立扩展项评估（若涉及硬件能力超出当前模拟范围，记录豁免）。
  - 详情：2026-08-25 通过 MSIX-03 实测 LAPIC EOI：LAPIC SVR=0x1FF (enable) + vector=64 投递 → ISR 抢占 → `nvme_msix_irq_handler` 执行 `handle_interrupt` 后 `send_eoi` (apic_write EOI 寄存器) 退出。x86_64 路径完整。aarch64 GIC ITS MSI 未实装，本工程范围豁免（依赖硬件平台，超出当前模拟器能力）。
  - 状态：[X]

### 验证门槛

- **MSIX-07. 端到端验收**
  - 描述：QEMU x86_64 启动 + `-device nvme`，NVMe 控制器初始化走 MSI-X 中断路径，klog 确认 vector 分配 + 中断触发 + CQ 处理。
  - 方案：`scripts/qemu_boot_test.sh` 变体（-nic none + nvme 设备）+ 双架构编译 0w0e + clippy 0 警告。
  - 详情：2026-08-25 验收通过。klog 关键序列：`[MSI-X] Enabled base_vec=64` → `[MSIX-03][diag] rflags=0x2 IF=false svr=0x1FF lapic_id=0 msix ctrl=0x8040 entry: addr=0xFEE00000 data=64 vec_ctrl=0x0` → `[NVMe] MSI-X IRQ 1 fired (total 1)` → `[NVMe] MSI-X IRQ 2 fired (total 2)` → `[MSIX-03] ISR-driven io read: Ok(())`。表明：(a) MSI-X enable 成功且配置正确；(b) LAPIC 状态可投递；(c) admin CQ + I/O CQ 完成中断均成功触发 IDT 入口；(d) `nvme_msix_irq_handler` 完成 CQ 处理；(e) `submit_io_command_isr` 在中断路径下成功返回 Ok。
  - 状态：[X]

### 决策记录

- **DECISION-070**
  - 描述：MSI-X 完整接入独立工程成立（2026-08-25 用户决策"规划接入工程"，替代委托人自建 B06/B07 分册概念）。
  - 方案：分册 4 保留当前 IDT+NVMe 接入基线；剩余验证/完整化（QEMU 实测、virtio-pci、多队列、LAPIC EOI、aarch64 ITS）在本工程按 MSIX-03~07 推进。委托人自建的 DECISION-067/068/069 作废。
  - 状态：[X]
