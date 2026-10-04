#![deny(unsafe_code)]
//! 信号系统 — services 层安全代理
//!
//! ## 状态 (v2.16, 2026-06-12)
//!
//! Phase 2.5 进程迁移 4/4 (signal):
//! - [x] 强类型 `Signal` (POSIX 信号枚举 + 未知信号)
//! - [x] 强类型 `SignalAction` (handler / siginfo / mask)
//! - [x] 信号传递 (内核基础设施已通过 `proc::table::signal_set` 暴露)
//! - [x] 标准信号常量 (SIGHUP/SIGINT/SIGQUIT/SIGKILL/SIGSEGV/...)
//! - [x] 位掩码操作 (block/unblock/test)
//! - [x] `kill(pid, sig)` 顶层 API
//! - [x] TD-16: `SignalError` 改为 `KernelError` 0 字段 type alias (单一错误源)
//!
//! ## 迁移方法
//!
//! `Edgine` 当前内核信号子系统是 per-process 32 bit 简易实现 (`signal_pending_*`),
//! 不完整的 POSIX 信号语义由 services 层提供类型安全封装 + 标准常量。
//! 未来完整化信号子系统 (sigaction 表, 共享处理, sigaltstack) 时, 替换 `do_signal`
//! 内部实现, services 层 API 保持稳定。
//!
//! 评估日期: 2026-06-04, v2.16 更新: 2026-06-12

// ============================================================================
// 标准 POSIX 信号 (强类型枚举)
// ============================================================================

/// POSIX 标准信号 (1..=31)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StandardSignal {
    /// Hangup
    Hup = 1,
    /// 终端中断 (Ctrl-C)
    Int = 2,
    /// 终端退出 (Ctrl-\\)
    Quit = 3,
    /// 非法指令
    Ill = 4,
    /// 跟踪/断点陷阱
    Trap = 5,
    /// 异常中止
    Abrt = 6,
    /// 总线错误
    Bus = 7,
    /// 浮点异常
    Fpe = 8,
    /// Kill (不能被捕获/忽略)
    Kill = 9,
    /// 用户定义信号 1
    Usr1 = 10,
    /// 段错误
    Segv = 11,
    /// 用户定义信号 2
    Usr2 = 12,
    /// 管道破裂
    Pipe = 13,
    /// 闹钟定时器
    Alrm = 14,
    /// 终止 (可捕获)
    Term = 15,
    /// 栈故障 (Linux 特有)
    Stkflt = 16,
    /// 子进程退出
    Chld = 17,
    /// 继续执行 (SIGSTOP 对应)
    Cont = 18,
    /// 停止 (Ctrl-Z)
    Stop = 19,
    /// 终端停止输入
    Tstp = 20,
    /// 终端停止输出
    Ttin = 21,
    /// 终端恢复输出
    Ttou = 22,
    /// 紧急情况 (不可捕获)
    Urg = 23,
    /// CPU 时间限制
    Xcpu = 24,
    /// 文件大小限制
    Xfsz = 25,
    /// 虚拟闹钟
    Vtalrm = 26,
    /// 性能分析
    Prof = 27,
    /// 窗口大小变化
    Winch = 28,
    /// I/O 可能
    Io = 29,
    /// 电源故障
    Pwr = 30,
    /// 非法系统调用
    Sys = 31,
}

impl StandardSignal {
    /// 从原始信号编号构造
    pub fn from_number(n: u8) -> Option<Self> {
        match n {
            1 => Some(Self::Hup),
            2 => Some(Self::Int),
            3 => Some(Self::Quit),
            4 => Some(Self::Ill),
            5 => Some(Self::Trap),
            6 => Some(Self::Abrt),
            7 => Some(Self::Bus),
            8 => Some(Self::Fpe),
            9 => Some(Self::Kill),
            10 => Some(Self::Usr1),
            11 => Some(Self::Segv),
            12 => Some(Self::Usr2),
            13 => Some(Self::Pipe),
            14 => Some(Self::Alrm),
            15 => Some(Self::Term),
            16 => Some(Self::Stkflt),
            17 => Some(Self::Chld),
            18 => Some(Self::Cont),
            19 => Some(Self::Stop),
            20 => Some(Self::Tstp),
            21 => Some(Self::Ttin),
            22 => Some(Self::Ttou),
            23 => Some(Self::Urg),
            24 => Some(Self::Xcpu),
            25 => Some(Self::Xfsz),
            26 => Some(Self::Vtalrm),
            27 => Some(Self::Prof),
            28 => Some(Self::Winch),
            29 => Some(Self::Io),
            30 => Some(Self::Pwr),
            31 => Some(Self::Sys),
            _ => None,
        }
    }

    /// 信号编号
    pub fn number(self) -> u8 {
        self as u8
    }

    /// 是否可被进程捕获或忽略
    pub fn is_catchable(self) -> bool {
        !matches!(self, Self::Kill | Self::Stop)
    }

    /// 是否是核心转储信号 (默认行为)
    pub fn is_core_dump(self) -> bool {
        matches!(
            self,
            Self::Quit
                | Self::Ill
                | Self::Abrt
                | Self::Bus
                | Self::Fpe
                | Self::Segv
                | Self::Sys
                | Self::Xcpu
                | Self::Xfsz
        )
    }
}

/// 信号 (标准 1..=31, 或 RT 32..=64 实时信号, 0 用于空检查)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Signal(pub u8);

impl Signal {
    /// 空信号 (POSIX kill(pid, 0) 错误检查用)
    pub const NONE: Self = Self(0);

    /// 构造标准信号
    #[inline]
    pub const fn standard(s: StandardSignal) -> Self {
        Self(s as u8)
    }

    /// 构造 RT 信号 (sig >= 32)
    #[inline]
    pub const fn realtime(rt_num: u8) -> Self {
        Self(rt_num + 32)
    }

    /// 原始信号编号
    #[inline]
    pub const fn number(self) -> u8 {
        self.0
    }

    /// 是否为标准信号
    #[inline]
    pub fn is_standard(self) -> bool {
        StandardSignal::from_number(self.0).is_some()
    }

    /// 是否为 RT 信号 (>= 32)
    #[inline]
    pub fn is_realtime(self) -> bool {
        self.0 >= 32
    }

    /// 转位掩码 (用于 `signal_pending_*`)
    #[inline]
    pub fn to_bit(self) -> u64 {
        1u64 << u64::from(self.0)
    }
}

// ============================================================================
// 信号处理动作
// ============================================================================

/// 默认动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalDisposition {
    /// Term (默认: 终止进程)
    Term,
    /// Ign (默认: 忽略)
    Ign,
    /// Core (默认: 核心转储)
    Core,
    /// Stop (默认: 停止)
    Stop,
    /// Cont (默认: 继续)
    Cont,
}

impl SignalDisposition {
    /// POSIX 标准信号默认动作
    pub fn default_for(sig: StandardSignal) -> Self {
        match sig {
            StandardSignal::Chld | StandardSignal::Urg => Self::Ign,
            StandardSignal::Stop
            | StandardSignal::Tstp
            | StandardSignal::Ttin
            | StandardSignal::Ttou => Self::Stop,
            StandardSignal::Cont => Self::Cont,
            s if s.is_core_dump() => Self::Core,
            _ => Self::Term,
        }
    }
}

/// 信号处理动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAction {
    /// 默认动作
    Default,
    /// 忽略
    Ignore,
    /// 用户态处理函数 (handler 入口地址)
    Handler(u64),
}

impl SignalAction {
    /// 是否为默认
    pub fn is_default(self) -> bool {
        matches!(self, Self::Default)
    }
}

// ============================================================================
// 信号错误
// ============================================================================

/// 信号错误 — TD-16: 改用 `KernelError` 单一来源, 0 字段 type alias.
///
/// 历史定义 (v2.15) 5 字段 (NoSuchProcess/PermissionDenied/InvalidSignal/
/// ProcessExited/Other(i32)) 全部对应 `KernelError` 已覆盖的 POSIX 类别, 没有
/// signal 子系统私有错误, 因此退化为 alias. 旧名保留以避免破坏外部引用.
pub use crate::services::error::KernelError as SignalError;

pub type SignalResult<T> = Result<T, SignalError>;

// ============================================================================
// 信号传递 (委托 proc::table)
// ============================================================================

/// 向指定进程发送信号
///
/// **实现**: 设置 `signal_pending` 位, 由内核在返回用户态前检查并分发
///
/// # Errors
///
/// - 目标进程不存在(`kill(pid, 0)` 检查) → `NoSuchProcess`
/// - 信号编号 `> 63` → `InvalidArgument`
/// (sig 上限 63 避开 `1u64 << 64` UB)
pub fn send(pid: crate::framework::proc::Pid, sig: Signal) -> SignalResult<()> {
    if sig == Signal::NONE {
        // POSIX: kill(pid, 0) 仅检查进程存在, 不发送
        return crate::services::proc::table::with(pid, |_p| ()).ok_or(SignalError::NoSuchProcess);
    }
    if sig.0 > 63 {
        return Err(SignalError::InvalidArgument);
    }
    crate::services::proc::table::signal_set(pid, u32::from(sig.0))
        .map_err(|_| SignalError::NoSuchProcess)
}

/// 检查进程是否有信号待处理
pub fn pending(pid: crate::framework::proc::Pid) -> Option<u64> {
    crate::services::proc::table::signal_get(pid)
}

/// 清除进程的信号位
///
/// # Errors
///
/// 当目标进程不存在时返回 `NoSuchProcess`.
pub fn clear(pid: crate::framework::proc::Pid, mask: u64) -> SignalResult<()> {
    crate::services::proc::table::signal_clear(pid, mask).map_err(|_| SignalError::NoSuchProcess)
}

// ============================================================================
// 便利函数
// ============================================================================

/// 终止进程 (等价 kill(pid, SIGKILL))
///
/// # Errors
///
/// 当进程不存在或信号非法时返回对应的 `SignalError`(由 `send` 传播).
pub fn kill(pid: crate::framework::proc::Pid) -> SignalResult<()> {
    send(pid, Signal::standard(StandardSignal::Kill))
}

/// 中断进程 (等价 kill(pid, SIGINT))
///
/// # Errors
///
/// 当进程不存在或信号非法时返回对应的 `SignalError`(由 `send` 传播).
pub fn interrupt(pid: crate::framework::proc::Pid) -> SignalResult<()> {
    send(pid, Signal::standard(StandardSignal::Int))
}

/// 停止进程 (等价 kill(pid, SIGSTOP))
///
/// # Errors
///
/// 当进程不存在或信号非法时返回对应的 `SignalError`(由 `send` 传播).
pub fn stop(pid: crate::framework::proc::Pid) -> SignalResult<()> {
    send(pid, Signal::standard(StandardSignal::Stop))
}

/// 唤醒已停止的进程 (等价 kill(pid, SIGCONT))
///
/// # Errors
///
/// 当进程不存在或信号非法时返回对应的 `SignalError`(由 `send` 传播).
pub fn cont(pid: crate::framework::proc::Pid) -> SignalResult<()> {
    send(pid, Signal::standard(StandardSignal::Cont))
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 越界用户指针 (`>= USER_ADDR_MAX`): 被 `check_user_buf` 拒绝而非解引用
    const BAD_USER_PTR: u64 = 0x8000_0000_0000_0000;

    #[test]
    fn signal_round_trip() {
        assert_eq!(Signal::standard(StandardSignal::Int).number(), 2);
        assert_eq!(StandardSignal::from_number(2), Some(StandardSignal::Int));
        assert_eq!(StandardSignal::from_number(0), None);
    }

    #[test]
    fn signal_catchable() {
        assert!(StandardSignal::Int.is_catchable());
        assert!(!StandardSignal::Kill.is_catchable());
        assert!(!StandardSignal::Stop.is_catchable());
    }

    #[test]
    fn signal_core_dump() {
        assert!(StandardSignal::Segv.is_core_dump());
        assert!(StandardSignal::Bus.is_core_dump());
        assert!(!StandardSignal::Int.is_core_dump());
        assert!(!StandardSignal::Term.is_core_dump());
    }

    #[test]
    fn signal_default_disposition() {
        assert_eq!(
            SignalDisposition::default_for(StandardSignal::Chld),
            SignalDisposition::Ign
        );
        assert_eq!(
            SignalDisposition::default_for(StandardSignal::Stop),
            SignalDisposition::Stop
        );
        assert_eq!(
            SignalDisposition::default_for(StandardSignal::Segv),
            SignalDisposition::Core
        );
        assert_eq!(
            SignalDisposition::default_for(StandardSignal::Term),
            SignalDisposition::Term
        );
    }

    #[test]
    fn signal_realtime() {
        let rt = Signal::realtime(1);
        assert_eq!(rt.number(), 33);
        assert!(rt.is_realtime());
        assert!(!rt.is_standard());

        let std = Signal::standard(StandardSignal::Term);
        assert!(!std.is_realtime());
        assert!(std.is_standard());
    }

    #[test]
    fn signal_bit() {
        let sig = Signal::standard(StandardSignal::Int);
        assert_eq!(sig.to_bit(), 1u64 << 2);

        let rt = Signal::realtime(0);
        assert_eq!(rt.to_bit(), 1u64 << 32);
    }

    /// 临时信号屏蔽字替换/恢复 (`ppoll` / `epoll_pwait` 共用策略)
    ///
    /// 本用例读取/改写当前进程屏蔽字; 与 `services::fs` 的临时进程用例 (会改写
    /// 全局 `SCHEDULER.current`) 由 cargo test runner 并行执行, 若读到正在拆除
    /// 的临时进程将触发 use-after-free, 故共享 `FS_PROC_TEST_LOCK` 串行化.
    #[test]
    fn test_temporary_sigmask_swap() {
        use crate::framework::proc::{
            get_blocked_mask, process_get_current_pid, sanitize_blocked_mask, set_blocked_mask,
        };

        let _lock = crate::services::fs::FS_GLOBAL_TEST_LOCK.lock();

        // sigmask == NULL: 直接执行, sigsetsize 不参与校验
        assert_eq!(
            with_temporary_sigmask(0, 99, || Ok(7)),
            Ok(7),
            "sigmask=NULL 应透传闭包结果"
        );

        // 不可屏蔽信号位被剔除
        assert_eq!(
            sanitize_blocked_mask(u64::MAX),
            !((1u64 << 9) | (1u64 << 19)),
            "SIGKILL/SIGSTOP 位应被剔除"
        );

        // 有当前进程时: 替换 → 闭包内可见新掩码 → 返回后恢复
        let pid = process_get_current_pid();
        if pid != 0 {
            let original = get_blocked_mask(pid);
            // sigmask 指针越界 → EFAULT (此处仅验证错误路径不污染原掩码)
            let err = with_temporary_sigmask(BAD_USER_PTR, 8, || Ok(9));
            assert!(err.is_err(), "非法 sigmask 指针应返回错误");
            assert_eq!(get_blocked_mask(pid), original, "错误路径不应改变屏蔽字");
        }
        // sigsetsize != 8 → EINVAL (sigmask 非 NULL 时校验)
        match with_temporary_sigmask(BAD_USER_PTR, 4, || Ok(9)) {
            Err(crate::framework::syscall::Errno::EINVAL) => {}
            _ => panic!("sigsetsize != 8 应返回 EINVAL"),
        }
        // 恢复现场 (测试自身不留残余屏蔽字)
        if pid != 0 {
            set_blocked_mask(pid, 0);
        }
    }
}

// ============================================================================
// Syscall 安全代理
// ============================================================================

/// kill 系统调用安全代理
///
/// 验证: 信号编号 0..=63 (0 = 检查存在, 1..=31 = 标准信号, 32..=63 = RT 信号),
///       目标 pid 接受 POSIX 4 种语义 (pid>0 单进程,
///        pid=0 同进程组, pid=-1 全部, pid<-1 |pid| 进程组).
/// (sig 上限 63 避开 `1u64 << 64` UB)
///
/// # Errors
///
/// - 信号编号不在 `0..=63` 范围 → `EINVAL`
/// - 底层 `sys_kill` 返回负值时转换为对应的 `Errno`
pub fn kill_syscall(pid: i32, sig: i32) -> Result<usize, crate::framework::syscall::Errno> {
    use crate::framework::syscall::Errno;

    // 验证信号编号 (POSIX kill: 0 = 检查存在, 1..=63 = 标准 + RT 信号)
    if !(0..=63).contains(&sig) {
        return Err(Errno::EINVAL);
    }
    // 验证 pid 范围 (POSIX: pid 必须非 0, -1, <-1 之一; pid=0 合法)
    // 原约束 pid <= 0 -> ESRCH 已移除 (TRACK-315B7C 解决)
    // 最小校验: pid 至少 0 或负数 (i32 范围), 由 framework 内部 4 路径分发

    let ret = crate::framework::syscall::api::sys_kill(pid, sig);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// `tgkill(tgid, tid, sig)` 策略 — 向进程内指定线程发送信号
///
/// 校验链: 参数范围 → 目标存在性 → tgid/tid 归属关系 → 投递.
/// `sig == 0` 为存在性探测 (不投递, 合法返回 0).
///
// SIMPLIFIED: 当前无线程组模型 (tid == pid, 每个进程即自身线程组), 以
// `tgid == tid` 等价校验替代 Linux "tid 必须属于 tgid 线程组" 判定;
// 影响面: 多线程进程内定向投递暂不可用 (线程组到来前无此语义);
// 何时需扩展: multithreading-project K-06 引入线程组后改为查询 tid
// 所属进程的 tgid 字段比对.
///
/// # Errors
///
/// - `tgid <= 0` 或 `tid <= 0` 或 `sig` 不在 `0..=63` → `EINVAL`
/// - tgid/tid 不匹配或目标进程不存在 → `ESRCH`
#[expect(
    clippy::similar_names,
    reason = "变量名相似表达同族概念 (tgid/tid); 重命名会破坏 POSIX 语义对应, 仅在确实混淆时才人工拆分"
)]
pub fn tgkill_syscall(
    tgid: i32,
    tid: i32,
    sig: i32,
) -> Result<usize, crate::framework::syscall::Errno> {
    use crate::framework::syscall::Errno;

    if tgid <= 0 || tid <= 0 {
        return Err(Errno::EINVAL);
    }
    if !(0..=63).contains(&sig) {
        return Err(Errno::EINVAL);
    }

    // 目标存在性校验 (tid 在当前模型下即 pid)
    let target =
        crate::framework::proc::api::process_with(tid as u32, |p| p.pid).ok_or(Errno::ESRCH)?;
    if target.0 != tgid as u32 {
        // 无线程组模型下 tid 所属进程的 tgid 即其 pid
        return Err(Errno::ESRCH);
    }
    if sig == 0 {
        return Ok(0);
    }

    let ret = crate::framework::syscall::api::sys_kill(tid, sig);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// `rt_sigaction` 系统调用安全代理
///
/// 验证: signum 1..=31 (标准信号) 或 32..=63 (RT信号)
/// (sig 上限 63 避开 `1u64 << 64` UB)
///
/// # Errors
///
/// - 信号编号不在 `1..=63` 范围或试图操作不可捕获信号(`SIGKILL`/`SIGSTOP`) → `EINVAL`
/// - 底层 `sys_rt_sigaction` 返回负值时转换为对应的 `Errno`
pub fn rt_sigaction_syscall(
    signum: i32,
    act: u64,
    oact: u64,
) -> Result<usize, crate::framework::syscall::Errno> {
    use crate::framework::syscall::Errno;

    // 验证信号编号 (SIGKILL=9 和 SIGSTOP=19 不可捕获)
    if !(1..=63).contains(&signum) {
        return Err(Errno::EINVAL);
    }
    if signum == 9 || signum == 19 {
        return Err(Errno::EINVAL);
    }

    let ret = crate::framework::syscall::api::sys_rt_sigaction(signum, act, oact);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// `rt_sigprocmask` 系统调用安全代理
///
/// 验证: how 有效 (`SIG_BLOCK=0`, `SIG_UNBLOCK=1`, `SIG_SETMASK=2`)
///
/// # Errors
///
/// - `how` 不在 `0..=2` 范围 → `EINVAL`
/// - 底层 `sys_rt_sigprocmask` 返回负值时转换为对应的 `Errno`
pub fn rt_sigprocmask_syscall(
    how: i32,
    set: u64,
    oset: u64,
) -> Result<usize, crate::framework::syscall::Errno> {
    use crate::framework::syscall::Errno;

    // 验证 how
    if !(0..=2).contains(&how) {
        return Err(Errno::EINVAL);
    }

    let ret = crate::framework::syscall::api::sys_rt_sigprocmask(how, set, oset);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

/// 临时替换信号屏蔽字执行等待 (`ppoll` / `epoll_pwait` 共用策略, T1 G5)
///
/// Linux 语义: `sigmask` 非 NULL 时以原子方式替换当前屏蔽字, 等待结束后恢复
/// 原值. 本实现以"设置 → 执行 → 恢复"的顺序等效实现: 等待期间屏蔽字保持替换
/// 后的值, 故等待中被唤醒的信号按替换后的屏蔽字判定是否投递.
///
/// 不可屏蔽信号 (`SIGKILL`/`SIGSTOP`) 经
/// `framework::proc::sanitize_blocked_mask` 剔除, 与 `rt_sigprocmask` 同款.
///
/// # Errors
///
/// - `sigsetsize != 8` (u64 尺寸的 `sigset_t`) → `EINVAL`
/// - `sigmask` 指向的用户内存不可读 → `EFAULT`
/// - 无当前进程 → `ESRCH`
pub fn with_temporary_sigmask<T, F>(
    sigmask_ptr: u64,
    sigsetsize: u64,
    f: F,
) -> Result<T, crate::framework::syscall::Errno>
where
    F: FnOnce() -> Result<T, crate::framework::syscall::Errno>,
{
    use crate::framework::syscall::Errno;

    // sigmask == NULL: 不替换屏蔽字 (sigsetsize 按 Linux 语义忽略)
    if sigmask_ptr == 0 {
        return f();
    }
    if sigsetsize != core::mem::size_of::<u64>() as u64 {
        return Err(Errno::EINVAL);
    }
    let Some(new_mask) = crate::framework::syscall::api::read_u64_from_user(sigmask_ptr) else {
        return Err(Errno::EFAULT);
    };
    let pid = crate::framework::proc::process_get_current_pid();
    if pid == 0 {
        return Err(Errno::ESRCH);
    }

    let old_mask = crate::framework::proc::get_blocked_mask(pid);
    crate::framework::proc::set_blocked_mask(
        pid,
        crate::framework::proc::sanitize_blocked_mask(new_mask),
    );
    let result = f();
    crate::framework::proc::set_blocked_mask(pid, old_mask);
    result
}

/// P1-I-45: sigaltstack 系统调用安全代理
///
/// 验证: 调用方身份 (current pid 必然存在) + 委托到 framework `sys_sigaltstack`.
///
/// 注: `ss` 与 `old_ss` 的合法性 (用户缓冲可读/可写) 由 framework 侧
/// `raw::check_user_buf` 再次校验, services 层不重复检查.
///
/// # Errors
///
/// 当底层 `sys_sigaltstack` 返回负值(如用户缓冲非法)时转换为对应的 `Errno`.
pub fn sigaltstack_syscall(
    ss: u64,
    old_ss: u64,
) -> Result<usize, crate::framework::syscall::Errno> {
    use crate::framework::syscall::Errno;

    let ret = crate::framework::syscall::api::sys_sigaltstack(ss, old_ss);
    if ret < 0 {
        Err(Errno::from_ret(ret))
    } else {
        Ok(ret as usize)
    }
}

// ============================================================================
// REVAL-1: services 层 SignalDecision 实现
// ============================================================================
//
// T-06 信号投递策略: framework/proc/signal_trait.rs 已定义 SignalDecision
// trait, 之前的 SKIP 理由 (中断路径高频) 通过 `OnceLock<&'static dyn>`
// 零成本读取引用解决 (已经是 vtable 一次解析, 后续调用直接通过指针).
// 本文件实现 StandardSignalPolicy, 在 services::proc::init() 中注册.

use crate::framework::proc::SignalDefaultAction;
use crate::framework::proc::signal_trait::SignalDecision;

/// 标准 POSIX 信号策略 (services 端实现)
///
/// 行为与 `FallbackSignalPolicy` 一致, 但放置在 services 层
/// 体现 framekernel 架构: framework 只保留 unsafe 机制, 策略归 services.
///
/// # Safety
///
/// 本结构体所有方法为纯函数, 不涉及硬件/内存/线程, 无 unsafe 需求.
pub struct StandardSignalPolicy;

impl SignalDecision for StandardSignalPolicy {
    fn default_action(&self, sig: u8) -> SignalDefaultAction {
        match sig {
            17 | 23 => SignalDefaultAction::Ign,
            19 | 20 | 21 | 22 => SignalDefaultAction::Stop,
            18 => SignalDefaultAction::Cont,
            3 | 4 | 6 | 7 | 8 | 11 | 31 | 24 | 25 => SignalDefaultAction::Core,
            _ => SignalDefaultAction::Term,
        }
    }

    fn is_uncatchable(&self, sig: u8) -> bool {
        sig == 9 || sig == 19
    }

    fn pick_next_signal(&self, deliverable: u64) -> Option<u8> {
        if deliverable == 0 {
            return None;
        }
        let sig_bit = deliverable.trailing_zeros() as u8;
        // sig_bit ∈ [1, 63]: 对应信号 1..=63 (bit 0 = sig 1)
        // sig=64 对应 bit 63 (不是 64), u8 上限避免 trailing_zeros()=64 UB
        if sig_bit == 0 || sig_bit >= 64 {
            return None;
        }
        Some(sig_bit)
    }
}

/// 注册标准信号策略到 framework
///
/// 由 `services::proc::init()` 调用. 只能成功一次, 后续调用返回 `Err(())`.
/// 启动期重复注册由 `proc::init()` 以 panic 暴露 (B05-44 返工);
/// 本 API 自身仍返回 `Err` 供运行时调用方降级处理.
///
/// # Errors
///
/// 当标准信号策略已被注册时返回 `Err(())`.
pub fn register_standard_signal_policy() -> Result<(), ()> {
    static POLICY: StandardSignalPolicy = StandardSignalPolicy;
    crate::framework::proc::register_signal_decision(&POLICY).map_err(|_| ())
}
