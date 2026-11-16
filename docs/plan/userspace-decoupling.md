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
- wasm 桩 (两架构): [functions/wasm/stub.rs:17-29](../../src/kernel/functions/wasm/stub.rs#L17) (`wasmstub.bin` / feature 点亮 `wasmrun.bin`)

默认构建 (无 initramfs) `_kernel_end=0x3f5000` (仅 44 KiB 余量); initramfs 载体腿叠 237 KiB
cpio 即顶破 4 MiB. cpio 由 7 个用户 ELF (~218 KiB, **未剥离符号**) 主导. **剥离符号只买
~35 KiB, 是止血不是解**——内核镜像本就不该承载用户态.

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
  [functions/wasm/stub.rs](../../src/kernel/functions/wasm/stub.rs) 的全部用户态 `include_bytes!`
  与内嵌回退分支; exec `\0asm` 派发改「从 VFS 读 `/bin/wasmstub`(或 `wasmrun`)」.
- **状态**: []
- **详情**: 用户裁定「严格零嵌入 (对齐 Linux)」. 代价: **所有进入用户态的启动腿** (`-kernel`
  裸起) 必须改 `-initrd` 携 cpio, 否则只能停在内核 (对齐 Linux「无 initrd 即 mount 不到 root」).

### 决策 B: 双协议 boot-module 载体

- **描述**: x86_64 与 aarch64 各用本平台 boot 协议交出 initrd 位置, 内核从该内存区读 cpio.
- **方案**: x86_64 = **Multiboot2 tag 3 (MODULES)** (现 [boot/mod.rs:252](../../src/kernel/privileged/boot/mod.rs#L252)
  `parse_multiboot2` 只处理 tag 4/6/8, tag 3 落 `_ => {}`); aarch64 = **DTB
  `/chosen/linux,initrd-start` + `linux,initrd-end`** ([privileged/dtb.rs](../../src/kernel/privileged/dtb.rs) 新增解析).
  二者统一产出 `BootInfo` 的 `initrd_base/size`, PMM 预留下界以保护该区不被复用.
- **状态**: []
- **详情**: 同概念 (boot 交位置、内核读区、PMM reserve) 两套协议. `privileged` 面新增 unsafe
  (裸指针读 boot 结构) → 必配 `// SAFETY:` (F4). QEMU `-kernel kernel.flat -initrd cpio` 是否
  按 MB2 模块装载须 D0 实测.

### 决策 C: aarch64 EGFS 磁盘根接入

- **描述**: aarch64 经 virtio-blk 发现并挂 EGFS 根、从磁盘装载用户态, 补齐生产形态.
- **方案**: 去 [lib.rs:940](../../src/kernel/lib.rs#L940) `target_arch="x86_64"` 门控;
  接 virtio-blk → EGFS `init()` 自动发现 → `vfs_mount_internal("/", "egfs")`; 补 aarch64
  EGFS 磁盘镜像生成 (QEMU virt 加 virtio-blk 盘) + 磁盘根启动腿.
- **状态**: []
- **详情**: T-01 现状 (aarch64 有 virtio-blk 无 FS 消费). 镜像生成方案 (主机 `mkfs.egfs` 工具
  或缺则先 x86_64 建盘后复用) 待 D0 定; 现无 EGFS 格式化工具, 是 D0 的开放子项.

### 决策 D: x86_64 kview 低半区恒等窗口退役 (单独处理)

- **描述**: 退役造成 K-08 的 x86_64 低半区 kview 恒等窗口, 回归高半区直射区 (aarch64 为参照),
  从根上消除「内核镜像增长撞用户区」类约束.
- **方案**: 让 x86_64 的 `copy_*_user` / 缺页路径经**高半区直射区**访问物理页 (如 aarch64 的
  `phys_to_virt`), 不再依赖低半区恒等窗口护镜像; 相应调整
  [mm/mod.rs `user_va_lower_bound`](../../src/kernel/privileged/mm/mod.rs#L254) 与
  [vmm_x86_64.rs `boot_identity_tables`](../../src/kernel/privileged/vmm_x86_64.rs#L1163).
- **状态**: []
- **详情**: 用户裁定「纳入本次插队项**但单独处理**」. 触及 #89 `kview` 修双视图分叉的根基, 与
  内容解耦正交; 施工 D1-D3 落地后内核镜像已回 4 MiB 内, K-08 不再触发 ⇒ 决策 D 是「根治天花板」而非
  解 K-08 的必需品, 风险高、跨度大, **单开为收尾独立批次**, 不阻塞 D1-D3.

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
- **状态**: []
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
- **状态**: []
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

### D4: 启动腿改造 (-initrd 携载)

- **描述**: 全部进入用户态的启动腿改 `-initrd <cpio>`, 使零嵌入后仍能进 Ring 3.
- **方案**: 逐脚本改 `-kernel` 起法附 `-initrd`; 覆盖门槛 5 (`qemu_boot_test.sh`) / e2e / P-62
  feature 运行腿 / wasmi 腿; `initramfs_boot_test.sh` 由「feature 载体腿」转为默认形态.
- **状态**: []
- **详情**: 波及 12+ 腿 (D0 清单). 每条改后单独真跑一次取证 (红先归因, §QEMU 纪律).

### D5: EGFS 接入 aarch64

- **描述**: aarch64 virtio-blk → EGFS 挂根 → 从磁盘装载用户态.
- **方案**: 去 [lib.rs:940](../../src/kernel/lib.rs#L940) 门控; 接 EGFS 自动发现挂载; 补 aarch64
  EGFS 磁盘镜像生成; 新增 aarch64 磁盘根 QEMU 腿.
- **状态**: []
- **详情**: D0 定的镜像生成方案落地. 与 D1-D4 正交 (boot 载体 vs 运行时磁盘根); 可并行.

### D6: 收口 (七门槛 + 文档 + 台账)

- **描述**: 全量验收与文档/台账同步.
- **方案**: §2.3 七门槛全绿 (audit full + test-host + test-kernel-host + ci-rustdoc 分跑); 同步
  explain/design (载体与地址布局现状) + 本册状态 + 台账; 按路径提交推送.
- **状态**: []

## 验收判据

- **底线**: §2.3 七门槛全绿, 任何阶段不可豁免.
- **解耦**: 内核镜像零用户态字节 (`include_bytes!` 用户态面清零); 内核本体 `_kernel_end` 远低于
  4 MiB; x86_64 K-08 因镜像缩小自然不再触发.
- **双架构**: x86_64 (MB2 tag 3) 与 aarch64 (DTB `/chosen`) 均从 boot-module 装载 `/init` 并进
  Ring 3; aarch64 EGFS 磁盘根可挂载并跑用户态.
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
