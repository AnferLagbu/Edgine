# 进程地址空间描述符 (mm) 生命周期缺口（独立工程）

> 一句话说清为什么有这个计划: 生产 ELF 装载路径**从不安装 `MmStruct`**, 于是每个用户进程
> 的 `Process::mm` 恒为空; `mmap` 的「无 mm 回退」把**物理地址**当映射地址直接返给用户态,
> 首次写入即翻译故障. 这不是 wasm 专属 bug —— 它是「用户态动态内存」这条链的系统性缺口,
> 只是 [wasm-userspace-migration.md](./wasm-userspace-migration.md) P1 的 wasmi 运行时是
> **第一个真行使 `mmap` 的用户程序**, 才把它暴露出来. 本册由
> [userspace-decoupling.md](./userspace-decoupling.md) D4-d 施工中发现并归因, 用户裁定
> **单独立项**, 解耦批先收口; P1 的 wasmi 端到端腿在本工程落地前暂停.

---

## 背景与根因

### 已验证的因果链 (源码读数)

1. **装载面不建 mm**: [user_proc.rs:1907](../../src/kernel/privileged/proc/user_proc.rs#L1907)
   `load_elf_from_memory` 与 [proc_ops.rs:679](../../src/kernel/privileged/proc/proc_ops.rs#L679)
   `proc_exec_replace` 都直接布置段/栈并切页表, **不创建也不安装 `MmStruct`**.
   `Process::mm` 字段本身存在 ([process.rs:185](../../src/kernel/privileged/proc/process.rs#L185)
   `pub mm: AtomicPtr<MmStruct>`), 但生产路径无人写它.
2. **fork 继承空值**: [proc_ops.rs:1149-1151](../../src/kernel/privileged/proc/proc_ops.rs#L1149-L1151)
   按指针继承父进程 mm —— 父为空则子恒为空, 缺口沿进程树传播.
3. **带 mm 的装载器已存在但零生产调用者**:
   [elf/mod.rs:156](../../src/kernel/privileged/proc/elf/mod.rs#L156) `elf_load(mm, …)` 在
   [elf/mod.rs:247](../../src/kernel/privileged/proc/elf/mod.rs#L247) 调
   `vma_set_current_mm`; 其 functions 侧包装
   [functions/proc/elf.rs:166](../../src/kernel/functions/proc/elf.rs#L166) `load` 全仓
   **零生产调用点** (只有 host 侧契约锁引用). 即「正确的装载器写好了, 生产走的是另一条」.
4. **mmap 回退返物理地址**: [functions/mm/mmap.rs:351-353](../../src/kernel/functions/mm/mmap.rs#L351-L353)
   无 mm 时 `return ptr as i64`; ptr 来自
   [syscall/api.rs:166-173](../../src/kernel/privileged/syscall/api.rs#L166-L173)
   `mmap_get_mm_or_alloc` → [syscall/mod.rs:284](../../src/kernel/privileged/syscall/mod.rs#L284)
   `raw::alloc_pages` → [mm/api.rs:119-123](../../src/kernel/privileged/mm/api.rs#L119-L123)
   `pmm_alloc_pages`, 后者 `PhysAddr.0 as *mut u8` —— **物理地址**, 既未插 VMA 也未建用户映射.
5. **按需分页因此无从生效**: [page_fault.rs:246](../../src/kernel/privileged/mm/page_fault.rs#L246)
   `handle_user_page_fault` 靠 `get_current_mm`
   ([vma.rs:1377](../../src/kernel/privileged/mm/vma.rs#L1377)) 找 VMA 才能补页; mm 为空 ⇒
   匿名映射的缺页没有可查的描述符, 故障只能走终止面.
6. **首个真实消费者**: [heap.rs:197](../../src/user/lib/src/heap.rs#L197) `backend_map` →
   `sys::mmap(PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS)`; wasmi 解释器的线性内存与
   实例表都要堆分配, 故 P1 的 `wasmrun` 一跑就命中.

### 真跑取证 (wasmi 腿, 双架构)

`scripts/wasmi_boot_test.sh` (P1 资产, 尚未接门禁) 以 `KERNEL_FEATURES=wasm_userspace` 真构建真启动:

- **x86_64**: `[IDT] user exception: vec=14 err=0x6 rip=0x402622 rsp=0x7FFFFFFCEEC8 cr2=0x1F800000`
  (另一轮 `cr2=0x1F840000`), 子进程被终止 ⇒ `[WASMI] exit=5632` (= 22 << 8).
  `err=0x6` = 用户态写 + 页不存在; `cr2` 落在**物理内存区**而非用户 VA 区, 与第 4 跳一致.
- **aarch64**: `EL0 sync fault: pid=20 ESR=0x92000045 FAR=0x5FA00000 ELR=0x4025C0 -> terminate`
  (另一进程 `FAR=0x5FCA0000`), 同样 `[WASMI] exit=5632`. `ESR=0x92000045` = EL0 数据异常
  (翻译故障, WnR=1), `FAR` 同样是物理地址形态.

两架构的故障地址都**不是**用户 VA 布局里的地址, 而是 PMM 分出来的帧号 —— 这是「返回物理地址」
而非「映射没建好」的判别证据.

### 为什么此前没被发现

- 既有用户程序 (init / eash / proctest / exectest / wasmstub) 都不行使 `mmap`: 内核解释器桩
  的线性内存由内核侧布置, 用户态栈/段由装载器直接映射, 故 mm 为空一路无感.
- host 侧测不到: `copy_from_user` / `copy_to_user` 在 host ring-3 读 CR4 必 SIGSEGV
  (见 [syscall-followup.md](./syscall-followup.md) 的 CR4/SIGSEGV 缺口条), 用户内存搬运与
  真实页表语义只能由 QEMU 真跑腿覆盖; 而 P1 之前没有任何腿跑「会分配堆的用户程序」.

## 安全面 (I2 / I4)

缺口不只是功能 bug, 它同时触碰两条安全不变式:

- **I4 (用户内存只能经 privileged 安全代理)**: `mmap` 把物理帧地址交给用户态, 用户随后对该
  地址的读写**不经任何校验面** —— 地址合法性判定 (`check_user_buf` / `user_va_lower_bound`)
  是按用户 VA 布局设计的, 物理地址形态绕过其前提.
- **I2 (内核内存不可被 functions/用户非法访问)**: x86_64 的 kview 在低半区保留
  `[0, ceil(kernel_end/2MiB))` 恒等窗口 (见 [userspace-decoupling.md](./userspace-decoupling.md)
  背景节). 若 PMM 分出的帧恰落在该窗口内, 用户态拿到的「映射地址」就**恒等映射到内核镜像**
  ⇒ 用户可写内核内存. 本次取证读数 `cr2=0x1F800000` 在窗口外 (窗口上界随
  `kernel_end=0x3E8000` 为 4 MiB), 故未实际越权, 但**这是布局巧合, 不是防线**:
  帧号由 PMM 分配顺序决定, 换个内存尺寸/启动顺序即可能落进窗口.
- 决策 D (kview 低半区退役) 会消掉恒等窗口这一具体通道, 但**不修根因** —— 返回物理地址
  本身仍违反 I4, 且 aarch64 无该窗口也照样故障.

⇒ 本工程属**安全敏感 + TCB 边界**, 按 AGENTS.md §11.3 必须先由用户裁定实现路径, 不得自行选择.

## 施工分解

### M0: 归因与证据固定

- **描述**: 把「mm 恒空 → mmap 返物理地址 → 首写翻译故障」这条链归因到源码与真跑读数.
- **方案**: 源码六跳追踪 (见背景节) + 双架构 wasmi 腿取证 + 安全面判定.
- **状态**: [X]
- **详情**: 归因完成, 证据即本册背景节; **未做任何 privileged 修改** (用户裁定: 解耦批先收口,
  mm 手术单独立项). wasmi 腿因此**未接入 `ci/audit.sh`** —— 该脚本的 `err()` 是致命退出,
  接一条已知红的腿会让整个 full 档审计红, 掩盖其余门槛读数.

### M1: 实现路径裁定 (A 简约 / B 相对完整)

- **描述**: 按 §11.3 由用户在两条路径间显式授权, 之后才施工.
- **方案**:
  - **方案 A (简约)**: 只让「装载即建 mm」成立 —— 生产装载路径改走已有
    `elf_load(mm, …)` (或在其旁安装 mm), 使 `get_current_mm` 非空; mmap 的裸页回退
    **原样保留**但不再被生产命中. 估算 ~150-300 行 (含 SAFETY 注释与 host 契约),
    验证成本 = 双架构构建 + wasmi 腿真跑 + 既有启动腿无回归.
    代价: 回退支路仍是死形态 (F9 风险: 零生产调用者的分支需真删或真接线, 不能留标注),
    且 `munmap` 的裸页释放支路同样悬空; 后续要再开一批收口.
  - **方案 B (相对完整)**: mm 生命周期闭合 —— 装载建 mm + exec 换 mm (旧 mm 释放) +
    fork/clone 的 mm 继承与 COW 分层口径对齐 + **删掉 mmap/munmap 的裸页回退**
    (无 mm 即 `Enomem`, 不再返物理地址) + 退出路径释放 mm. 估算 ~600-1000 行,
    验证成本 = 上述全部 + 进程生命周期回归 (exit/exit_group/exec 组合) +
    I2/I4 专项判据 (返回值必落在用户 VA 区).
    代价: 触及 exec/fork/exit 三条既有稳定链, 回归面大; 但一次把安全面与死支路同时收口.
  - **推荐**: B. 理由: 缺口本质是安全面 (I4) 而非功能缺失, A 留下的物理地址回退支路
    仍是可被命中的越权通道 (只要将来任何路径又不装 mm), 与 §4.2「违反不变式 = 重新设计,
    不允许补丁式补救」相悖; 且 A 的死支路在 F9 下迟早要删, 等于把 B 的一半推迟并多付一次回归.
- **状态**: [] (待用户裁定)
- **详情**: 裁定前不动 privileged. 业界参照: Linux `mm_struct` 由 `exec` 建立
  (`begin_new_exec` → `exec_mmap`), fork 走 `dup_mm`, 退出走 `exit_mmap`;
  Asterinas 的 framekernel 把页表机制留 privileged、VMA 策略放 safe 侧 ——
  Edgine 已有同形分层 (`privileged/mm/vma.rs` 机制 + `functions/mm/mmap.rs` 策略),
  缺的只是**生产装载路径接上这一层**, 不是新造机制.

### M2: 装载路径安装 mm (施工主体)

- **描述**: 让每个用户进程自装载起就持有有效 `MmStruct`.
- **方案**: 按 M1 裁定选 A 或 B 的装载面; 复用既有 `elf_load(mm, …)` 而非另写一套;
  `Process::mm` 的写入点单点化 (与 A-16「CURRENT_MM 改当前任务取」口径一致).
- **状态**: [] (阻塞于 M1)
- **详情**: 必须同时决定「段/栈映射由谁建」—— 现装载器直接改页表, 若改走 VMA 面则
  按需分页与 COW 的既有真跑腿 ([aarch64_cow_boot_test.sh](../../scripts/aarch64_cow_boot_test.sh))
  判据须复跑, 不得只靠 host 形状锁.

### M3: mmap/munmap 裸页回退处置

- **描述**: 消除「返物理地址给用户态」这条支路.
- **方案**: B 路径下真删 (无 mm → `Enomem`); A 路径下需给出该支路的合法消费者或同批删.
- **状态**: [] (阻塞于 M1)
- **详情**: F9 死代码零容忍 —— 不允许以 `allow(dead_code)` 留置; `munmap_free_pages`
  ([syscall/api.rs:176](../../src/kernel/privileged/syscall/api.rs#L176)) 与
  `mmap_get_mm_or_alloc` 是一对, 必须同批处置, 否则留下「能分配不能释放」的半截链.

### M4: 回归装备 (host 契约 + QEMU 真跑腿)

- **描述**: 把缺口钉成不可静默复发的判据.
- **方案**:
  - host 契约锁: 生产装载路径必须安装 mm (源码形态) + `mmap` 入口不得存在返物理地址支路;
  - QEMU 真跑腿: wasmi 腿 (`scripts/wasmi_boot_test.sh`, 双架构) 判 `hello from wasmi` +
    `[WASMI] exit=0`; 另加一条**用户 VA 区间断言** (mmap 返回值必 ≥ `user_va_lower_bound`),
    使「又返物理地址」在日志层即红, 而不必等首写故障;
  - 接入 `ci/audit.sh` full 档 (位置与载体腿/压力腿的构建交错约束同口径).
- **状态**: []
- **详情**: 负控制必做 —— 把 mm 安装点摘掉应转红 (证明判据非恒真, 见
  `audit_vacuous_needle.py` 口径).

### M5: 收口 (七门槛 + 台账 + P1 恢复)

- **描述**: 全量验收, 恢复 P1.
- **方案**: §2.3 七门槛全绿 (audit full / test-host / test-kernel-host / ci-rustdoc 分跑);
  同步 explain (地址空间生命周期现状) + 本册状态 + 台账; 按路径提交推送;
  随后解除 [wasm-userspace-migration.md](./wasm-userspace-migration.md) P1 的暂停.
- **状态**: []

## 附: ELF 整文件读上限的余量 (记录, 不在本工程擅自改)

- **描述**: `execve` 与 wasm 选桩都按 `ELF_MAX_SIZE = 1 MiB`
  ([exec.rs:26](../../src/kernel/functions/proc/exec.rs#L26)) **整文件**读入
  (`read_file_to_vec` / `read_file_kernel` 均以此为上限), 而 wasmi 运行时桩 stripped 后
  已接近该上限.
- **方案**: 记录余量; 若将来超限, 正解是**按可加载段尺寸定界** (只读 PT_LOAD 覆盖的字节 /
  分段读), 而不是抬高上限 —— 抬上限会把「一次分配整个文件」的内存放大面继续扩大.
- **状态**: [] (待用户决定是否单开一批)
- **详情**: 实测读数 (D4-d `--strip-all` 后, 按架构 objcopy):
  `wasmrun.bin` x86_64 **692,400 B** / aarch64 **793,640 B**, 上限 1,048,576 B
  ⇒ 余量 x86_64 **356,176 B (~348 KiB)** / aarch64 **254,936 B (~249 KiB)**.
  未 strip 时分别为 1,209,424 B / 1,427,776 B, **双双超限** —— 这是 D4-d 载体腿红的直接成因,
  已由 strip 收口 (未动任何内核上限). wasmi 全量 WASI (P2) 只会让桩更大, 故余量是长期约束.

## 验收判据

- **底线**: §2.3 七门槛全绿, 任何阶段不可豁免.
- **功能**: 双架构 wasmi 腿 `hello from wasmi` + `[WASMI] exit=0`; 既有启动腿 (载体/headline/
  COW/压力) 无回归.
- **安全**: `mmap` 返回值恒落在用户 VA 区 (≥ `user_va_lower_bound`), 不存在把物理地址交给
  用户态的支路; I2/I4 自检逐项通过.
- **形态**: 生产装载路径安装 mm 有 host 契约锁 + 负控制; 裸页回退按裁定真删或真接线, 无 F9 抑制.