# 用户态与内核镜像解耦 + aarch64 EGFS 根 + kview 低半区窗口退役（插队工程）

> 一句话说清为什么有这个计划: 内核把全部用户态内容 (init / 程序 / wasm 桩 / cpio) 用
> `include_bytes!` 编译期嵌进 rodata, 把「用户态增长」硬绑到受 K-08 (< 4 MiB) 约束的
> 内核镜像上; WASM 退用户态后 wasmi 运行时 + 载荷只会让耦合加剧. 本册把用户态内容**移出**
> 内核镜像 (对齐 Linux: boot-module initrd → EGFS 磁盘根), 补齐 aarch64 EGFS 根, 并在收尾
> 单独退役 x86_64 造成 K-08 的低半区 kview 恒等窗口. **本册插队于
> [wasm-userspace-migration.md](./wasm-userspace-migration.md) P1 之前**: P1 wasmi demo 的
> x86_64 腿正因 K-08 无法落地, 解耦是本前提. 归属与方向权威依据见
> [service-placement-design.md](../design/service-placement-design.md) §四、
> [wasm-runtime-design.md](../design/wasm-runtime-design.md) §七; EGFS 现状证据见
> [egfs-persistent-fs-assessment.md](../report/egfs-persistent-fs-assessment.md) T-01.

---

## 背景与根因

**K-08 的来历**: 内核镜像末尾 `_kernel_end` 必须 ≤ `USER_CODE_BASE=0x400000` (4 MiB)
([privileged/boot/mod.rs:351](../../src/kernel/privileged/boot/mod.rs#L351),
[config/memory.rs:59](../../src/kernel/privileged/config/memory.rs#L59)). x86_64 的 kview
(内核视图根表) 在低半区保留 `[0, ceil(kernel_end/2MiB))` 的 boot 恒等映射护内核镜像, 用户
映射下界取 `max(恒等窗口上界, USER_CODE_BASE)` ([privileged/mm/mod.rs:254](../../src/kernel/privileged/mm/mod.rs#L254)).
内核镜像一长, 恒等窗口上探即撞用户区 → boot 期 fail-closed 停机.

**根因不是「内存不够」, 是「内核镜像承载用户态内容」**: 当前 `include_bytes!` 嵌入点:
- initramfs cpio (两架构): [functions/init.rs:62-67](../../src/kernel/functions/init.rs#L62) (`initramfs` feature)
- init.bin 回退 (x86_64): [functions/init.rs:153](../../src/kernel/functions/init.rs#L153); (aarch64): [functions/init.rs:200](../../src/kernel/functions/init.rs#L200)
- wasm 桩 (两架构): `functions/wasm/stub.rs` 的 `wasmstub.bin` / feature 点亮 `wasmrun.bin`（该文件与 `wasmstub` 已随 WASM P4 删除）

默认构建 (无 initramfs) `_kernel_end=0x3f5000` (仅 44 KiB 余量); initramfs 载体腿叠 237 KiB
cpio 即顶破 4 MiB. cpio 由 7 个用户 ELF (~218 KiB, **未剥离符号**) 主导. **剥离符号只买
~35 KiB, 是止血不是解**——内核镜像本就不该承载用户态.

> 上述嵌入点与读数是**立项当时**的形态 (历史记录). D3/D4 收口后三类嵌入均已真删, 出货默认
> 构建 (lockdep 常开) 实测 `kernel_end=0x3E8000` (x86_64, 距 `USER_CODE_BASE=0x400000`
> 余 96 KiB), 用户态字节零嵌入; 详见 D3/D4 各自的收口条.

**Linux 的三层解耦 (参照)**:
1. 地址空间层: 内核高半区 (`0xFFFFFFFF80000000+`, KASLR) + 直射区 (`page_offset_base` 线性
   映射全部物理 RAM), 用户低半区 `0x400000`/`0x5555…`/`0x7f…`; 内核经 high-half 直射访问
   物理页, **不在低半区留随镜像增长的恒等窗口** ⇒ 内核尺寸与用户布局两独立维度, 无 K-08 类
   约束.
2. boot 载体层: 内核 (vmlinuz) 与 initramfs 是**两独立制品**; bootloader 把 initrd 作独立 boot
   module 装进内存, 经 boot protocol 报位置; 内核 reserve 该区、解 cpio 进 rootfs、跑 `/init`.
3. 运行时层: `pivot_root` 到磁盘真实根; 所有程序/库/模块是磁盘文件, `execve`/`mmap` 按需装载;
   **内核嵌零个用户态程序**; WASM 运行时与 `.wasm` 在 Linux 就是普通用户态 ELF + 普通文件.

**aarch64 在 Edgine 里是「已对齐 Linux」的那一半**: 内核链高半区 (`KERNEL_BASE=0xFFFF_0000_0000_0000`,
[mm/mod.rs:228](../../src/kernel/privileged/mm/mod.rs#L228)) + 线性直射区 (`phys_to_virt(p)=p+KERNEL_BASE`,
[mm/mod.rs:432](../../src/kernel/privileged/mm/mod.rs#L432)), **无低半区恒等窗口 ⇒ 无 K-08**
(K-08 断言仅 `x86_64` 编译, [boot/mod.rs:350](../../src/kernel/privileged/boot/mod.rs#L350)).
故「地址空间解耦」aarch64 已达标、无需改; K-08 是 x86_64 kview 低半区窗口这一**自选设计的代价**.

**但内容耦合在 aarch64 同样存在** (cpio/init.bin/wasm 桩照样 `include_bytes!`); 且 aarch64
无 K-08, 大 cpio 只**静默撑大镜像、多占 PMM 预留**, 比 x86_64 的响铃 panic 更隐蔽. 且:
- aarch64 **无 boot-module 载体通路**: 无 Multiboot (x86 BIOS 专属), [privileged/dtb.rs](../../src/kernel/privileged/dtb.rs)
  零解析 `initrd`/`/chosen` ⇒ 与 x86_64 的 MB2 tag 3 同概念、**两套协议**.
- aarch64 **EGFS 根未接**: EGFS 挂载被 `#[cfg(target_arch = "x86_64")]` 圈死
  ([lib.rs:938-969](../../src/kernel/lib.rs#L938)); aarch64 已注册 virtio-blk 但无 FS 消费者
  (T-01). 且无 `mkfs.egfs` 工具, x86_64 磁盘镜像 `edgine.img` 仅 MBR+kernel、EGFS 由
  `sys_boot_install` 运行时建; aarch64 真机介质是 FAT
  ([make_boot_medium.sh](../../scripts/make_boot_medium.sh)).

**真机引导现状 (本轮实测)**: 当前自动验证全都走 QEMU `-kernel kernel.flat`——由 QEMU **内建
multiboot loader** 读 MB1 头自载, **QEMU 专属、绕开真实 loader**; 自建
[stage1.asm](../../src/kernel/privileged/boot/stage1.asm#L23) (`TOTAL_SECT=3200`) 只读 **1.56 MiB**
到 0x100000, 而 `kernel.flat` = **2.66 MiB** (实测) ⇒ **裸 MBR 盘引导当前读不完内核、真机不可用**
(`run-disk` 的 `-drive edgine.img` 同受此限). 真机可用路径是文档化但 CI 从未跑的两条: x86_64
**GRUB2 multiboot2 ISO** ([Makefile iso](../../Makefile#L466) `multiboot2 /boot/kernel.bin`)、aarch64
**arm64 Image + U-Boot extlinux/booti** ([make_boot_medium.sh](../../scripts/make_boot_medium.sh#L165)).
⇒ 真机引导路径当前不可用, 需 loader 现代化 (前置阶段 L0).

## 设计决策

### 决策 A: 严格零嵌入 (内核镜像零用户态字节)

- **描述**: 内核镜像不嵌任何用户态字节 (连 init.bin 与 wasm 桩也不嵌), 全部用户态内容由
  boot-module initrd (QEMU `-initrd` / GRUB `module2` / U-Boot initrd) 提供; `/init` 与桩均
  从 initrd 经 VFS 读取.
- **方案**: 删 [functions/init.rs](../../src/kernel/functions/init.rs) 与
  `functions/wasm/stub.rs`（已随 WASM P4 删除）的全部用户态 `include_bytes!`
  与内嵌回退分支; exec `\0asm` 派发改「从 VFS 读 `/bin/wasmrun`」（P4 后唯一路径）.
- **状态**: [X] (由 D3/D4-c/D4-d 落地; 全仓 `include_bytes!` 只剩字体与 x86 `stage1.bin` 两项非用户态产物, exec 的 `\0asm` 派发经 `wasm/stub.rs` 从 VFS 读桩)
- **详情**: 用户裁定「严格零嵌入 (对齐 Linux)」. 代价: **所有进入用户态的启动腿** (`-kernel`
  裸起) 必须改 `-initrd` 携 cpio, 否则只能停在内核 (对齐 Linux「无 initrd 即 mount 不到 root」).

### 决策 B: 双协议 boot-module 载体

- **描述**: x86_64 与 aarch64 各用本平台 boot 协议交出 initrd 位置, 内核从该内存区读 cpio.
- **方案**: x86_64 = **Multiboot2 tag 3 (MODULES)** (现 [boot/mod.rs:252](../../src/kernel/privileged/boot/mod.rs#L252)
  `parse_multiboot2` 只处理 tag 4/6/8, tag 3 落 `_ => {}`); aarch64 = **DTB
  `/chosen/linux,initrd-start` + `linux,initrd-end`** ([privileged/dtb.rs](../../src/kernel/privileged/dtb.rs) 新增解析).
  二者统一产出 `BootInfo` 的 `initrd_base/size`, PMM 预留下界以保护该区不被复用.
- **状态**: [X] (由 D1/D2 落地: x86_64 `parse_multiboot2` 的 tag 3 分支 + aarch64 `dtb.rs` 的 `/chosen` `linux,initrd-start/end` 解析, 统一产出 `BootInfo` 的 initrd 区)
- **详情**: 同概念 (boot 交位置、内核读区、PMM reserve) 两套协议. `privileged` 面新增 unsafe
  (裸指针读 boot 结构) → 必配 `// SAFETY:` (F4). QEMU `-kernel kernel.flat -initrd cpio` 是否
  按 MB2 模块装载须 D0 实测.

### 决策 C: aarch64 EGFS 磁盘根接入

- **描述**: aarch64 经 virtio-blk 发现并挂 EGFS 根、从磁盘装载用户态, 补齐生产形态.
- **方案**: 去 [lib.rs](../../src/kernel/lib.rs) EGFS 挂载块的 `target_arch="x86_64"` 门控;
  接 virtio-blk → EGFS `init()` 自动发现 → `vfs_mount_safe("/disk", "egfs")` (**D5-e 裁定 B**:
  引导根恒为 ramfs, EGFS 只挂非根挂载点, 由用户态 init `pivot_root` 切根进磁盘根; 原
  `vfs_mount_internal("/", "egfs")` 挂根方案作废); 补 aarch64 EGFS 磁盘镜像生成 (QEMU virt
  加 virtio-blk 空白盘, 内核自格式化) + 磁盘根启动腿.
- **状态**: [X] (由 D5 落地: 架构中立挂载 + virtio-blk legacy 超时修复 + 挂载 NUL 终止修复 + D5-e 引导根/磁盘根分离 + D5-h 用户态切根; aarch64 三相腿实测 `EGFS mounted at /disk (disk)` → `disk root installed: 12 files` → `[SWROOT] PASS: pivot to /disk` 全绿)
- **详情**: T-01 现状 (aarch64 有 virtio-blk 无 FS 消费). 镜像生成方案 (主机 `mkfs.egfs` 工具
  或缺则先 x86_64 建盘后复用) 待 D0 定; 现无 EGFS 格式化工具, 是 D0 的开放子项.

### 决策 D: x86_64 kview 低半区恒等窗口退役 (单独处理)

- **描述**: 退役造成 K-08 的 x86_64 低半区 kview 恒等窗口, 回归高半区直射区 (aarch64 为参照),
  从根上消除「内核镜像增长撞用户区」类约束.
- **方案**: 让 x86_64 的 `copy_*_user` / 缺页路径经**高半区直射区**访问物理页 (如 aarch64 的
  `phys_to_virt`), 不再依赖低半区恒等窗口护镜像; 相应调整
  [mm/mod.rs `user_va_lower_bound`](../../src/kernel/privileged/mm/mod.rs#L272) 与
  [vmm_x86_64.rs `boot_identity_tables`](../../src/kernel/privileged/mm/vmm_x86_64.rs#L1166).
  预研已把方案拆为 **D-1 最小路线**（kview 保留用户镜像，只退役 boot 恒等窗口，不拆 kview）与
  **D-2 完整路线**（`copy_*_user` 也改直射区）；详见
  [decision-d-kview-retirement.md](./decision-d-kview-retirement.md).
- **状态**: []
- **详情**: 用户裁定「纳入本次插队项**但单独处理**」. 触及 #89 `kview` 修双视图分叉的根基, 与
  内容解耦正交; 施工 D1-D3 落地后内核镜像已回 4 MiB 内, K-08 不再触发 ⇒ 决策 D 是「根治天花板」而非
  解 K-08 的必需品, 风险高、跨度大, **单开为收尾独立批次**, 不阻塞 D1-D3. 预研阶段暂不改状态；
  D-1 若评审通过，才是决策 D 的最小闭环。

## 施工分解

### L0: 真机引导 loader 现代化 (前置阶段)

- **描述**: 弃坏掉的自研 stage1, 改用生态成熟 loader, 并让 CI 真启动真实介质以验真机路径.
- **方案**: x86_64 = **GRUB2 multiboot2** (`grub-pc` BIOS + `grub-efi` UEFI 双覆盖; `grub.cfg` 增
  `module2 <cpio>` 把 initrd 作 MB2 tag 3 交出); aarch64 = **U-Boot extlinux `initrd` / `booti`**
  (arm64 Image + DTB). **退役** [stage1.asm](../../src/kernel/privileged/boot/stage1.asm) 及
  `edgine.img` 裸 MBR 盘产物 / `run-disk`; CI 用 QEMU 启 **真实 GRUB ISO(SeaBIOS) + `-bios OVMF`(UEFI)**
  与 U-Boot FAT 盘, 断言 GRUB/U-Boot 经 `multiboot2 module2` / `initrd` 装载内核与 cpio——比
  `-kernel`(QEMU 内建 loader) 贴近真机.
- **状态**: []
- **详情**: 用户裁定「GRUB2 + U-Boot 退 stage1」. stage1 1.56 MiB 读盘上限 < kernel 2.66 MiB, 修补
  它 = 重造轮子 (BIOS int13 多段读 / 跨固件脆) 且还要自实现 initrd 装载; 用 GRUB/U-Boot 一举两得
  (真机引导 + boot-module 载体, 正是 D1/D2 所需). UEFI-native (EFI-stub / EDK2) 为 BIOS 退场后的
  长期演进、后置 (见开放议题). 与 D1 协同: GRUB `module2` 就是 D1 的真机 tag 3 来源.
- **L0 spike 实测 (2026-11-16)**: 工具齐备 (qemu 11.1 / grub2-mkrescue 2.12 / `multiboot2.mod` 同时
  有 i386-pc 与 x86_64-efi / OVMF CODE+VARS / xorriso; `dtc` 缺, 以字节 grep 验 DTB). `make iso` 出的
  GRUB ISO 在 **SeaBIOS(BIOS) 与 OVMF(UEFI)** 均启动到 `Entering Ring 3 (pid=4)` + 心跳正常;
  在 `grub.cfg` 加 `module2 /boot/initramfs.cpio` 重打包后 **BIOS 与 UEFI 仍到 Ring 3 且不崩**
  ⇒ GRUB 载模块机制与 BIOS+UEFI 双覆盖成立. (模块内容到达内核的**结构**见 D1 — 尚未据此断言.)

### D0: 调研与前提验证

- **描述**: 落地前的协议/工具/波及面实测, 杜绝把假设当事实.
- **方案**:
  - 实测 QEMU `-kernel kernel.flat -initrd cpio`(x86_64) 与 `-M virt -kernel Image -initrd cpio`(aarch64)
    是否分别按 MB2 模块 / DTB `/chosen` 交出 initrd 位置 (QEMU 镜像侧快速验证; 真机 loader 的
    module 行为与 stage1 退役归 L0).
  - 枚举全部进入用户态的启动腿 (`qemu_boot_test.sh` / `qemu_slaac_test.sh` / `initramfs_boot_test.sh` /
    `aarch64_cow_boot_test.sh` / `thread_stress_test.sh` / `gic_stress_test.sh` / `qemu_mcast_test.sh` /
    `pci_devices_boot_test.sh` / `lockdep_feature_probe.sh` / `lockdep_overhead_ab.sh` /
    `wasmi_boot_test.sh` / `qemu_liveness_probe.sh` 等) 与各自 `-kernel` 起法.
  - 定 aarch64 EGFS 磁盘镜像生成方案 (无 `mkfs.egfs` 工具).
- **状态**: [X] (三项调研均已出数: 双架构 initrd 交出方式实测归 D1/D2; 启动腿枚举与迁移归 D4-b; aarch64 镜像方案定为**空盘自格式化**, 无需 host `mkfs.egfs` —— 该工具仍列开放议题, 但不再是 D5 的前置)
- **详情**: D0 实测结论决定 D1/D2/D3 的具体形态; 不得跳过. L0 spike (2026-11-16) 已确认: 工具
  齐备、GRUB BIOS+UEFI 均到 Ring 3、aarch64 DTB `/chosen` 交出 initrd; 余项 = 各 loader 的
  `MULTIBOOT_MAGIC` 与模块结构确认 (归 D1 落地时以 `MULTIBOOT_MAGIC` 实测 + 回归锁).

### D1: x86_64 boot-module (MB2 tag 3)

- **描述**: x86_64 内核经 MB2 tag 3 取得 initrd base/size, PMM 预留, 从该区读 cpio.
- **方案**: `parse_multiboot2` 补 tag 3 分支; `BootInfo` 增 `initrd_base/initrd_size`;
  `pmm_init` 增 initrd 区预留; 消费者切换 (从模块区读 cpio + 删 `include_bytes!`) 归 **D3 同批**
  (经 privileged 安全 API, 避 blob 访问器在 feature-off 时成死代码); D1 只做「识别+预留+可观测」.
- **状态**: [X] (plumbing 完成并验证; cpio 消费者与全 §2.3 收口于 D3/D6)
- **详情**: `parse_multiboot1` (MB1 mods: `mods_count`@0x14 / `mods_addr`@0x18, 16B 项首元素) 与
  `parse_multiboot2` (MB2 tag3: mod_start@+8 / mod_end@+12) **双解析** → `BootInfo.initrd_base/size`;
  `boot_memory_init` 于 `pmm_init_bitmap` 后 `reserve_range` 预留; 启动报
  `Boot module initrd: base/size`. 新 unsafe 均配 `// SAFETY:`.
  **连带修复预存 magic bug**: [boot.asm](../../src/kernel/privileged/boot/boot.asm) 的 `mov al,0xFF`
  (屏蔽 8259 PIC) 在保存 magic 之前冲掉 EAX 低字节 ⇒ MB1/MB2 magic 恒不匹配, 致内存图 / mem_size /
  帧缓冲 / 引导模块**全部从未解析** (PMM 长期被限 128MB). 修法: 保存 info+magic 的 mov 提前到该指令前.
  **实测 (2026-11-16)**: 修后 QEMU flat magic=0x2BADB002、GRUB module2 magic=0x36D76289, mem=511MB,
  帧缓冲 tag8 恢复解析, **默认 qemu_boot_test x86_64 全绿** (SMP/网络/e2e 零回归); **GRUB module2 →
  `Boot module initrd: base=0x6C0000 size=242780`(=cpio 精确字节)** ⇒ 模块检测成立. `test-kernel-host`
  1347 passed / 0; quick 审计(clippy+fmt)绿.
  **两处新发现 (归 D3)**: (a) QEMU `-kernel flat -initrd` **不交付模块**(无 MB1 mods) ⇒ CI 模块腿须走
  GRUB 启动(合 L0); (b) GRUB initrd 落 0x6C0000, 与内核堆预留区 `[0x5F4000,0xDF4000]` 重叠 ⇒
  `reserve_range` 报 overlap、堆迟早覆盖 cpio —— **D3 读 cpio 前须先解 initrd 放置/堆布局冲突**.
  **附带治理**: F7/F9 审计补 `src/user/vendor/` vendored 排除 (P1 wasmi 第三方 .rs 英文注释/死代码
  抑制不再污染全审计). 回归锁余项 = GRUB module2 启动腿 + 显式 magic 行断言 (并入 D4 启动腿批次).
- **L0 spike 关键发现 (影响 D1 范围)**: 模块交付结构**随 loader 而异** —— QEMU `-kernel -initrd`
  走 **Multiboot1 `mods` 字段** (`mods_count`@MB1 info 偏移 0x14 / `mods_addr`@0x18, 每模块 16 字节:
  mod_start/mod_end/string/rfu); GRUB `multiboot2 module2` 走 **MB2 tag 3** (`mod_start`@+8 /
  `mod_end`@+12); GRUB `multiboot`(MB1) 走 MB1 mods. ⇒ **D1 必须同时**在 `parse_multiboot1`
  (补 mods 解析) 与 `parse_multiboot2`(补 tag 3) 取模块, 不能只加 tag 3. 各 loader 实际走哪型由
  D0 用 `MULTIBOOT_MAGIC` 实测确认 (QEMU 侧预计 MB1).

### D2: aarch64 boot-module (DTB /chosen)

- **描述**: aarch64 内核经 DTB `/chosen` 的 initrd-start/end 取得 initrd 区, 同上管线.
- **方案**: [privileged/dtb.rs](../../src/kernel/privileged/dtb.rs) 新增 `/chosen` initrd-start/end
  解析, 填 `BootInfo`; 其余与 D1 共用模块区读法.
- **状态**: [X] (解析+预留完成并实测; 消费者与全 §2.3 于 D3/D6)
- **详情**: DTB 属性按 32/64 位 cell 宽度解析; 与 aarch64 entry 已有的 `_fdt_addr`
  高半区别名衔接 ([boot/aarch64/entry.rs](../../src/kernel/privileged/boot/aarch64/entry.rs)).
- **L0 spike 实测 (2026-11-16)**: `qemu-system-aarch64 -M virt -kernel Image -initrd cpio -M virt,dumpdtb=…`
  导出的 DTB 字节含 `/chosen` 节点与 `linux,initrd-start` + `linux,initrd-end` 两属性 ⇒ **aarch64
  的 initrd 位置由 QEMU virt 确实经 DTB `/chosen` 交出**, D2 的 aarch64 前提成立.
- **D2 实装实测 (2026-11-16)**: `dtb.rs` 的 `Parser` 以 `chosen_depth` 识别根 `/chosen`, 读
  `linux,initrd-start/end` (大端单元格 4/8 字节, `decode_be_cells`) → `DtbInfo.initrd_start/end`;
  `entry.rs` 的 `apply_fdt_overrides` 经 `dtb::set_initrd_region` 持久化; `boot::init` aarch64
  分支取 `dtb::initrd_region` 填 `BootInfo`. aarch64 QEMU `-initrd` 实测:
  `Boot module initrd: base=0x48000000 size=750388`(=cpio 精确字节)、`initrd region reserved`(页对齐
  预留成功, **aarch64 无 x86 那类与堆重叠**)、到 `Entering EL0 (pid=5)`. 双架构 build 6/6、quick
  审计(clippy 含 `collapsible_if` 修复 + fmt)绿、host 结构锁 needle 同批重指(`VENDORED_DIRS` 加 vendor).

### D3: 严格零嵌入收口 (删全部用户态 include_bytes!)

- **描述**: 移除内核镜像内全部用户态字节与内嵌回退, `/init` 与 wasm 桩均从 initrd VFS 取.
- **方案**: 删 init.bin 回退分支 (x86_64 [init.rs:153](../../src/kernel/functions/init.rs#L153) /
  aarch64 [init.rs:200](../../src/kernel/functions/init.rs#L200)); 删 [wasm/stub.rs](../../src/kernel/functions/wasm/stub.rs)
  两架构 `include_bytes!`, exec `\0asm` 派发改从 VFS 读桩 (`/bin/wasmstub` 默认 / `/bin/wasmrun`
  wasm_userspace 腿, cpio 相应含对应桩); `build.rs` 相应护栏/`rerun-if-changed` 收口.
- **状态**: [X] (三类嵌入全删; cpio 经 D3-cpio 子批, init.bin 经 D4-c, wasm 桩经 D4-d)
- **详情**: 真删 (F9), 非 `allow(dead_code)`. 与 P1 交互: P1 的「feature 选桩」由「编译期嵌哪个桩」
  改为「initrd 内含哪个桩 + init exec 哪个」; carrier 无关的 P1 资产 (wasmrun/wasmi vendoring/
  wasi fixture) 保留.
- **D3/D4 强耦合 = 一次原子切换 (D3 准备前发现)**: ① 删 cpio `include_bytes!` 后 initramfs 载体腿
  (`-kernel flat` 无模块) 即**失去源** —— `-kernel flat -initrd` 不交付模块(D1 实测评), 必须改走
  GRUB `module2` 启动(=D4). ② 消费者切模块**必须先删 cpio 嵌入才可验证**: 只要嵌入 cpio 在,
  initramfs 构建镜像含 ~237KB cpio → K-08 先 panic, 到不了消费者. ③ 删 init.bin / wasm 桩同理牵连
  exec `\0asm`(改读 VFS) 与默认启动腿. ⇒ 正确落地 = **一次性原子切换**: 删三类嵌入 + 消费者切模块
  + exec 读 VFS + 全部启动腿改 GRUB 携 module(含 wasm 桩入 cpio) + build.rs 护栏收口, 不可拆半
  (拆半必留红腿). ④ 前置: x86 GRUB initrd 落 `0x6C0000` 与堆预留区重叠 —— 消费前须解放置冲突
  (候选: 堆改址避开 initrd / 读进安全缓冲 / 调 GRUB 布局); 实测堆峰值仅 ~16KB、initrd 距堆起点
  ~800KB, 故当前实际未覆盖, 但须加固.
- **D3 子步已落 (2026-11-16)**: privileged 新增 `boot::initrd_blob()` (feature `initramfs` 前转
  至 privileged, 避 feature-off 死代码), `phys_to_virt` 高半区直射取 initrd 为只读切片供 functions;
  `load_init_from_initramfs` 改为**优先模块、回落内嵌 cpio**. 默认构建/双架构 build 6/6/quick 审计/host
  契约测试全绿(两处 feature needle 同批重指). 余项 = 删三类嵌入 + 启动腿改 GRUB 携 module + exec 读
  VFS 的**原子批**(见上条耦合), 未做.
- **D3 cpio 解耦已落地 (2026-11-16, 主目标)**: 删 `INITRAMFS_CPIO` 三处 `include_bytes!` + build.rs cpio
  嵌入护栏; `load_init_from_initramfs` 只认 boot-module; 载体腿 x86 改 **GRUB multiboot2 + module2**、aarch64
  加 `-initrd`; K-07b 契约测试重写为「不嵌入、消费者读模块」。**双架构载体腿实测绿**: x86
  `Entering Ring 3 (init from /init)`、aarch64 `Entering EL0 (init from /init)`, 解包/线程/EEVDF/exec/wasm
  全 PASS; **x86 K-08 因 cpio 出镜像而解除**(feature 构建核回小)。build all 6/6、quick 审计绿。**余(严格全
  零嵌入尾项)**: init.bin 与 wasm 桩仍嵌入(约合默认 0x3f5000, 非 K-08 主因) + exec `\0asm` 读 VFS + 默认
  启动腿改 module —— 归后续子批/D4.
- **D3 收口 (严格零嵌入达成)**: 上述尾项由 D4-c (init.bin) 与 D4-d (wasm 桩 + exec 读 VFS) 补齐.
  内核侧 `include_bytes!` 只剩**非用户态**两项: 字体 `font8x16.raw` 与 x86_64 引导码 `stage1.bin`
  (后者随 L0 loader 现代化退役). 出货默认构建实测 `kernel_end=0x3E8000` (x86_64),
  `verify_image_arch.py` 的「镜像逐字节包含产物」降为信息提示 (硬判据 = 产物 `e_machine`),
  两架构载体腿均报 `[INFO 零嵌入] …未包含…完整字节 (D4 零嵌入下为常态)`.

### D4: 启动腿改造 (-initrd 携载)

- **描述**: 全部进入用户态的启动腿改 `-initrd <cpio>`, 使零嵌入后仍能进 Ring 3.
- **方案**: 逐脚本改 `-kernel` 起法附 `-initrd`; 覆盖门槛 5 (`qemu_boot_test.sh`) / e2e / P-62
  feature 运行腿 / wasmi 腿; `initramfs_boot_test.sh` 由「feature 载体腿」转为默认形态.
- **状态**: [X] (载体迁移全部已落; wasmi 腿属 P1 资产, 暂停于 mm 生命周期工程)
- **详情**: 波及 12+ 腿 (D0 清单). 每条改后单独真跑一次取证 (红先归因, §QEMU 纪律).
- **D4-b 载体迁移已落 (2026-11-16)**: 全 user-mode 腿改 boot-module 载体, 单源 helper
  `scripts/boot_module.sh` (`boot_module_build` / `boot_module_iso_x86` / `boot_module_carrier_args`:
  x86 = `-cdrom boot_module.iso`(GRUB multiboot2 + module2), aarch64 = `-kernel img -initrd cpio`).
  覆盖 headline `qemu_boot_test.sh`(双架构主腿 + e2e + P-62 feature 运行维四 feature + lockdep 构建面探针)、
  `initramfs_boot_test.sh`、`thread_stress` / `qemu_mcast` / `pci_devices` / `gic_stress` /
  `qemu_liveness_probe`(gdb) / `qemu_slaac`(handoff) / `aarch64_cow_boot_test.sh`. 每次真构建真启动取证.
- **D4-c 删 init.bin 嵌入已落 (2026-11-16)**: (a) `load_init_from_initramfs` 与
  `boot::boot_initrd_blob()` 去 `initramfs` feature 门控, 只认 boot-module, **无内嵌回退** ——
  无模块即报错 + 停机/`qemu_exit`; (b) `src/kernel/build.rs` 删 init.bin 存在性护栏与
  `rerun-if-changed`; (c) `verify_image_arch.py` 镜像逐字节包含项降为信息提示 (硬判据 = 产物
  `e_machine`, 产物仍是本架构 cpio 来源); (d) K-07b 契约锁改 `assert_absent("falling back to init.bin")`.
  读数: build all 6/6 绿 (含 P-62 needle 同批重指 `KERNEL_FEATURES="$feat initramfs"`),
  headline QEMU 双架构绿 (`init from /init` 经 module), aarch64 COW 腿绿.
- **D4-d wasm 桩去嵌入 + exec 读 VFS 已落**: (a) [wasm/stub.rs](../../src/kernel/functions/wasm/stub.rs)
  删两架构 `include_bytes!`, 只留 `stub_path()` (`wasm_userspace` 点亮 → `/bin/wasmrun`, 否则
  `/bin/wasmstub`; **两桩恒在 cpio 内**) 与 `is_wasm_image()`; (b)
  [exec.rs](../../src/kernel/functions/proc/exec.rs) 的 `\0asm` 分派改以无 fd 的
  `read_file_kernel(stub_path(), 0, ELF_MAX_SIZE)` 从 VFS 读桩, 读不到即 `Enoexec` (不静默回落);
  桩 `Vec` 绑定在分派块**外** (块内 `let` 到块尾析构 ⇒ 交给 `proc_exec_replace` 的指针悬垂);
  (c) `build.rs` 删 wasm 桩护栏, 只剩 x86_64 `stage1.bin` 一项嵌入护栏;
  (d) **exec 上限红归因与收口**: `wasmrun` 未剥离符号时 x86_64 1,209,424 B / aarch64 1,427,776 B
  **双双超 `ELF_MAX_SIZE = 1 MiB`** ⇒ 载体腿 ENOEXEC. 收口方式 = Makefile 全部 18 条
  ELF → `.bin` 规则改 `$(OBJCOPY) --strip-all` (**按架构 objcopy**; GNU 原生 `strip` 读不了
  aarch64 ELF), **未抬高任何内核上限**. 剥离后 692,400 B / 793,640 B, 余量 ~348 KiB / ~249 KiB
  (长期约束记于 [mm-lifecycle-project.md](./mm-lifecycle-project.md) 附条).
  读数: build all 6/6、双架构载体腿五条判据全绿 (解包 12 条目 / `init from /init` / `[THREAD] PASS` /
  EEVDF 守恒 / `[EXEC] PASS` / W-7 wasm 端到端).
- **D4-d 附带发现 = mm 生命周期缺口 (单独立项, 不在本批修)**: wasmi 腿 (P1 资产) 双架构真跑红,
  归因为「生产 ELF 装载从不安装 `MmStruct` ⇒ `mmap` 无 mm 回退把**物理地址**返给用户态 ⇒
  首写翻译故障」(x86 `cr2=0x1F800000` / aarch64 `FAR=0x5FA00000`), 同时触碰 I2/I4.
  用户裁定**单独立项** ⇒ [mm-lifecycle-project.md](./mm-lifecycle-project.md);
  P1 wasmi 端到端暂停, 其脚本 `scripts/wasmi_boot_test.sh` 保留为未接线资产
  (**不接 `ci/audit.sh`**: 该脚本 `err()` 致命退出, 接已知红腿会掩盖其余门槛读数).
- **D4-e 退役空 `initramfs` feature 已落 (用户裁定「本轮退役」)**: D4-c/D4-d 后该 feature
  门控**零行代码** (解包无条件执行) ⇒ 空旋钮真删. 四份 Cargo.toml (kernel 壳 / functions /
  privileged / `src/rust` 壳) 删声明; `boot_module.sh` 的 `boot_module_build` 转
  `make ARCH=<arch> all` (出货默认, lockdep 常开), 另留 `boot_module_build_nolockdep`
  供 liveness/perf 测量腿 (其理由是**观测开销失真**, 与 K-08 无关, 故 `KERNEL_NO_DEFAULT`
  只在载体腿退役、测量腿保留); 载体腿删 feature 切换与尾部复原, 直接跑出货默认镜像;
  wasmi 腿改 `KERNEL_FEATURES="wasm_userspace"`; 八处脚本措辞「initramfs-feature 核」→「默认核」;
  host 契约锁 `test_initramfs_feature_is_off_by_default` 重写为 `test_initramfs_feature_retired`
  (四份 Cargo.toml 负向钉 + `scripts/*.sh` 树级扫描拒收向 `KERNEL_FEATURES` 塞 initramfs),
  载体腿构建针重指 `make ARCH="$arch" all` 并负向钉住不得再出现 `KERNEL_FEATURES` /
  `KERNEL_NO_DEFAULT`; P-62 needle 同批重指 `KERNEL_FEATURES="$feat" all`.
  读数: build all **6/6**、`make test-host` **197 个测试二进制全 ok**、双架构载体腿
  **五条判据全绿** (出货默认镜像, x86 `kernel_end=0x3E8000` ⇒ K-08 未触发, 余 96 KiB).
- **D4-f 真机介质载体补齐已落 (2026-11-16)**: 零嵌入后介质必须自带 boot-module, 否则真机无 PID 1.
  (a) x86_64: `Makefile` `iso` 目标 (`iso: all user`) 的 GRUB 配置补 `module2 /boot/initramfs.cpio initramfs`,
  [make_boot_medium.sh](../../scripts/make_boot_medium.sh) 复用该目标 (不另写一份 GRUB 逻辑);
  (b) aarch64: 构建改 `make ARCH=aarch64 all user`, cpio 经 `mcopy` 落 FAT 分区 `/initramfs.cpio`,
  `extlinux.conf` 补 `initrd /initramfs.cpio` 行, `boot.cmd` 补 `fatload … initramfs.cpio` 且
  `booti` 第二参数由 `-` 改为 initrd 地址 (`-` = 不带载体); 新增 `AARCH64_INITRD_ADDR=0x48000000`
  (DRAM 基址 + 128 MiB, 避开 Image / BSS / U-Boot 自搬迁区 ⇒ 前提板载 DRAM ≥ 256 MiB);
  (c) [guide-hardware-boot.md](../explain/guide-hardware-boot.md) 同步介质内容表、载体链三里程碑与三条排障行
  (里程碑串与 [init.rs](../../src/kernel/functions/init.rs) 逐字同源; 数值 pid 双架构实测均为 5,
  门禁「PID 1」是首个用户进程的语义而非号, 已在该页写明).
  回归锁: [initramfs_carrier_k07b_test.rs](../../host-tests/tests/initramfs_carrier_k07b_test.rs) 新增 5 例
  (共 17), 其中 `test_iso_delivers_cpio_as_grub_module2` 钉「拷入 + 声明」两侧同时在位,
  `test_aarch64_medium_delivers_cpio_via_chosen` 钉 extlinux 行 / `booti` 第二参数 / `mcopy` 三处.
  **验证边界 (诚实口径)**: x86_64 ISO 路径由 D4-b 的 GRUB 载体腿真启动覆盖; **aarch64 U-Boot 真机引导未跑**
  (无板子; QEMU 侧走 `-kernel img -initrd`, 不经 `sysboot`/`booti` 与 FAT 读取), 介质内容与引导配置只有
  host 形状锁 ⇒ 该真跑面属 L0 loader 现代化议题, 本批不宣称已验证.
- **D4-g MB2 内存图优先级已修 (2026-11-16, 用户裁定「本轮修复」)**: [boot/mod.rs](../../src/kernel/privileged/boot/mod.rs)
  `parse_multiboot2` 原以 tag4 (基本内存信息) **无条件赋值** `mem_size`, 而 GRUB 在 SeaBIOS 与 OVMF 下
  都把 tag4 排在 tag6 之后 ⇒ 已算出的内存图结果被覆盖. OVMF 的 tag4 `mem_upper` 只描述 1 MiB 以上首个
  连续块 (实测 7168 KB), 于是 `-m 512` 的机器被读成 8 MB, PMM 零空闲页、启动期分配即 panic.
  改为 `map_mem_size: Option<u64>` / `basic_mem_size: Option<u64>` 双轨, 收尾 `map_mem_size.or(basic_mem_size)`
  ⇒ **与标签出现次序无关**, 有图用图、无图才兜底 (与 Linux 以 e820/EFI 图为准同构).
  回归锁 = 该文件内联 6 例 (`mb2_tag4_before_tag6_map_is_authoritative` / `…tag6_before_tag4…` /
  `…independent_of_tag_order` / `…tag4_only_falls_back_to_basic` / `…without_available_entry_falls_back` /
  `…no_mem_tags_uses_default`), 两种次序都钉. UEFI 真跑实测 `Boot info: mem=511 MB` (修复前为 8).
- **D4-h 早期预留区表 + 引导模块搬移已落 (2026-11-16, 用户裁定「方案 B 相对完整」)**: 引导器自选落点
  可能与内核固定堆窗共址 (initrd 被后续分配覆写). 新增
  [early_reserve.rs](../../src/kernel/privileged/mm/early_reserve.rs) (memblock 式早期区表:
  `EarlyRegion` + `MAX_EARLY_REGIONS`, 容量单源 = `limits::MAX_FIRMWARE_REGIONS + 8`, 三个 `const fn`
  纯函数 `align_down_region` / `align_up_region` / `ranges_overlap`);
  [pmm.rs](../../src/kernel/privileged/mm/pmm.rs) 接 `early_reserve` (就地预留) /
  `early_reserve_relocatable` (落在堆窗内则搬到互斥区并**逐字节搬载荷**) / `early_region` /
  `early_region_count` 与同名自由函数; 表满 / 零长度 / 索引越界一律 **fail-closed 停机**, 不静默丢条目
  (丢一条 = 该区被当空闲页分配出去). 登记方 = firmware 保留区 + initrd + DTB + MB2 信息块本体.
  回归锁 = [pmm_early_reserve_test.rs](../../host-tests/tests/pmm_early_reserve_test.rs) 9 例
  (6 行为: 就地标记 / 窗外不动 / 窗内搬移且载荷存活 / 搬移后页不被 buddy 放出 / 不可搬者留原地 /
  两次搬移目的地互斥; 3 fail-closed `should_panic`), 另两轮负控制 (摘搬移 / 摘预留) 均转红后逐字节复原.
  UEFI 真跑实测 `[PMM] Early relocate 'initrd': 0x… -> 0x…` 在位.
- **D4-i 甲 MB2 指针接线 + inline RSDP 解析已修 (2026-11-16, 用户裁定「本轮相对完整修复」)**: UEFI 真跑
  暴露两处预存缺陷 —— (a) MB2 信息块指针硬编码 0 ⇒ ACPI 发现面拿不到固件表;
  (b) MB2 tag14/15 的载荷是 RSDP 的**内联副本** (spec §3.6.16/17), 原代码把它当**指针**读.
  修法: 指针经 `BootInfo` 真接线; [acpi.rs](../../src/kernel/privileged/arch/x86_64/acpi.rs) 按内联副本解析,
  两标签并存时优先 tag15 (v2, 带 XSDT) 且**与次序无关**, v1/v2 各按自身面校验和校验, v2 校验和坏则回落 v1.
  回归锁 = 该文件新增 7 例内联测试 (内联而非指针 / 优先 v2 / 仅 v1 用 tag14 / 偏移计标签对齐填充 /
  坏 v2 校验和回落 / 无 ACPI 标签返 None / 截断 v2 面拒收).
- **D4-i 乙 firmware 保留区进 PMM 早期预留已落 (2026-11-16)**: MB2 内存图 `type != 1` 项收进
  `FirmwareRegions` (容量单源 `limits::MAX_FIRMWARE_REGIONS`, 溢出 fail-closed),
  [lib.rs](../../src/kernel/lib.rs) 在 PMM 建立后逐区 `pmm_early_reserve(…, "firmware")`,
  并另预留 MB2 信息块本体 (`"mb2-info"`) —— ACPI 发现面在 PMM 之后才读它, 该区必须活到运行期.
  回归锁 = boot/mod.rs 内联 5 例 (非 available 项成区 / 与标签次序无关 / 零长度保留项忽略 /
  全 available 图零区 / 表溢出 fail-closed). UEFI 真跑实测 **声明 14 区 / 登记 14 区** 两侧对账;
  tag6 覆盖面与 Linux 必须保留的 EFI 区逐项核对见
  [uefi-thread-exit-hang.md](./uefi-thread-exit-hang.md)「归因」节 (13/13 覆盖).
- **D4-i 丙 UEFI 真跑腿 + 门禁接线已落 (2026-11-16, 用户裁定「登记独立工程 + 腿按可达面接线」)**:
  新增 [uefi_boot_test.sh](../../scripts/uefi_boot_test.sh) (OVMF pflash: CODE 只读 + VARS **每轮私有可写副本**;
  GRUB ISO 经 `boot_module.sh` 单源 helper; 9 组判据 = 载体链 / ACPI 经 MB2 (反向禁 BIOS 扫描回退) /
  双核上线 + TLB / firmware 两侧对账 / 内存图权威 / initrd 搬移 / 用户态探针可达面 / 驱动与网络 / P-62 能力行).
  firmware 区数下界 `MIN_FW_REGIONS=10` 兼作 **UEFI/BIOS 判别器** (SeaBIOS e820 实测 7 区, OVMF 13-14 区)
  ⇒ pflash 未生效退化成 BIOS 启动即红, 不产生假绿. `--probe` 是 OVMF 发现的单源,
  [ci/audit.sh](../../ci/audit.sh) full 步骤 4 据此决定跑腿或 `skip_leg "uefi"` 三态 (门禁侧不另抄候选路径清单).
  接线形状锁 = [ci_uefi_boot_leg_test.rs](../../host-tests/tests/ci_uefi_boot_leg_test.rs) 6 例
  (24 条判据串与 12 个产出方逐字同源 / 每条判据有红分支且取数失败不得读成零 / 双向针与两侧对账 /
  真 OVMF 产物 + 自检早于启动 + aarch64 显式拒绝 + 里程碑不得截断 / THREAD 显式不判 / audit 真调用 + 三态).
  **`[THREAD]` 显式不判**: UEFI 下 init 线程组探针确定性挂死, 已归因为预存缺陷 (BIOS 同镜像 5/5 PASS,
  UEFI 5/5 FAIL `reap-timeout`; 摘掉 firmware 预留的负控制三轮从未通过且 2/3 轮挂得更早 ⇒ 非 D4-i 乙 引入),
  另立 [uefi-thread-exit-hang.md](./uefi-thread-exit-hang.md) (U-1 双核 `group_exit_leg` / U-2 单核更早静默挂死),
  本腿无条件打印横幅点名该册 —— **不是静默跳过**; 修复后须把 THREAD PASS 行加回硬断言并删横幅.
  读数: 真跑 **25 项全绿** (`mem=511 MB` / firmware 14 声明 14 登记 / 解包 12 条目 / AP count=2 /
  online CPUs=2 / TLS·SIG·EXEC(cwd+root)·WASM·EEVDF 守恒·KPTI·e1000·DHCP 全在位);
  负控制 5 轮全部转红后逐字节复原 (摘 THREAD 横幅 / 里程碑改 `[THREAD]` 截断 / 判别器下界归零 /
  摘 audit 三态 `skip_leg` 四轮各只红对应那一例; 真跑轮把下界抬到 20 > 实测 14 ⇒ **恰红 firmware 一项**,
  其余 23 项仍绿 ⇒ 判据相互独立且非恒真).

### D5: EGFS 接入 aarch64

- **描述**: aarch64 virtio-blk → EGFS 挂 `/disk` → 用户态切根进磁盘根 → 从磁盘装载用户态.
- **方案**: 去 [lib.rs](../../src/kernel/lib.rs) 门控; 接 EGFS 自动发现挂载 (非根挂载点 `/disk`);
  补 aarch64 EGFS 磁盘镜像生成 (空白盘自格式化); 新增 aarch64 磁盘根 QEMU 三相腿.
- **状态**: [X] (D5-a…D5-j 全部落地; 三相腿 30 项判据全绿实测: 空盘首启自格式化 + 12 文件落盘 +
  用户态切根读回 `/init`, 同盘二启不重格式化不重装且切根仍成立, 摘盘负控制走 memory mode + SKIP;
  全 §2.3 于 D6 收口)
- **详情**: D0 定的镜像生成方案落地. 与 D1-D4 正交 (boot 载体 vs 运行时磁盘根); 可并行.
- **D5-a 实测 (2026-11-16) + 真前置**: 去 [lib.rs] EGFS 挂根块的 `target_arch="x86_64"` 门控(改架构中立)
  后: (a) aarch64 **无盘** → `EGFS memory mode` + `Entering EL0` ✓ 无回归; (b) aarch64 + 空白 virtio-blk 盘 →
  virtio-blk 识别 128MB、EGFS 发现 1 设备、读 uberblock→**旧无效→`formatting first drive`→`FORMAT: Writing
  fresh EGFS v2`**(空盘自格式化机制在 EGFS 层走通, 无需 host mkfs), **但随即 `virtio-blk: 请求超时`(5s 多条)**
  ⇒ **aarch64 virtio-blk I/O 超时, 格式/读写无法完成、挂载阻塞**. ⇒ **D5 真前置 = 修 aarch64 virtio-blk 请求
  超时**(与历史 aarch64 virtio-net TX 超时 P-31 同族, blk 版), 属驱动层根因工程, 单独立项**(D5-PR)**; 门控去
  x86-only 本身安全(无盘路径无回归), 但 with-disk 到 virtio-blk 超时前不成立, 不能宣称 D5 完成.
- **D5-PR virtio-blk legacy 超时已修并验证 (2026-11-16)**: blk 的 legacy 初始化按 net(P-31) 模板补齐——`new()` 对 legacy
  先 `set_guest_page_size(4096)`(须在写 PFN 前), `finalize()` legacy 走 `QueueNum→QueueAlign(4096)→
  setup_queue_legacy(desc_paddr)`(legacy 无 QueueReady), modern 走原三段地址+Ready(原 SIMPLIFIED「legacy 不可用」即此).
  **aarch64 空盘实测**: `virtio-blk 请求超时`消失, **`FORMAT: Complete` + `EGFS Initialized (disk, 1 drive)`**(秒级);
  x86 不受影响(走 AHCI/NVMe). ⇒ 驱动层 D5-PR 闭合. **新余项 = `EGFS mount failed`**: 格式成功后
  `vfs_mount_internal("/","egfs")` 返非零 —— aarch64 VFS 挂根步骤待查(下一子项, 非驱动问题).
- **D5 收口 (2026-11-16, aarch64)**: 修 `vfs_mount_internal` **调用处的 NUL 终止 bug** —— 原
  `b"/".as_ptr()` / `b"egfs".as_ptr()` 无终止符, `ptr_to_str` 越界读致 fs_name≠"egfs" → `FsType::Unknown`
  → `-EOPNOTSUPP(-95)`, EGFS 挂根恒失败 (此 bug **x86 磁盘根亦从未成功**, 因无盘未触发). 改用
  `vfs_mount_safe("/","egfs")`(自动 NUL 终止). **aarch64 空盘实测**: `FORMAT: Complete` →
  **`Root filesystem: EGFS (disk)`** → `Entering EL0`; 无盘 → memory mode 无回归; x86 无盘不受影响.
  ⇒ **EGFS 接入 aarch64 成立** (空盘自格式化 + virtio-blk legacy 修复 + 挂根 NUL 修复三件齐备).
  **后续改判**: 该轮只证到「内核把 EGFS 挂上」, 未证用户态真从磁盘根跑; 挂根拓扑随即被 D5-e 裁定 B
  取代 (引导根恒为 ramfs, EGFS 挂 `/disk`, 用户态切根), 上面这行 `Root filesystem:` 日志已退役.
- **D5-b 三相腿落地**: 新增 [aarch64_egfs_disk_boot_test.sh](../../scripts/aarch64_egfs_disk_boot_test.sh)
  (相 1 空盘首启 / 相 2 同盘二启 / 相 3 摘盘负控制), 接入 [ci/audit.sh](../../ci/audit.sh) `full` 步骤 4;
  host 侧配套 [egfs_disk_persist_test.rs](../../host-tests/tests/egfs_disk_persist_test.rs). 每次新建空白
  raw 盘由内核自格式化 (无需 host mkfs, D0 定的方案); 只新增判据, 不改 `qemu_boot_test.sh` 既有判据;
  x86_64 **明确拒绝**而非跳过 (它走 PCI AHCI/NVMe, 在本腿上只会得到零判据的空绿). 接线形状锁
  [ci_aarch64_egfs_disk_leg_test.rs](../../host-tests/tests/ci_aarch64_egfs_disk_leg_test.rs) **8 条验收
  / 9 例**: 判据串与产出方同源 (含用户态 init 的三态 SWROOT 串) · 三相各有红分支且硬零经 `count_of`
  取数 (非十进制读数即红, 不把「测不出」读成「零条」) · 空白盘只新建一次 (相 2 复用同盘, 否则持久化
  判据恒真) · 摘盘负控制在 · 载体 + 串口日志 + 架构自检早于启动 + 恰挂 1 块盘 · audit `full` 真调用
  且失败走 `err` + 独立取证日志 · D5-g 安装接线/幂等/回读三相判据 · D5-h 切根终态标记与三态判别
  (第 8 条为 D6-c ③ 补钉, 两条负控制各恰红 1 项).
- **D5-c 调研**: switch_root 面 (chroot / pivot_root / ABI 编号 / 分发 / 根前缀均已存在, 故 B 方案
  增量远小于原估) + EGFS 写路径缺口清单 (见 D5-g).
- **D5-d uberblock drive 权威化 + 显式失败 (裁定 B 相对完整)**: 删 `vdev_0_drive_id()` 的
  `.map_or(0, …)` 静默兜底 (EGFS 的 `drive` 是 **EGDF 全局设备下标**, 0 号未必是块设备);
  `add_vdev` 移到读 uberblock **之前** (原序使 128 个槽位全读失败 ⇒ 每次启动误判空盘重新格式化);
  分区偏移取自同一 vdev (原独立计数器在多盘循环里漂移); `write_uberblock_to_disk` 两处静默失败
  (`!is_valid` 早退返 0 / `let _ = write_sector` 丢错) 改显式报错; `write_bp` 对非 512 倍数载荷补零
  整扇区写 (原样交给按整扇区校验的 `write_sectors` 会被拒写); `write_dir_entry_record` 恒写 0 改真
  对象号 (读侧一直按对象号解析 ⇒ 冷恢复后名字全绑到对象 0, 名字还在而文件全打不开). 落点断言 =
  分区起始 + txg 槽位的物理 LBA, 错 drive 与漏分区偏移双向负控制.
- **D5-e 引导根恒为 ramfs + EGFS 挂 /disk (裁定 B, Linux 式 switch_root)**: 内核不再让 EGFS 抢占
  `/`; `boot_rootfs_and_probes` 改挂 `/disk`, 并由 [init.rs](../../src/kernel/functions/init.rs) 的
  `install_disk_root()` 把 boot-module cpio **全量**安装进磁盘根 (幂等: `/disk/init` 已在则跳过重装,
  避免覆盖用户改动), 安装后回读 `/disk/init` 验真落盘. 改判理由: 原挂根形态覆盖 initramfs 根使
  用户态起不来, 且与业界 initramfs → `switch_root` 形态相反.
- **D5-f pivot_root syscall 面**: 新增 `VfsManager::is_mount_point` (**精确匹配**, 刻意不用
  `find_mount` 的最长前缀归并 —— 后者把任意子路径都认成挂载点, 判据恒真); `pivot_root_syscall`
  次序钉为 plan → 挂载点判据 → `unmount(?)` → 切前缀, 摘除失败整体失败, 不留「根已切但旧根仍可达」
  的半完成态. `pivot_root_plan` 保持纯函数 (`kernel_tests/sys.rs` 6 个用例不动). host 锁
  [pivot_root_switch_test.rs](../../host-tests/tests/pivot_root_switch_test.rs) 5 例 (真实 VFS +
  临时进程夹具), 4 条负控制逐条转红后按 sha256 逐字节复原.
- **D5-g EGFS 写路径可靠性 (四处)**: ① `fs_create` 实装 —— 原走 trait 默认 `NotSupported`, 令 cpio
  解包对每个常规文件静默失败 (实测 12 条目只落 1 条, 那 1 条还是被无条件计数的目录);
  ② `EgfsData::mkdir` 返回对象号被 `fs_mkdir` 适配器读成失败 (它只认 0) ⇒ 成功改返 0;
  ③ 挂载根 `stat("/")` 在平坦命名空间里查 `""` 键恒 `None` ⇒ `pivot_root` 的 `require_directory`
  判新根 ENOENT, 补 `objset.get_root()` 分支; ④ **二启恢复出来的磁盘根是只读的** ——
  `EgfsDataset::new` 的 `state` 缺省 `Creating`, 而 `mount_drive` 的成功恢复分支只调
  `restore_metadata` 换对象表与目录索引, 从不翻状态位 ⇒ `is_writeable()` 恒假, `create_dir` /
  `create_file` 一律 `None`. 修法 = 新增 `EgfsDataset::activate()` 只翻 state/mounted, 在
  `restore_metadata` 尾部调用 (**不能改调 `init()`**: 它会 `objset.init()` 推入全新根目录并复位
  `next_obj_id`, 抹掉刚恢复的表). 归因读数: 相 2 `[SWROOT] FAIL: mkdir /disk/oldroot rc=-1`
  连测 5 轮 (串口逐字符并发写会吃掉返回码数字, 故重复手动启动取干净行). host 锁
  `second_boot_restored_disk_root_stays_writeable` 两侧都钉 (盘上既有名字仍在 + 新名字能建能写,
  只判后者会让「重建空表」冒充修复); 摘 `activate()` 负控制恰红 mkdir 一项 (left -5 / right 0),
  恢复前提断言仍绿.
- **D5-h init 用户态切根 + 腿判据**: userlib 补 `SYS_PIVOT_ROOT = 155` 与 `env_pivot_root`;
  init 的 `switch_root_probe()` 置于**所有探针之前**, 三态 PASS / FAIL / SKIP (A-14 同口径:
  失败不 panic, 后续探针仍在引导根下跑, 信号由腿消费); 切根后以**视图路径** `/init` 读回 ——
  旧根挂载已被摘除, 该路径只能解析到磁盘根, 故读到字节即证明用户态文件访问真落到 EGFS.
  腿每相各等自己的终态标记 (只等 `Entering EL0` 会在切根发生前就收机). SIMPLIFIED: 不 re-exec
  新根的 `/init`, 本进程映像仍来自引导根, 只有文件访问改走磁盘根. 负控制: 摘 `switch_root_probe()`
  调用 ⇒ 恰红 3 项 (相 1/2 的 PASS + 相 3 的 SKIP), 全部硬零判据仍绿; 翻转 SKIP 守卫极性 ⇒
  恰红 6 项 (相 1/2 各红 `need PASS` + `forbid SKIP`, 相 3 红 `need SKIP` + `forbid FAIL`),
  证明三态判别与 forbid 侧都非恒真. 两条负控制后 init 与腿脚本均按 sha256 逐字节复原.
- **D5-i SGEG 冻结改判 + 摘 EGFS 散落 pwm 门 (用户裁定)**: 现版本 SGEG **冻结待重设计**, 不为
  EGFS 去适配一个将来要整体作废的账号/会话层 ⇒ 已落地的 165 行适配 (6 文件) 全部反向 Edit 回退,
  sha256 与 HEAD 逐字节一致; 改摘 EGFS 内 12 处 `check_permission` 调用点, EGFS 回到与 ramfs 一致的
  「FS 层不自带判定」形态, 判定整体留给 v2 的唯一出口 (`docs/design/sgeg-design.md` 的出口收敛主张).
  代价: v2 落地前 EGFS 无访问控制 (与 ramfs 现状同口径, 也与设计册「默认关闭、无策略时零差异」一致).
- **D5-j 二启恢复路径挂死**: `init()` 曾无条件调 `setup_zil_datasets()`, 后者持 `datasets` 锁跨
  `deserialize_dataset_metadata` → `restore_metadata` (自取同一把**不可重入** `IrqSpinLock`) ⇒
  同核自旋死等; 同一次重做还 push 出第二个 root 数据集, 并把 txg 从盘上值回退到 1 (新写入落回旧
  uberblock 槽位). 只在盘上 `root_bp` 非空 (即成功恢复过的二启) 时可达, 首启格式化写的是 null
  `root_bp`, 故相 1 恒绿掩盖了它. 修法: `init()` 尾部加 `if !self.is_initialized()` 守卫 +
  `setup_zil_datasets` 先放锁再 deserialize. host 锁 2 例 (行为 + 源码形状), 负控制两条
  (数据集数左 2 右 1 / 锁作用域未闭合).

### D6: 收口 (七门槛 + 文档 + 台账)

- **描述**: 全量验收与文档/台账同步.
- **方案**: §2.3 七门槛全绿 (audit full + test-host + test-kernel-host + ci-rustdoc 分跑); 同步
  explain/design (载体与地址布局现状) + 本册状态 + 台账; 按路径提交推送.
- **状态**: [X] (七门槛实测全绿, 读数见下; 本册 12 个状态字段 = 10 `[X]` / 2 `[]`, 仍开放 = 决策 D + L0)
- **D6-a 七门槛实测读数 (2026-11-17, 同一棵树串行跑; 判据 = 日志内 `*-RC=` 回显, 非后台通知退出码)**:
  | 门槛 | 命令 | 读数 |
  |---|---|---|
  | 1/2/3/5 | `bash ci/audit.sh full` | **AUDIT-RC=0** |
  | 4 | `make test-host` | **HOST-RC=0** — 202 个测试二进制 / 1597 passed / 0 failed |
  | 6 | `make test-kernel-host` | **KHOST-RC=0** — workspace 三成员 / 1372 passed / 0 failed |
  | 7 | `make -f Makefile.ci ci-rustdoc` | **RUSTDOC-RC=0** — x86_64 与 aarch64 两腿都真跑完, 0 源码级 link 错误 + 0 私有项链接 |

  audit `full` 分项: QEMU 双架构真实启动 **2/2** (FAIL_OK=0) · P-62 feature 运行维 **4/4** · lockdep
  构建面双架构两侧 · thread-stress **6/6 × 3 轮 × 2 架构** · Lockbud **4/4 crate 实编分析零发现** ·
  性能指标腿条目全部在绝对预算内 · 文档路径新鲜度腿 (反引号 + markdown 链接两形态) 通过 ·
  `scan_obsolete_naming` / `audit_vacuous_needle` / `audit_selftest` **12/12** 通过 · aarch64 COW 四判据
  全符合 (#108 丙) · UEFI(OVMF) 可达面判据全符合 (D4-i 丙, `[THREAD]` 明示不判) · aarch64 EGFS 磁盘
  挂载三相 **30 项判据全符合** (D5) · **本轮因环境缺失未执行腿数: 0**.
  文档回填后再跑一次 `bash ci/audit.sh full` 复验 (代码树未变, 只动本册与台账): **AUDIT-RC=0**,
  分项读数同上, 未执行腿数仍 0.
- **D6-b 台账同步**: `progress-active-tasks.md` 补 D6 收口 blockquote + 甲表新增本册行 (此前「整表重算
  补齐 5 行」只落 4 行, 本册缺失属 §9.2 漂移) + 本册计数按口径 C 重算 (12 行首状态字段, 无勾选/表格行级
  状态, 故 A=B=C). 自指行由 79 → 80.
- **D6-c 本轮门禁红三处 (均由本轮改动直接导致, 按 §11.5 本轮修复, 先归因再改钉)**:
  ① `scan_obsolete_naming` 判 `egfs_disk_persist_test.rs` 的 `b"ANTX"` 字面量违规 —— ANTX 既是废弃
  项目代号也是**盘上持久签名魔数**, 仓内既有写侧 (`boot_install`) 与读侧 (`scan_all_drives`) 一律
  **逐字节拼写**; 测试改用同一形态 + WHY 注释, **不改盘上格式也不给扫描器加豁免**.
  ② A-3b 覆盖率棘轮判 `vfs_manager.rs::mounts` 新增取锁点未分类 —— `is_mount_point()` (D5-f) 合法
  新增 1 点, 走 sanctioned `--update-lock-order-baseline` 重录并同步 `ref-lock-order.md` 读数.
  ③ `make test-host` 判 D5-b 形状锁 `test_no_disk_negative_control_is_present` 红 —— **stale needle
  而非脚本缺陷**: D5-h 给 `boot_once` 加了 `<终态标记>` 位, 旧针仍要求裸 `boot_once "$P3_LOG"`;
  改钉整行 `boot_once "$P3_LOG" "$SWROOT_SKIP"` (同时钉住「不带盘」与「等 SKIP 不等 PASS」两面,
  严格强于旧针), 并补第 8 条验收锁 D5-h 三态判据 (此前**无任何形状锁**). 两条负控制各恰红 1 项
  (删相 3 `forbid $SWROOT_PASS` ⇒ 第 8 条红; 相 3 标记 SKIP→PASS ⇒ 第 4 条红), 脚本按 sha256 逐字节复原.

## 验收判据

- **底线**: §2.3 七门槛全绿, 任何阶段不可豁免.
- **解耦**: 内核镜像零用户态字节 (`include_bytes!` 用户态面清零); 内核本体 `_kernel_end` 远低于
  4 MiB; x86_64 K-08 因镜像缩小自然不再触发.
- **双架构**: x86_64 (MB2 tag 3) 与 aarch64 (DTB `/chosen`) 均从 boot-module 装载 `/init` 并进
  Ring 3; aarch64 带盘时 EGFS 只挂**非根挂载点** `/disk`, 用户态 `pivot_root` 切根进磁盘根并从
  新根读回 `/init` (三相腿: 空盘首启 / 同盘二启 / 摘盘负控制, 30 项判据); 无盘时探针仍在引导根
  下跑并判 `[SWROOT] SKIP`.
- **退役 (决策 D)**: x86_64 低半区 kview 恒等窗口退役后 `user_va_lower_bound` 不再随 kernel_end 增长;
  回归锁保护 copy_*_user 直射区路径.

## 开放议题 (不在本轮施工)

- **UEFI-native loader 演进**: L0 用 GRUB2 覆盖 BIOS + UEFI 两类真机; 待老 BIOS 机退场后可进一步上
  EFI-stub / EDK2 原生 loader (直接读文件系统载 kernel+initrd), 作为 BIOS 弃用后的长期形态.
- **aarch64 EFI config table initrd**: UEFI 直启时 initrd 可经 `LINUX_EFI_INITRD_MEDIA_GUID`; 本轮
  先走 DTB `/chosen` (QEMU virt / U-Boot).
- **mkfs EGFS 工具**: aarch64 磁盘镜像生成的格式化工具缺失; 若复用 x86_64 建盘再挂 aarch64 有
  端序/布局风险, 待 D0 评估.
- **D4 与 KPTI 的关系**: 退役低半区恒等窗口后, KPTI 高半区共享面与 `copy_*_user` 直射区路径的
  交互须专项回归 (随 D4).
