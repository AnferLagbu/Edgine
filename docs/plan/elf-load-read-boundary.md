# ELF 装载读入按可加载段定界（独立工程）

> 一句话说清为什么有这个计划: 当前 `execve` 与启动装载都按 `ELF_MAX_SIZE = 1 MiB` **整文件**读入，
> 文件里只要带上调试段/符号表就可能先撞上限，而真正需要进内核的只有 ELF 头、程序头表和
> `PT_LOAD` / `PT_INTERP` 覆盖的字节。本工程把「文件大小 ≤ 1 MiB」改成「**可加载前缀 ≤ 1 MiB**」，
> 并让 wasm 选桩只读 4 字节魔数，不抬任何上限。
>
> **来源**: [mm-lifecycle-project.md](./mm-lifecycle-project.md) 的「附: ELF 整文件读上限的余量」；
> 2026-11-27 用户裁定单开。该附项记录的实测余量（aarch64 stripped `wasmrun.bin` 仅余 ~249 KiB）
> 是长期约束，不是一次性问题。
>
> **关系**: mm M0–M5 已收口，本工程只动「装载前的文件读入边界」，不改 `MmStruct` 生命周期、
> 不改页表/COW 语义、不新增 ELF 装载器实现。

---

## 1. 立项依据

### 1.1 现状: 四个读入点都按整文件上限

- **常量**: [exec.rs:26](../../src/kernel/functions/proc/exec.rs#L26)
  `pub(crate) const ELF_MAX_SIZE: usize = 1024 * 1024;`
- **execve 主映像**: [exec.rs:112](../../src/kernel/functions/proc/exec.rs#L112)
  `read_file_to_vec(path, current_pwm, ELF_MAX_SIZE)`，读出整个文件后才判 wasm 魔数 / 交给装载器。
- **wasm 运行时桩**: [exec.rs:123](../../src/kernel/functions/proc/exec.rs#L123)
  `read_file_kernel(WASM_RUNTIME_PATH, 0, ELF_MAX_SIZE)`，同样整文件读入。
- **PT_INTERP 解释器**: [exec.rs:137](../../src/kernel/functions/proc/exec.rs#L137)
  `read_file_kernel(path, current_pwm, ELF_MAX_SIZE)`。
- **启动装载**: [init.rs:82](../../src/kernel/functions/init.rs#L82) / [:91](../../src/kernel/functions/init.rs#L91)
  分别整文件读 `/init` 与其 `PT_INTERP`；[:153](../../src/kernel/functions/init.rs#L153) 的
  `/disk/init` 回读也以同一常量作整文件上限（只用于完整性判读，不进装载器）。

### 1.2 两个读函数把「上限」实现成「整文件大小上限」

- [fs/api.rs:93](../../src/kernel/functions/fs/api.rs#L93) `read_file_to_vec`（syscall 路径）:
  `vfs_stat` 得 `st.size`，`size > max_size` 直接 `None`；随后 `vec![0u8; size]` 整文件分配。
- [fs/api.rs:129](../../src/kernel/functions/fs/api.rs#L129) `read_file_kernel`（启动期无 fd 路径）:
  同样 `size > max_size` 直接 `None`，再 `vec![0u8; size]` 后一次 `inode.read(0, ...)` 读满。

⇒ 即使可加载段只有 700 KiB，只要文件因 `.symtab` / `.debug_*` / 非装载 section 超过 1 MiB，
装载前置就被判失败；而内核实际从未消费这些字节。

### 1.3 为什么现在必须单开

- **余量读数**（[mm-lifecycle-project.md](./mm-lifecycle-project.md) 附项，D4-d `--strip-all` 后）:
  `wasmrun.bin` x86_64 **692,400 B** / aarch64 **793,640 B**，距 1 MiB 只剩
  **356,176 B (~348 KiB)** / **254,936 B (~249 KiB)**；未 strip 时分别 **1,209,424 B** /
  **1,427,776 B**，**双双超限**。
- wasm P2 的全量 WASI（38 函数）只会让用户态运行时桩继续变大，余量是**长期**约束；
  下一次加功能就可能再次撞上限，且症状是「文件读不出」而不是「段装不下」，归因很远。
- 抬上限会把「一次分配整个文件」的内存放大面继续扩大（1 MiB → 2 MiB 只是把问题推迟）；
  正确方向是让**读入量**与实际装载需求对齐。

### 1.4 装载器实际只需要前缀

- [verify.rs:95](../../src/kernel/privileged/proc/elf/verify.rs#L95) `verify_elf` 只要求
  `e_phoff + e_phnum * e_phentsize <= elf_size`（[:134](../../src/kernel/privileged/proc/elf/verify.rs#L134)），
  并校验魔数/class/machine/phentsize/phnum ≤ 128。
- [mod.rs:258](../../src/kernel/privileged/proc/elf/mod.rs#L258) `elf_load_segment` 只从
  `image[p_offset .. p_offset + p_filesz]` 拷贝 `PT_LOAD` 的文件字节；`p_memsz > p_filesz`
  的 BSS 部分由装载器清零，不依赖文件尾。
- [mod.rs:431](../../src/kernel/privileged/proc/elf/mod.rs#L431) `interp_path` 只读
  `PT_INTERP` 的 `p_offset + p_filesz` 区间。
- [user_proc.rs:2248](../../src/kernel/privileged/proc/user_proc.rs#L2248)
  `load_elf_from_memory_with_interp` 接收连续 `&[u8]`；只要缓冲区覆盖上述前缀，装载行为不变。

⇒ **最小正确读入量 = max(程序头表末尾, 所有 PT_LOAD 文件区间末尾, PT_INTERP 文件区间末尾)**；
非装载 section 可以根本不读。

## 2. 设计立场

1. **1 MiB 保留为「可加载前缀预算」**，不再解释为「文件大小上限」。文件名/常量名暂保留
   `ELF_MAX_SIZE`（避免无意义改名扩散），注释改为「可加载前缀字节预算」；不新增第二个上限。
2. **两段式读入**:
   - 第一段读「头部探测窗口」（命名常量，建议 `ELF_HEAD_PROBE_MAX = 128 KiB`），足够覆盖
     `e_phoff + 128 * sizeof(Elf64Phdr)`；用它解析 ELF 头与程序头表；
   - 由 `PT_LOAD` / `PT_INTERP` / phdr 表算出 `loadable_prefix_len`；若 > 1 MiB 或 phdr 表
     不在探测窗口内 ⇒ **ENOEXEC 失败**（fail-closed，不退回整文件读）；
   - 第二段只读 `loadable_prefix_len` 字节并交给现有装载器。
3. **wasm 选桩只读 4 字节魔数**: `execve` 先读文件前 4 字节判 `\0asm`；命中后只读
   `/bin/wasmrun` 的可加载前缀，**不再整文件读 wasm 模块**。模块本体由用户态 wasmi 按
   `argv[0]` 自己经 VFS 读，内核无需持有。
4. **不改装载器接口**: 仍传连续 `&[u8]` 前缀，不改 `elf_load_segment` / `interp_path` /
   `load_elf_from_memory_with_interp` 的签名；本工程只改「读多少、读到哪」。
5. **边界**:
   - 非装载 section（`.symtab` / `.debug_*` / 注释等）**不读入**，这是本工程的目标而不是遗漏；
   - 仍要求 `PT_LOAD` 文件区间连续可寻址；`p_offset + p_filesz` 超过实际文件长度 ⇒ 失败；
   - 不支持从磁盘按页懒加载 ELF（属更后面的按需分页工程，不在本册）。

### 对标摘要（2026-11-27）

源码级对照与行号见 [elf-load-read-survey.md](../report/elf-load-read-survey.md)。四条直接结论:

- **没有内核把整个 ELF 读进常驻缓冲**: Linux 只读 256 B 头部 + ≤64 KiB phdr 表，
  `PT_LOAD` 用 `vm_mmap` 映射文件页；FreeBSD 只映射首部一页 + 按需读 phdr 表 +
  `vm_object` 映射段；xv6 按「ELF 头 → 逐个 phdr → 按段按页」读；Redox 把 `execve`
  放在用户态，按头 / phdr / 段分别 `pread`。Edgine 的连续前缀读是这条有界读路线的折中，
  不是异类。
- **短读一律失败**: Linux `elf_read()` 短读返回 `-EIO`，Redox `pread_all()` 读到 0 字节
  `ENOEXEC`，xv6 每次 `readi` 必须读满。故 E-2 的「第二段读回长度必须等于计算值」不是
  额外保守，而是对标后的最低要求。
- **畸形段不跳过**: FreeBSD / Redox 对 `p_filesz > p_memsz` 或文件区间越界直接
  `ENOEXEC`，Linux 至少以 `-EINVAL` / `-ENOMEM` 失败；三家都不 `continue`。
  Edgine 当前 `elf_load_segment` 对前者 `continue`、对后者 `Ok(0)` `continue`
  （[mod.rs:221](../../src/kernel/privileged/proc/elf/mod.rs#L221)、
  [:284](../../src/kernel/privileged/proc/elf/mod.rs#L284)），须在 `loadable_prefix_len`
  阶段拒绝，不能把畸形段当空段。
- **1 MiB 前缀预算合理**: Linux 的 phdr 表硬上限是 64 KiB，Redox 的 header + phdr 表
  上限是 1 MiB；显式上限是常态，不因对标抬到 2 MiB / 4 MiB。

## 3. 施工条目

### E-0 立项与读入面盘点

- **描述**: 核清 `ELF_MAX_SIZE` 的全部消费点、读函数的失败语义、装载器对缓冲区的最小需求，
  形成本册 §1/§2 的源码读数。
- **方案**: 只读调研，不改代码；把「整文件上限」与「可加载前缀」的差异钉进本册。
- **状态**: [X]（2026-11-27 立项）
- **详情**: 消费点 = `exec.rs` 3 处（主映像 / wasm 运行时 / PT_INTERP）+ `init.rs` 3 处
  （`/init` / 其 PT_INTERP / `/disk/init` 回读）；读函数 = `read_file_to_vec` /
  `read_file_kernel` 两形态，均在 `size > max_size` 时直接失败且整文件分配；装载器最小需求 =
  phdr 表 + 全部 `PT_LOAD` 文件区间 + `PT_INTERP` 文件区间，`verify_elf` 的边界判据与
  `elf_load_segment` 的拷贝面见 §1.4 行号。用户 2026-11-27 裁定附项单开，本册承接，
  mm 册附项只保留余量读数。同日追加其他内核对标：Linux / FreeBSD / xv6 / Redox 均不整文件读，
  证据、差异与本册的可执行修订点见 [elf-load-read-survey.md](../report/elf-load-read-survey.md)
  与 §2「对标摘要」。

### E-1 有界前缀读 API（functions/fs）

- **描述**: 在 `functions/fs/api.rs` 增加「最多读前 N 字节」的安全 API，供 ELF/wasm 读入面使用；
  保留现有整文件 API 给真正需要全文的调用者（如 `/disk/init` 回读）。
- **方案**:
  - 新增 `read_file_prefix_to_vec(path: *const u8, pwm: u64, max_size: usize)`（syscall 路径）:
    `vfs_stat` 后取 `read_len = min(size, max_size)`；`read_len == 0` 返回 `None`；
    分配 `read_len`，`vfs_read_safe` 后按实际 `n` 截断；**不因 `size > max_size` 失败**。
  - 新增 `read_file_prefix_kernel(path: &str, pwm: u64, max_size: usize)`（启动期无 fd 路径）:
    与 `read_file_kernel` 同源，但 `read_len = min(size, max_size)`，只 `inode.read(0, &mut buf)`。
  - 两个函数沿用现有权限判定（`pwm` 传给 stat/open/read），错误一律 `None`；不新增 `unsafe`。
- **状态**: []
- **详情**: 待施工；落地后补 host/kernel 单测（`max_size` 小于文件、等于文件、文件为空、
  权限拒绝、短读截断五臂），并确认没有新增 `#[allow]` / `#[expect]`。

### E-2 可加载前缀定界 + exec/init 接线

- **描述**: 在 privileged 层提供「由 ELF 头/program header 计算最小读入前缀」的安全函数，
  并在进入装载器前拒绝 `p_filesz > p_memsz` / 文件区间越界等畸形段，把 `execve`、启动装载
  与其解释器读取切到 E-1 的两段式前缀读。
- **方案**:
  - 新增 `privileged::proc::elf::loadable_prefix_len(image: &[u8], limit: usize)`（或同层等价 API）:
    复用 `verify_elf` 的头/phdr 校验；遍历 phdr，累计 `e_phoff + phnum*phentsize`、
    每个 `PT_LOAD` 的 `p_offset + p_filesz`、`PT_INTERP` 的 `p_offset + p_filesz`；
    任一 `checked_add` 溢出、phdr 表不在 `image` 内、结果 `> limit` ⇒ 失败。
    同时校验 `p_filesz <= p_memsz`；违反即失败（当前 `mod.rs:221` 是 `continue`，
    不能把畸形段当空段；FreeBSD / Redox 直接 `ENOEXEC`，Linux 至少以
    `-EINVAL` / `-ENOMEM` 失败，三家都不跳过）。
  - `functions/proc/exec.rs`: 第一段读 `ELF_HEAD_PROBE_MAX`（命名常量）→ 算前缀 →
    第二段按前缀读主映像；`PT_INTERP` 用同型两段读。`interp_path` 与装载器都只消费
    第二段的完整前缀（探测窗口只用于算长度，不拿它解析 `PT_INTERP` 内容）。
  - 第二段读回长度必须**等于** `loadable_prefix_len`；短读即 ENOEXEC。否则
    `elf_load_segment` 对 `p_offset + p_filesz > elf_size` 的段会静默 `Ok(0)` 跳过，
    把「文件被截短」伪装成「段为空」。对标确认这是最低要求：Linux `elf_read` 短读 `-EIO`，
    Redox `pread_all` 读到 0 字节 `ENOEXEC`，xv6 每次 `readi` 必须读满。
  - `functions/init.rs`: `/init` 与其 `PT_INTERP` 走同一 helper；`/disk/init` 回读保留整文件
    语义（只判完整性，不进装载器），不强行改造。
  - 常量注释改为「可加载前缀预算」，不新增「文件大小上限」。
- **状态**: []
- **详情**: 待施工；负控制至少四条 —（a）phdr 表越出探测窗口 ⇒ ENOEXEC；
  （b）合成 ELF 的 `PT_LOAD` 尾部超过 1 MiB ⇒ 失败；（c）`p_filesz > p_memsz` ⇒ ENOEXEC；
  （d）按计算前缀截断文件使第二段短读 ⇒ ENOEXEC。正控制 — 合成 ELF 文件尾带
  >1 MiB 的非装载段、可加载前缀 < 1 MiB ⇒ 成功装载。

### E-3 wasm 选桩 4 字节魔数探测

- **描述**: `execve` 不再为判 wasm 魔数读整个模块文件；只读前 4 字节，命中后按 E-2 读
  `/bin/wasmrun` 的 ELF 前缀。
- **方案**:
  - 主映像读入顺序改为: `read_file_prefix_to_vec(path, pwm, WASM_MAGIC_LEN)`（4 字节）→
    `is_wasm_image`；命中 ⇒ 读 wasm 运行时前缀、跳过模块本体；未命中 ⇒ 从 ELF 头开始
    走 E-2 两段读。
  - wasm 模块大小不再受 `ELF_MAX_SIZE` 约束；内核不持有模块内容（现状本就不持有，
    只是先前为了判魔数整文件读了一遍）。
  - 保持 `WASM_RUNTIME_PATH` 与 `argv[0]` 传模块路径的既有语义不变。
- **状态**: []
- **详情**: 待施工；验收须覆盖「wasm 模块 > 1 MiB 仍能 exec 到 `/bin/wasmrun`」与
  「非 wasm 文件不会被误判」两侧。

### E-4 回归、门禁与文档

- **描述**: 把「整文件读」形态钉成不可回退的契约，并用未 strip 的运行时桩做真跑验收。
- **方案**:
  - host 契约锁: `exec.rs` / `init.rs` 的 ELF 主映像、解释器、wasm 运行时读取不得再以
    `ELF_MAX_SIZE` 调 `read_file_to_vec` / `read_file_kernel` 整文件读；wasm 魔数探测必须是
    4 字节前缀读；
  - kernel 单测: `loadable_prefix_len` 的 phdr/PT_LOAD/PT_INTERP/溢出/超预算/探测窗口
    六臂，另加 `p_filesz > p_memsz` 一臂；exec 接线测试另加「第二段短读」一臂，共八臂；
  - QEMU 真跑: 用**未 strip** 的 `wasmrun.bin`（>1 MiB，可加载前缀 < 1 MiB）跑既有 wasmi
    腿，判 `hello from wasmi` + `[WASMI] exit=0`；负控制把 `PT_LOAD` 人为扩到 >1 MiB ⇒ 拒绝；
  - 七门槛复跑；若需要新增 explain，落 `docs/explain/explain-elf-loading.md` 并在
    `mm-lifecycle-project.md` 附项/本册互链。
- **状态**: []
- **详情**: 待施工；门禁读数与负控制样本在收口时回填本册，不写占位断言。

## 4. 验收判据

- **功能**: 双架构 wasmi 腿（含未 strip 运行时桩）`hello from wasmi` + `[WASMI] exit=0`；
  ELF 静态/动态装载、既有 init 载体腿无回归。
- **安全**: 任何读入都不得超过 `ELF_MAX_SIZE` 前缀预算；畸形 phdr / `p_filesz > p_memsz` /
  段越界 / 短读 / 溢出一律 fail-closed；不因读入边界放宽而把非装载字节带入内核缓冲。
- **形态**: `execve` / `init` 的 ELF 读取走前缀 API；wasm 魔数只读 4 字节；无新增
  `#[allow]` / `#[expect]`，F9 基线不动。
- **底线**: §2.3 七门槛全绿；host / kernel-host / QEMU / initramfs / wasmi / rustdoc 全量复跑。

## 5. 边界（不做什么）

- **不抬上限**: 1 MiB 仍是可加载前缀预算；靠「少读非装载段」解决，不靠 2 MiB/4 MiB。
- **不改装载器**: 不把 `&[u8]` 改成 inode/callback，不做按段稀疏读；畸形段在
  `loadable_prefix_len` 阶段前置拒绝，不改 `elf_load_segment` 的签名与拷贝路径。
  该形态属后续按需分页工程。
- **固定探测窗口**: 128 KiB 是当前取舍，phdr 表位于窗口之后的合法 ELF 会被拒绝；
  未来若需支持任意 `e_phoff`，改成「ELF 头 → `e_phoff` 处 phdr 表 → 前缀」三段读是
  Linux / xv6 的形态，不在本册扩大 E-1 API。
- **不改 mm / exec 生命周期**: `MmStruct` 计数、页表转移、argv/envp 落盘均不在本册射程。
- **不处理 `/disk/init` 回读**: 它是完整性判读，不是装载读入；保留整文件语义。
