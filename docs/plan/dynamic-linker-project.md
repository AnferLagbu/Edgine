# Linux 动态装载工程（上游 ld-musl / ld-linux 先行）

> 状态：D1–D6 主体施工完成；x86_64 与 aarch64 上游 `ld-musl` 均已真跑通过（`[DYN] PASS`），缺解释器失败路径与 `elfld` 退役已落地；剩余扩展失败路径与可选 pthread/dlopen。目标不是“自研一个动态链接器”，而是让**上游 Linux 动态链接器**在 Edgine 上按 Linux 语义跑起来，并让自研 `elfld` 退役。T7 已于 2026-11-25 按方案一以双架构 musl 收口，glibc/vDSO 归本册 D7 可选工程，不阻塞 T7。D7 G0 已在 x86_64 真实 glibc `true` 上通过，G1 的带 stdout `hello` 也已通过（aarch64 暂无载体，SKIP）；G2 已完成启动 syscall 补齐与 `rseq` 取证，G3 已完成无 VDSO 时间面取证并修复 `time`/`getcpu` ABI，G4 pthread/clone3/TLS 子腿已完成；C1 clone3 高级字段已收口；L1 glibc `dlopen` 第二共享对象已收口；P1 `prlimit64` 跨进程能力面已收口；R1 `rseq` 注册/退出语义已收口；T1 真实 RTC/墙钟 epoch 已收口；A1 aarch64 glibc 运行时载体已收口；V1 最小 VDSO 经复核无硬触发，评估收口为不实现；D7 延后项剩余 0 项。

## 0. 边界澄清：内核不实现动态链接

动态链接器（`ld-musl` / `ld-linux`）是**用户态 ELF 解释器**，符号解析、重定位、库搜索、`dlopen` 全在用户态完成。内核不解析 `.dynsym`、不读 `.rela`、不做重定位。

内核只做 Linux `execve` 对 `PT_INTERP` 的最小契约：

1. 读主程序的 `PT_INTERP` 字符串；
2. 打开并读取解释器 ELF；
3. 把主程序与解释器都映射进新地址空间；
4. 在用户栈上构造 `argc/argv/envp/auxv`；
5. 把入口设为解释器入口，并给出 `AT_BASE`/`AT_ENTRY`/`AT_PHDR`；
6. 提供解释器运行所需的 `mmap/mprotect/open/read/...` 与 `mm` 生命周期。

之后发生的事全在用户态：`ld-musl` 自己打开 `libc.musl-*`、映射、重定位、跑 init、跳到 `main`。

**为什么不能“只把 ld-musl 当普通程序 exec”**：可以手工执行 `ld-musl /path/prog` 做临时验证，但标准外部程序自带 `PT_INTERP`，`execve` 必须按 Linux 语义把控制权交给解释器；手工调用只是测试夹具，不是验收路径。而且解释器自身启动仍需要 auxv 与 `mm`，内核工作不会因此消失。

**glibc 同理**：`ld-linux` 与 `libc.so.6` 都是用户态；内核不实现 glibc，只补它额外依赖的 syscall/VDSO 面。

### 0.1 职责切分

| 层 | 归属 | 内容 |
|---|---|---|
| 用户态 linker | `ld-musl` / `ld-linux` | 解析 `PT_DYNAMIC`、符号查找、重定位、库搜索、init、`dlopen` |
| 用户态 libc | `libc.musl-*` / `libc.so.6` | C 库、TLS、pthread、启动代码 |
| 内核 ELF 装载 | Edgine | `PT_INTERP`、双映像 `PT_LOAD`、入口、`AT_*`、用户栈 |
| 内核内存/syscall | Edgine | `mm`、`mmap/mprotect/brk`、`open/read/fstat`、TLS 寄存器 |
| 内核 VDSO（可选） | Edgine | 只映射用户态代码页；不是 glibc 实现 |

## 0.2 结论

- **不写自研 linker**：直接使用上游 `ld-musl-<arch>.so.1`（后续再评估 glibc `ld-linux`）。
- 内核负责 Linux `PT_INTERP` 语义：装载主程序 + 解释器、构造 auxv、把入口交给解释器。
- **前置硬依赖是 `mm` 生命周期 M2–M5**：动态链接器是第一个真正行使 `mmap/mprotect/munmap/brk` 的用户程序；`mm` 不装，`mmap` 会回退返物理地址，动态链接必失败。
- 估算（1 名熟悉该树的工程师，含测试与文档，不含 review/CI 排队）：
  - **动态 musl（x86_64 + aarch64）**：约 **27–41 人日**，P50 约 **32 人日**（≈1.5 人月；含缓冲约 2 人月）。
  - 其中 `mm` M2–M5 占 **8–12 人日**；若 `mm` 已单独完成，动态链接器本体约 **19–29 人日**。
  - **glibc 动态**（可选后置）：最小单线程 hello 再加 **8–14 人日**；含 pthread/rseq/VDSO 的可用面 **15–25 人日**。主要成本在 VDSO、rseq、`set_tid_address`/`prlimit64` 与 glibc 启动面。
- 变更量估算：生产代码 **1,500–2,500 行**（含 `mm` 与 `elfld` 删除），测试/夹具/文档 **700–1,200 行**；真正的大头是回归面，不是行数。

## 1. 现状边界（已核）

| 面 | 现状 | 对动态链接的影响 |
|---|---|---|
| `PT_INTERP` | `prepare_elf_image` 把 Linux 解释器路径改写成 `/usr/libexec/elfld.so`，内核不加载第二个解释器 | **必须删改写**；改为读取并装载真实解释器 |
| 解释器装载 | `load_elf_from_memory` 只装一个 ELF，入口直接设为主程序入口 | 需要双映像装载与 `AT_BASE`/`AT_ENTRY` |
| auxv | 栈上只有 argc/argv/envp；全树无 `AT_PHDR`/`AT_BASE`/`AT_RANDOM` 等 | 动态链接器无 auxv 无法启动 |
| `mm` | 生产装载路径不安装 `MmStruct`；`mmap` 无 mm 时回退分配裸页并返物理地址 | `mmap`/`mprotect`/`brk` 不可用；动态链接器首步即失败 |
| `mmap` | 匿名映射可用；`MAP_FIXED` 未实现（hint 精确落点但不拆除旧 VMA）；私有文件映射靠 #PF COW | musl 加载共享库通常用 `MAP_FIXED`，会撞已有 VMA |
| 文件映射 | `fd_mmap_backing` 已接；`MAP_PRIVATE` 可写靠 COW；`pcache` 已有 | 需验证 libc `.so` 的 text/data/bss/RELRO 组合 |
| TLS | x86 `arch_prctl(ARCH_SET_FS)` 已实装；aarch64 有 `TPIDR_EL0` 写入与切换恢复 | 基本具备，需用动态 TLS 程序证实 |
| 启动 syscall | `getrandom` 有；`set_tid_address`/`prlimit64`/`rseq` **全树无** | G1 的 `true`/`hello` 未触发；G2 先用真实调用探针取证再补 |
| VDSO | 无 | musl 可不用；glibc 需要评估 fallback 或最小 VDSO |
| `elfld` | 102 行桩，仅静态 PIE；`edgine-gcc` 注入 `/usr/libexec/elfld.so` | 整体退役，不再扩成 linker |

## 2. 范围

### 做

1. 删除 `PT_INTERP` 改写与 `EDGINE_INTERP`；保留真实解释器路径。
2. 内核装载主程序 + 解释器两个 ELF 映像到同一新地址空间，入口交给解释器。
3. 构造 Linux 初始栈：`argc/argv/envp` + `auxv`（至少 `AT_PHDR/AT_PHENT/AT_PHNUM/AT_BASE/AT_ENTRY/AT_PAGESZ/AT_RANDOM/AT_EXECFN/AT_UID/AT_EUID/AT_GID/AT_EGID/AT_SECURE/AT_PLATFORM/AT_HWCAP/AT_CLKTCK/AT_SYSINFO_EHDR`）。
4. 完成 `mm` M2–M5：装载安装 mm、exec 换 mm、fork/clone 继承、退出释放、删除 `mmap/munmap` 裸页回退。
5. 补 `MAP_FIXED`（先拆除重叠 VMA 再插入）、必要的 `MAP_FIXED_NOREPLACE` 语义、文件映射私有可写/RELRO/mprotect 验证。
6. 工具链与根镜像：准备 `ld-musl` + `libc.so` + 动态 hello；退役 `tools/elfld`、`tools/edgine-gcc` 的解释器注入。
7. 验收：x86_64 与 aarch64 动态 musl hello 真跑；QEMU/UEFI；静态腿不回归。

### 不做（第一波）

- 不自研 linker、不扩 `elfld`、不做符号解析/重定位实现。
- 不做 glibc 优先；不把 glibc 作为 musl 的前置。
- 不做 VDSO 完整实现；`AT_SYSINFO_EHDR = 0` 先跑 musl。
- 不做 `dlopen`/`dlsym` 全量验收；先证明“动态可执行文件能启动并输出”。
- 不做内核模块形态的 linker；不引入私有 syscall。

## 3. 工作分解

### D0：冻结与取证（0.5–1 人日）

- 冻结“上游 musl 解释器 + 内核 PT_INTERP 语义”路线。
- 取一个动态 musl hello，记录 `readelf -l` 的 `PT_INTERP`、`PT_LOAD`、`PT_GNU_RELRO`、`PT_GNU_STACK`。
- 确认根镜像中解释器与 libc 的真实路径/大小；确认 `ELF_MAX_SIZE` 是否够单文件读取。
- 出口：路线、路径、fixture、失败基线固定。

**D0 实测（2026-11-24，Alpine 3.20 minirootfs）**

| 对象 | 大小 | 类型 | 关键段 |
|---|---:|---|---|
| `ld-musl-x86_64.so.1` | 649,832 B | ET_DYN，入口 `0x62b11` | 4 个 `PT_LOAD`；`PT_GNU_RELRO` `0x520`；`PT_GNU_STACK` RW |
| `iconv` | 24,520 B | 动态 PIE，`PT_INTERP=/lib/ld-musl-x86_64.so.1` | `NEEDED libc.musl-x86_64.so.1`；`PT_GNU_RELRO` |
| `busybox` | 808,712 B | 动态 PIE，同上 | `PT_INTERP` `filesz=0x19`；4 个 `PT_LOAD`；`PT_GNU_RELRO` `0x37d0` |

结论：

- `ELF_MAX_SIZE = 1 MiB` 对 musl loader（650 KB）与 busybox（809 KB）都够，但 busybox 余量仅约 **233 KiB**；第一波优先用 `iconv`/`getconf` 这类 20–40 KB 的纯 libc 动态程序，busybox 只作压力项。
- Alpine 的 `/lib/libc.musl-x86_64.so.1` 是 `ld-musl-x86_64.so.1` 的**符号链接**；根镜像必须同时提供 `libc.musl-x86_64.so.1` 名字，否则 `NEEDED` 解析失败。
- 主程序是 PIE 时，`AT_PHDR` 不能直接取 `e_phoff`；必须取“包含 `e_phoff` 的那个 `PT_LOAD`”的 `p_vaddr + (e_phoff - p_offset) + load_bias`。非 PIE 才是 `0x400000 + e_phoff`。
- `ld-musl` 自身没有 `PT_INTERP`；它是 ET_DYN，必须由内核按 `AT_BASE` 装载，不能递归解释。
- 第一波 `AT_SYSINFO_EHDR = 0`；musl 不依赖 VDSO。

### D1：`mm` M2–M5（8–12 人日，前置）

- M2（已施工，未提交）：生产装载路径创建 `MmStruct`，每个 `PT_LOAD` 同步插入匿名 VMA，初始化 `start_stack/mmap_base/start_brk/brk`；非执行段置 `NX`。
- M3（已施工，未提交）：删除 `mmap_get_mm_or_alloc` / `munmap_free_pages` 裸页回退；无 mm 一律 `ENOMEM`。
- M4（部分）：`make test-host` 已加 mm 安装/VMA 与无 mm fail-closed 契约锁；双架构 QEMU 启动通过。尚未加“`mmap` 返回值必在用户 VA 区”的运行期断言。
- M5（未开始）：exec/fork/exit 生命周期回归、`MmStruct` 释放/refcount、七门槛。
- 出口：动态链接器能拿到合法 `mmap` 返回值，`mprotect/brk` 不因无 mm 失败。

> M2/M3 的 x86 QEMU 腿曾出现既有 `[SCHED] SNAP END` 缺失与 e2e 超时噪声；2026-11-25 已定位为 x86 信号返回跳板落在 NX 用户栈导致的 `instruction-fetch #PF` 风暴，并改为固定只读可执行跳板页。当前 x86 `[SIG] PASS`、`[tcp] Listening`、TCP/UDP e2e 通过；aarch64 全绿。

### D2：双映像 ELF 装载（5–7 人日）

- 已施工（未提交）：`elf::interp_path` 从 `PT_INTERP` 读绝对路径，不改写原映像；functions 层经 VFS 读解释器，`init` 与 `execve` 共用。
- 已施工（未提交）：`load_elf_from_memory_with_interp` + `load_elf_images` 在同一 `cr3/mm` 装载主映像与解释器；解释器按 ET_DYN 选 mmap 区基址，入口改交解释器；`brk` 只跟主映像，解释器占用 mmap 基址后上抬 `mmap_base`。
- 保留旧 `proc_exec_replace` / `user_proc_load_elf_from_memory` 为静态单映像包装，新增 `_with_interp` FFI 入口。
- D3 已施工（未提交）：初始栈写 `argc/argv/envp/auxv`；`AT_PHDR` 由包含 `e_phoff` 的 `PT_LOAD` 反推；`init` 也建立带 auxv 的空参数栈。
- 装载顺序：先主程序/解释器都成功，再切换；失败回滚，保持旧映像。
- 设置 `entry = interp_entry`；记录 `main_entry`、`interp_base`、`phdr`。
- 出口：两个 ELF 的 `PT_LOAD` 都落地，入口是解释器，旧 `PT_INTERP` 改写路径零调用。

### D3：初始栈与 auxv（3–5 人日）

- 已提交（`adc6903c`）：`setup_user_stack_in_table` 在 `argc/argv/envp` 之后写 auxv 两字对，再放字符串与 16 字节 `AT_RANDOM` 区；`exec` 即使 argv/envp 为空也建栈。
- 已提交（`adc6903c`）：`AT_PHDR` 由包含 `e_phoff` 的 `PT_LOAD` 反推；`AT_PHENT/AT_PHNUM/AT_BASE/AT_ENTRY/AT_PAGESZ/AT_EXECFN/AT_RANDOM` 已写入。
- 已提交（`adc6903c`）：`AT_UID/EUID/GID/EGID/AT_SECURE/AT_HWCAP/AT_CLKTCK/AT_HWCAP2/AT_SYSINFO_EHDR=0` 已写入；`AT_PLATFORM` 尚未写，musl 与 D7 G1 的 glibc `true`/`hello` 均未依赖，暂不写，等真实 hwcaps/ifunc 需求。
- 验证（2026-11-25）：x86 e2e 超时真因不是 auxv/栈布局，而是 x86 `rt_sigreturn` 跳板被复制到用户 BSS 栈页；该页 NX，执行跳板触发 instruction-fetch #PF，页故障处理器把它当数据缺页反复映射，制造逐页 TLB shootdown 风暴。修复为固定只读可执行跳板页 `0x500000`（`PRESENT|USER`，不带 NX/WRITABLE），信号返回不再执行栈上代码。
- 取指缺页语义补齐（2026-11-25）：`handle_vma_fault_with_mm` 对 `instruction && VMA NX` 直接返回 `SignalSegv`，不再匿名分配同一 NX 页；可执行 VMA 的 not-present 取指仍保留按需分配（JIT/匿名 `PROT_EXEC`）。这样即使再出现“在 NX 数据页执行”的用户行为，也不会演变成 TLB shootdown 风暴。
- aarch64 固定跳板移植（2026-11-25，`03aec35c`）：`SIGRETURN_TRAMPOLINE_ADDR` 与 `map_sigreturn_trampoline` 解除 x86_64 cfg，两架构共用 `0x500000` 只读可执行页；aarch64 `enter_handler` 把 `x30` 指向该页，信号投递不再把 8 字节跳板复制到 NX 用户栈。物理页清零/写入改走 `raw::phys_to_kern_mut`，兼容 aarch64 KPTI EL1 高半区视图。验证：aarch64 `[SIG] PASS`（svc 腿 + IRQ spin-live 腿）、`[tcp] Listening`、e2e 3/3；x86 回归 `[SIG] PASS`/`[tcp] Listening`/e2e 通过；`make test-kernel-host` 640/0、`./ci/build.sh all` 6/0。
- `AT_RANDOM` 当前由 ASLR 栈址 / `cr3` 派生（不在装载期调用时钟或 canary CAS 熵池）；D7 再接 `getrandom`/硬件熵源。
- 出口：解释器能读到完整 auxv 并定位主程序 phdr；D3 的静态 init/exec 栈已通过 aarch64 验收，x86 仍受既有观测/超时噪声影响。

### D4：mmap/VFS/syscall 缺口（5–8 人日，可与 D2/D3 并行）

- `MAP_FIXED`：先 `remove_range` 拆除重叠，再插入；`MAP_FIXED_NOREPLACE` 冲突返 `EEXIST`。
- 校验 `MAP_PRIVATE` 文件映射的可写 COW、`mprotect(PROT_READ)` 的 RELRO、`PROT_NONE` guard。
- 校验 `openat/pread/fstat/readlink/close` 足以让解释器找到并映射 libc。
- `set_tid_address`/`prlimit64`：G2 已按宿主 glibc 启动序列取证并补齐；`rseq`
  保持 `ENOSYS`，等 G4 pthread 再定注册/退出语义。
- 仍待补专项断言：`MAP_PRIVATE` 文件写 COW 的“写后不污页缓存”；`set_tid_address`
  的退出清理与 `prlimit64` 跨进程权限面尚无独立集成断言。
- `/proc/self/exe`：若 musl 的 `AT_EXECFN` 路径不足，再补；第一波先不强制。
- 已施工（2026-11-25，`beb6c07d`）：`MAP_FIXED` / `MAP_FIXED_NOREPLACE` 固定落点；固定地址必须页对齐、不低于用户 VA 下界、不越过 `USER_ADDR_MAX`。`MAP_FIXED` 先按范围注销文件页缓存持有、拆 VMA 与用户页表项，再插入；`MAP_FIXED_NOREPLACE` 冲突返 `EEXIST`，不拆旧映射。无当前 CR3 时固定落点 fail-closed（只拆描述符，不触碰 VMM）。
- 已施工（2026-11-25，`beb6c07d`）：`PROT_NONE` 保留 VMA 占位但不置 `PRESENT`，缺页路径见到无 `PRESENT` 的 VMA 直接 `SIGSEGV`，不再被匿名页分配“修复”成可读页；`mprotect(PROT_NONE)` 同步清 PTE 的 present 位。
- 已施工（2026-11-25，`beb6c07d`）：文件映射 VMA 不参与相邻合并，避免不同 `inode_id/offset` 被合成一个 VMA 后缺页读错文件页；同权限文件重叠直接拒绝。
- 已验收（2026-11-25，D5 x86_64）：真实 `ld-musl` 完成 libc 装载、重定位与 RELRO，动态 hello 输出 `hello dynamic musl` 并以 0 退出；`mprotect(PROT_READ)` 不再返回 `ENOMEM`。`openat/pread/fstat/readlink/close` 的映射序列已由外部 loader 实际覆盖。
- 仍待补专项断言：`MAP_PRIVATE` 文件写 COW 的“写后不污页缓存”与 `set_tid_address` / `prlimit64` 入口；musl 与 D7 G1 均未触发，按 G2 取证再补。
- 出口：解释器能打开 libc、按 `MAP_FIXED` 落段、做重定位、跑 `main`。

### D5：工具链、根镜像与 `elfld` 退役（2–3 人日）

- 已提交（`775a9679`）：新增 x86_64 动态 fixture `scripts/fixtures/dynamic-musl-x86_64/`：
  - `hello_dyn` 10,040 B，`PT_INTERP=/lib/ld-musl-x86_64.so.1`，`NEEDED=libc.musl-x86_64.so.1`；
  - `ld-musl-x86_64.so.1` 649,832 B，来自 Alpine 3.20 minirootfs；
  - `libc.musl-x86_64.so.1` 同一对象副本（Alpine 内为符号链接；cpio 解析器当前不支持 symlink，故复制同字节）；
  - `hello.c` / `build_fixture.sh` / `README.md` / `SHA256SUMS`，记录三者 sha256。
- 已提交（`775a9679`）：`scripts/make_initramfs_cpio.py` 归档新增 `bin/dyntest`、`lib/ld-musl-x86_64.so.1`、`lib/libc.musl-x86_64.so.1`；`Makefile` 新增 `DYN_MUSL_FIXTURE_DIR`/`USER_DYNTEST`/`USER_DYN_LOADER`/`USER_DYN_LIBC` 与仅 x86_64 的 `DYN_CPIO_DEPS`。
- 已提交（`775a9679`）：`init` 静态，`dyn_probe()` 在 `exec_probe()` 后对 `/bin/dyntest` 发真 `execve`；父进程按 `wstatus==0` 打印 `[DYN] PASS: dynamic musl execve`，失败打印 status/reap；缺失载体打印 SKIP（aarch64 当前走此腿）。
- 已提交（`775a9679`）：`scripts/qemu_boot_test.sh` x86_64 分支新增 fail-closed 断言：必须出现 `[DYN] PASS` 且不得出现 `[DYN] FAIL`；该行缺失即整轮 RC=1，不再让“内核启动 1/1”掩盖动态装载失败。
- 根因与修复（2026-11-25）：首次真跑时 `ld-musl` 进入重定位，但 RELRO 阶段报 `RELRO protection failed: No error information`，动态子进程 127 退出。GDB 抓到 `mprotect` 参数为 RELRO 页，`mm.mprotect` 返回 `Err(ENOMEM)`；根因不是 `mprotect` 保护逻辑，而是 `proc_exec_replace_with_interp` 的 `replace_user_space` 只换了 CR3/entry/stack，**没有把临时进程新建的 `MmStruct` 转移给当前 PID**。当前进程仍持 init 的旧 mm，RELRO 页不在旧 VMA 表中，故 `to_modify` 为空返 `ENOMEM`。musl `ldso` 用 `__syscall` 读原始返回值、不写 `errno`，所以文案是 `No error information` 而非真实 ENOMEM。
- 修复（`b2a9ec91`）：`replace_user_space` 增加 `new_mm` 参数，换表同时 `store_mm(new_mm)`；exec 成功后清空临时进程的 mm 槽位，所有权随地址空间转移。验证：x86_64 QEMU 动态 hello 打印 `hello dynamic musl`、`[DYN] PASS`，启动门禁 `1/1` 且新增 DYN 断言通过；aarch64 `[DYN] SKIP: no carrier`，`[SIG] PASS`/TCP/e2e 不回归。
- `elfld` 已退役（`d6505490`）：`tools/elfld` 删除、`edgine-gcc` 停止注入、内核 `EDGINE_INTERP`/`rewrite_interp_path`/`prepare_elf_image` 全删；静态 ELF 路径保留。D6 缺解释器失败路径已由 `81eea0ba` 接入。
- 出口：树中零 `elfld`/`EDGINE_INTERP`；动态 ELF 的 `PT_INTERP` 保持原样。

### D6：动态 musl 验收（4–6 人日）

- 已施工（`e89cc918`）：x86_64 动态 `hello_dyn` 输出 `hello dynamic musl`，0 退出；`readelf -l` 确认 `PT_INTERP=/lib/ld-musl-x86_64.so.1` 未被改写。
- 已施工（`e89cc918`）：aarch64 采用 Alpine 3.20.3 minirootfs 的真实 busybox 动态 PIE（919,232 B，`PT_INTERP=/lib/ld-musl-aarch64.so.1`，`NEEDED=libc.musl-aarch64.so.1`），init 以 argv0 `/bin/busybox` + applet `echo` 执行 `/bin/dyntest`；上游 loader 完成 libc 装载/重定位/RELRO 后输出 `hello dynamic musl` 并 0 退出。该腿同时覆盖 aarch64 `TPIDR_EL0` TLS 与解释器入口。
- 已施工（`e89cc918`）：`qemu_boot_test.sh` 两架构各自 fail-closed 要求 `[DYN] PASS` 且无 `[DYN] FAIL`；实测双架构 QEMU 启动门禁均 `1/1`，aarch64 e2e 3/3、x86 e2e 不回归。
- 已施工（`81eea0ba`）：缺解释器失败路径双架构造桩（145 B ELF，PT_INTERP 指向不存在路径），`proc_exec` 返回 `-ENOENT`，`[DYN] NEG PASS`；QEMU 两架构 fail-closed 断言均通过。
- 已施工（`d6505490`）：`elfld` 全链退役；`ci/build.sh all` 6/0、host-tests 与 `make test-kernel-host` 全绿、UEFI 全判据通过（`978e2b2f` 修 no-op clear 误红）。
- 已完成：x86_64 与 aarch64 上游 `ld-musl` 真跑（外部程序输出 + `[DYN] PASS`）；缺解释器路径返回 `ENOENT`（`[DYN] NEG PASS`）；`elfld` 改写链与旧工具退役；`ci/audit.sh full` 全绿（含双架构 QEMU/UEFI）；T7-c 动态腿转绿。
- 剩余（D6 扩展，不阻塞本阶段出口）：相对 `PT_INTERP` 已由 `[DYN] FMT PASS` 覆盖（`ENOEXEC`），无 NUL/越界等形态由 `interp_path` 单测覆盖；musl 的 libc 与 `ld-musl` 同一对象，`libc 缺失` 不构成独立失败面（归 glibc 后置）；glibc/vDSO 后置。
- 可选加分：动态 `pthread_create`（TLS + mmap 栈）；`dlopen` 只做记录，不作第一波门槛。
- 出口（已达成）：T7-c 动态腿转绿；`elfld` 退役；文档/台账闭合。

### D7：glibc（可选后置，不阻塞 T7）

口径：T7 已于 2026-11-25 按方案一以双架构 musl 收口；本 D7 是 glibc/vDSO 扩展工程，T7 不再以 glibc 为阻塞项。以下工作按需推进，不影响 T7 的 `[X]` 状态。

glibc 与 musl 共用 D1–D4 的装载/`mmap`/auxv 基础，但多出四类差异：

| 差异 | 现状 | glibc 影响 |
|---|---|---|
| VDSO | 全树无，当前只能 `AT_SYSINFO_EHDR = 0` | glibc 多数时间函数有 syscall fallback；若启动期硬依赖 VDSO，则必须补最小 VDSO |
| `set_tid_address` | 已实现（G2）：`218 ↔ 96`，写 `clear_child_tid`，返回当前 TID | glibc 启动实际调用；入口不校验 `tidptr`，退出清理静默失败 |
| `prlimit64` | 已实现（G2）：`302 ↔ 261`，支持查询/设置 `[cur,max]` | glibc 启动用 `pid=0, RLIMIT_STACK, new=NULL` 查询；非 init 改其他 pid 返回 `EPERM` |
| `rseq` | 已实现（R1）：`334 ↔ aarch64 293`，注册/注销、`rseq_cs` 信号/退出清理；legacy 32B，CPU ID 返回用户态刷新 | glibc 启动真实注册；pthread 每线程注册由内核维护，G4 并发腿通过 |
| `clone3` | 有入口，但只提取 flags/stack/tid/tls，部分高级字段忽略 | glibc `pthread_create` 可能走 `clone3`；需确认 fallback 或补齐常用字段 |

工作分解：

- **G0 取证（x86_64 已完成 2026-11-25）**：glibc 动态 `true` + `ld-linux` + `libc.so.6` 根镜像已接入 initramfs；`/bin/glibc_true` 真实 `execve` 返回 0，串口打印 `[GLIBC] PASS: dynamic glibc execve`；aarch64 暂无载体，探针打印 SKIP。G0 收敛了 `newfstatat` 参数位移、Linux `struct stat` 布局、内核侧 `fstat` 跨用户边界、文件映射 VMA 拆分 offset、初始栈 16 字节对齐五处缺口。证据见 [d7-g0-glibc-true.md](../report/d7-g0-glibc-true.md)。
- **G1 最小启动（x86_64 已完成 2026-11-25）**：真实 glibc 动态 `hello` 已跑通，串口出现 `hello dynamic glibc` 与 `[GLIBC] HELLO PASS`；在 `AT_PLATFORM` 缺失、`AT_HWCAP*=0`、`AT_SYSINFO_EHDR=0` 现状下仍通过。结论是这三个 auxv 不是该腿根因，不预先补；证据见 [d7-g1-glibc-hello.md](../report/d7-g1-glibc-hello.md)。G2 复核后确认 glibc 启动实际调用了 `set_tid_address`/`rseq`/`prlimit64`，G1 当时返回 `ENOSYS` 但被 glibc 容错。
- **G2 启动 syscall（x86_64 已完成 2026-11-25）**：新增 `glibc_g2` 原始 syscall 探针，补 `set_tid_address` 与 `prlimit64`（含 canonical/aarch64 编号、dispatch、ABI 互逆测试、initramfs/init/QEMU 门禁）；`rseq` 不映射，保持 `-ENOSYS`。x86_64 串口 `SET_TID PASS`、`PRLIMIT PASS`、`RSEQ ENOSYS`、`[GLIBC] G2 PASS`，aarch64 仍 SKIP。宿主 GDB 证实 glibc 启动序列为 `set_tid_address → rseq → prlimit64(RLIMIT_STACK)`，并更正 G1 “未触发”的表述。证据见 [d7-g2-glibc-startup-syscalls.md](../report/d7-g2-glibc-startup-syscalls.md)。
  G2 留下的 5 项边界已登记在下方 `D7-DEFERRED` 表（G3/G4 另补 V1/T1/C1/L1 四项）；后续切片开工前逐项核对，不得静默跳过。
- **G3 时间路径/无 VDSO（x86_64 已完成 2026-11-25）**：`AT_SYSINFO_EHDR=0` 下新增 `glibc_time` 探针，`clock_gettime`/`gettimeofday`/`time`/`clock_getres`/`getcpu` 全部走 syscall fallback；串口 `[TIME] PASS`、`[GLIBC] TIME PASS: no-vdso fallback`。结论是当前单线程时间面**不需要最小 VDSO**，不实现 VDSO。首轮同时暴露并修复两个 ABI 缺口：`time_syscall` 返回毫秒 tick（应为墙钟秒），`sys_getcpu` 返回打包值而不写用户 `cpu*/node*` 指针（改为 Linux `getcpu` 语义）。证据见 [d7-g3-glibc-time-vdso.md](../report/d7-g3-glibc-time-vdso.md)。
- **G4 pthread/clone3/TLS（x86_64 已完成 2026-11-25）**：新增 `glibc_pthread` 探针，真实 glibc `pthread_create`/TLS/`pthread_join`/`pthread_mutex` 全部通过，串口 `[PTHREAD] PASS`、`[GLIBC] PTHREAD PASS: glibc pthread`。首轮定位并修复 `clone3` 栈参数语义：`struct clone_args.stack` 是栈底、`stack_size` 是大小，内核原先直接把栈底当初始 RSP；补 `clone3_stack_top()` 换算为栈顶并补零长度/溢出测试。调试期串口确认 glibc 实际走 `clone3`（`flags=0x3D0F00`、`stack_size=0x7FFF80`、`ret=Ok(35)`）。`rseq` 仍保持 `-ENOSYS`，pthread 腿证明 glibc 当前可 fallback。证据见 [d7-g4-glibc-pthread.md](../report/d7-g4-glibc-pthread.md)。
- **G4 dlopen（x86_64 已完成 2026-11-26）**：新增 `glibc_dlopen` 与独立 `libdlprobe.so`，`RTLD_NOW|RTLD_LOCAL` 装载第二共享对象，`dlsym` 解析 `probe_add`/`probe_get`，首次调用 `12`，缺失符号 `NULL`，`dlclose` 后重新 `dlopen` 使 `probe_counter` 回到 `5`；串口 `[DLOPEN] PASS`、`[GLIBC] DLOPEN PASS: glibc dlopen`，G0–G4 全绿。首轮 `malloc(): corrupted top size` 的根因不是 `pread64`，而是 `MmStruct::find_free_range` 将 mmap 游标回退到低位 VMA 末尾、让 glibc 把 `libc.so.6` 映射到 brk 堆区，以及 `set_brk` 把页对齐值写回 `mm->brk` 破坏 glibc top chunk 记账；两处均已根治并补回归单测。证据见 [d7-l1-glibc-dlopen.md](../report/d7-l1-glibc-dlopen.md)。
- **V1 最小 VDSO 触发条件评估（2026-11-26 收口：不实现）**：在 `AT_SYSINFO_EHDR=0` 下复核 x86_64 与 aarch64 真实 glibc：`clock_gettime`/`gettimeofday`/`time`/`getcpu`/`clock_getres` 全部 syscall fallback 且语义一致，`[GLIBC] TIME PASS: no-vdso fallback`；G0/G1/G2/G4/C1/L1 与 pthread/clone3/dlopen 均无 VDSO 硬依赖。按触发条件表逐项核对，五项均未触发；最小 VDSO 需新增两架构 ELF/符号/版本/映射/生命周期与用户可执行页回归面，当前无消费者，故不实现。证据见 [d7-v1-vdso-evaluation.md](../report/d7-v1-vdso-evaluation.md)。
- **A1 aarch64 glibc 运行时载体（2026-11-26 收口）**：以 Debian 13 arm64 glibc 2.41 的 `ld-linux-aarch64.so.1` + `libc.so.6` 与七个真实动态探针组成 `scripts/fixtures/dynamic-glibc-aarch64/`，Makefile/initramfs/init/QEMU 门禁全部按架构泛化。首轮暴露 EL0 取指翻译缺页被归 `Other`（`EC=0x20/IFSC=1`），放行后 G0/G1/G2/L1 转绿；随后修复 aarch64 glibc `time()` 走 `CLOCK_REALTIME_COARSE=5` 的时钟 ID 缺口，以及 glibc 主线程直接写 `TPIDR_EL0`、内核 `tls_base` 滞后导致切回时被 0 覆盖的 TLS 恢复缺陷（syscall 入口/调度切走点回收真实值）。aarch64 QEMU `[GLIBC] G2/TIME/PTHREAD/CLONE3/DLOPEN PASS` 全绿，x86_64 回归全绿；`bash ci/audit.sh full` RC=0（17/17 + 11/11、QEMU 2/2、thread-stress 6/6、Lockbud 4/4、未执行 0）。证据见 [d7-a1-glibc-aarch64.md](../report/d7-a1-glibc-aarch64.md)。
- **T1 真实 RTC/墙钟 epoch（2026-11-26 收口）**：x86_64 CMOS RTC 与 aarch64 PL031 接入启动路径，RTC epoch 注入 `TimeSync.epoch_base_ns`，`CLOCK_REALTIME` / `gettimeofday` / `time()` 返回真实日期；`settimeofday` 与 glibc 实际使用的 `clock_settime` 同源（canonical 227 ↔ aarch64 112），成功后尽力回写 RTC。QEMU x86_64 `source=rtc`、aarch64 `source=hint`（`edgine.rtc_epoch` 提示），连续启动 epoch 随宿主推进；`[TIME] EPOCH PASS` / `[TIME] SET PASS` 双绿；`bash ci/audit.sh full` RC=0（17/17 + 11/11、QEMU 2/2、Lockbud 4/4、未执行 0）。证据见 [d7-t1-rtc-epoch.md](../report/d7-t1-rtc-epoch.md)。

#### D7 延后项登记（`D7-DEFERRED`）

> 本节是 D7 G2/G3/G4 留下的**未收口边界登记**，不是已完成清单。pthread/rseq、rlimit 收敛、aarch64 glibc 载体或真实 RTC/VDSO 切片开工前，必须逐项核对本表；处理一项时把状态改为 `[X]`，并在提交/报告中回填验收证据。**禁止只删除本表行而不留收口记录**，否则这些边界会随任务推进被遗忘。
>
> 检索锚点：`D7-DEFERRED`。状态口径：`[ ]` 延后/未开工，`[~]` 进行中，`[X]` 已收口并回填证据。本轮登记 9 项（G2 五项 + G3 两项 + G4 两项），均不计入 T7 的 `7 / 0`；C1、L1、S1、G1、P1、R1、T1、A1、V1 收口后当前剩余 0 项。

| ID | 未收口边界 | 状态 | 当前口径 | 解除触发 | 验收证据 |
|---|---|---|---|---|---|
| `D7-DEFERRED-P1` | `prlimit64` 跨进程权限 | `[X]` | 2026-11-26 收口：新增 SYSTEM 域 `SYSTEM_CAP_SET_RLIMIT = 1 << 10`；`prlimit64` 改他 pid 要求该能力（bootstrap PWM 0 恒有），`setrlimit`/`prlimit64` 的 `RlimitTable::set(is_privileged)` 统一用同一能力判定，不再硬编码 `pid == 1`；无能力仍可查询自身/降低限制，提高 hard limit 或跨 pid 写返回 `EPERM` | 后续若把 rlimit 拆成独立 capability 命名空间，或引入 PID namespace 目标翻译时另开工 | 内核单测 `test_prlimit64_cross_process_requires_rlimit_capability`：非 bootstrap 无能力→他 pid `EPERM`，自身/`pid=0` 查询通过，bootstrap 他 pid 通过；`./ci/build.sh all` 7/0、`make test-kernel-host` 3 套 1472 passed、`bash ci/audit.sh full` RC=0（QEMU 2/2）；证据见 `docs/report/d7-p1-prlimit-capability.md` |
| `D7-DEFERRED-R1` | `rseq` 注册/退出语义 | `[X]` | 2026-11-26 收口：`SYS_RSEQ=334` 接入 dispatch，ABI 层 `334 ↔ aarch64 293` 双向映射；`Process` 增 `rseq_ptr/rseq_len/rseq_sig` 原子状态；注册接受 `len>=32`（glibc feature=20 时按 32 注册）、32B 对齐，已注册同参 `EBUSY`/异参 `EINVAL`/异签名 `EPERM`，注销校验后写 `cpu_id=0xFFFF_FFFF`；`do_signal_deliver` 返回用户态前刷新 CPU ID 并在有信号时清 `rseq_cs`，`process_exit` 清 `rseq_cs` 后摘注册，exec 直接摘注册。CPU ID 刷新走返回用户态入口（syscall/中断），不在切换前直写 next 用户页 | 后续若出现纯用户态长临界区跨 CPU 迁移的强语义需求，再补调度器切换点 CPU ID 写 | `[G2] RSEQ PASS`：glibc 自注册 `feature=20 reg_len=32 cpu=0/0`，重复注册 EBUSY、非法 flags/len EINVAL、错签名 EPERM、注销后 `cpu=0/0xFFFFFFFF`、重注册成功、信号 handler 见 `rseq_cs=0`；`[PTHREAD] RSEQ EXIT PASS`：worker 注销 glibc 区→注册自有区→置 `rseq_cs`→退出，join 后为 0；`[G2]`/`[PTHREAD]` 与 QEMU x86_64 串口门禁通过，aarch64 QEMU 1/1；ABI host 测试锁 `(334,293)`；证据见 `docs/report/d7-r1-rseq.md` |
| `D7-DEFERRED-S1` | `set_tid_address` 退出清理集成断言 | `[X]` | 2026-11-26 收口：`exit_cleanup` 改为先摘登记再写用户字，写路径走 `userptr::write_struct_to_user` 的区间/映射/逐页可写/SMAP 完整判据，不再 `check_user_ptr` 后裸 `write_volatile`；写失败跳过 futex 唤醒。新增用户库 `set_tid_address` wrapper 与 init `[TIDCLEAR]` 两腿：共享 `AtomicU32` 好指针写 0 并唤醒，fork 子进程登记只读 `.text` 坏指针后 `wait4` 正常退出 | 后续扩展 `CLONE_CHILD_CLEARTID` 与 `set_tid_address` 的组退出/live>1 语义时另开工；当前坏指针腿用独立子进程，未覆盖“线程组内坏指针 + 兄弟线程存活”组合 | `[TIDCLEAR] PASS`（x86_64 + aarch64）；`qemu_boot_test.sh` 双架构 RC=0；证据见 `docs/report/d7-s1-set-tid-address.md` |
| `D7-DEFERRED-G1` | `getrlimit` 与 `prlimit64` 口径统一 | `[X]` | 2026-11-26 收口：新增 `rlimit_for_process(pid, resource)` 单源 helper，`getrlimit` 读当前进程 `rlimit_table`，`prlimit64` 复用同一 helper；`getrlimit` 保留 `rlim_ptr==0 → EINVAL`，非法范围先 `validate_user_buf → EFAULT`，不再恒 `RLIM_INFINITY` | 后续把 `getrlimit` 错误码收敛到 Linux `EFAULT`、或扩展新资源号时另开工 | x86_64 glibc G2 逐项对比 16 个资源：`[G2] GETRLIMIT PASS`、`resources=16 consistent`；`make test-kernel-host` 819/640/12 全绿；证据见 `docs/report/d7-g1-rlimit-consistency.md` |
| `D7-DEFERRED-A1` | aarch64 glibc G2/G3/G4 运行时载体 | `[X]` | 2026-11-26 收口：Debian 13 arm64 glibc 2.41 的 loader/libc + 七个动态探针组成预编译夹具；Makefile/initramfs/init/QEMU 门禁按架构泛化；修复 EL0 取指翻译缺页、`CLOCK_REALTIME_COARSE=5` 时钟 ID、aarch64 `TPIDR_EL0` 内核字段滞后三处根因 | 后续更换 glibc 版本、扩展 `dlopen` 依赖链或出现 VDSO 硬依赖时另开工 | aarch64 `[GLIBC] PASS/HELLO PASS/G2 PASS/TIME PASS/PTHREAD PASS/CLONE3 PASS/DLOPEN PASS` 全绿；`[PTHREAD] RSEQ EXIT PASS`、`[CLONE3] PASS`；x86_64 回归全绿；`./ci/build.sh all` 7/0；`make test-kernel-host` 1481、`make test-host` 1676；`qemu_boot_test.sh` 双架构 1/1；`bash ci/audit.sh full` RC=0（17/17 + 11/11、QEMU 2/2、thread-stress 6/6、Lockbud 4/4、未执行 0）；证据见 `docs/report/d7-a1-glibc-aarch64.md` |
| `D7-DEFERRED-V1` | 最小 VDSO | `[X]` | 2026-11-26 评估收口（不实现）：`AT_SYSINFO_EHDR=0` 下 x86_64 与 aarch64 真实 glibc 的 `clock_gettime/gettimeofday/time/getcpu/clock_getres` 全部 syscall fallback 且语义一致，`[GLIBC] TIME PASS: no-vdso fallback`；G0/G1/G2/G4/C1/L1 均无 VDSO 硬依赖。按触发条件表五项均未触发，最小 VDSO 的 ELF/符号/版本/映射/生命周期与新增用户可执行页回归面无当前收益，故不实现 | glibc 升级后启动/时间路径硬失败且 fallback 不可规避；应用显式要求 `AT_SYSINFO_EHDR!=0`；性能门槛要求绕开 syscall；新架构/ABI 需要 VDSO | 双架构 `[TIME] PASS` + `[GLIBC] TIME PASS: no-vdso fallback`，`time` 与 `gettimeofday`/`CLOCK_REALTIME` 一致性为 0；触发条件逐项核对见 `docs/report/d7-v1-vdso-evaluation.md` |
| `D7-DEFERRED-T1` | 真实 RTC/墙钟 epoch | `[X]` | 2026-11-26 收口：x86_64 CMOS（UIP/BCD/12h/世纪）与 aarch64 PL031（DTB `arm,pl031`，QEMU 缺省 `0x0901_0000`）读取 epoch，注入 `TimeSync.epoch_base_ns`；`CLOCK_REALTIME`/`gettimeofday`/`time()` 同源返回真实日期，`CLOCK_MONOTONIC` 不受影响；`settimeofday` 与 glibc 转发的 `clock_settime` 统一走 `apply_wall_clock_ns`，成功后尽力回写 RTC；RTC 低于 2020 时可用 `/chosen` `edgine,rtc-epoch` 或 `bootargs edgine.rtc_epoch=` 提示 | 引入时区/本地时间语义、NTP/PTP 周期校准 RTC、或需要 `clock_adjtime` 联动时另开工 | x86_64 `[BOOT] RTC wall clock epoch: 1795682291s (UTC, source=rtc)`、`[TIME] EPOCH PASS`、`[TIME] SET PASS`（target=after，restore=0）；aarch64 `source=hint` 且 epoch≥2025；连续 x86 QEMU 启动 epoch 0845→1625→1910→2291；`qemu_boot_test.sh` 双架构 `rtc_epoch_check` 绿；`make test-kernel-host` 1480、`make test-host` 1676；`bash ci/audit.sh full` RC=0（17/17 + 11/11、QEMU 2/2、Lockbud 4/4、未执行 0，静态锁序 1327/871）；证据见 `docs/report/d7-t1-rtc-epoch.md` |
| `D7-DEFERRED-C1` | `clone3` 高级字段 | `[X]` | 2026-11-26 收口：`exit_signal` 落 PCB 并由组最后成员退出投递；`CLONE_PIDFD` 创建前校验写回地址、创建后 `pidfd_open`/`CLOEXEC`/写回；`set_tid`/`set_tid_size`/`cgroup` 显式 `ENOSYS`；`size>=64` 分级读字段；`exit_signal` 按 Linux `_NSIG=64` 校验；`wait4` 接受 `__WALL`；legacy clone 对 `CLONE_PIDFD`/`CLONE_INTO_CGROUP` 显式 `EINVAL` | 后续 cgroup/PID namespace 实装时替换 ENOSYS；pidfd 创建后失败保留子进程；通用 poll/ppoll timeout 不阻塞，pidfd 只保证零超时快照 | `[CLONE3] PASS` + `[GLIBC] CLONE3 PASS`；G0–G4 全绿；证据见 `docs/report/d7-c1-glibc-clone3.md` |
| `D7-DEFERRED-L1` | glibc `dlopen` 多共享对象 | `[X]` | 2026-11-26 收口：新增 `glibc_dlopen`/`libdlprobe.so`，覆盖第二 `.so` 装载、`dlsym`/`GLOB_DAT` 重定位、缺失符号、`dlclose` 与重新装载；同时根治 `find_free_range` 游标回退抢占 brk 与 `set_brk` 页对齐回写两个根因 | 扩展 libc 依赖链/`RTLD_GLOBAL`/构造析构/TLS 卸载语义时另开工 | `[DLOPEN] PASS` + `[GLIBC] DLOPEN PASS`；G0–G4 全绿；证据见 `docs/report/d7-l1-glibc-dlopen.md` |

估算：

- **最小 glibc 动态 hello（单线程、无 VDSO、无 dlopen）**：**8–14 人日**。
- **含 pthread / rseq / VDSO 的可用面**：**15–25 人日**。
- 只有确认 VDSO 可完全 fallback、rseq 不致命时，才取下限；否则按上限排。

## 4. 估算汇总

| 工作包 | 人日（低–高） | 生产代码 | 说明 |
|---|---:|---:|---|
| D0 冻结/取证 | 0.5–1 | 0 | 只读与 fixture |
| D1 `mm` M2–M5 | 8–12 | 600–1,000 | 动态链接硬前置 |
| D2 双映像 ELF | 5–7 | 350–550 | `PT_INTERP` + 两套 `PT_LOAD` |
| D3 栈/auxv | 3–5 | 250–400 | `AT_PHDR` 计算是易错点 |
| D4 mmap/VFS | 5–8 | 350–600 | `MAP_FIXED`、RELRO、文件映射 |
| D5 工具链/退役 | 2–3 | −226 + 150 | 删 `elfld`，加 fixture/根镜像 |
| D6 验收/CI | 4–6 | 400–700（测试） | 双架构 + 回归 |
| **musl 合计** | **27–41** | **1,500–2,500** | P50 ≈ 32 人日 |
| D7 glibc（可选） | 8–25 | 500–900 | 最小 hello 8–14；含 pthread/rseq/VDSO 15–25 |

- **P50**：约 **32 人日**（≈1.5 人月）；含评审/CI 排队与一次返工，按 **2 人月** 做预算更稳。
- **P90**：约 **41 人日**（≈2 人月）；若 `mm` M2 暴露预存 COW/文件映射缺陷，可能上探 **2.5 人月**。
- 只算“动态链接器本体”（D0、D2–D6，不含 D1）：**19–29 人日**。

## 5. 关键路径

```text
D1 mm M2–M5
  └─ D2 双映像 ELF
       └─ D3 auxv/栈
            └─ D6 动态 musl 验收
D4 mmap/VFS ── 与 D2/D3 并行，D6 前必须收口
D5 工具链/elfld 退役 ── 可提前，D6 前必须收口
D7 glibc ── D6 后可选
```

- **D1 是硬门**：不装 mm，`mmap/mprotect/brk` 全部不可用；动态链接器连 libc 都映射不了。
- **D2/D3 是同一提交面**：只装解释器不给 auxv，解释器会在启动时读空指针；只给 auxv 不装解释器，入口还是主程序。
- **D4 的 `MAP_FIXED` 是第二硬门**：musl 加载共享库通常先保留地址区间，再 `MAP_FIXED` 落段；当前 VMA 插入遇到不同 flags 会拒绝重叠。
- **D5 的退役必须晚于 D6 首次通过**：先证明真实解释器可用，再删 `elfld`；否则失去唯一动态路径。

## 6. 验收判据

- 动态 musl hello 在 x86_64/aarch64 均输出固定字符串并 `exit(0)`。
- `readelf -l` 显示 `PT_INTERP` 仍为 `/lib/ld-musl-*.so.1`，不是 `/usr/libexec/elfld.so`。
- 树中 `rg "elfld|EDGINE_INTERP|prepare_elf_image"` 零生产命中（历史文档除外）。
- 静态 musl 双架构腿不回归；`./ci/build.sh all` 6/6、`make test-host`、`make test-kernel-host`、`ci/audit.sh full`、QEMU/UEFI 全绿。
- 动态链接器是首个真实 `mmap` 消费者：必须有一条运行时断言“mmap 返回值 ≥ `user_va_lower_bound`”，防止回退返物理地址。
- 失败路径不 panic、不半切换：解释器缺失/损坏时旧进程映像保持不变。

## 7. 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| `mm` M2 暴露预存 COW/文件映射缺陷 | D1 上探，D6 连锁失败 | 先做 M4 的 QEMU 真跑腿；不要只靠 host 形状锁 |
| `MAP_FIXED` 语义不完整 | libc 映射失败、随机崩 | 先实现“拆除重叠再插入”；用 musl 真实加载序列验收 |
| `AT_PHDR` 计算错 | 解释器读错 phdr，启动即崩 | 从包含 `e_phoff` 的 `PT_LOAD` 反算，host 单测覆盖非零 `p_offset` |
| `AT_RANDOM` 缺失/不足 | 栈保护/ASLR 初始化失败 | `getrandom` fail-closed；16 字节写到栈并校验 |
| aarch64 TLS/入口差异 | x86 过、arm 崩 | 双架构同批验收；`TPIDR_EL0` 在 exec 后清零并重装 |
| glibc 依赖 VDSO/rseq | glibc 不能作为第一波 | 明确 musl 先行；glibc 单列 D7 |
| `ELF_MAX_SIZE` 1 MiB 限制 | 大 libc/解释器读不全 | 先按单文件核尺；超限改为按 `PT_LOAD` 分段读，不抬上限 |
| 退役过早 | 失去唯一动态路径 | D5 删除放在 D6 首次通过之后 |

## 8. `elfld` 退役清单（已完成，`d6505490`）

- [X] 删除 `tools/elfld/elfld.c`、`tools/elfld/build.sh`。
- [X] `tools/edgine-gcc` 去掉 `--dynamic-linker,/usr/libexec/elfld.so` 与 rpath 注入，改为纯 gcc 透明转发。
- [X] 删除 `src/kernel/privileged/proc/elf/mod.rs` 的 `EDGINE_INTERP`、`LINUX_INTERP_PREFIXES`、`needs_interp_rewrite`、`rewrite_interp_path`、`prepare_elf_image` 改写分支及其专项回绕测试。
- [X] `src/kernel/functions/proc/exec.rs` 与 `src/kernel/functions/init.rs` 无 `prepare_elf_image` 调用点；host 形状锁已钉住。
- [X] `scripts/requirements.sh`/Makefile/CI 无 `elfld` 构建引用；根镜像不注入 `/usr/libexec/elfld.so`。
- [X] 保留静态 ELF 路径；静态二进制无 `PT_INTERP`，不依赖 `elfld`。
