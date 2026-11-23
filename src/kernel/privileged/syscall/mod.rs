pub mod api;
pub mod dispatch;
/// T-03: 系统调用分发决策 trait
pub mod dispatch_trait;
pub mod epoll;
pub mod futex;
pub mod madvise_mlock;
pub mod timerfd;

/// Syscall 模块 — `Edgine` 原生系统调用分发
///
/// 编号空间 (DECISION-037, 2026-08-03):
///   0-299   : 直接使用 Linux 标准 syscall 编号 (POSIX/syscall 透明兼容)
///   300-399 : 保留
///   400-499 : SGEG 私有 syscall
///   500-599 : Edgine 自由 syscall (EG_*) — 进程 / 内存 / 文件基础
///   600-699 : Edgine 自由 syscall (EG_*) — 网络 / IPC
///   700-799 : Edgine 自由 syscall (EG_*) — 设备 / 系统
///   800-899 : Edgine 自由 syscall (EG_*) — 扩展
///
/// 0-299 直接走 Linux ABI, 无翻译层. 500+ 与 Linux 错开, 避免与未来 Linux 新增 syscall 冲突.
///
/// # 双层分发契约 (B09-18, docs/plan/archive/syscall-dispatch-cleanup.md)
///
/// 调用链: 中断入口 → functions 策略分发 (优先) → 返回 ENOSYS 时 privileged 回退层兜底.
///
/// - **回退层内容限定**: 仅允许 "框架机制资源 (EG_*) + 未迁移项 (SYS_*/SGEG/FB) + ENOSYS 哨兵",
///   **禁止新增与 functions 重叠的真实实现分支** (重叠分支 = functions 优先命中下的死代码).
/// - **新 syscall 归属规范**: Linux 标准编号 (`SYS_*`) → 实现在 functions;
///   EG 独有编号 (`EG_*`) 按"机制资源 / 处理策略"二分归属:
///   框架持有的机制资源 (页表 / 中断 / 上下文 / 硬件原语) 留 privileged 回退层;
///   仅做参数校验 + 编排的处理策略 (可 0-unsafe 且不直接调用 privileged 内部机制) 迁 functions.
/// - 编号常量唯一定义于 `types.rs` (B09-17 归位), 禁止在其他文件重复定义.
// 公共接口 re-export — 避免跨子系统直接访问内部子模块
pub use epoll::{EPOLLERR, EPOLLHUP, EPOLLIN, EPOLLOUT, EPOLLRDHUP, epoll_pwake};
// timerfd 槽位机制 — 供 functions 侧 `TimerFdFile` 对象消费 (fd 对象模型阶段 3);
// 槽位表与 HrTimer 必须留在 privileged 静态存储, 故以安全槽位 API 作公开出口.
pub use timerfd::{
    SetTimeError, TFD_MAX_SLOTS, TakeExpiry, alloc_slot, free_slot, get_time, poll_ready, set_time,
    set_wake_fd, take_expiry,
};
pub use types::Errno;
pub use types::*;

// dispatch_trait 公共接口 re-export - T-03 策略-机制分离
pub use dispatch_trait::{
    FallbackSyscallDispatch, SyscallDispatch, current_syscall_dispatch, register_syscall_dispatch,
};
pub mod types;

pub use dispatch::*;

/// 校验单个用户态指针是否落在用户可访问地址范围内.
#[must_use]
pub fn validate_user_ptr(ptr: u64) -> bool {
    crate::privileged::userptr::validate_user_ptr(ptr)
}

/// 校验用户态缓冲区 (基址 + 长度) 是否完整落在用户可访问地址范围内.
#[must_use]
pub fn validate_user_buf(ptr: u64, len: u64) -> bool {
    crate::privileged::userptr::validate_user_buf(ptr, len)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
///
/// # Safety
///
/// 调用者处于内核上下文. `ptr` 是已校验的用户态指针.
pub unsafe extern "C" fn syscall_init() {
    // SAFETY: klog_write 是 C-ABI 日志函数；byte string literal 是 'static
    // 字节切片，传递给 C 时按指针 + 长度使用。
    unsafe {
        crate::privileged::klog::klog_write(
            1,
            7,
            core::ptr::null(),
            core::ptr::null(),
            0,
            b"POSIX syscall subsystem ready".as_ptr(),
        );
    }

    // 注册 epoll 的 fd 关闭通知回调, 解耦 fs→syscall 依赖
    // SAFETY: epoll_pwake 是 'static 函数指针, 在内核运行期间始终有效.
    unsafe {
        crate::privileged::fd_notify::register_pwake(
            crate::privileged::syscall::epoll::epoll_pwake,
        );
    }
}

// ============================================================================
// raw 子模块 — 集中所有 unsafe 操作与 FFI 声明
// ============================================================================
//
// 设计目的：
// 1. 隔离 unsafe 到单一文件作用域，降低 sys_* 业务函数的认知负载
// 2. 复用 functions/sgeg、functions/freg 的"raw 子模块"模式
// 3. 为 Phase 2.5.1 的 60+ unsafe 函数提供统一的 SAFETY 注释入口
//
// 调用契约：
// - 所有 read_* / write_* 函数均要求调用方先调用 check_user_ptr 或
//   check_user_buf 完成边界校验；否则会触发 UAF/越界写。
// - 所有 FFI 包装函数 (sm_*_call、read_keyboard_byte 等) 假定在中断
//   上下文中调用，不可在持锁睡眠上下文中调用。
// ============================================================================

/// 特权态原始访问接口: 集中 syscall 侧的全部 unsafe FFI 声明与用户内存读写原语.
pub mod raw {
    // ============= 集中 FFI 声明 =============
    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    unsafe extern "C" {
        // 时间
        fn timer_get_ticks() -> u64;
        // smoltcp 网络栈 — 已迁移到 net_socket.rs 路径
        // 链接器符号声明按"符号存在性"门控: `_kernel_start` / `_kernel_end` 由
        // x86_64.ld 与 aarch64.ld 共同定义 (kernel_test 镜像同源链接), 唯一不存在
        // 该符号的构建是 host-test (无 ld 脚本产物), 故仅排除该维; 与 `stack_bottom`
        // (仅 not(host-test)) / `USER_CR3_SAVE_ASM` (arch 项编码"只此架构存在") 同构.
        // host 守卫不受影响: 引用点函数体内为 host 分叉, host 侧不引用该符号,
        // 误引用即编译期 E0425. 未被引用的维 (kernel_test / aarch64) 声明不产生 lint
        // 告警, 也不产生符号引用 (实测 audit quick 四维全绿). 块内 timer_get_ticks 不受影响.
        #[cfg(not(feature = "host-test"))]
        static _kernel_start: u8;
        #[cfg(not(feature = "host-test"))]
        static _kernel_end: u8;
    }

    // x86_64 专属: 键盘 — kernel_test 模式下不接触真实硬件
    #[cfg(all(target_arch = "x86_64", not(feature = "kernel_test")))]
    // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
    unsafe extern "C" {
        fn keyboard_has_data() -> bool;
        fn keyboard_get_char() -> i32;
    }

    // ============= 用户指针校验（safe 包装） =============

    /// 校验单个用户指针是否在合法范围 [1, `USER_ADDR_MAX`)
    #[must_use]
    pub fn check_user_ptr(ptr: u64) -> bool {
        super::validate_user_ptr(ptr)
    }

    /// 校验用户缓冲区 [ptr, ptr+len) 是否完全在用户空间
    #[must_use]
    pub fn check_user_buf(ptr: u64, len: u64) -> bool {
        crate::privileged::userptr::validate_user_buf(ptr, len)
    }

    // ============= 用户态读写助手（I4 安全代理，自带完整判据） =============
    //
    // 下列助手是该模块内用户内存访问的**单一入口**: 每个函数自行执行
    // `userptr::validate_user_buf` / `validate_user_aligned` 判据并以
    // `smap_begin` / `smap_end` 包裹访问, 调用方无需 (也不应) 重复校验.
    // 判据缺失时返回 `None` / `false`, 不产生任何用户内存访问.

    /// 读一个 u64 用户值; 判据不满足 (NULL/越界/回绕/未映射/未对齐) 返回 `None`.
    ///
    /// 入参是用户地址数值 (非裸指针): 判据通过前不做任何用户内存访问.
    #[must_use]
    pub fn read_u64(ptr: u64) -> Option<u64> {
        if !crate::privileged::userptr::validate_user_aligned::<u64>(ptr) {
            return None;
        }
        // SAFETY: 上方已校验该地址的 8 字节区间合法且已映射, 且对齐到 8;
        // smap_begin/end 临时取消/恢复 CR4.SMAP 对用户页的拦截.
        // read_volatile 只读按值取, 不构造引用.
        unsafe {
            crate::privileged::mm::copy_user::smap_begin();
            let v = core::ptr::read_volatile(ptr as *const u64);
            crate::privileged::mm::copy_user::smap_end();
            Some(v)
        }
    }

    /// 写一个 u64 到用户地址; 判据不满足返回 `false` 且不写入.
    #[must_use]
    pub fn write_u64(ptr: u64, val: u64) -> bool {
        if !crate::privileged::userptr::validate_user_aligned::<u64>(ptr) {
            return false;
        }
        // SAFETY: 同 `read_u64` 的判据; write_volatile 只写按值, 不构造引用.
        unsafe {
            crate::privileged::mm::copy_user::smap_begin();
            core::ptr::write_volatile(ptr as *mut u64, val);
            crate::privileged::mm::copy_user::smap_end();
        }
        true
    }

    /// 写两个 u64 到用户地址 (用于 rlimit cur/max); 判据不满足返回 `false`.
    #[must_use]
    pub fn write_u64_pair(ptr: u64, cur: u64, max: u64) -> bool {
        if !ptr.is_multiple_of(core::mem::align_of::<u64>() as u64)
            || !crate::privileged::userptr::validate_user_buf(ptr, 16)
        {
            return false;
        }
        // SAFETY: 上方已校验 16 字节区间合法且已映射; 两次 write_volatile 均只写按值.
        unsafe {
            let p = ptr as *mut u64;
            crate::privileged::mm::copy_user::smap_begin();
            core::ptr::write_volatile(p, cur);
            core::ptr::write_volatile(p.add(1), max);
            crate::privileged::mm::copy_user::smap_end();
        }
        true
    }

    /// 复制结构体到用户地址（repr(C) 类型）; 判据不满足返回 `false`.
    pub fn write_struct<T: Copy>(dst: u64, src: &T) -> bool {
        if !crate::privileged::userptr::validate_user_aligned::<T>(dst) {
            return false;
        }
        // SAFETY: 上方已校验 size_of::<T>() 字节区间合法且已映射, 且 dst 对齐到
        // align_of::<T>(); write_volatile 只写按值.
        unsafe {
            crate::privileged::mm::copy_user::smap_begin();
            core::ptr::write_volatile(dst as *mut T, *src);
            crate::privileged::mm::copy_user::smap_end();
        }
        true
    }

    /// Safe 包装: 在 functions 层用, 写一个 repr(C) 结构体到 user 指针.
    ///
    /// 调用方无需 unsafe 块. 判据由 `write_struct` 自带.
    pub fn write_struct_to_user<T: Copy>(dst_ptr: u64, src: &T) -> bool {
        write_struct(dst_ptr, src)
    }

    /// Safe 包装: 写两个 u64 到 user 指针 (`rlim_cur`, `rlim_max`).
    ///
    /// 调用方无需 unsafe 块. 判据由 `write_u64_pair` 自带.
    #[must_use]
    pub fn write_rlimit_to_user(ptr: u64, cur: u64, max: u64) -> bool {
        write_u64_pair(ptr, cur, max)
    }

    /// Safe 包装: 在 functions 层用, 写一个 u64 到 user 指针.
    ///
    /// 调用方无需 unsafe 块. 判据由 `write_u64` 自带.
    #[must_use]
    pub fn write_u64_to_user(dst_ptr: u64, val: u64) -> bool {
        write_u64(dst_ptr, val)
    }

    /// Safe 包装: 从 user 指针读一个 repr(C) 结构体.
    ///
    /// 内部先做完整区间判据后读; 结构体按 `read_unaligned` 读取, 故不要求对齐.
    pub fn read_struct_from_user<T: Copy>(src_ptr: u64, dst: &mut T) -> bool {
        let size = core::mem::size_of::<T>() as u64;
        if src_ptr == 0 || !check_user_buf(src_ptr, size) {
            return false;
        }
        // SAFETY: check_user_buf 已校验 size_of::<T>() 字节区间合法且已映射;
        // read_unaligned 不要求对齐, 结果按值写入内核侧 dst.
        unsafe {
            crate::privileged::mm::copy_user::smap_begin();
            *dst = core::ptr::read_unaligned(src_ptr as *const T);
            crate::privileged::mm::copy_user::smap_end();
        }
        true
    }

    // ============= 设备输入抽象 =============

    /// `从键盘读取一个字节（x86_64` 专属）。None 表示无数据。
    /// # Safety
    /// FFI 调用，需在中断上下文。
    #[cfg(all(target_arch = "x86_64", not(feature = "kernel_test")))]
    #[must_use]
    pub fn read_keyboard_byte() -> Option<u8> {
        // SAFETY: `keyboard_has_data` / `keyboard_get_char` 是 driver 的 C-ABI 导出;
        // 本函数由中断上下文调用 (`disable_interrupts` 已持有), 两者只轮询键盘控制器.
        unsafe {
            if keyboard_has_data() {
                let c = keyboard_get_char();
                if c > 0 {
                    Some(crate::privileged::glue::u8_from_i32(c))
                } else {
                    None
                }
            } else {
                None
            }
        }
    }

    // ============= 物理内存分配 =============

    /// 分配 count 个连续物理页。
    /// 委托到 `mm::api::pmm_alloc_pages`.
    #[must_use]
    pub fn alloc_pages(count: u64) -> *mut u8 {
        crate::privileged::mm::pmm_alloc_pages(crate::privileged::glue::usize_from_u64(count))
    }

    /// 释放 count 个连续物理页。
    /// 委托到 `mm::api::pmm_free_pages`.
    pub fn free_pages(addr: *mut u8, count: u64) {
        crate::privileged::mm::pmm_free_pages(addr, crate::privileged::glue::usize_from_u64(count));
    }

    // ============= 时间 =============

    /// 从用户指针读一个 u64. None 表示失败.
    ///
    /// 内部执行完整区间判据 (含已映射) 后以 `read_unaligned` 读取 (不要求对齐),
    /// 并以 `smap_begin` / `smap_end` 包裹访问.
    #[must_use]
    pub fn read_u64_from_user(src_ptr: u64) -> Option<u64> {
        if src_ptr == 0 || !check_user_buf(src_ptr, 8) {
            return None;
        }
        // SAFETY: check_user_buf 已校验 8 字节区间合法且已映射; read_unaligned
        // 不要求对齐; smap_begin/end 取消/恢复 CR4.SMAP 对用户页的拦截.
        unsafe {
            crate::privileged::mm::copy_user::smap_begin();
            let v = core::ptr::read_unaligned(src_ptr as *const u64);
            crate::privileged::mm::copy_user::smap_end();
            Some(v)
        }
    }

    /// 读取 tick 计数（1ms 粒度）。
    /// # Safety
    /// FFI 调用，硬件定时器寄存器读取。
    #[must_use]
    pub fn get_ticks() -> u64 {
        // SAFETY: timer_get_ticks 是 C-ABI 函数，读取 PIT/HPET 计数器，
        // 无副作用。
        unsafe { timer_get_ticks() }
    }

    // ============= smoltcp 网络栈 FFI 包装 =============

    // ============= 链接器符号访问 =============

    /// 内核映像起始虚拟地址。
    /// # Safety
    /// 链接器符号，仅在 boot 后有效。
    #[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
    // 符号桩化 (host-test): 触发 borrow_as_ptr 的 `&_kernel_start as *const u8`
    // 仅存在于真机分支, expect 须同步收窄.
    #[must_use]
    pub fn kernel_start_ptr() -> *const u8 {
        // 符号桩化 (host-test): host 无 _kernel_start 链接脚本符号且不执行
        // sys_boot_install 内核映像布局查询, 常量中性返回 null.
        #[cfg(not(feature = "host-test"))]
        {
            // SAFETY: _kernel_start 是链接器符号 (extern "C")，是静态地址，
            // boot 后由 VMM 建立映射可读。
            &raw const _kernel_start
        }
        #[cfg(feature = "host-test")]
        {
            core::ptr::null()
        }
    }

    /// 内核映像结束物理地址（已减 `HHDM_OFFSET`）。
    /// # SAFETY: `链接器符号，hhdm_offset` 必须与启动时一致。
    #[cfg(all(not(feature = "kernel_test"), target_arch = "x86_64"))]
    // 符号桩化 (host-test): 触发 borrow_as_ptr 的 `&_kernel_end as *const u8`
    // 仅存在于真机分支, expect 须同步收窄.
    #[must_use]
    pub fn kernel_end_phys(hhdm_offset: usize) -> usize {
        // 符号桩化 (host-test): host 无 _kernel_end 链接脚本符号, 常量中性返回 0.
        #[cfg(not(feature = "host-test"))]
        {
            // SAFETY: `_kernel_end` 是链接脚本导出的符号, 取其地址不解引用;
            // `hhdm_offset` 由 `kernel_end_phys` 的入口契约要求与启动时一致.
            (&raw const _kernel_end as usize).wrapping_sub(hhdm_offset)
        }
        #[cfg(feature = "host-test")]
        {
            // E-04: host 桩分支消费参数, 保持与裸机分支结构对称
            let _ = hhdm_offset;
            0
        }
    }

    // ============= CPU 控制指令集中点 =============

    /// 64 位模式下 `lidt` 读取的伪描述符 (IDTR 内存形态).
    ///
    /// 布局 = 2 字节 limit + 8 字节 base, 共 10 字节. `packed` 是必需的: 默认
    /// `repr(C)` 会把 base 对齐到偏移 8 (总长 16), 而 CPU 按偏移 2 读取 base.
    #[cfg(target_arch = "x86_64")]
    #[repr(C, packed)]
    struct Idtr {
        /// IDT 字节数减一; 0 表示只有第 0 号表项可用 (本处刻意置 0 以制造三重故障).
        limit: u16,
        /// IDT 线性基址; 本处置 0 (limit=0 时不会被解引用).
        base: u64,
    }

    /// 加载空 IDT 后触发异常，重启 `CPU（x86_64`）。
    ///
    /// # Safety
    ///
    /// 本函数不返回；调用方须确保已关闭其他 CPU。
    #[cfg(target_arch = "x86_64")]
    pub unsafe fn reboot_via_idt() -> ! {
        // 形态约束 (㊿ 已修): ① 64 位 IDTR 恒为 10 字节 (2 字节 limit + 8 字节 base),
        // `Idtr` 的 `repr(C, packed)` 布局保证 base 落在偏移 2 且无填充; ② `int 0`
        // 会向当前内核栈压入 EFLAGS/CS/RIP, 故 asm 不声明 `nostack` —— 调用方按
        // `# Safety` 契约在有效内核栈上调用.
        let idtr = Idtr { limit: 0, base: 0 };
        // SAFETY: 本块是 `reboot_via_idt` 的实现体, 其 `# Safety` 契约要求调用方已关闭
        // 其它 CPU (本函数不返回)。`lidt [rdi]` 以 rdi 指向的 10 字节伪描述符加载空 IDT
        // (limit=0, base=0), 随后 `int 0` 在无门可用的 IDT 上触发异常 → 双重故障 →
        // 三重故障复位, 这是 x86 无键盘控制器时的重启手段; 尾部 `spin_loop` 循环只是
        // 复位未生效时的兜底, 不依赖 asm 正常返回。
        unsafe {
            core::arch::asm!(
                "lidt [rdi]",
                "int 0",
                in("rdi") &raw const idtr,
            );
            loop {
                core::hint::spin_loop();
            }
        }
    }

    /// 通过 PSCI `SYSTEM_RESET` 重启（aarch64）。
    ///
    /// # Safety
    ///
    /// 本函数不返回；调用方须确保已关闭其他 CPU。
    #[cfg(target_arch = "aarch64")]
    pub unsafe fn reboot_via_psci() -> ! {
        // 51 (已修): 重启语义现由仓内唯一 PSCI 实现承担 ——
        // `arch/aarch64/psci.rs` 按设备树 `/psci.method` 选择 HVC/SMC conduit
        // (QEMU virt = HVC)、以 `PSCI_SYSTEM_RESET (0x8400_0009)` 作功能号, 并在
        // `invoke` 内声明 SMCCC 破坏的 x1..x3. 本地重写汇编会同时丢掉这三项,
        // 故此处只做转发; `system_reset` 内部在 PSCI 不可用时以写零地址触发异常,
        // 同样不返回, 满足本函数的发散签名.
        crate::privileged::arch::aarch64::psci::system_reset()
    }

    /// ㊿ 回归锁: IDTR 内存形态必须是 CPU 读取的 10 字节 (base 紧跟 limit).
    ///
    /// 旧实参 `&[0u16; 4]` 是 8 字节, `lidt` 会越界多读 2 字节; 若有人把
    /// `Idtr` 改回默认 `repr(C)`, base 会落到偏移 8 且总长变 16, 本测试即红.
    #[cfg(all(test, target_arch = "x86_64"))]
    mod tests {
        use super::Idtr;

        #[test]
        fn idtr_layout_is_cpu_ten_byte_form() {
            assert_eq!(
                core::mem::size_of::<Idtr>(),
                10,
                "64 位 IDTR 必须是 10 字节"
            );
            assert_eq!(
                core::mem::offset_of!(Idtr, base),
                2,
                "base 必须紧跟 2 字节 limit (不能有对齐填充)"
            );
        }
    }
}
