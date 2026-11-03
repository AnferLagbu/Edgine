#![deny(unsafe_code)]
//! execve 系统调用 — functions 层策略
//!
//! ## 职责拆分 (T2 批 1, syscall-followup)
//!
//! - functions (本文件): 指针校验策略 + argv/envp 扫描 + 可执行性判定 + SUID
//!   提权判定 + 委托进程替换
//! - privileged 机制: [`crate::privileged::proc::proc_exec_replace`] (进程替换/
//!   页表切换 + 新栈 argv/envp 落盘), `privileged::syscall::api::read_u64_from_user`
//!   (用户内存 safe 读取)
//!
//! 原 `privileged::syscall::execve::ExecveResult` 包装类型随回退分支迁移删除
//! (其唯一消费者是 privileged 回退分支), functions 直接返回精确 Errno
//! (`EFAULT` / `ENOENT` / `EACCES`), 语义与原 `from_ret` 映射一致.

use crate::privileged::syscall::Errno;

/// `AT_FDCWD` — 相对路径基于当前工作目录 (Linux ABI)
const AT_FDCWD: i32 = -100;
/// `execveat` 标志: 空 `pathname` 表示执行 `dirfd` 指向的文件自身
const AT_EMPTY_PATH: i32 = 0x1000;
/// `execveat` 标志: 不跟随 `pathname` 末尾的符号链接
const AT_SYMLINK_NOFOLLOW: i32 = 0x100;

/// ELF 映像可读上限 (1 MiB) — 阶段 4b 自 `privileged::proc::api` 下沉
pub(crate) const ELF_MAX_SIZE: usize = 1024 * 1024;

/// 扫描用户空间 `NULL` 结尾的字符串指针数组, 返回条目数.
///
/// `base == 0` 视为空向量 (返回 0); 槽位本身或任一字符串指针未通过用户指针
/// 校验、或槽位不可读, 一律返回 `EFAULT` —— 与逐项读取 argv 的既有口径一致.
fn scan_user_strvec(base: u64) -> Result<u32, Errno> {
    use crate::privileged::syscall::api;

    if base == 0 {
        return Ok(0);
    }
    if !api::validate_user_ptr(base) {
        return Err(Errno::Efault);
    }

    let mut count: u32 = 0;
    let mut slot = base;
    loop {
        if !api::validate_user_ptr(slot) {
            return Err(Errno::Efault);
        }
        let Some(entry) = api::read_u64_from_user(slot) else {
            return Err(Errno::Efault);
        };
        if entry == 0 {
            return Ok(count);
        }
        if !api::validate_user_ptr(entry) {
            return Err(Errno::Efault);
        }
        count += 1;
        slot += 8;
    }
}

/// execve(path, argv, envp) 系统调用策略
///
/// `path` / `argv` / `envp` 均为用户空间指针 (u64 形式). argv 与 envp 以
/// `NULL` 结尾, 逐项校验后计数, 字符串内容由 privileged 在旧地址空间存活期间
/// 复制进新进程栈 (K-07b 批2).
///
/// # Errors
/// - `path` 为空或未通过用户指针校验 → `EFAULT`
/// - `argv` / `envp` 数组遍历中任一指针 (槽位或条目) 非法 → `EFAULT`
/// - 目标不存在或不可 stat → `ENOENT`
/// - 目标非常规文件, 或常规文件无任一执行位 (Linux 语义, root 亦然) → `EACCES`
/// - 进程替换失败 → `ENOENT` (与原 privileged 实现语义一致)
pub fn execve_syscall(path: u64, argv: u64, envp: u64) -> Result<usize, Errno> {
    use crate::privileged::syscall::api;

    if path == 0 || !api::validate_user_ptr(path) {
        return Err(Errno::Efault);
    }

    let arg_count = scan_user_strvec(argv)?;
    let env_count = scan_user_strvec(envp)?;

    // 可执行性判定 + SUID 处理: 常规文件须至少一位执行位 (Linux 对 root 也不放宽)
    // —— 该判据同时钉住 initramfs 解包是否保留归档 mode 位: 未保留时归档里的
    // 0755 被默认 0644 覆盖, 一切 execve 都会在此 EACCES.
    let current_pwm = crate::privileged::sgeg::get_current_pwm();
    let Some(st) = crate::functions::fs::api::vfs_stat_safe(path as *const u8, current_pwm) else {
        return Err(Errno::Enoent);
    };
    if st.file_type != crate::functions::fs::VfsFileType::File.as_u8() || (st.perm & 0o111) == 0 {
        return Err(Errno::Eacces);
    }
    if (st.perm & 0o4000) != 0 && st.owner_pwm != 0 {
        crate::privileged::sgeg::elevate_for_suid(st.owner_pwm);
    }

    // 阶段 4b: ELF 文件 I/O 下沉 functions — 读取字节 + PT_INTERP 改写,
    // 再交 privileged 纯机制 (进程替换/页表切换).
    let Some(mut elf) =
        crate::functions::fs::api::read_file_to_vec(path as *const u8, current_pwm, ELF_MAX_SIZE)
    else {
        return Err(Errno::Enoent);
    };
    crate::privileged::proc::elf::prepare_elf_image(&mut elf);

    // 进程替换 (privileged 机制: ELF 加载 + 地址空间切换 + 新栈 argv/envp)
    let result = crate::privileged::proc::proc_exec_replace(
        elf.as_ptr() as u64,
        elf.len() as u64,
        argv,
        arg_count,
        envp,
        env_count,
    );
    if result < 0 {
        Err(Errno::Enoent)
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
        return Err(Errno::Einval);
    }
    if dirfd != AT_FDCWD {
        return Err(Errno::Enotsup);
    }

    // AT_EMPTY_PATH: 空 pathname 表示执行 dirfd 指向的文件自身 (本实装不支持)
    if flags & AT_EMPTY_PATH != 0 {
        let mut first_byte = 0u8;
        if !crate::privileged::syscall::api::read_struct_from_user(pathname, &mut first_byte) {
            return Err(Errno::Efault);
        }
        if first_byte == 0 {
            return Err(Errno::Enotsup);
        }
    }

    execve_syscall(pathname, argv, envp)
}
