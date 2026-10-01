//! QueenX 内核 (纯 Rust 实现) — 框内核 (Framekernel)
//!
//! ## 架构概览 (Asterinas OSTD 范式)
//!
//! ```text
//! kernel/
//! ├── framework/   # 【唯一 TCB / 唯一允许 unsafe】底层硬件基座
//! │   ├── arch/      架构特定 (GDT/IDT/APIC/MMU/GIC)
//! │   ├── boot/      引导协议 (Multiboot2/UEFI/...)
//! │   ├── cpu/       CPU 探测 (CPUID/MSR/TSC/缓存/拓扑)
//! │   ├── mm/        物理/虚拟内存 (PMM/VMM/Slab/Kmalloc)
//! │   ├── irq/       中断控制器底层
//! │   ├── idt/       中断描述符表
//! │   ├── dma/       DMA 引擎
//! │   ├── driver/    原生硬件驱动 (寄存器/时序)
//! │   ├── net/       网络硬件 + 协议栈
//! │   ├── fs/        文件系统底层 (VFS 抽象 + 块设备层)
//! │   ├── ipc/       IPC 底层 (内核态通道)
//! │   ├── credo/     身份/密码学硬件
//! │   ├── chitin/    设备框架底层
//! │   ├── barrier/   弹性恢复底层
//! │   ├── console/   串口/终端硬件
//! │   ├── klog/      日志硬件输出
//! │   ├── config/    硬件相关配置
//! │   ├── smp/       多核支持
//! │   ├── lib/       底层工具
//! │   ├── link/      链接脚本
//! │   ├── alloc/     全局分配器
//! │   ├── sync/      同步原语 TCB (11 子模块: spinlock/mutex/rwlock/rcu/atomic/seqlock/types/arch/once_lock/once_cell/irq_spinlock)
//! │   ├── proc/      进程管理 TCB (12 子模块: types/process/thread/session/elf/api/scheduler/scheduler_ex/cfs/cpu_queue/oomd/user_proc)
//! │   ├── sched/     调度器特质
//! │   ├── syscall/   系统调用底层
//! │   ├── timer/     时钟底层
//! │   ├── wasm/      WASM 运行时底层
//! │   ├── pci/       PCI 设备底层
//! │   ├── tests/     框架单元测试
//! │   └── frame.rs/vmspace.rs/usermode.rs/userctx.rs/userptr.rs
//! │     iomem.rs/ioport.rs/irqline.rs/dma_buf.rs/page_table.rs
//! │     cpu_local.rs/racy_cell.rs
//! │     net_socket.rs/credo_pwm.rs/proc_elf.rs/syscall_init.rs
//! │
//! └── services/    # 【全 safe / #![deny(unsafe_code)]】业务层
//!     ├── driver/    设备驱动 safe wrapper
//!     ├── fs/        文件系统业务 (VFS + 4 FS 实现)
//!     ├── net/       网络业务 (socket)
//!     ├── ipc/       IPC 业务
//!     ├── proc/      进程子系统
//!     ├── sync/      同步原语业务封装
//!     ├── syscall/   系统调用分发
//!     ├── credo/     身份/密码学业务
//!     ├── chitin/    用户态驱动框架
//!     ├── barrier/   弹性归因业务
//!     ├── console/   控制台业务 (services 实际通过 framework::console 复用)
//!     ├── klog/      日志业务 (services 实际通过 framework::klog 复用)
//!     └── wasm/      WASM 运行时
//! ```
//!
//! ## 设计理念
//!
//! - **TCB 收拢**: 所有 `unsafe` 与硬件裸操作集中于 `framework/`
//! - **业务隔离**: `services/` 全目录 `#![deny(unsafe_code)]`, 100% safe
//! - **类型安全**: 利用枚举、Option、Result 消除不安全代码
//! - **零成本抽象**: 关键路径性能与 C 版本相当
//! - **模块化**: 每个子系统独立可测试

// B08-12: 顶层约束按 host-test feature 门控 — host 编译 (std) 时剥离裸机专属约束:
// no_std → std (host-test 下启用 std 提供 panic/alloc handler); no_main → 测试 harness 提供 main;
// alloc_error_handler 为 no_std 专属 nightly feature, host-test 下 std 自带.
#![cfg_attr(not(feature = "host-test"), no_std)]
#![cfg_attr(not(feature = "host-test"), no_main)]
#![cfg_attr(not(feature = "host-test"), feature(alloc_error_handler))]
// I-09: 移除 `#![feature(asm)]`. nightly 1.97 中 `core::arch::asm!` 已稳定,
// 源码中所有 asm 调用已走 `core::arch::asm!`, 顶层 feature gate 不再需要.
// ============================================================================
// ✅ 全局警告抑制配置 (内核开发环境特有)
// ============================================================================

//! 允许的警告类别 (符合 OS 内核开发最佳实践)

// I-09: 移除 `#![allow(stable_features)]` — 该 allow 仅用于 asm feature
// 声明, 已一并移除, 不再有 unstable 特性走 stable 路径.

// 1. 全局单例模式 - 内核中常见且必要
#![allow(static_mut_refs)]
// 32个: TRUST_CHAIN, TOKEN_MANAGER 等全局可变静态

// 2. C 语言兼容性 - 与原有 C 代码保持一致的命名风格
#![allow(non_upper_case_globals)] // 函数名: kfree, kmalloc 等
#![allow(non_camel_case_types)] // 类型名: C 互操作裸类型等
#![allow(non_snake_case)] // 变量名: io_port 等

// 4. FFI 边界 - C/Rust 互操作不可避免
#![allow(improper_ctypes)]
// 3个: IrqSaveFlags, FFI 类型

// 5. 安全相关 - 已迁出 (2026-09-26 F9 裁决: 150+ 处冗余 unsafe 块全量清理, 移除 crate 级 allow(unused_unsafe))

// 6. Clippy: 内核代码中原始指针解引用是固有操作，由调用者保证安全性
#![allow(clippy::not_unsafe_ptr_arg_deref)]
// 7. Clippy: 内核内部 &self → &mut T 模式（如 Mutex::get_mut、UnsafeCell 包装）
#![allow(clippy::mut_from_ref)]
// 8. Clippy: 内核 C 字符串字面量 — 多用于 FFI，接收方类型多样（*const u8/i8/c_char）
#![allow(clippy::manual_c_str_literals)]
// 9. Clippy: 内核文档注释风格 — 模块级 doc comment 后空行是既存惯例
#![allow(clippy::empty_line_after_doc_comments)]
// 10. Clippy: Result<_, ()> — 内核错误路径使用 () 作为错误值是有意设计
#![allow(clippy::result_unit_err)]
// rustfmt 整改后函数体行数普遍增长 10-20%, 原 100 行阈值过紧; 放宽至 200
#![allow(clippy::too_many_lines)]
// 11. Clippy: module_inception — 内核模块命名（如 fs/nestfs/nestfs.rs）是架构惯例
#![allow(clippy::module_inception)]
// 12. Clippy: new_without_default — 内核对象通常不应有无参默认构造
#![allow(clippy::new_without_default)]
// 13. Clippy: collapsible_if — 内核路径中的 if 嵌套有时是为了可读性
#![allow(clippy::collapsible_if)]
// 14. Clippy: single_match — match 单分支有时比 if-let 更清晰地表明穷尽性
#![allow(clippy::single_match)]
// 15. Clippy: too_many_arguments — 内核 API 参数数量由协议决定
#![allow(clippy::too_many_arguments)]
// 16. Clippy: type_complexity — 内核类型天然复杂（Box<dyn Fn> 等）
#![allow(clippy::type_complexity)]
// 17. Clippy: transmute_ptr_to_ptr — 内核 FFI 中的指针转换是显式约定
#![allow(clippy::transmute_ptr_to_ptr)]
// 18. Clippy: missing_transmute_annotations — 部分 transmute 转换缺显式标注, 全局豁免
#![allow(clippy::missing_transmute_annotations)]
// 19. Clippy: let_and_return — 内核错误路径中中间变量有助于可读性
#![allow(clippy::let_and_return)]
// 20. Clippy: wrong_self_convention — 内核 to_*/as_* 的 self 约定与 std 不同
#![allow(clippy::wrong_self_convention)]
// 21. Clippy: needless_range_loop — 内核中部分循环显式索引是有意可读性选择
#![allow(clippy::needless_range_loop)]
// 22. Clippy: manual_find — 显式循环比 .find() 在某些场景更清晰
#![allow(clippy::manual_find)]
// 23. Clippy: unnecessary_cast — 部分显式 cast 是内核惯例
#![allow(clippy::unnecessary_cast)]
// 24. Clippy: double_parens — 已迁出 (经脚本逐个审查确认无触发, 移除 allow)
// 25. Clippy: unnecessary_lazy_evaluations — 惰性求值显式化是内核可读性选择
#![allow(clippy::unnecessary_lazy_evaluations)]
// 26. Clippy: manual_div_ceil
#![allow(clippy::manual_div_ceil)]
// 27. Clippy: match_like_matches_macro — 已迁出 (经脚本逐个审查确认无触发, 移除 allow)
// 28. Clippy: manual_unwrap_or_default / manual_unwrap_or — 显式 match 更清晰
#![allow(clippy::manual_unwrap_or_default)]
#![allow(clippy::manual_unwrap_or)]
// 29. Clippy: unnecessary_map_or — 显式 map_or 可读性更好
#![allow(clippy::unnecessary_map_or)]
// 30. Clippy: derivable_impls — 部分内核 impl 有文档注释需要保留
#![allow(clippy::derivable_impls)]
// 31. Clippy: manual_checked_ops — 内核显式检查除法更直观
#![allow(clippy::manual_checked_ops)]
// 32. Clippy: question_mark — 内核错误路径保留显式 match 更清晰
#![allow(clippy::question_mark)]
// 33. Clippy: manual_range_patterns — 显式范围 vs range pattern 可读性各有优劣
#![allow(clippy::manual_range_patterns)]
// 34. Clippy: manual_flatten — 显式 if-let 比 .flatten() 更清晰
#![allow(clippy::manual_flatten)]
// 35. Clippy: collapsible_match — 保留嵌套 match 结构
#![allow(clippy::collapsible_match)]
// 36. Clippy: let_unit_value — 已迁出 (经脚本逐个审查确认无触发, 移除 allow)
// 37. Clippy: empty_loop — 内核自旋等待
#![allow(clippy::empty_loop)]
// 38. Clippy: explicit_counter_loop — 显式计数器在测试代码中更直观
#![allow(clippy::explicit_counter_loop)]
// 39. Clippy: pointers_in_nomem_asm_block — 内核 ASM 代码必须传指针
#![allow(clippy::pointers_in_nomem_asm_block)]
// 40. Clippy: empty_line_after_outer_attr — 已迁出 (经脚本逐个审查确认无触发, 移除 allow)
// 41. Clippy: doc_lazy_continuation / doc_overindented_list_items — 内核文档风格
#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::doc_overindented_list_items)]
// 42. Clippy: must_use_candidate — 内核内部 API 大量返回 Result/Option, 全标注 #[must_use] 会增加 200+ 行噪音
//     重要公共 API 在函数定义处已加 #[must_use]; 内部 helper 不强制
#![allow(clippy::must_use_candidate)]
// 43. Clippy: unreadable_literal — 内核硬件常量 (MMIO 地址/位掩码/魔数) 经常是固定位模式, 加下划线分隔反而降低可读性
//     与硬件规范直接对齐 (如 0xDEADBEEF, 0x74726976 = "virt" 小端); 改下划线会影响阅读与 SPEC 比对
#![allow(clippy::unreadable_literal)]
// 44. Clippy: inline_always — 内核大量 #[inline(always)] 是性能关键 (中断处理/锁内热路径); 全局保持显式标注
#![allow(clippy::inline_always)]
// 45. Clippy: large_stack_arrays — 已迁出 (经脚本逐个审查确认无触发, 移除 allow)
// 46. Clippy: struct_field_names / pub_underscore_fields — 结构体字段命名约定是模块内风格 (如 page_state 含 page_* 字段); pub _xxx 是内核模块内 convention
#![allow(clippy::struct_field_names)]
#![allow(clippy::pub_underscore_fields)]
// 47. Clippy: struct_excessive_bools — 状态标志结构体用多个 bool 字段是常见模式; 当前实现无需重构
#![allow(clippy::struct_excessive_bools)]
// 48. Clippy: doc_markdown — 内核文档使用中文 + 硬件术语 (MMIO/MSI/APIC 等) 不加反引号是约定; 阶段 3 已处理部分
#![allow(clippy::doc_markdown)]
// 49. Clippy: ptr_as_ptr — 部分 macro (如 klog_fmt) 内部 ptr cast 无法 expect 兜底; 真实代码 expect 已兑底
#![allow(clippy::ptr_as_ptr)]
// 50. Clippy: cast_ptr_alignment — 部分 MMIO 寄存器地址已知对齐 (硬件规范); macro 内 cast_ptr_alignment 无法 expect
#![allow(clippy::cast_ptr_alignment)]
// 51. Clippy: zero_sized_map_values / missing_fields_in_debug — 内核 struct 字段设计选择; 当前实现合理
#![allow(clippy::zero_sized_map_values)]
#![allow(clippy::missing_fields_in_debug)]
// 52. Clippy: cast_lossless — 已迁出 (经脚本逐个审查确认无触发, 移除 allow)
// 53. Clippy: duplicated_attributes — 已迁出 (经脚本逐个审查确认无触发, 移除 allow)

// B08-12 注: host-test (std) 下 alloc 经 extern prelude 可见, 此处无条件声明不会产生
// 重复 lang item — 之前观察到的 E0152 duplicate lang item 是 src/rust/.cargo/config.toml
// 的 build-std 配置在 src/rust 目录内运行时的假象 (host-tests 从仓库根/host-tests 构建时
// 不加载该 config). 门控掉 extern crate alloc 反而导致 std 模式下 alloc:: 路径不可解析.
extern crate alloc;

// B08-12: 全局分配器 (KernelAllocator) 为裸机专属 — host-test (std) 下禁用,
// 避免 Rust 全局分配 (含 std 自身初始化) 走内核 kmalloc → IrqSpinLock → cli (SIGSEGV).
#[cfg(not(feature = "host-test"))]
mod memory_allocator;
// ============================================================================
// 顶层声明: 仅 2 个目录
// ============================================================================

/// 框内核 Framework (TCB) — 唯一允许 unsafe 的模块
pub mod framework;

/// Services 层 — 去特权 100% safe Rust (框内核架构)
pub mod services;
// B08-12: 以下符号仅 panic_handler/kernel_test 路径使用, host-test 下为死代码
#[cfg(not(feature = "host-test"))]
use core::panic::PanicInfo;
#[cfg(not(feature = "host-test"))]
use core::sync::atomic::Ordering;
// B08-12: host-test 下 panic 由 std 提供, 内核 panic_handler 仅裸机/内核测试模式生效
#[cfg(not(feature = "host-test"))]
#[panic_handler]
#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
fn panic(info: &PanicInfo) -> ! {
    crate::framework::barrier::PANIC_FLAG.store(true, Ordering::SeqCst);

    // 诊断 (TRACK-INIT-RING3-PANIC): 先直接输出 panic location (file:line),
    // 绕过 PanicInfo::Display 格式化 (其内部 slice 索引在栈/数据被破坏时
    // 可能递归 panic → 无法看到原始 panic 点). location 是静态字符串, 不分配.
    if crate::framework::klog::KLOG_INIT.load(Ordering::Acquire) {
        if let Some(loc) = info.location() {
            crate::framework::klog::serial_write_bytes(b"\n[PANIC LOC] ");
            crate::framework::klog::serial_write_bytes(loc.file().as_bytes());
            crate::framework::klog::serial_write_bytes(b":");
            let mut line_buf = [0u8; 16];
            let mut idx = 16usize;
            let mut n = u64::from(loc.line());
            if n == 0 {
                line_buf[0] = b'0';
                idx = 0;
            } else {
                while n > 0 && idx > 0 {
                    idx -= 1;
                    line_buf[idx] = b'0' + (n % 10) as u8;
                    n /= 10;
                }
            }
            crate::framework::klog::serial_write_bytes(&line_buf[idx..]);
            crate::framework::klog::serial_write_bytes(b"\n");
        }
    }

    // 修复 (TRACK-INIT-RING3-PANIC): 中断上下文 panic 时禁止分配内存.
    // 原实现 `alloc::format!` 分配 String → k_malloc → KernelHeap IrqSpinLock,
    // 若 panic 发生在 IRQ 上下文 (如中断路径内存分配) 会递归 panic → 跳 0x0 #UD.
    // 改用栈缓冲 CursorWriter (framework::klog, 纯 core::fmt 不分配) 格式化消息.
    let mut msg_buf = [0u8; 256];
    let mut msg_cursor: usize = 0;
    let _ = core::fmt::write(
        &mut crate::framework::klog::CursorWriter::new(&mut msg_buf, &mut msg_cursor),
        format_args!("{info}"),
    );
    let msg: &str = core::str::from_utf8(&msg_buf[..msg_cursor]).unwrap_or("PANIC (fmt failed)");
    let bytes = msg.as_bytes();
    let len = bytes.len().min(127);
    {
        let mut panic_msg = crate::framework::barrier::PANIC_MSG.lock();
        panic_msg[..len].copy_from_slice(&bytes[..len]);
        panic_msg[len] = 0;
    }

    // 先捕获寄存器状态 — 仅 x86_64 可从 asm 读取, 其余架构保持零值
    #[cfg(target_arch = "x86_64")]
    let mut regs: [u64; 16] = [0; 16];
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!(
            "mov {0}, rax", "mov {1}, rbx", "mov {2}, rcx",
            "mov {3}, rdx", "mov {4}, rsi", "mov {5}, rdi",
            out(reg) regs[0], out(reg) regs[1], out(reg) regs[2],
            out(reg) regs[3], out(reg) regs[4], out(reg) regs[5],
            options(nostack, preserves_flags)
        );
        core::arch::asm!(
            "mov {0}, rbp", "mov {1}, rsp", "mov {2}, r8",
            "mov {3}, r9",  "mov {4}, r10", "mov {5}, r11",
            out(reg) regs[6], out(reg) regs[7], out(reg) regs[8],
            out(reg) regs[9], out(reg) regs[10], out(reg) regs[11],
            options(nostack, preserves_flags)
        );
        core::arch::asm!(
            "mov {0}, r12", "mov {1}, r13", "mov {2}, r14", "mov {3}, r15",
            out(reg) regs[12], out(reg) regs[13], out(reg) regs[14], out(reg) regs[15],
            options(nostack, preserves_flags)
        );
    }
    #[cfg(not(target_arch = "x86_64"))]
    let regs: [u64; 16] = [0; 16];
    let reg_names: [[u8; 4]; 16] = [
        *b"RAX ", *b"RBX ", *b"RCX ", *b"RDX ", *b"RSI ", *b"RDI ", *b"RBP ", *b"RSP ", *b"R8  ",
        *b"R9  ", *b"R10 ", *b"R11 ", *b"R12 ", *b"R13 ", *b"R14 ", *b"R15 ",
    ];
    // cr2/cr3 仅 x86_64 有对应寄存器, 其余架构保持 0
    #[cfg(target_arch = "x86_64")]
    let (cr2, cr3_val): (u64, u64) = unsafe {
        let mut cr2: u64 = 0;
        let mut cr3_val: u64 = 0;
        core::arch::asm!("mov {}, cr2", out(reg) cr2);
        core::arch::asm!("mov {}, cr3", out(reg) cr3_val);
        (cr2, cr3_val)
    };
    #[cfg(not(target_arch = "x86_64"))]
    let (cr2, cr3_val): (u64, u64) = (0, 0);

    // 1. 串口输出崩溃信息
    if crate::framework::klog::KLOG_INIT.load(Ordering::Acquire) {
        crate::framework::klog::serial_write_bytes(b"\n========== KERNEL PANIC ==========\n");
        crate::framework::klog::serial_write_bytes(msg.as_bytes());
        crate::framework::klog::serial_write_bytes(b"\n--- Register Dump ---\n");
        for i in 0..16 {
            crate::framework::klog::serial_write_bytes(b"  ");
            crate::framework::klog::serial_write_bytes(&reg_names[i]);
            crate::framework::klog::serial_write_bytes(b"= 0x");
            let mut hex_buf = [0u8; 16];
            let v = regs[i];
            for (d, item) in hex_buf.iter_mut().enumerate() {
                let nibble = ((v >> (60 - d * 4)) & 0xF) as u8;
                *item = if nibble < 10 {
                    b'0' + nibble
                } else {
                    b'a' + nibble - 10
                };
            }
            crate::framework::klog::serial_write_bytes(&hex_buf);
            if i % 4 == 3 {
                crate::framework::klog::serial_write_bytes(b"\n");
            }
        }
        crate::framework::klog::serial_write_bytes(b"  CR2= 0x");
        for d in 0..16 {
            let nibble = ((cr2 >> (60 - d * 4)) & 0xF) as u8;
            crate::framework::klog::serial_write_bytes(&[if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            }]);
        }
        crate::framework::klog::serial_write_bytes(b"  CR3= 0x");
        for d in 0..16 {
            let nibble = ((cr3_val >> (60 - d * 4)) & 0xF) as u8;
            crate::framework::klog::serial_write_bytes(&[if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            }]);
        }
        crate::framework::klog::serial_write_bytes(b"\n===================================\n");
    }

    // 2. 图形控制台输出崩溃信息
    crate::framework::console::gfx_console_panic_reclaim(msg);
    crate::framework::console::gfx_console_panic_write("\n--- Register Dump ---\n");
    for i in 0..16 {
        let mut buf = [0u8; 64];
        let mut cursor: usize = 0;
        write_hex_to_buf(&mut buf, &mut cursor, regs[i]);
        // 修复 (TRACK-INIT-RING3-PANIC): 原 `alloc::format!` 在中断上下文 panic
        // 时分配内存 → 递归 panic → 栈破坏. 改用栈缓冲 CursorWriter (不分配).
        let mut line_buf = [0u8; 64];
        let mut line_cur = 0usize;
        let _ = core::fmt::write(
            &mut crate::framework::klog::CursorWriter::new(&mut line_buf, &mut line_cur),
            format_args!(
                "  {} = {}\n",
                core::str::from_utf8(&reg_names[i]).unwrap_or("?? "),
                core::str::from_utf8(&buf[..cursor]).unwrap_or("?")
            ),
        );
        let line = core::str::from_utf8(&line_buf[..line_cur]).unwrap_or("?\n");
        crate::framework::console::gfx_console_panic_write(line);
    }
    {
        let mut cr2_str = [0u8; 32];
        let mut cur: usize = 0;
        write_hex_to_buf(&mut cr2_str, &mut cur, cr2);
        let mut line_buf = [0u8; 40];
        let mut line_cur = 0usize;
        let _ = core::fmt::write(
            &mut crate::framework::klog::CursorWriter::new(&mut line_buf, &mut line_cur),
            format_args!(
                "  CR2= {}\n",
                core::str::from_utf8(&cr2_str[..cur]).unwrap_or("?")
            ),
        );
        let line = core::str::from_utf8(&line_buf[..line_cur]).unwrap_or("?\n");
        crate::framework::console::gfx_console_panic_write(line);
        let mut cr3_str = [0u8; 32];
        let mut c3: usize = 0;
        write_hex_to_buf(&mut cr3_str, &mut c3, cr3_val);
        let mut line_buf2 = [0u8; 40];
        let mut line_cur2 = 0usize;
        let _ = core::fmt::write(
            &mut crate::framework::klog::CursorWriter::new(&mut line_buf2, &mut line_cur2),
            format_args!(
                "  CR3= {}\n",
                core::str::from_utf8(&cr3_str[..c3]).unwrap_or("?")
            ),
        );
        let line2 = core::str::from_utf8(&line_buf2[..line_cur2]).unwrap_or("?\n");
        crate::framework::console::gfx_console_panic_write(line2);
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("int 0x82", options(noreturn));
    }
    #[cfg(target_arch = "aarch64")]
    {
        // AArch64 栏栈恢复: 直接调用恢复逻辑进行域回滚
        unsafe extern "C" {
            fn recovery_try_recover_from_idt() -> i32;
        }
        let result = unsafe { recovery_try_recover_from_idt() };
        if result >= 0 {
            // 域状态已回滚到一致快照, 记录恢复事件
            crate::framework::klog::serial_write_bytes(
                b"\n[RECOVERY] Barrier-stack: domain rolled back\n",
            );
            crate::framework::barrier::PANIC_FLAG
                .store(false, core::sync::atomic::Ordering::SeqCst);
        } else {
            crate::framework::klog::serial_write_bytes(
                b"\n[RECOVERY] Barrier-stack: recovery failed, halting\n",
            );
        }
        loop {
            unsafe {
                core::arch::asm!("wfi");
            }
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

// B08-12: 仅 panic_handler 使用, host-test 下为死代码
#[cfg(not(feature = "host-test"))]
fn write_hex_to_buf(buf: &mut [u8], cursor: &mut usize, value: u64) {
    for d in 0..16 {
        if *cursor >= buf.len() {
            break;
        }
        let nibble = ((value >> (60 - d * 4)) & 0xF) as u8;
        buf[*cursor] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
        *cursor += 1;
    }
}

#[cfg(not(test))]
// B08-12: host-test 下 std 自带 alloc error handler, 内核版仅裸机/内核测试模式生效
#[cfg(not(feature = "host-test"))]
#[alloc_error_handler]
fn alloc_error(layout: alloc::alloc::Layout) -> ! {
    panic!("Allocation error: {layout:?}");
}

/// 内核启动入口 (引导跳转目标).
///
/// 按依赖顺序初始化各子系统: `KLog` → Boot 栈 canary 校验 → 配置校验 →
/// 架构初始化 → 内存/进程/网络等子系统 → 进入用户态.
///
/// # Panics
/// Boot 栈 canary 校验失败 (栈溢出至栈底) 时立即 panic, 断言内核状态不可信.
#[unsafe(no_mangle)]
// J-01 (2026-09-08): used_underscore_binding expect 仅裸机分支 (not kernel_test) 生效 —
// kernel_test 分支无 `_xxx` 绑定使用, expect 在 feature 下 unfulfilled.
// T1 G7 host 符号桩化: 真机门控推广为 not(any(kernel_test, host-test)),
// 该 expect 的触发代码同属真机分支, 条件同步推广.
#[cfg_attr(
    not(any(feature = "kernel_test", feature = "host-test")),
    expect(
        clippy::used_underscore_binding,
        reason = "下划线前缀表示私有约定或局部清理; 重命名需追改所有访问点, 风险高"
    )
)]
#[expect(
    clippy::too_many_lines,
    reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
)]
// 符号桩化 (host-test): 触发 unreadable_literal 的硬件常量位于真机引导块,
// host-test 维不再触发, expect 须同步收窄.
#[cfg_attr(
    not(feature = "host-test"),
    expect(
        clippy::unreadable_literal,
        reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
    )
)]
pub extern "C" fn kernel_init() {
    // 0. KLog — 自举串口驱动, 必须先于所有子系统
    unsafe {
        crate::framework::klog::klog_init();
    }
    crate::klog_boot_info!("QueenX starting");

    // 0.05. Boot 栈 canary 验证 — 检测 trampoline → kernel_init 路径上的栈溢出.
    // canary 在 boot.asm trampoline64_high (x86_64) 或 entry.rs (aarch64) 写入 stack_bottom,
    // 若被覆盖则说明 boot 栈已溢出至栈底, 内核状态不可信, 立即 panic.
    assert!(
        crate::framework::proc::check_boot_stack_canary(),
        "[BOOT] stack canary corrupted! Boot stack overflow detected. \
         Stack size=256KB, canary at stack_bottom was overwritten \
         during trampoline→kernel_init transition."
    );
    crate::klog_boot_info!("Boot stack canary verified");

    // 0.05. ConfigValidateHook 注册契约点 (DECISION-K 项 2: 机制 init 后立即注册策略)
    // 注册在 framework config::init() 之前 — services validate 依赖闭包轻 (smp/arch
    // 查询 + slog), 可极早注册; 未注册时 config::init() 跳过校验 + 日志 (启动增强,
    // 逻辑错误降级原则, 不 panic)。
    crate::services::config::validate::register_default_config_validate_hook()
        .expect("config validate hook registered (kernel_init 早期契约点)");

    // 0.1. Config validation — 验证系统配置一致性
    // 必须在 klog_init 之后调用, 以便错误上报
    crate::framework::config::init();
    crate::klog_boot_info!("Configuration validated");

    // 测试模式: 跳过常规初始化, 运行单元测试
    #[cfg(feature = "kernel_test")]
    {
        // J-01 (2026-09-08): 三个 const 集中块首 (items_after_statements 清理) —
        // kmalloc 堆大小与 PMM bitmap 间隙布局常量, 供下方初始化步骤使用
        const KMALLOC_HEAP_SIZE: u64 = 16 * 1024 * 1024;
        const GAP_SIZE: u64 = 0x200000;
        const BITMAP_GAP_SIZE: u64 = 0x200000;

        // 测试模式下也校验配置一致性
        crate::framework::config::init();

        <crate::framework::arch::CurrentArch as crate::framework::arch::InterruptArch>::interrupt_disable(
        );

        let boot_info = crate::framework::boot::init();
        crate::framework::mm::pmm::pmm_init(boot_info.mem_size, boot_info.kernel_end);
        crate::framework::mm::vmm::vmm_init();
        let heap_start = crate::framework::mm::VirtAddr(
            crate::framework::mm::KERNEL_BASE + boot_info.kernel_end + 0x200000,
        );

        crate::framework::mm::kmalloc::get_kmalloc_mut().init(heap_start, KMALLOC_HEAP_SIZE);

        // 诊断: kmalloc init 后检查页表
        {
            let read_u64 = |phys: u64, idx: usize| -> u64 {
                let va = phys + crate::framework::mm::KERNEL_BASE + idx as u64 * 8;
                unsafe { core::ptr::read_volatile(va as *const u64) }
            };
            let pd24 = read_u64(0x109000, 24);
            let pd63 = read_u64(0x109000, 63);
            crate::klog_boot_info!(
                "[PAGETABLE] after kmalloc: pd[24]=0x{:016X} pd[63]=0x{:016X}",
                pd24,
                pd63
            );
        }
        // 必须包含 kernel_end 到 heap_start 之间的 2MB 间隙，
        // 否则 PMM bitmap 会放在 kmalloc 堆内部，导致 bitmap 与堆数据互相覆盖。
        // 必须包含 heap_end 到 bitmap 之间的 2MB 间隙，
        // 否则 bitmap 与 heap 共享同一个 2MB 块，heap 扩展拆分 2MB 巨页时会覆盖 bitmap 的 PTE。
        // GAP_SIZE + KMALLOC_HEAP_SIZE + BITMAP_GAP_SIZE = 0x200000 + 16MB + 0x200000 = 20MB
        crate::framework::mm::pmm::pmm_init_bitmap(GAP_SIZE + KMALLOC_HEAP_SIZE + BITMAP_GAP_SIZE);

        // 诊断: dump 页表关键条目 (PML4[256]→pdpt_high[0]→pd[24]/[63])
        {
            let read_u64 = |phys: u64, idx: usize| -> u64 {
                let va = phys + crate::framework::mm::KERNEL_BASE + idx as u64 * 8;
                unsafe { core::ptr::read_volatile(va as *const u64) }
            };
            let pml4_256 = read_u64(0x102000, 256);
            let pdpt_0 = read_u64(0x104000, 0);
            let pd24 = read_u64(0x109000, 24);
            let pd63 = read_u64(0x109000, 63);
            let pd0 = read_u64(0x109000, 0);
            crate::klog_boot_info!(
                "[PAGETABLE] pml4[256]=0x{:016X} pdpt[0]=0x{:016X} pd[0]=0x{:016X} pd[24]=0x{:016X} pd[63]=0x{:016X}",
                pml4_256,
                pdpt_0,
                pd0,
                pd24,
                pd63
            );
        }

        // I-24 启动顺序契约: GDT/TSS init (set_ist[0..4]) 必须在 IDT init 之前.
        // cpu_init() 在 gdt_init() 之前调用 (kpti_init 依赖 has_invpcid() → get_cpu_info() → cpu_init).
        // 正常路径由 interrupt_late_init 处理; 测试模式需显式调用.
        #[cfg(target_arch = "x86_64")]
        {
            crate::framework::cpu::cpu_init();
            crate::framework::arch::x86_64::gdt::gdt_init();
        }

        <crate::framework::arch::CurrentArch as crate::framework::arch::Arch>::interrupt_early_init(
        );
        crate::klog_boot_info!("Test mode: interrupt early init done");

        crate::framework::smp::init();
        crate::klog_boot_info!("Test mode: SMP BSP registered");
        crate::framework::proc::scheduler::init();
        crate::klog_boot_info!("Test mode: Scheduler ready");

        #[cfg(feature = "fault_injection")]
        {
            let rate = option_env!("FAULT_RATE")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(50);
            crate::framework::barrier::fault_inject::FAULT_INJECTION_RATE
                .store(rate, core::sync::atomic::Ordering::Relaxed);
            crate::klog_boot_info!("[CHAOS] Fault injection enabled, rate={}/1000", rate);
        }

        crate::framework::tests::test_runner_init();
        crate::klog_boot_info!("Tests complete");

        let r = crate::framework::tests::runner();
        let failed = r.failed.load(Ordering::SeqCst);
        crate::framework::debug::qemu_exit(failed == 0);
    }

    // 1. Boot Info — 获取内存布局
    // 门控语义 (T1 G7 host 符号桩化): 真机路径 = not(any(kernel_test, host-test)),
    // kt/ht 同属"测试环境" (E-03 约定扩展). host-test 下不编译裸机引导链.
    #[cfg(not(any(feature = "kernel_test", feature = "host-test")))]
    {
        let boot_info = crate::framework::boot::init();
        crate::klog_boot_info!(
            "Boot info: mem={} MB, kernel_end=0x{:X}",
            boot_info.mem_size / (1024 * 1024),
            boot_info.kernel_end
        );

        // 2. PMM — 物理内存管理器初始化
        crate::framework::mm::pmm::pmm_init(boot_info.mem_size, boot_info.kernel_end);
        crate::klog_boot_info!("PMM initialized");

        // 3. VMM — 虚拟内存管理器初始化 (必须在PMM之后)
        crate::framework::mm::vmm::vmm_init();
        crate::klog_boot_info!("VMM initialized");

        // 3.1 VMM 初始化后验证: 确保 GLOBAL_VMM OnceLock 已正确完成初始化.
        // 若 VMM init 内部静默失败 (如页错误导致 OnceLock 状态停留在 IN_PROGRESS),
        // 此处提前 panic 并给出明确诊断信息, 避免后续 get_vmm() 时信息不足.
        let vmm_state = crate::framework::mm::vmm::vmm_debug_state();
        assert!(
            vmm_state == 2,
            "[VMM] initialization verification failed: OnceLock state={vmm_state} (expected 2=DONE). \
             VMM init may have panicked or been interrupted."
        );

        // 4. kmalloc — 内核堆初始化
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        const KMALLOC_HEAP_SIZE: u64 = 8 * 1024 * 1024; // 8 MB (b4d: 实测全程峰值 16KB, 由 16MB 缩容)
        // 双架构统一为 KERNEL_BASE + 物理偏移: 堆的**物理布局**不变
        // (仍自 kernel_end + 0x200000 起), 仅访问别名改走高半区直射区,
        // 从而在 TTBR0/CR3 切至 per-process 视图时堆仍经内核高半区可达
        // (aarch64 原先直取物理地址, 依赖低半区恒等, 见 L1-05)。
        let heap_start = crate::framework::mm::VirtAddr(
            crate::framework::mm::KERNEL_BASE + boot_info.kernel_end + 0x200000,
        );

        crate::framework::mm::kmalloc::get_kmalloc_mut().init(heap_start, KMALLOC_HEAP_SIZE);

        crate::klog_boot_info!(
            "kmalloc initialized at 0x{:X}, size={} MB",
            heap_start.0,
            KMALLOC_HEAP_SIZE / (1024 * 1024)
        );

        // 5. PMM Bitmap — 初始化位图分配器
        // 必须包含 kernel_end 与 heap_start 之间的 2MB 间隙,
        // 否则 PMM 位图会从 kmalloc 堆内分配
        // (页 7165+), alloc_table() 清零新分配页表页时会导致堆破坏
        //
        // 还必须包含 heap_end 与位图之间的 2MB 间隙,
        // 否则位图与堆共享 2MB 大页, 堆
        // 扩容时的 2MB 大页拆分会覆盖位图 PTE.
        // GAP_SIZE + KMALLOC_HEAP_SIZE + BITMAP_GAP_SIZE = 0x200000 + 8MB + 0x200000 = 10MB
        // 注: 堆扩容自 heap_end 向上映射, 仅能吃到尾部 BITMAP_GAP_SIZE(2MB),
        // 故堆最大容量 = KMALLOC_HEAP_SIZE + BITMAP_GAP_SIZE = 10MB.
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        const GAP_SIZE: u64 = 0x200000;
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        const BITMAP_GAP_SIZE: u64 = 0x200000;
        let reserved_after_kernel = GAP_SIZE + KMALLOC_HEAP_SIZE + BITMAP_GAP_SIZE;
        crate::framework::mm::pmm::pmm_init_bitmap(reserved_after_kernel);
        crate::klog_boot_info!("PMM bitmap initialized");

        // --- 栏栈恢复域 (前移至中断使能前, 避免竞态) ---
        // 在中断使能前注册 PMM + PROC 域, 使定时器 IRQ
        // 不会与 RECOVERY_MANAGER 自旋锁上的域注册竞态
        crate::framework::mm::pmm::pmm_register_barrier_domain();
        crate::framework::proc::process::proc_register_barrier_domain();
        #[cfg(target_arch = "aarch64")]
        unsafe {
            crate::framework::arch::aarch64::barrier::enable_barrier_sgi();
        }
        crate::klog_boot_info!("Barrier-stack recovery domains registered (PMM=3, PROC=4)");

        // 5.5. Swap — 物理内存回收/换出 (B3 完整实现)
        // 必须在 PMM + VMM + kmalloc 初始化之后 (使用 pmm/vmm 接口)
        // 必须在 interrupt_late_init 之前 (softirq 注册依赖 IRQ 子系统)
        // 实际上 softirq 是 static handler 表, 不强制 init 顺序, 但保持 init 流程清晰:
        //   swap_init (PMM 之后) → kswapd_init (interrupt_late_init 之后, scheduler tick 之前)
        if crate::services::mm::swap::swap_init() {
            crate::klog_boot_info!("Swap subsystem initialized");
        } else {
            crate::klog_boot_info!("Swap subsystem init FAILED (degraded mode)");
        }

        // 5.75. IPC 策略注册契约点 (DECISION-K 2026-09-12: 注册点前置)
        // 紧随 framework ipc_init() 后立即 (kernel_init 早期, 删去 VFS 后约束) —
        // DefaultIpcStrategy 零字段构造 + static 零初始化 + OnceLock 存指针, 注册零
        // 依赖; 策略方法惰性调用 (用户态 syscall 才执行 *_safe, 彼时 VFS 早已就绪),
        // 注册点与调用点分离。未注册降级: FFI 返回 ENOSYS + 日志 (逻辑错误降级原则)。
        crate::services::ipc::strategy::register_default_ipc_strategy()
            .expect("ipc strategy registered (kernel_init 早期契约点)");

        // 6. 中断/异常设置
        <crate::framework::arch::CurrentArch as crate::framework::arch::Arch>::interrupt_late_init(
        );
        crate::klog_boot_info!("Interrupt subsystem ready");

        // 6.5. kswapd softirq 注册 (依赖 IRQ 子系统, scheduler tick 触发 wakeup)
        crate::services::mm::swap::kswapd_init();

        // 7. Timer 初始化 (中断延后到网络就绪后启用)
        match crate::framework::timer::timer_init(1000) {
            Ok(_freq) => {
                crate::klog_boot_info!("Timer configured");
                #[cfg(target_arch = "x86_64")]
                let _ = crate::framework::timer::irq::register_timer_irq();
            }
            Err(_msg) => {
                let _ = _msg;
            }
        }

        // 8. Scheduler
        crate::framework::proc::scheduler::init();
        crate::framework::proc::scheduler_ex::init();
        crate::klog_boot_info!("Scheduler ready");

        // 8-1. eBPF — services::debug::ebpf::init() 注册契约 (DECISION-K 统一
        // 模式): bpf_init (幂等) + 标准 verifier 注册 (T4-3 Safe Policy
        // Injection). 预存欠账修复: 此前 verifier 注册位于 scheduler_init FFI
        // (无生产调用者), 生产环境 verifier 从未注册 — 本行接通注册链路.
        crate::services::debug::ebpf::init();

        // 9. VFS
        // services::fs::init() — FsBackend 策略 + VFS poll 策略 + NestFS
        // FileSystem/热插拔监听器注册 (DECISION-K 项 6 注册点前置)。预存欠账:
        // 此前 services::fs::init() 无调用者, make_ramfs_inode 钩子 (第二十三批)
        // 恒命中 FallbackFsBackend Err(NotInitialized), ramfs open/create 路径
        // 在生产环境被回退策略拦截 — 本行注册为回归修复。
        crate::services::fs::init();
        crate::services::fs::vfs_manager::init();
        crate::klog_boot_info!("VFS ready");

        // 9-1. UDS (AF_UNIX) — Phase C.3
        crate::services::net::unix::uds_init();
        crate::klog_boot_info!("UDS subsystem initialized");

        // 10. Network (smoltcp + 网卡驱动)
        {
            // 阶段 3 (驱动双份合并): services 网络驱动权威 (e1000 + virtio-net
            // 复合探测) 注册探测回调槽 (services→framework 单向), 实际设备
            // 探测在 qx_net_init → nic_probe_all 经槽位单向拉取。
            crate::services::driver::net::net_init();

            crate::framework::net::init::qx_net_init();

            crate::klog_boot_info!("Network subsystem initialized");
        }

        // 10-10.6. Driver subsystem init
        crate::framework::driver::init_all();
        // §6.4 直接方案 B: x86_64 字符设备 (vga/serial) 权威迁 services,
        // 由 crate root (合法双向编排者) 调用 services char_init 注册进 Chitin.
        #[cfg(target_arch = "x86_64")]
        crate::services::driver::char::char_init();
        // §6.4 直接方案 B: virtio-blk 权威迁 services (aarch64 QEMU -M virt 主战场;
        // x86_64 走 PCI AHCI/NVMe, 此调用探测 virtio-mmio 无设备即跳过)
        crate::services::driver::virtio::blk_init();
        // DECISION-H storage 专项 3 号子步: PCI AHCI/NVMe 权威迁 services,
        // 由 crate root (合法双向编排者) 调用 services storage_init 接管控制器
        // 初始化与 Chitin 注册 (framework storage_init 仅余 ATA 回退路径)。
        #[cfg(target_arch = "x86_64")]
        crate::services::driver::storage::storage_init();
        // 2-D USB 整体下沉: xHCI 探测/注册权威实装于 services::driver::usb,
        // 由 crate root (合法双向编排者) 调用 services usb_init 接管
        // PCI 发现 + Chitin 注册 (framework 侧 usb 模块已删除)。
        crate::services::driver::usb::usb_init();
        // MIG-008 接线补齐: 显示控制器 (HDMI/DP/DisplayManager) 权威实装于
        // services::driver::display, 由 crate root (合法双向编排者) 调用
        // services display_init 注册工厂回调, 再经 framework display_probe_controllers
        // 单向拉取触发 Chitin 注册 (DECISION-K 单向注册契约)。
        crate::services::driver::display::display_init();
        crate::framework::driver::display_probe_controllers();
        crate::klog_boot_info!("Driver subsystem initialized");
        {
            let chitin_count = crate::framework::chitin::chitin_count() as u64;
            let block_count = crate::framework::chitin::chitin_count_by_proto(
                crate::framework::chitin::ChitinProto::Block,
            ) as u64;
            let net_count = crate::framework::chitin::chitin_count_by_proto(
                crate::framework::chitin::ChitinProto::Net,
            ) as u64;
            let input_count = crate::framework::chitin::chitin_count_by_proto(
                crate::framework::chitin::ChitinProto::Input,
            ) as u64;
            crate::klog_boot_info!(
                "Chitin: {} device(s) [blk={} net={} input={}]",
                chitin_count,
                block_count,
                net_count,
                input_count
            );
        }

        // NestFS + 磁盘挂载 — BlockDevice 注册表自动发现多块磁盘 (支持 ATA/NVMe/virtio-blk)
        // 门控语义 (T1 G7): 与上方真机引导块一致, 真机路径 = not(any(kernel_test, host-test)).
        #[cfg(all(
            not(any(feature = "kernel_test", feature = "host-test")),
            target_arch = "x86_64"
        ))]
        {
            let nestfs = crate::services::fs::nestfs::nestfs::get_nestfs();
            // init() 会自动扫描所有块设备, 发现 QueenX 签名的磁盘并挂载
            nestfs.init();

            if nestfs.is_disk_mode() {
                crate::services::fs::nestfs::nestfs::get_nestfs()
                    .spa
                    .disk_present
                    .store(true, core::sync::atomic::Ordering::Release);
                let r =
                    crate::services::fs::api::vfs_mount_internal(b"/".as_ptr(), b"nestfs".as_ptr());
                if r == 0 {
                    let n_drives = crate::services::fs::nestfs::nestfs::get_nestfs()
                        .drives_discovered
                        .lock()
                        .len() as u64;
                    if n_drives > 1 {
                        crate::klog_boot_info!("Root filesystem: NestFS ({} drives)", n_drives);
                    } else {
                        crate::klog_boot_info!("Root filesystem: NestFS (disk)");
                    }
                } else {
                    crate::klog_boot_info!("NestFS mount failed");
                }
            } else {
                crate::klog_boot_info!("NestFS: running in memory mode (no disk)");
            }
        }

        // 启动定时器 (延迟到所有子系统初始化完成后)
        #[cfg(target_arch = "aarch64")]
        {
            let interval = crate::framework::arch::aarch64::exception::TIMER_INTERVAL_TICKS
                .load(core::sync::atomic::Ordering::Relaxed);
            crate::framework::arch::aarch64::timer::start_interval(interval);
        }

        // 11-1. 运行期跨核 TLB 失效探针 (S-13) — 多核已上线 (interrupt_late_init
        // 完成 AP 启动) 且各子系统就绪后一次性自检, 进入用户态前完成.
        // 判别力来源与注入覆盖见 docs/plan/tlb-shootdown-epoch.md §5 注入 3.
        #[cfg(target_arch = "x86_64")]
        if !crate::framework::mm::tlb_probe_selftest() {
            crate::klog_kern_warn!(
                "[SMP] TLB probe FAILED — cross-core TLB invalidation unreliable"
            );
        }

        crate::klog_boot_info!("QueenX initialized, entering user mode...");

        // 11. Syscall 子系统初始化 (必须在 interrupt_late_init 之后, launch_first_user_process 之前)
        // 11a. framework 层: MSR/STAR/LSTAR 配置 + epoll 回调注册
        crate::framework::syscall_init::syscall_init();
        // 11b. services 层: 系统调用分发策略注册
        crate::services::syscall::init();
        crate::klog_boot_info!("Syscall subsystem ready");

        // 11.5. 进入 Ring 3 前最终 boot 栈 canary 验证.
        // 内核初始化全程 (PMM→VMM→kmalloc→中断→调度→网络→VFS→驱动→syscall)
        // 均在 boot 栈上运行, 此处做最终溢出检测, 确保进入用户态前栈完整性.
        assert!(
            crate::framework::proc::check_boot_stack_canary(),
            "[BOOT] stack canary corrupted before Ring 3 entry! \
             Boot stack overflow during kernel init sequence. \
             Stack size=128KB."
        );
        crate::klog_boot_info!("Boot stack canary verified (pre-Ring3)");

        // 12. Launch first user process

        crate::services::init::launch_first_user_process();

        // 不可达: launch_first_user_process 不会返回
    } // kernel_test 分支结束
}
