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
//! 2. 解压 boot-module 交付的 initramfs cpio 到 ramfs
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
#[must_use]
pub fn init_launch_status() -> u32 {
    crate::privileged::proc::init_launch_status()
}

/// init 是否已运行 (>= 3 表示已进入 Ring 3)
#[inline]
#[must_use]
pub fn is_init_running() -> bool {
    init_launch_status() >= INIT_STATUS_RUNNING
}

// ============================================================================
// initramfs 载体装载 (K-07b: 用户态程序经文件系统进入, 双架构共用)
// ============================================================================

// initramfs cpio 不再编译期嵌入 (用户裁定 D3 严格零嵌入): 由引导器作为 boot-module
// 交付内存, 经 `privileged::boot::initrd_blob()` 取用 (见下方 `load_init_from_initramfs`).

/// 解压 initramfs 并把归档里的 `/init` 载入为 PID 1, 返回其 pid.
///
/// 返回 `<= 0` 表示载体不可用 (归档为空 / 解压失败 / 读不到 `/init` / ELF 装载
/// 失败). 严格零嵌入 (D4): 无内嵌 `init.bin` 回退, 调用方按失败处理 (报错 + 停机 /
/// `qemu_exit`). 本函数**不**切入用户态: 进入方式与失败兜底按架构不同, 由调用方决定.
///
/// 取镜像链路: PID 1 经 VFS 读出 → `prepare_elf_image` → 纯机制装载, 即 `execve`
/// 同款路径, 是 K-07b 用户态运行射程的入口.
fn load_init_from_initramfs() -> i32 {
    // 严格零嵌入 (裁定 A): cpio 只来自引导器 boot-module; 无模块无 cpio → -1 (上层按失败停机).
    let Some(blob) = crate::privileged::boot::initrd_blob() else {
        crate::klog_boot_info!("[USER] no boot-module initrd (cpio must come from bootloader)");
        return -1;
    };
    crate::klog_boot_info!("[USER] initramfs from boot module ({} bytes)", blob.len());
    let count = match crate::functions::fs::initramfs::unpack(blob, "/", 0) {
        Ok(count) => count,
        Err(e) => {
            crate::klog_boot_info!("[USER] initramfs unpack failed: {}", e);
            return -2;
        }
    };
    crate::klog_boot_info!("[USER] initramfs: {} files unpacked", count);
    crate::privileged::proc::set_init_status(INIT_STATUS_LOADING);

    let Some(mut elf) = crate::functions::fs::api::read_file_kernel(
        "/init",
        0,
        crate::functions::proc::exec::ELF_MAX_SIZE,
    ) else {
        return -3;
    };
    crate::privileged::proc::elf::prepare_elf_image(&mut elf);
    crate::privileged::proc::mechanism::user_proc_load_elf_from_memory(
        elf.as_ptr(),
        elf.len() as u64,
        0,
    )
}

// ============================================================================
// 磁盘根首启安装 (D5-g: Linux 式 switch_root 的前置, 载体全量落到 EGFS 盘)
// ============================================================================

/// 磁盘首启时把 boot-module 的 initramfs 全量安装到 `/disk`, 供用户态 init 切根.
///
/// 引导根恒为 ramfs (D5-e 裁定), EGFS 只挂非根挂载点 `/disk`; 盘上若无 `/init`,
/// 用户态切根后就没有可执行体, 故挂载成功后把同一份 cpio 归档再解压一遍到 `/disk`.
///
/// 幂等: 盘上已有 `/disk/init` 即整段跳过 —— 磁盘根是持久化的, 二启重装会覆盖
/// 用户改动 (与 uberblock 持久化同一立场).
///
/// 失败只记日志不中止启动: 引导根仍是 ramfs, 用户态照跑; 切根侧按失败继续.
pub fn install_disk_root() {
    if crate::functions::fs::api::vfs_stat_safe(c"/disk/init".as_ptr().cast(), 0).is_some() {
        crate::klog_boot_info!("[USER] /disk/init present (skip disk-root install)");
        return;
    }
    let Some(blob) = crate::privileged::boot::initrd_blob() else {
        crate::klog_boot_info!("[USER] disk-root install skipped: no boot-module initrd");
        return;
    };
    let count = match crate::functions::fs::initramfs::unpack(blob, "/disk", 0) {
        Ok(count) => count,
        Err(e) => {
            crate::klog_boot_info!("[USER] disk-root unpack failed: {}", e);
            return;
        }
    };
    let _ = crate::functions::fs::api::vfs_sync();
    // 写路径曾静默丢数据 (#345/#348): 只数解压条目证不了字节真落盘, 必须回读.
    let readback = crate::functions::fs::api::read_file_kernel(
        "/disk/init",
        0,
        crate::functions::proc::exec::ELF_MAX_SIZE,
    );
    match readback {
        Some(elf) if !elf.is_empty() => crate::klog_boot_info!(
            "[USER] disk root installed: {} files, /disk/init read back {} bytes",
            count,
            elf.len()
        ),
        _ => crate::klog_boot_info!(
            "[USER] disk root install incomplete: {} files unpacked, /disk/init unreadable",
            count
        ),
    }
}

// ============================================================================
// boot 启动编排 (阶段 4b: 自 privileged::proc::launch_first_user_process 下沉)
// ============================================================================

/// 启动首个用户进程 (init) — 阶段 4b 自 privileged 下沉的 boot 编排.
///
/// 编排步骤:
/// 1. 挂载 ramfs 为 `/`
/// 2. 解压 boot-module 交付的 initramfs cpio 到 ramfs
/// 3. 加载 `/init` ELF (仅 boot-module, 无内嵌回退), 创建 PID 1
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

    // 2. 解压 boot-module initramfs 并从 ramfs 加载 /init (双架构同形)
    crate::privileged::proc::set_init_status(INIT_STATUS_UNPACKING);

    #[cfg(target_arch = "x86_64")]
    {
        // 严格零嵌入 (D4): init 只来自引导器 boot-module (initramfs cpio); 无内嵌 init.bin 回退.
        let pid = load_init_from_initramfs();
        if pid > 0 {
            crate::privileged::proc::set_init_status(INIT_STATUS_RUNNING);
            crate::klog_boot_info!("[USER] Entering Ring 3 (init from /init, pid={})...", pid);
            crate::privileged::proc::mechanism::enter_user_process(pid.cast_unsigned());
        }
        crate::klog_err!(
            Boot,
            "[USER] no boot-module /init (zero-embed; bootloader must supply initramfs)"
        );
        crate::privileged::debug::qemu_exit(false);
    }

    #[cfg(target_arch = "aarch64")]
    {
        // 禁用 IRQ 以防止 timer 中断在 ELF 加载/进程创建期间干扰,
        // 导致非确定性挂起 (PMM 分配/页表操作/调度器状态不一致).
        // SAFETY: 其后 enter_user_process 以 `-> !` 进入 EL0 不再返回本 EL1 上下文;
        // 进入 EL0 的 PSTATE 由 enter_user 写入 SPSR_EL1 (I 位清零 ⇒ EL0 可被抢占),
        // 与本处 EL1 侧的中断屏蔽状态无关, 故无需恢复.
        let _saved = crate::arch!(interrupt_disable());

        // 严格零嵌入 (D4): init 只来自 boot-module (DTB /chosen initrd); 无内嵌 init.bin 回退;
        // 无 module /init ⇒ 落到下方 halt 循环 (不再回退 init.bin).
        let pid = load_init_from_initramfs();
        if pid > 0 {
            crate::privileged::proc::set_init_status(INIT_STATUS_RUNNING);
            crate::klog_boot_info!("[USER] Entering EL0 (init from /init, pid={})...", pid);
            crate::privileged::proc::mechanism::enter_user_process(pid.cast_unsigned());
        }
        crate::klog_err!(
            Boot,
            "[USER] no boot-module /init (zero-embed; bootloader must supply initramfs)"
        );
    }

    // x86_64 分支以 qemu_exit(`-> !`) 收尾; aarch64 分支落此 halt 循环.
    #[cfg(target_arch = "aarch64")]
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
