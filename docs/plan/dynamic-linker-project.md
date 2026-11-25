# Linux 动态装载工程（上游 ld-musl / ld-linux 先行）

> 状态：D1–D6 主体施工完成；x86_64 与 aarch64 上游 `ld-musl` 均已真跑通过（`[DYN] PASS`），缺解释器失败路径与 `elfld` 退役已落地；剩余扩展失败路径与可选 pthread/dlopen。目标不是“自研一个动态链接器”，而是让**上游 Linux 动态链接器**在 Edgine 上按 Linux 语义跑起来，并让自研 `elfld` 退役。T7 已于 2026-11-25 按方案一以双架构 musl 收口，glibc/vDSO 归本册 D7 可选工程，不阻塞 T7。D7 G0 已在 x86_64 真实 glibc `true` 上通过，G1 的带 stdout `hello` 也已通过（aarch64 暂无载体，SKIP）；G2/G3 待开。

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
- `set_tid_address`/`prlimit64`：G1 未触发；G2 先用真实调用探针取证，再补入口。
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
| `set_tid_address` | 无 syscall；但 `clear_child_tid` 字段与退出清理已存在 | glibc 启动/线程清理可能需要；可复用现有字段，成本较低 |
| `prlimit64` | 无；`getrlimit/setrlimit` 已有 | glibc 可能直接调 `prlimit64`；缺省时看 fallback，必要时补 |
| `rseq` | 无 | glibc 2.35+ 可能注册 rseq；若 ENOSYS 不致命则可后置，若致命则要补注册/退出语义 |
| `clone3` | 有入口，但只提取 flags/stack/tid/tls，部分高级字段忽略 | glibc `pthread_create` 可能走 `clone3`；需确认 fallback 或补齐常用字段 |

工作分解：

- **G0 取证（x86_64 已完成 2026-11-25）**：glibc 动态 `true` + `ld-linux` + `libc.so.6` 根镜像已接入 initramfs；`/bin/glibc_true` 真实 `execve` 返回 0，串口打印 `[GLIBC] PASS: dynamic glibc execve`；aarch64 暂无载体，探针打印 SKIP。G0 收敛了 `newfstatat` 参数位移、Linux `struct stat` 布局、内核侧 `fstat` 跨用户边界、文件映射 VMA 拆分 offset、初始栈 16 字节对齐五处缺口。证据见 [d7-g0-glibc-true.md](../report/d7-g0-glibc-true.md)。
- **G1 最小启动（x86_64 已完成 2026-11-25）**：真实 glibc 动态 `hello` 已跑通，串口出现 `hello dynamic glibc` 与 `[GLIBC] HELLO PASS`；在 `AT_PLATFORM` 缺失、`AT_HWCAP*=0`、`AT_SYSINFO_EHDR=0` 现状下仍通过。结论是这三个 auxv 不是该腿根因，不预先补；证据见 [d7-g1-glibc-hello.md](../report/d7-g1-glibc-hello.md)。剩余 2–4 人日原本用于 auxv，改由 G2/G4 按真实调用再花。
- **G2 启动 syscall（先取证）**：G1 的 hello 没有触发 `set_tid_address`/`prlimit64`/`rseq`，所以不先加空桩；用一个会真实调用它们的单线程探针确认 `ENOSYS` 后再补。若届时 pthread 提前进入，再把 `clone3` 常用字段与 rseq 并入。原估 3–7 人日，按真实调用面重估。
- **G3 VDSO**：仅当 `AT_SYSINFO_EHDR=0` 失败时做；最小 `clock_gettime/gettimeofday/time/getcpu` + ELF 映射 + 符号解析，5–8 人日。
- **G4 线程/加载器**：`clone3` 常用字段补齐 2–4 人日；glibc `pthread_create`/TLS/`dlopen` 记录 2–4 人日。

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
