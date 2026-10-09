#![deny(unsafe_code)]
//! init 启动子系统 — functions 层安全代理
//!
//! @SAFE: 本文件不含 unsafe 代码。
//! 所有 unsafe 操作已委托至 privileged::proc::api。
//!
//! ## 职责
//!
//! - 查询 init 启动状态 (0=未启动, 1=initramfs 解压, 2=加载, 3=Ring 3)
//! - 提供类型安全的常量供其他服务引用
//!
//! ## 启动流程 (由本模块 [`launch_first_user_process`] 编排驱动)
//!
//! 1. 挂载 ramfs 为 `/`
//! 2. 解压 initramfs cpio 到 ramfs (feature = "initramfs")
//! 3. 加载 `/init` ELF, 创建 PID 1
//! 4. 加入调度器, 切换 Ring 3

// ============================================================================
// init 启动状态常量
// ============================================================================

/// 未启动
pub const INIT_STATUS_NOT_STARTED: u32 = 0;
/// initramfs 解压中
pub const INIT_STATUS_UNPACKING: u32 = 1;
/// init ELF 加载中
pub const INIT_STATUS_LOADING: u32 = 2;
/// 已 Ring 3 进入 (init 运行中)
pub const INIT_STATUS_RUNNING: u32 = 3;

// ============================================================================
// safe 状态查询 API
// ============================================================================

/// 查询 init 启动状态
#[inline]
pub fn init_launch_status() -> u32 {
    crate::privileged::proc::init_launch_status()
}

/// init 是否已运行 (>= 3 表示已进入 Ring 3)
#[inline]
pub fn is_init_running() -> bool {
    init_launch_status() >= INIT_STATUS_RUNNING
}

// ============================================================================
// boot 启动编排 (阶段 4b: 自 privileged::proc::launch_first_user_process 下沉)
// ============================================================================

/// 启动首个用户进程 (init) — 阶段 4b 自 privileged 下沉的 boot 编排.
///
/// 编排步骤:
/// 1. 挂载 ramfs 为 `/`
/// 2. 解压 initramfs cpio 到 ramfs (`feature = "initramfs"`)
/// 3. 加载 `/init` ELF (失败回退内嵌 `init.bin`), 创建 PID 1
/// 4. 经 privileged 机制入口进入 Ring 3 / EL0
///
/// 正常路径下进入用户态不返回; 若机制入口返回, 尾部停机兜底.
pub fn launch_first_user_process() -> ! {
    crate::klog_boot_info!("[USER] Launching init process...");

    // 1. 挂载 ramfs 为根文件系统
    let mount_result = crate::functions::fs::vfs_mount_safe("/", "ramfs");
    crate::klog_boot_info!("[USER] ramfs mount result={}", mount_result);
    if mount_result < 0 {
        crate::klog_boot_info!("[USER] Warning: ramfs mount on / failed ({})", mount_result);
    }

    // 2. 解压 initramfs 并/或加载内嵌 init (x86_64)
    crate::privileged::proc::set_init_status(INIT_STATUS_UNPACKING);

    #[cfg(target_arch = "x86_64")]
    {
        #[cfg(feature = "initramfs")]
        {
            let initramfs = include_bytes!("../../../other/build/x86_64/user/initramfs.cpio");
            if !initramfs.is_empty() {
                match crate::functions::fs::initramfs::unpack(initramfs) {
                    Ok(count) => {
                        crate::klog_boot_info!("[USER] initramfs: {} files unpacked", count);
                        crate::privileged::proc::set_init_status(INIT_STATUS_LOADING);
                        // 加载 /init: VFS 读文件 + PT_INTERP 改写 + privileged 纯机制加载
                        let pid = match crate::functions::fs::api::read_file_to_vec(
                            b"/init\0".as_ptr(),
                            0,
                            crate::functions::proc::exec::ELF_MAX_SIZE,
                        ) {
                            Some(mut elf) => {
                                crate::privileged::proc::elf::prepare_elf_image(&mut elf);
                                crate::privileged::proc::mechanism::user_proc_load_elf_from_memory(
                                    elf.as_ptr(),
                                    elf.len() as u64,
                                    0,
                                )
                            }
                            None => -1,
                        };
                        if pid > 0 {
                            crate::privileged::proc::set_init_status(INIT_STATUS_RUNNING);
                            crate::klog_boot_info!(
                                "[USER] Entering Ring 3 (init from /init, pid={})...",
                                pid
                            );
                            crate::privileged::proc::mechanism::enter_user_process(pid as u32);
                        }
                        crate::klog_boot_info!("[USER] /init not found, falling back to init.bin");
                    }
                    Err(e) => {
                        crate::klog_boot_info!("[USER] initramfs unpack failed: {}", e);
                    }
                }
            }
        }

        // 回退: 直接加载内嵌的 init.bin.
        // 判据用 target_os 而非 feature: host target 的 host-test / kernel_test
        // 两个 lint 维度与 host-tests 都会编译本块, 不桩化即让 host 侧编译硬依赖
        // 裸机产物 (other/build/ 仅由 make 生成). 本函数为裸机 init 入口, host 不可达,
        // 取空切片 ⇒ 走下方 bin.is_empty() 报错退出分支.
        #[cfg(target_os = "none")]
        let bin = include_bytes!("../../../other/build/x86_64/user/init.bin");
        #[cfg(not(target_os = "none"))]
        let bin: &[u8] = &[];

        if bin.is_empty() {
            crate::klog_err!(Boot, "[USER] init binary is empty");
            crate::privileged::debug::qemu_exit(false);
        }

        let pid = crate::privileged::proc::mechanism::user_proc_load_elf_from_memory(
            bin.as_ptr(),
            bin.len() as u64,
            0,
        );
        if pid <= 0 {
            crate::klog_err!(Boot, "[USER] Failed to load init ELF, pid={}", pid);
            crate::privileged::debug::qemu_exit(false);
        }

        crate::privileged::proc::set_init_status(INIT_STATUS_RUNNING);
        crate::klog_boot_info!("[USER] Entering Ring 3 (init pid={})...", pid);
        crate::privileged::proc::mechanism::enter_user_process(pid as u32);
    }

    #[cfg(target_arch = "aarch64")]
    {
        // 禁用 IRQ 以防止 timer 中断在 ELF 加载/进程创建期间干扰,
        // 导致非确定性挂起 (PMM 分配/页表操作/调度器状态不一致).
        // SAFETY: 其后 enter_user_process 以 `-> !` 进入 EL0 不再返回本 EL1 上下文;
        // 进入 EL0 的 PSTATE 由 enter_user 写入 SPSR_EL1 (I 位清零 ⇒ EL0 可被抢占),
        // 与本处 EL1 侧的中断屏蔽状态无关, 故无需恢复.
        let _saved = crate::arch!(interrupt_disable());

        let bin = include_bytes!("../../../other/build/aarch64/user/init.bin");
        let bin_size = bin.len() as u64;
        if bin_size == 0 {
            crate::klog_boot_info!("[USER] init ELF is empty");
            loop {
                crate::arch!(halt());
            }
        }

        let pid = crate::privileged::proc::mechanism::user_proc_load_elf_from_memory(
            bin.as_ptr(),
            bin_size,
            0,
        );
        if pid <= 0 {
            crate::klog_err!(Boot, "[USER] Failed to load init ELF");
            loop {
                crate::arch!(halt());
            }
        }

        crate::privileged::proc::set_init_status(INIT_STATUS_RUNNING);
        crate::klog_boot_info!("[USER] Entering EL0 (init pid={})...", pid);
        crate::privileged::proc::mechanism::enter_user_process(pid as u32);
    }

    loop {
        crate::arch!(halt());
    }
}

// ============================================================================
// 单元测试 (host)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_status_not_running_when_zero() {
        // 启动前状态应为 0
        // 注: host 进程下 status 可能为 0, 验证语义
        assert!(!is_init_running() || init_launch_status() == 0);
    }

    #[test]
    fn test_init_status_constants_distinct() {
        assert_ne!(INIT_STATUS_NOT_STARTED, INIT_STATUS_UNPACKING);
        assert_ne!(INIT_STATUS_UNPACKING, INIT_STATUS_LOADING);
        assert_ne!(INIT_STATUS_LOADING, INIT_STATUS_RUNNING);
    }
}
