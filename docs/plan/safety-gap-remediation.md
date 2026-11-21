# 安全缺口修复工程（门禁健康度收敛 surface 的 27 项）

> 一句话说清为什么有这个计划: [gate-health-convergence.md](./gate-health-convergence.md) 批 5d–5g
> 逐处补写 SAFETY 时，按「先读内核实装再落笔」的纪律**读出了 27 项与注释工程无关的真实缺口**——
> 其中 ELF `PT_INTERP` 改写的范围判据是未检查加法（用户态可控字节 ⇒ 越界写）、`sendmsg`/`recvmsg`
> 的逐 iov `base`/`len` 从不校验（可触内核内存）、`auth_login` 的 note/password 在 ABI 两侧参数序
> 颠倒（`login` 命令应恒失败）。这些当时按 §11.5 **只报告不处置**，就地登记为
> `// 已登记缺口 (批6 待裁, 本批不改)`。2026-11-19 用户裁定: **A 组单独立项分批修复，
> B 组并入同一工程后排批**。本册即该裁定落地的工程。
>
> 本册**不重新编号**：项号沿用 gate-health 册的 ①–58 全局编号，便于与源码内就地登记、
> 与该册四段发现清单（批 5d / 5e / 5f / 5g）交叉对照。

---

## 一、口径与硬约束

### 1.1 两组划分（用户裁定原样采用）

| 组 | 含义 | 项数 | 批序 |
|---|---|---|---|
| **A** | **可达**真缺陷与安全缺口：有生产调用路径，用户态或并发可触发 | 12 | R1–R5 |
| **B** | **latent**：约束错配 / 别名 UB / 全仓零调用方 / 当前不可达 / 冻结面 | 15 | R6–R8 |

A 组 12 项 = ⑥ ⑩ ⑭ ㉚ ㉛ ㉜ ㊹ ㊺ ㊽ ㊾ ㊿ 51。
B 组 15 项 = ⑪ ⑫ ⑬ ⑰ ⑳ ㉓ ㉕ ㊶ ㊸ ㊻ ㊼ ㊱ ㊲ 52 56。

**分组不是优先级豁免**：B 组的「零调用方」只说明今天没有触发路径，不说明约束正确；
一旦后续工程接线（如 WASM P1 使 `mmap` 真行使、kexec/UEFI 路径被启用），latent 即刻转可达。
故 B 组必须在本工程内闭合，不得以「无人调用」为由长期搁置。

### 1.2 每项修复的四步（缺一不可）

1. **先写能复现的测试**（§11.4 目标驱动）：host 侧可复现的写 `host-tests` 契约锁 / 内联单测；
   涉及真用户内存搬运的**不可在 host ring-3 复现**（`copy_from/to_user` 读 CR4 必 SIGSEGV），
   归 privileged 单测 + QEMU/e2e 腿，host 只锁契约（口径见
   [unresolved-issues-2026-08-09.md](./unresolved-issues-2026-08-09.md) 的 CR4/SIGSEGV 条目）。
2. **修实装**，不是修注释。注释只是把缺口显式化，缺口本体在代码里。
3. **把源码内的 `已登记缺口 (批6 待裁, 本批不改)` 段改写为真实依据或删除**——
   修完后该段若留着就是新的失真注释（本工程正是为消除失真注释而立的姊妹册
   [safety-comment-fidelity.md](./safety-comment-fidelity.md) 的射程）。
4. **七门槛**（判据单源 = `AGENTS.md` §2.3，本册不复制清单）+ 本项负控制（注入回归 → 红 → 复原）。

### 1.3 三条边界

- **52 属 SGEG 现版冻结面**：用户 2026-11-17 裁定现版冻结待重设计，**禁为其它工程适配现版**。
  故 R8 对 52 **只裁定不施工**（记录正确形态，等 SGEG 重设计工程一并处理）。
- **涉及架构关键路径的批次先问路径**：R4（分配器对齐契约）、R5（定时器回调 + 锁序）、
  R6（同步原语 `Sync` 约束）触及 §11.3 的「同步原语 / 安全敏感模块 / TCB 边界」，
  施工前必须按简约 vs 相对完整二选一取得授权，不得自行选路。
- **不夹带**：本册只改清单内的项。施工中读到的新缺口按 §11.5 登记并提问，不顺手扩围。

### 1.4 行号口径

下表与批次条目只给**文件 + 符号名**。gate-health 册登记时的行号是**补写 SAFETY 之前**的读数，
批 5d–5g 纯增注释后已整体下移（例：`hrtimer_cancel` 登记为 `:390`，现树在 `:354`；
`strlen` 的判据行登记为 `:61`，现树在 `:89`）。**施工时一律以现树 grep 读数为准**，
不得沿用本册或 gate-health 册的历史行号。

### 1.5 登记本身也须复核（批 6 立项时已订正 4 条）

gate-health 册的发现清单是**补写 SAFETY 当时**的快照，批 6 立项前逐项现树复核，
已订正下列各条（**施工时仍须再核一次**，不得把本册当作免检依据）：

- **⑳ 不是「注释撒谎」**：`remap_pic` 真身在 `privileged/idt/table.rs`（**不是**
  `arch/x86_64/interrupt.rs`，该文件不存在），批 5e 补的 SAFETY **已显式写明**
  「开头读出的旧掩码 m/s 未被恢复，由 `let _ = (m, s)` 显式丢弃」，且末尾把两片掩码置
  `0xFF` 屏蔽全部 legacy IRQ、再由 `enable_irq` 逐条放开是**刻意设计** ⇒ 争点是
  「开头那次读是否多余」，须裁定**删多余读**还是**真恢复**，不得径行改行为。
- **54 已有就地订正注**：`proc/scheduler.rs` 的「本函数开头 cli」下方**已补**
  「实际 cli 在调用方 `schedule()` 首行」⇒ 该项属姊妹册（注释面），修法是改原句并删冗余注。
- **55 本轮 grep 未命中**：`freg/domain.rs` 全文只有 `IrqSpinLock`、无 `spin::Mutex`
  ⇒ 施工前须复核该项是否仍成立（属姊妹册）。
- **⑲ / ㉖ / ㉟ / ㉗ 的订正见姊妹册** [safety-comment-fidelity.md](./safety-comment-fidelity.md)
  §1.5（文件存在性、已披露冗余、不存在的函数名三类误判）。

---

## 二、清单（27 项 × 现树位置 × 风险）

| 项 | 组 | 现树位置（文件 + 符号） | 缺口本体 | 风险 |
|---|---|---|---|---|
| ㊽ | A | `privileged/proc/elf/mod.rs` `rewrite_interp_path` | `interp_offset + interp_len > elf_size` 是未检查加法，两个 ELF 字段转 usize 相加可回绕 ⇒ 绕过判据后 `copy_nonoverlapping` / `write_bytes` 越界写 | **全清单最高**：ELF 由用户态提供 |
| ㉛ | A | `privileged/net/init/sm_fi.rs` 逐 iov 路径 | 注释宣称 `base`/`len` 已校验，functions 侧从不校验 ⇒ 拷贝长度与目的址无界 | **可触内核内存**（I4） |
| ⑥ | A | `functions/sgeg/auth.rs` `auth_login_syscall` ↔ userlib `sys2(SYS_SGEG_LOGIN, …)` ↔ `src/user/eash/src/commands/identity.rs` | note/password 两侧参数序颠倒 ⇒ 内核拿密码串查用户、拿 note 串验密码 | 用户可见：`login` 应恒失败 |
| ㉚ | A | `privileged/net/syscall.rs` 六处入口 | SAFETY 宣称指针「已校验」而实装无任何校验（`addr/addrlen`、`src_ptr/src_len_ptr`、两处 `msg_ptr`、两处 `addr_ptr/addrlen_ptr`） | I4 |
| ㉜ | A | `privileged/net/syscall.rs` `raw_read_*` / `raw_write_*` + `privileged/cpu/mod.rs` 代理清单 | 不走 stac/clac 而 CR4.SMAP 已按 CPUID 开启 ⇒ 用户页访问 #PF；安全代理清单漏列这两个入口 | I4 + 可达即故障 |
| ㊾ | A | `privileged/userptr.rs` `validate_user_buf` + `privileged/syscall/dispatch.rs` `raw::read_u64` 调用点 | 只判地址区间上界，不判对齐、不判已映射，`ptr + len` 未检查可回绕；`read_u64` 的 SAFETY 声称对齐已校验 | I4 |
| ㊿ | A | `privileged/syscall/mod.rs` `reboot_via_idt` | 64 位 IDTR 是 10 字节（2 limit + 8 base）而实参 `&[0u16; 4]` = 8 字节 ⇒ `lidt` 越界读 2 字节；`int 0` 压栈与 `options(nostack)` 矛盾 | 可达（reboot syscall） |
| 51 | A | `privileged/syscall/mod.rs` `reboot_via_psci` | x0=0 不是 SYSTEM_RESET（仓内 `arch/aarch64/psci.rs` 已有 `PSCI_SYSTEM_RESET = 0x8400_0009` + `system_reset()`）；QEMU virt conduit 是 HVC 而非 SVC；SMCCC 破坏 x0..x3 未声明 clobber | 可达（aarch64 reboot） |
| ⑩ | A | `privileged/glue/string.rs` `strlen` | `while *ptr != 0 && len < MAX_CSTR_LEN` 求值次序颠倒 ⇒ 达上限仍多读 1 字节；同族 `glue/cstr.rs` 是先判上限再解引用，**修法现成** | FFI 越界读 1 字节 |
| ㊺ | A | `src/kernel/memory_allocator.rs` 小对象路径 | `+TAG_SIZE` 使返回指针只保证 8 字节对齐，`mm/kmalloc.rs` 的 ALIGNMENT=16 与 PMM 4096 都被前移破坏 ⇒ `layout.align() > 8` 契约不成立（大对象路径 tag_offset=0 不受影响） | 分配器契约 |
| ㊹ | A | `privileged/timer/hrtimer.rs` `hrtimer_cancel` + `HrTimer.callback` | cancel 只置 Inactive + `queue_seq=0`、**不出队** ⇒ 调用方释放 timer 后队列仍持悬垂 `NonNull`；`callback` 是普通非原子字段，另一核 `init(&mut self, …)` 可普通写 ⇒ 数据竞争 + &/&mut 别名 | UAF + 竞争 |
| ⑭ | A | `privileged/mm/swap.rs` `swap_in` / `swap_out` | 持 `SWAP.lock`（层级 6）期间调 `pmm::get_pmm().alloc_page()`（内部取 PMM 锁，层级 1）⇒ **6→1 降序嵌套**，与 `docs/explain/ref-lock-order.md` 声明的升序相反 | 锁序违例（AB-BA） |
| ⑪ | B | `privileged/sync/rwlock.rs` `unsafe impl<T: ?Sized + Send> Sync for RwLock<T>` | 缺 `T: Sync`：`read()`/`try_read()` 把 `&T` 同时交给多个读者，`T: Send` 不足以让 `&T` 跨线程共享；vendored spin 用 `T: Send + Sync` | latent（唯一泛型消费面 `functions/sync/scoped.rs` 未以 `!Sync` 的 T 实例化） |
| ⑫ | B | `privileged/mm/slab.rs` `slab_dump_cache` | `# Safety` 只要求「合法 C 字符串」却用 `from_utf8_unchecked`（非 UTF-8 即 UB），且逐字节扫描无长度上限 | latent（FFI 导出、全仓零调用方） |
| ⑬ | B | `privileged/mm/vmm_x86_64.rs` `Vmm::ensure_pml4_user` | 不取 `VMM_LOCK` 就对共享 `KERNEL_PML4` 槽位取 `&mut` 做读-改-写 ⇒ 同索引并发即两个 `&mut` 别名 | latent（FFI 导出、零调用方） |
| ⑰ | B | `privileged/arch/x86_64/acpi.rs` `acpi_reboot` SystemMemory 分支 | 只取 FADT GAS 低 32 位地址、不消费 RegisterBitWidth/AccessSize（恒字节写）、不校验该地址是否在恒等映射窗内（窗外即缺页而非重启） | 已知局限 |
| ⑳ | B | `privileged/idt/table.rs` `remap_pic` | 读旧主/从掩码后以 `let _ = (m, s)` 丢弃；**批 5e 的 SAFETY 已显式披露此事**，且末尾置 `0xFF` 屏蔽全部 legacy IRQ 再逐条放开是刻意设计 ⇒ 争点是「开头那次读是否多余」 | 待裁定（删读 / 真恢复），非注释失真 |
| ㉓ | B | `privileged/arch/aarch64/gic.rs` `verify_post_conditions` | 读引导核静态别名，而配置路径用按亲和定位的帧 ⇒ 多核下二者可能不同核 | 多核正确性 |
| ㉕ | B | `privileged/boot/mod.rs` `parse_multiboot2` | 不校验 `tag_size >= 16`，tag 3 载荷 +12 的越界防护只到 8 字节标签头 | 引导面已知局限 |
| ㊶ | B | `src/kernel/lib.rs` 存根 asm + `privileged/idt/table.rs` `RecoveryAction::Recovered` | `options(noreturn)` 与实装不符：Recovered 分支正常返回、x86_64 分支此后无停机循环即函数尾 ⇒ `-> !` 落空属 UB 面 | UB 面 |
| ㊸ | B | `privileged/klog/mod.rs` `cstr_slice` | 返回 `&'static [u8]` 把 C 侧缓冲生命周期谎称 static，实际依据只是「调用内同步消费」⇒ 留存即 UAF | 生命周期伪造 |
| ㊻ | B | `privileged/freg/recoverable.rs` `unsafe impl<T: Snapshot + Sync> Sync for RecoverableMutex<T>` | 内层 `IrqSpinLock<T>` 的 Sync impl 要求 `T: Send`，而 **Sync 不蕴含 Send** ⇒ 约束错配 | 约束错配 |
| ㊼ | B | `privileged/proc/elf/mod.rs` `elf_validate` | 用 `read_unaligned` 读头、从未确立 8 字节对齐，却 `&*(… as *const Elf64Header)` 构造要求对齐的共享引用（经 usize 中转不消除）；且返回 `&'static` 伪造 | 对齐 UB + 生命周期伪造 |
| ㊱ | B | `privileged/driver/kexec.rs` `load_segment` | 把 doc 声称的物理地址当 VA 用（不加 `KERNEL_BASE`）、`dst_addr` 未经 PMM 校验映射/可写、无 src/dst 重叠检查 | latent（零调用方） |
| ㊲ | B | `privileged/driver/uefi.rs` 五处系统表读 | 当前全不可达（唯一调用方 `boot_deferred_init` 传 0），且对齐/映射/固件预留均未校验、doc 的 PA/VA 口径不一致 | latent（不可达） |
| 52 | B | `privileged/sgeg/audit.rs` `AuditLog` | 模块文档声称「写入经 IrqSpinLock 保护」，实装只有 `[AuditEntry; 256]` + `AtomicUsize` ⇒ &/&mut 别名 UB + 回绕竞争 | **冻结面：只裁定不施工** |
| 56 | B | `privileged/glue/string.rs` 三处 asm | 内层 `stp` 标 `options(nostack, readonly)` 而 stp 确实写内存 ⇒ LLVM 可据此重排/消除后续读；`rep stosb` 递增 rdi、递减 rcx 至 0 却未声明 clobber；比较族全仓零调用方 | 编译器优化面 UB |

---

## 三、批次条目

### 批 R1・最高危三条（㊽ ㉛ ⑥）

描述：修 ELF `PT_INTERP` 改写的未检查加法（㊽）、`sm_fi` 逐 iov `base`/`len` 无界（㉛）、
`auth_login` note/password 两侧参数序颠倒（⑥）。三条是清单里**用户态可直接触发**的最高危项。
方案：① ㊽ 改 `checked_add` 并把判据前移到构造任何指针之前，补「回绕输入被拒」的 host 单测
（ELF 字节数组可在 host 构造，不触 CR4）；② ㉛ 在 functions 侧逐 iov 走
`userptr::validate_user_buf`（与 ㊾ 同源），补长度/目的址无界的负控制；
③ ⑥ 先裁定**哪一侧是权威**（ABI 单源在 `edgine-abi`，note/password 序须两侧一致），
再改内核或 userlib 之一 + `eash identity` 端到端回归；④ 三处就地登记的缺口段改写为真实依据。
详情：㊽ `needs_interp_rewrite` / `rewrite_interp_path` 两处范围判据改 `checked_add`（回绕即跳过），
补内联单测 `interp_range_add_overflow_is_rejected` / `interp_range_exact_fit_is_accepted`；
㉛ 新增 functions 侧 `validate_iov_list`，`sendmsg_syscall`/`recvmsg_syscall` 逐条走
`validate_user_buf`，补 `host-tests/tests/sendmsg_iov_validation_test.rs`（内核地址 iov 入口即 EFAULT）
与内联边界单测；⑥ 裁定 ABI 权威序为 **note 在前**（`pwm_login(note, password)` + userlib `auth_login(n, p)` 一致），
修正 `auth_login_syscall` 形参与转发序，补 `host-tests/tests/sgeg_login_arg_order_test.rs`（正确序登录成功、对调序被拒）。
三项负控制均注入后转红（RC=101）并复原；七门槛全绿。
收口读数：`build.sh all` RC=0（Passed 6 / Failed 0）· `audit.sh full` RC=0（未执行腿数 0、
Lockbud 4/4、QEMU 2/2）· `make test-host` **206 套 / 1606 passed / 0 failed** ·
`make test-kernel-host` **12 + 811 + 558 = 1381 / 0 failed** · `qemu_boot_test.sh x86_64` 1/1 ·
`ci-rustdoc` 双架构 exit 0。（注：lockbud 腿在本环境 TTY 下因 cargo ANSI 色彩码使
「实编 crate 数」统计为 0，需 `CARGO_TERM_COLOR=never` 运行；非代码问题。）
状态：[X]

### 批 R2・I4 用户指针校验面收口（㉚ ㉜ ㊾）

描述：`privileged/net/syscall.rs` 六处「已校验」假称（㉚）、`raw_read_*`/`raw_write_*` 不走
stac/clac 而 SMAP 已开（㉜）、`validate_user_buf` 只判上界 + `ptr+len` 可回绕 + `read_u64`
假称对齐已校验（㊾）。三项同属 **I4「用户内存只能通过 privileged 安全代理」**。
方案：① 先把 `validate_user_buf` 补成**完整判据**（上界 + 加法回绕 + 对齐 + 已映射），
作为单一入口；② 六处 net 入口与 dispatch 的 `read_u64` 调用点全部改走该入口；
③ `raw_read_*`/`raw_write_*` 纳入 stac/clac 安全代理并补进 `cpu/mod.rs` 的代理清单；
④ 注意 `validate_user_buf` 现树有**三个同名定义**（`userptr.rs` / `syscall/api.rs` /
`syscall/mod.rs`），施工前须先核清转发关系，避免只改一处；⑤ host 只锁契约，
真搬运归 privileged 单测 + QEMU/e2e（CR4 口径）。
详情：核清转发关系后确认**单一定义**为 `privileged/userptr.rs::validate_user_buf`，
`syscall/api.rs` 与 `syscall/mod.rs` 仅为同名转发壳 ⇒ 只改单点即全量生效。落地内容：
① `validate_user_buf` 补 `checked_add` 回绕拒绝与逐页"已映射"判据（`get_current_pml4` +
`Vmm::translate_in_pml4` 逐页走查；host-test 构建无页表模型，映射面恒真，只锁区间/回绕契约），
新增 `validate_user_aligned::<T>` 承载 typed 访问的对齐判据；② `syscall/raw` 的
`read_u64` / `write_u64` / `write_u64_pair` / `write_struct` 改为**入参为用户地址数值的自校验安全函数**
（判据不满足返回 `None`/`false`，不做任何用户访问），dispatch 四个调用点与 `sys_fb_open`
改走该入口，`read_u64` 的"对齐已校验"假称由真实对齐判据兑现；③ `net/syscall.rs` 六入口
（accept / recvfrom / sendmsg / recvmsg / getsockname / getpeername）按 `sockaddr` 上限 28 字节、
`msghdr` 56 字节、`addrlen` 4 字节补完整校验；`raw_read_u32` / `raw_write_u32` /
`raw_read_sun_family` 与 `syscall/raw` 全部直读直写助手、`sm_fi` 的 sockaddr 回写与
`msghdr`/iov 直读直写均以 `smap_begin`/`smap_end` 包裹（`sockaddr` 回写同时从
`ptr::write` 改为 `write_unaligned`，消除 addrlen 对齐 UB），`cpu/mod.rs` 代理清单同步扩列；
④ 顺带修出 host-test 构建的 SMAP 探针：`smap_is_enabled` 在 host-test 下读 CR4 是特权指令
（SIGSEGV），改为 cfg 门控恒 false；⑤ `sm_fi.rs` 与 `net/syscall.rs` 的就地缺口登记段
改写为真实依据。新增 `host-tests/tests/user_ptr_validation_r2_test.rs`（6 用例：回绕、
对齐、raw 助手拒绝、api 助手拒绝、六入口 EFAULT、SMAP 接线源码锁）与 2 个内联单测；
三负控制均注入后转红（RC=101）并复原。
收口读数：`build.sh all` RC=0（Passed 6 / Failed 0）· `audit.sh full` RC=0（未执行腿数 0、
Lockbud 4/4、QEMU 2/2 + P-62 feature 4/4）· `make test-host` **207 套 / 1612 passed / 0 failed** ·
`make test-kernel-host` **12 + 811 + 560 = 1383 / 0 failed** · `qemu_boot_test.sh x86_64` 1/1
（含 TCP SOCKNAME/PEERNAME/RECVFROM 路径实跑）· `ci-rustdoc` 双架构 exit 0。
状态：[X]

### 批 R2.1・sockaddr 回传截断语义（R2 施工新发现）

描述：R2 收口 ㉚ 时复核实装发现 `sm_accept` / `sm_recvfrom` / `sm_getsockname` /
`sm_getpeername` 的 sockaddr 回写**不消费用户传入的 `*addrlen`**：V6 对端恒写 28 字节，
V4 调用方按 16 字节分配时会被写穿用户自己的缓冲区。内核内存安全面已由 ㉚ 的
28 字节映射校验兜住，属用户缓冲越界与 Linux 语义缺口。用户 2026-11-20 指示
**按根治处理**，故在本工程内闭合。
方案：以 Linux `move_addr_to_user` 语义为准 —— `*addrlen` 作为用户缓冲容量，只回写
`min(容量, 实际长度)` 字节；`*addrlen` 始终回填实际长度（16 / 28），即使发生截断。
容量读取集中在 `user_addr_capacity`（smap 包裹、上限 28 字节），`write_sockaddr`
先在内核栈构造完整结构再按容量 `copy_nonoverlapping` 拷回；syscall 层的 28 字节
校验保留为写量上界（校验是实际写量的超集，不放松安全判据）。
详情：`write_sockaddr` 新增 `cap` 参数并改为截断拷贝；五个调用点（accept、
recvfrom、getsockname，以及 getpeername 的两条分支）统一经 `user_addr_capacity`
取容量；`sm_accept` / `sm_recvfrom` / `sm_getsockname` / `sm_getpeername` 的
`# Safety` 文档同步改写。新增内联单测 `test_write_sockaddr_truncates_to_capacity`
（V6 容量 16：只写 16 字节且回填 28；容量 0：不写且回填 28；V4：回填 16）；
负控制把 `min(cap, size)` 注入为 `size` 后转红（RC=101）并复原。
收口读数：`build.sh all` RC=0（Passed 6 / Failed 0）· `audit.sh full` RC=0
（未执行腿数 0、Lockbud 4/4、QEMU 2/2 + feature 4/4）· `make test-host`
**207 套 / 1612 passed / 0 failed** · `make test-kernel-host`
**12 + 811 + 561 = 1384 / 0 failed** · `qemu_boot_test.sh x86_64` 1/1
（UDP PEERNAME / SOCKNAME / RECVFROM 实跑通过）· `ci-rustdoc` 双架构 exit 0。
状态：[X]

### 批 R3・reboot 双路径（㊿ 51）

描述：x86_64 `reboot_via_idt` 的 IDTR 10 字节 vs 8 字节实参 + `int 0` 与 `options(nostack)`
矛盾（㊿）；aarch64 `reboot_via_psci` 的功能号/conduit/clobber 三错（51）。
方案：① ㊿ 改 10 字节伪描述符（`#[repr(C, packed)]` limit: u16 + base: u64）并消解
`nostack` 与压栈的矛盾（改 `options(noreturn)` 或提供栈）；② 51 **改调仓内已有的
`arch/aarch64/psci.rs::system_reset()`**（`PSCI_SYSTEM_RESET = 0x8400_0009`），
不在 syscall 层重写 SMCCC 汇编；③ 两架构 reboot 路径补 QEMU 腿断言（真重启可观测）。
详情：① ㊿ `raw` 子模块新增 `#[repr(C, packed)] struct Idtr { limit: u16, base: u64 }`
（size 10 / base 偏移 2），asm 去掉 `options(nostack)` 改为「提供当前内核栈」并保留
`spin_loop` 兜底；内联单测 `idtr_layout_is_cpu_ten_byte_form` 锁 size 与
`offset_of!(Idtr, base)`。② 51 `reboot_via_psci` 只做转发 `psci::system_reset()`
（HVC/SMC conduit 由设备树探测、功能号 0x8400_0009、PSCI 不可用时写零地址触发异常，
仍发散）；同时补 `psci::invoke` 的 SMCCC clobber —— x1..x3 一并声明
`lateout("xN") _`（原只声明 x0，固件改写 x1..x3 后编译器可假设其保值）。③ 新增
`src/user/reboottest` 载荷（打印请求行后 `reboot(REBOOT_CMD_RESTART)`，机制返回即打
`[REBOOT] FAIL` 非零退出）+ Makefile 双架构 `reboottest.bin` 接线 +
`scripts/qemu_reboot_test.sh`（把 reboottest 打成专用 cpio 作 `/init`，**不带
`-no-reboot`**；判据 = 请求行 ≥2 次 + `Entering Ring 3`/`Entering EL0` ≥2 次 +
无 `[REBOOT] FAIL`），接入 `ci/audit.sh` full 档步骤 4 末位。
④ 施工调研另读出清单外用户可见缺口（§11.5 提问后按用户裁定「以用户侧约定为准」
本轮一并修）：`eash reboot` 传 0、`halt` 传 1，而内核 `reboot_mechanism` 原为
0=停机自旋 / 1=重启 ⇒ 两条命令行为互换。命令码上收 `edgine-abi::reboot`
（`REBOOT_CMD_RESTART=0` / `REBOOT_CMD_HALT=1`），内核分支按常量交换，eash 与
install 完成页改用同源常量；`host-tests/tests/reboot_cmd_semantics_test.rs` 四条锁
（ABI 值 / 内核分支落真复位路径 / 两侧调用形态 / 源码内缺口段清零）。
负控制三条（均注入后转红并复原）：去掉 `packed` ⇒ 内联单测 RC=101
（`64 位 IDTR 必须是 10 字节`）；`reboot_mechanism` 两分支写回原语义 ⇒ x86 真重启腿
RC=1（请求行仅 1 次）；`reboot_via_psci` 注入停机自旋 ⇒ aarch64 真重启腿 RC=1
（请求行仅 1 次）。
收口读数：`build.sh all` RC=0（Passed 6 / Failed 0）· `audit.sh full` RC=0
（未执行腿数 0、Lockbud 4/4、QEMU 2/2 + feature 4/4、线程组压力 6/6、reboot
真重启腿双架构通过）· `make test-host` **208 套 / 1616 passed / 0 failed** ·
`make test-kernel-host` **12 + 811 + 562 = 1385 / 0 failed** · `ci-rustdoc`
双架构 exit 0。
状态：[X]

### 批 R4・FFI 字符串与分配器契约（⑩ ㊺）

描述：`strlen` 求值次序颠倒多读 1 字节（⑩）；`memory_allocator.rs` 小对象路径 `+TAG_SIZE`
破坏 16 字节对齐（㊺）。
方案：① ⑩ 按同族 `glue/cstr.rs` 的现成形态改为**先判上限再解引用**，补「缓冲恰为
MAX_CSTR_LEN 字节且无 NUL」的临界单测（批 5c 的 `at_max` 用例正是该形态，靠 `to_cstr`
追加的 NUL 兜住，须改成直接暴露越界）；② ㊺ 先按 §11.3 问路径（简约＝把 tag 放在
对齐间隙内 / 相对完整＝重设计小对象头布局），再改并补 `layout.align() > 8` 的分配器单测。
详情：**㊺ 用户裁定「相对完整」**——标签与对齐偏移改为**尾随载荷**（`[数据指针 + size,
+16)` = 来源标签 u64 + 偏移 u64），数据指针 = 基础块指针 + `align_offset`，对任意合法
`Layout::align()` 成立；布局算式单源到新模块 `privileged/mm/alloc_meta.rs`
（`align_offset` / `pages_for`，纯函数便于 host 单测），分配器按「小对象 kmalloc /
大对象且对齐 ≤ 页沿用无元数据路径 / 其余 PMM + 元数据」三路路由——最后一支刻意保留
无元数据路径，避免整页大小（4096）的分配因 16 字节元数据多占一页。dealloc 以同一
`Layout` 复原路由、基础块指针与页数。**⑩** 改为 `while len < MAX_CSTR_LEN && *ptr != 0`
并将 `# Safety` 补为真实调用方义务；host 临界用例改用 `mmap` + `mprotect(PROT_NONE)`
守护页（缓冲恰 `MAX_CSTR_LEN` 字节且无 NUL，多读 1 字节即 SIGSEGV），不再依赖
`to_cstr` 追加的 NUL 兜住。两项负控制：⑩ 还原旧求值次序 ⇒ 守护页用例 **SIGSEGV
（RC=101）**；㊺ 令 `align_offset` 恒返 0 ⇒ `align_offset_rounds_up_within_one_step`
**FAILED（RC=101）**；均复原后转绿。
收口读数：`./ci/build.sh all` RC=0（Passed 6 / Failed 0）· `bash ci/audit.sh full`
RC=0（未执行腿数 0，含 QEMU 与 reboot 双架构真重启腿）· `make test-host` **208 套 /
1617 passed / 0 failed**（+1 守护页用例）· `make test-kernel-host`
**12 + 811 + 567 = 1390 / 0 failed**（+5 `alloc_meta` 单测）· `ci-rustdoc` 双架构
exit 0。
状态：[X]

### 批 R5・定时回调与锁序（㊹ ⑭）

描述：`hrtimer_cancel` 不出队 ⇒ 悬垂 `NonNull`，`callback` 非原子 ⇒ 跨核数据竞争（㊹）；
`swap_in` 持 SWAP 锁（6）期间取 PMM 锁（1）⇒ 降序嵌套（⑭）。
方案：① ㊹ 按 §11.3 先问路径：原拟「相对完整＝cancel 等待在途回调」经现树核查会与 timerfd
构成 AB-BA（`set_time` 持 `TFD_TABLE` 调 cancel，而 `timerfd_callback` 首行取同表锁）；二轮
询问后用户裁定按 **A+** 收口 —— 队列锁内真出队 + 回调原子化 + 取消后收敛，把「Pending /
Running 期间调用方须保活」写成硬契约；同步等待的长期解单独立项
[hrtimer-cancel-protocol.md](./hrtimer-cancel-protocol.md)（两档取消 + timerfd 回调叶子化）。
② ⑭ 按相对完整路径：页分配移出 SWAP 临界区（锁外取页），锁内复查 `initialized` 与
slot 仍为 Used，失败路径先释放守卫再归还页。
详情：㊹ `hrtimer_cancel` 在 `HRTIMER_QUEUE` 锁内 `retain` 摘除该 timer 的全部条目；
`callback` 由普通字段改 `AtomicUsize`（0 = 未设置 ⇒ `noop_callback`，保持 `uninit()` 为
const），`init` 由 `&mut self` 改 `&self`；`dispatch_expired` 迟到分支在序号为 0（已取消）时
把 Running 收敛回 Inactive，Periodic 分支仅在 `state == Running` 时重挂（防取消后被回调
复活）；`hrtimer_sleep` 返回前补 `hrtimer_cancel`，兑现「栈上 timer 不得留在队列」。新增 5 条
host 用例（`test_cancel_removes_timer_from_queue` / `test_cancel_then_drop_leaves_nothing_to_dispatch` /
`test_init_through_shared_ref_replaces_callback` / `test_dispatch_stale_after_cancel_converges_running` /
`test_periodic_dispatch_does_not_rearm_after_cancel`），队列相关用例经 AtomicBool 串行
（不用 `std::sync::Mutex`，避免仅测试使用的锁对象污染锁类表）；`poll_timer` 因 `init(&self)`
去掉多余 `mut`。⑭ `SwapArea::is_slot_used` 抽出并与 `free_slot` 共用；`swap_in` 锁外
`alloc_page`，锁内复查后 `read_slot` + `free_slot`，失败路径 `drop(guard)` 后 `free_page`；
新增内联 `slot_used_predicate_tracks_alloc_and_free` 与 host 形状锁 3 条
（alloc 前于取锁 / free_page 后于守卫释放 / slot 复查在锁内）。负控制：㊹ 移除 cancel 的
`retain` ⇒ `test_cancel_*` **2 failed（RC=101）**；⑭ `free_page` 前移到 `drop(guard)` 前 ⇒
`swap_in_frees_page_only_after_dropping_swap_guard` **FAILED（RC=101）**；`alloc_page` 移回锁内 ⇒
`swap_in_allocates_page_before_taking_swap_lock` **FAILED（RC=101）**；三项均复原后转绿。
锁序棘轮：`hrtimer.rs::HRTIMER_QUEUE` 未分类取锁点 5→7（cancel 1 处 + 测试 helper 1 处），
按工具许可路径 `--update-lock-order-baseline` 显式登记（快照仅该键 +2，其余不变）。
收口读数：`./ci/build.sh all` RC=0（Passed 6 / Failed 0）· `bash ci/audit.sh full`
RC=0（未执行腿数 0，含 Lockbud 与 QEMU）· `make test-host` **209 套 / 1620 passed /
0 failed**（+1 形状锁套件、+3 用例）· `make test-kernel-host` **12 + 811 + 573 = 1396 /
0 failed**（+5 hrtimer、+1 swap 单测）· `check_kernel_test_release_dim.sh` RC=0 ·
`ci-rustdoc` 双架构 exit 0。
状态：[X]

### 批 R6・latent 约束与别名（⑪ ⑫ ⑬ ㊻ ㊸ ㊶ ㊼）

描述：B 组里属**类型约束 / 生命周期 / 别名**的七项。
方案：① ⑪ `RwLock<T>: Sync` 补 `T: Sync`（与 vendored spin 同口径），先核唯一泛型消费面
`functions/sync/scoped.rs` 是否因此断编译；② ⑫ `slab_dump_cache` 改 `from_utf8`（失败即显式
拒绝）+ 加扫描上限；③ ⑬ `ensure_pml4_user` 取 `VMM_LOCK` 后再做读-改-写；
④ ㊻ `RecoverableMutex<T>: Sync` 约束改 `T: Snapshot + Send`（按内层 `IrqSpinLock<T>` 的真实
要求），并核 `RecoverableMutex<T>: Send` 那条是否同型错配；⑤ ㊸ `cstr_slice` 去掉伪造的
`'static`，改由调用方传入生命周期或返回 owned 定长缓冲；⑥ ㊶ 消解 `options(noreturn)`
与 Recovered 正常返回的矛盾（改 `-> !` 落空面或补停机循环）；⑦ ㊼ `elf_validate` 全程
`read_unaligned` 按值返回头（不构造对齐引用），并去掉伪造的 `&'static`。
**本批触及同步原语，施工前按 §11.3 问路径。**
状态：[]

### 批 R7・latent 不可达与引导面（⑰ ⑳ ㉓ ㉕ ㊱ ㊲）

描述：B 组里属**不可达 / 零调用方 / 引导与中断配置**的六项。
方案：① ⑰ `acpi_reboot` 消费 RegisterBitWidth/AccessSize 并校验地址在恒等映射窗内，
窗外显式失败而非缺页；② ⑳ `remap_pic`（`idt/table.rs`）先裁定「删多余的旧掩码读」还是「真恢复」——
现树 SAFETY 已显式披露丢弃且全屏蔽是刻意设计，**不得径行改行为**（§9.1 决策灰色地带须问）；
③ ㉓ `verify_post_conditions` 改读按亲和定位的帧（与配置路径同源）；④ ㉕
`parse_multiboot2` 补 `tag_size >= 16` 校验；⑤ ㊱ `load_segment` 明确 PA/VA 口径
（加 `KERNEL_BASE` 或改 doc）、`dst_addr` 经 PMM 校验映射/可写、补 src/dst 重叠检查；
⑥ ㊲ UEFI 五处系统表读补对齐/映射/固件预留校验并统一 PA/VA 口径——**当前全不可达，
故本批只补校验与口径，不改接线**（接线属解耦册 D4-i 的后续）。
状态：[]

### 批 R8・冻结面与 asm 契约（52 56）

描述：52 `sgeg/audit.rs` 的 `AuditLog` 别名 UB + 回绕竞争（**SGEG 现版冻结**）；
56 `glue/string.rs` 三处 asm 契约错（`stp` 的 `readonly`、`rep stosb` 的 clobber、比较族零调用方）。
方案：① 52 **只裁定不施工**：记录正确形态（写入须经 `IrqSpinLock` 或改原子槽 + 回绕判据），
等 SGEG 重设计工程一并处理；本批只把模块文档的假称改为与实装相符的描述，不动数据结构；
② 56 摘掉 `stp` 的错误 `readonly`、为 `rep stosb` 补 rdi/rcx 的 clobber 声明（或改
`inlateout`），比较族零调用方按 F9 的消除方式处置（实现使用路径 / 移入测试模块），
**不得加 `allow(dead_code)`**；③ 补 asm 契约的 host 侧形状锁（钉 options 与 clobber 清单）。
状态：[]

### 批 R9・收口

描述：本册状态字段全 `[X]`、`docs/plan/progress-active-tasks.md` 甲表行回填读数、
memory 更新、按路径提交推送。
方案：① 逐项复核 27 项的就地登记段均已改写或删除（grep `已登记缺口` 在本册射程内应归零）；
② 新文件先 `git add` 再跑 `scan_obsolete_naming` 与 `check_doc_paths`；③ 七门槛复跑取读数
（判据 = 日志内 `*-RC=` 回显，非后台通知退出码）；④ `git commit --only --pathspec-from-file`，
不夹带其余暂存态。
状态：[]

---

## 四、验证门槛

判据单源 = `AGENTS.md` §2.3（本册不复制清单，P-70 甲）。另加两条本册专属：

1. **每项一个复现测试**：修复前该测试必须红（负控制），修复后绿。只改注释不算闭合。
2. **`已登记缺口` 段清零**：本册射程内的源码就地登记段，修完必须改写为真实依据或删除；
   留着即制造新的失真注释（与姊妹册的判据冲突）。

> 门槛运行期间不改它读取面的源文件；根目录裸 `cargo fmt` 会改写 vendored smoltcp ⇒
> 只对目标 crate 用 `--manifest-path`；共享工作树禁 stash/reset，负控制用 `cp` + `sha256`。