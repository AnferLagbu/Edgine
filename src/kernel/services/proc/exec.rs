#![deny(unsafe_code)]
//! execve 系统调用 — services 层策略
//!
//! ## 职责拆分 (T2 批 1, syscall-followup)
//!
//! - services (本文件): 指针校验策略 + argv 扫描 + SUID 提权判定 + 委托进程替换
//! - framework 机制: [`crate::framework::proc::proc_exec_replace`] (进程替换/
//!   页表切换), `framework::syscall::api::read_u64_from_user` (用户内存 safe 读取)
//!
//! 原 `framework::syscall::execve::ExecveResult` 包装类型随回退分支迁移删除
//! (其唯一消费者是 framework 回退分支), services 直接返回精确 Errno
//! (`EFAULT` / `ENOENT`), 语义与原 `from_ret` 映射一致.

use crate::framework::syscall::Errno;

/// `AT_FDCWD` — 相对路径基于当前工作目录 (Linux ABI)
const AT_FDCWD: i32 = -100;
/// `execveat` 标志: 空 `pathname` 表示执行 `dirfd` 指向的文件自身
const AT_EMPTY_PATH: i32 = 0x1000;
/// `execveat` 标志: 不跟随 `pathname` 末尾的符号链接
const AT_SYMLINK_NOFOLLOW: i32 = 0x100;

/// ELF 映像可读上限 (1 MiB) — 阶段 4b 自 framework::proc::api 下沉
pub(crate) const ELF_MAX_SIZE: usize = 1024 * 1024;

/// execve(path, argv, envp) 系统调用策略
///
/// `path` / `argv` / `envp` 均为用户空间指针 (u64 形式); `envp` 当前忽略
/// (与原 framework 实现一致, 环境变量传递待进程基础设施扩展后实装).
///
/// # Errors
/// - `path` 为空或未通过用户指针校验 → `EFAULT`
/// - `argv` 数组遍历中任一指针 (槽位或条目) 非法 → `EFAULT`
/// - 进程替换失败 → `ENOENT` (与原 framework 实现语义一致)
#[expect(
    clippy::similar_names,
    reason = "argv/envp 是 POSIX execve ABI 标准参数名, 重命名会破坏与 Linux 语义的对应关系"
)]
pub fn execve_syscall(path: u64, argv: u64, envp: u64) -> Result<usize, Errno> {
    use crate::framework::syscall::api;
    use crate::framework::syscall::raw;

    if path == 0 || !raw::check_user_ptr(path) {
        return Err(Errno::EFAULT);
    }
    let _ = envp;

    // argv 扫描: 逐项读取用户空间指针数组, 统计 argc 直到 NULL 终止
    let mut argc: u32 = 0;
    if argv != 0 {
        if !raw::check_user_ptr(argv) {
            return Err(Errno::EFAULT);
        }
        let mut slot = argv;
        loop {
            if !raw::check_user_ptr(slot) {
                return Err(Errno::EFAULT);
            }
            let Some(entry) = api::read_u64_from_user(slot) else {
                return Err(Errno::EFAULT);
            };
            if entry == 0 {
                break;
            }
            if !raw::check_user_ptr(entry) {
                return Err(Errno::EFAULT);
            }
            argc += 1;
            slot += 8;
        }
    }

    // SUID 处理: 可执行文件带 setuid 位且属主非 root 时提权
    let mut st = crate::services::fs::VfsStat::default();
    let current_pwm = crate::framework::credo::get_current_pwm();
    let stat_result =
        crate::services::fs::api::vfs_stat_internal(path as *const u8, &raw mut st, current_pwm);
    if stat_result == 0 && (st.perm & 0o4000) != 0 && st.owner_pwm != 0 {
        crate::framework::credo::elevate_for_suid(st.owner_pwm);
    }

    // 阶段 4b: ELF 文件 I/O 下沉 services — 读取字节 + PT_INTERP 改写,
    // 再交 framework 纯机制 (进程替换/页表切换).
    let Some(mut elf) =
        crate::services::fs::api::read_file_to_vec(path as *const u8, current_pwm, ELF_MAX_SIZE)
    else {
        return Err(Errno::ENOENT);
    };
    crate::framework::proc::elf::prepare_elf_image(&mut elf);

    // 进程替换 (framework 机制: ELF 加载 + 地址空间切换)
    let result = crate::framework::proc::proc_exec_replace(
        elf.as_ptr(),
        elf.len() as u64,
        argv as *const *const u8,
        argc,
    );
    if result < 0 {
        Err(Errno::ENOENT)
    } else {
        Ok(0)
    }
}

/// `execveat(dirfd, pathname, argv, envp, flags)` — `execve` 的目录 fd 相对版本
///
/// 仅支持 `dirfd == AT_FDCWD` (与 `utimensat` / `fchownat` 的 AT_FDCWD-only
/// 现状一致); 参数与执行语义委托 [`execve_syscall`].
///
/// # Errors
///
/// - `flags` 含 `AT_EMPTY_PATH` / `AT_SYMLINK_NOFOLLOW` 之外的位 → `EINVAL`
/// - `dirfd` 非 `AT_FDCWD` → `ENOTSUP`
/// - `AT_EMPTY_PATH` 置位且 `pathname` 为空串 → `ENOTSUP`
/// - 其余错误与 [`execve_syscall`] 一致 (`EFAULT` / `ENOENT`)
///
/// SIMPLIFIED: 不支持目录 fd 相对路径解析与空路径执行 (`fexecve` 语义);
/// 影响面: 依赖 `AT_EMPTY_PATH` 的程序 (部分动态加载器 / 容器运行时) 不可用;
/// 何时需扩展: VFS 提供"目录 fd + 相对路径"解析机制后按 Linux 语义补齐.
pub fn execveat_syscall(
    dirfd: i32,
    pathname: u64,
    argv: u64,
    envp: u64,
    flags: i32,
) -> Result<usize, Errno> {
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW) != 0 {
        return Err(Errno::EINVAL);
    }
    if dirfd != AT_FDCWD {
        return Err(Errno::ENOTSUP);
    }

    // AT_EMPTY_PATH: 空 pathname 表示执行 dirfd 指向的文件自身 (本实装不支持)
    if flags & AT_EMPTY_PATH != 0 {
        let mut first_byte = 0u8;
        if !crate::framework::syscall::api::read_struct_from_user(pathname, &mut first_byte) {
            return Err(Errno::EFAULT);
        }
        if first_byte == 0 {
            return Err(Errno::ENOTSUP);
        }
    }

    execve_syscall(pathname, argv, envp)
}
