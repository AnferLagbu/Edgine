#![deny(unsafe_code)]
#![deny(missing_docs)]
//! 进程级 wasm 运行时注册表 (W-7 载体)
//!
//! `EG_WASM_LOAD` 把模块解析为 [`WasmInstance`] 并按**句柄**登记到全局表;
//! 实例本体归 functions 侧持有, privileged 侧只在 `Process.wasm_handle` 记录
//! 当前句柄 —— 归属校验 (句柄 == 进程字段) 使未加载进程无法借用或猜测他进程
//! 实例, fork 子进程从 0 开始、exec 清零.
//!
//! ## 生命周期
//!
//! - 正常路径: 用户态入口桩 `LOAD` → `RUN` → `GET_INFO` → `UNLOAD` 后退出.
//! - 异常路径 (进程被信号杀死 / exec 换映像): 注册表条目由下一次 `LOAD` 的
//!   `reap_stale` 按"进程仍存在且 `wasm_handle` 仍等于该句柄"判据回收;
//!   条目数上限 `MAX_WASM_INSTANCES` 保证回收前不会无界增长.
//!
//! ## 并发
//!
//! 实例在 `RUN` 期间从注册表中**取出** (槽位置 `None`), 执行完放回; 注册表锁
//! 不跨解释器执行持有, 因此 WASI 宿主调用内部的 VFS/网络锁不在本锁之下.
//! 同一句柄并发 `RUN` 的第二个调用看到空槽即 `EBUSY`.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::functions::wasm::instance::WasmInstance;
use crate::functions::wasm::module::parse_wasm;
use crate::functions::wasm::runtime::InterpreterConfig;
use crate::functions::wasm::types::{ExportKind, WasmError, WasmModule};
use crate::functions::wasm::wasi::WasiContext;
use crate::privileged::proc::{process_get_current_pid, process_with, process_with_mut};
use crate::privileged::sync::IrqSpinLock;
use crate::privileged::syscall::Errno;

/// 注册表条目上限 (超限回收后仍满则 `ENOSPC`).
const MAX_WASM_INSTANCES: usize = 64;
/// 模块文件读取上限, 与 exec 的 ELF 上限一致 (1 MiB).
const WASM_MAX_FILE_SIZE: usize = 1024 * 1024;
/// 用户参数字符串上限 (字节).
const MAX_USER_STRING: usize = 1024;
/// `argv` / `envp` 数组条目上限.
const MAX_STRVEC_ITEMS: usize = 64;

/// 全局实例注册表: handle → 运行时条目.
static RUNTIMES: IrqSpinLock<BTreeMap<u32, RuntimeEntry>> =
    IrqSpinLock::named_lazy("WASM_REGISTRY", BTreeMap::new());
/// 下一个句柄; 0 保留为"未加载".
static NEXT_HANDLE: AtomicU32 = AtomicU32::new(1);

/// 注册表条目
struct RuntimeEntry {
    /// 归属进程 pid.
    owner: u32,
    /// 入口导出名 (`_start` 优先, 否则 `main`).
    entry: &'static str,
    /// 实例状态 (`EG_WASM_STATUS_*`).
    status: u32,
    /// 最近一次运行记录的退出码.
    exit_code: i32,
    /// 最近一次运行记录的 gas 消耗.
    gas_used: u64,
    /// 解释器实例; `None` 表示正在执行 (已从槽位取出).
    instance: Option<WasmInstance>,
}

/// 当前进程 pid; idle (0) 或查询失败返回 `ESRCH`.
fn current_pid() -> Result<u32, Errno> {
    let pid = process_get_current_pid();
    if pid == 0 { Err(Errno::Esrch) } else { Ok(pid) }
}

/// 校验句柄属于当前进程并返回窄化后的句柄.
fn owned_handle(pid: u32, handle: u64) -> Result<u32, Errno> {
    let h = u32::try_from(handle).map_err(|_| Errno::Ebadf)?;
    if h == 0 {
        return Err(Errno::Ebadf);
    }
    let current =
        process_with(pid, |p| p.wasm_handle.load(Ordering::Acquire)).ok_or(Errno::Esrch)?;
    if current != h {
        return Err(Errno::Ebadf);
    }
    Ok(h)
}

/// 选择模块入口导出: `_start` 优先, 否则 `main`; 都没有返回 `ENOENT`.
fn entry_point(module: &WasmModule) -> Result<&'static str, Errno> {
    for wanted in ["_start", "main"] {
        let found = module
            .exports
            .iter()
            .any(|e| e.kind == ExportKind::Function && e.name == wanted.as_bytes());
        if found {
            return Ok(wanted);
        }
    }
    Err(Errno::Enoent)
}

/// 回收失效条目: 进程不存在, 或 `wasm_handle` 已不再指向该句柄 (exec / 他处
/// `LOAD`). 在持注册表锁时调用; `process_with` 取进程表锁, 两层同为层级 7.
fn reap_stale(map: &mut BTreeMap<u32, RuntimeEntry>) {
    map.retain(|handle, entry| {
        process_with(entry.owner, |p| {
            p.wasm_handle.load(Ordering::Acquire) == *handle
        })
        .unwrap_or(false)
    });
}

/// 分配新句柄 (跳过 0; 回绕后重复由注册表插入前 `reap_stale` 与本进程单实例约束兜底).
fn next_handle() -> u32 {
    loop {
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        if handle != 0 {
            return handle;
        }
    }
}

/// 已解析模块的登记入口 (供 `EG_WASM_LOAD` 与 W-7b 的 exec 派发共用).
///
/// 校验入口导出存在, 构造实例 (WASI 上下文含 `args` / `env`), 在注册表与
/// `Process.wasm_handle` 双侧完成归属登记.
///
/// # Errors
///
/// - 无当前进程 → `ESRCH`
/// - 模块缺 `_start` / `main` 导出 → `ENOENT`
/// - 本进程已有载体 → `EBUSY`
/// - 注册表满 (回收失效条目后仍满) → `ENOSPC`
pub fn load_module(
    module: WasmModule,
    args: Vec<String>,
    env: Vec<(String, String)>,
) -> Result<usize, Errno> {
    let pid = current_pid()?;
    let entry = entry_point(&module)?;

    let current =
        process_with(pid, |p| p.wasm_handle.load(Ordering::Acquire)).ok_or(Errno::Esrch)?;
    if current != 0 {
        return Err(Errno::Ebusy);
    }

    let mut ctx = WasiContext::new();
    ctx.args = args;
    ctx.env = env;
    let instance = WasmInstance::new(module, InterpreterConfig::default(), ctx);

    let handle = next_handle();
    {
        let mut map = RUNTIMES.lock();
        reap_stale(&mut map);
        if map.len() >= MAX_WASM_INSTANCES {
            return Err(Errno::Enospc);
        }
        map.insert(
            handle,
            RuntimeEntry {
                owner: pid,
                entry,
                status: edgine_abi::wasm::EG_WASM_STATUS_READY,
                exit_code: 0,
                gas_used: 0,
                instance: Some(instance),
            },
        );
    }

    let claimed = process_with_mut(pid, |p| {
        if p.wasm_handle.load(Ordering::Acquire) == 0 {
            p.wasm_handle.store(handle, Ordering::Release);
            true
        } else {
            false
        }
    })
    .unwrap_or(false);
    if !claimed {
        RUNTIMES.lock().remove(&handle);
        return Err(Errno::Ebusy);
    }
    Ok(handle as usize)
}

/// 从用户数组读取 `NULL` 结尾的字符串向量; `0` 表示空.
fn read_strvec(base: u64) -> Result<Vec<String>, Errno> {
    use crate::privileged::syscall::api::{
        read_struct_from_user, read_u64_from_user, validate_user_ptr,
    };

    if base == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut slot = base;
    for _ in 0..MAX_STRVEC_ITEMS {
        if !validate_user_ptr(slot) {
            return Err(Errno::Efault);
        }
        let Some(ptr) = read_u64_from_user(slot) else {
            return Err(Errno::Efault);
        };
        if ptr == 0 {
            return Ok(out);
        }
        if !validate_user_ptr(ptr) {
            return Err(Errno::Efault);
        }
        let mut bytes = Vec::new();
        let mut cursor = ptr;
        let mut terminated = false;
        for _ in 0..MAX_USER_STRING {
            let mut byte = 0u8;
            if !read_struct_from_user(cursor, &mut byte) {
                return Err(Errno::Efault);
            }
            if byte == 0 {
                terminated = true;
                break;
            }
            bytes.push(byte);
            cursor = cursor.checked_add(1).ok_or(Errno::Efault)?;
        }
        if !terminated {
            return Err(Errno::Enametoolong);
        }
        out.push(
            core::str::from_utf8(&bytes)
                .map_err(|_| Errno::Einval)?
                .to_string(),
        );
        slot = slot.checked_add(8).ok_or(Errno::Efault)?;
    }
    Err(Errno::E2BIG)
}

/// `EG_WASM_LOAD(path_ptr, argv_ptr, envp_ptr)`.
///
/// 读取模块文件 (≤1 MiB) 并解析; 参数字符串从用户内存复制 (`argv[0]` 若为模块
/// 路径也保留, 与 WASI `args_get` 的自然语义一致), 环境变量按首个 `=` 拆成
/// 键值对.
///
/// # Errors
///
/// - 路径/数组/字符串指针非法 → `EFAULT`
/// - 文件不存在、空文件或超过上限 → `ENOENT` / `EINVAL`
/// - 模块畸形 → `ENOEXEC` (见 [`Errno`] 的 `WasmError` 映射)
/// - 无 `_start` / `main` 导出 → `ENOENT`
/// - 本进程已有载体 → `EBUSY`; 注册表满 → `ENOSPC`
pub fn load_syscall(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> Result<usize, Errno> {
    let pwm = crate::privileged::sgeg::get_current_pwm();
    let Some(bytes) =
        crate::functions::fs::api::read_file_to_vec(path_ptr as *const u8, pwm, WASM_MAX_FILE_SIZE)
    else {
        return Err(Errno::Enoent);
    };
    let module = parse_wasm(&bytes).map_err(Errno::from)?;

    let args = read_strvec(argv_ptr)?;
    let env = read_strvec(envp_ptr)?
        .into_iter()
        .filter_map(|item| {
            let (key, value) = item.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect();

    load_module(module, args, env)
}

/// `EG_WASM_RUN(handle)`: 把实例从注册表取出, 执行入口导出到结束, 再放回.
///
/// SIMPLIFIED: 一次跑完 (受 `InterpreterConfig::gas` 上限), 不做步进让出/恢复;
/// 影响面: 长任务在单次 syscall 内完成, 抢占依赖内核既有调度点;
/// 何时需扩展: 解释器提供步预算/续跑状态后改为分片执行.
///
/// # Errors
///
/// - 句柄不属于当前进程 / 未加载 → `EBADF`
/// - 实例已完成或正在执行 → `EINVAL` / `EBUSY`
/// - 执行 trap → [`Errno`] 映射 (见 `WasmError → Errno`)
pub fn run_syscall(handle: u64) -> Result<usize, Errno> {
    let pid = current_pid()?;
    let h = owned_handle(pid, handle)?;

    let (mut instance, entry) = {
        let mut map = RUNTIMES.lock();
        let runtime = map.get_mut(&h).ok_or(Errno::Ebadf)?;
        if runtime.owner != pid {
            return Err(Errno::Ebadf);
        }
        if runtime.status != edgine_abi::wasm::EG_WASM_STATUS_READY {
            return Err(Errno::Einval);
        }
        let instance = runtime.instance.take().ok_or(Errno::Ebusy)?;
        (instance, runtime.entry)
    };

    let outcome = instance.call(entry, &[]);
    let gas_used = instance.interpreter().gas_used;
    let exit_code = instance.interpreter().exit_code;
    let (status, errno) = match outcome {
        Ok(_) | Err(WasmError::Terminated) => (edgine_abi::wasm::EG_WASM_STATUS_FINISHED, None),
        Err(error) => (
            edgine_abi::wasm::EG_WASM_STATUS_TRAPPED,
            Some(Errno::from(error)),
        ),
    };

    {
        let mut map = RUNTIMES.lock();
        if let Some(runtime) = map.get_mut(&h) {
            runtime.status = status;
            runtime.exit_code = exit_code;
            runtime.gas_used = gas_used;
            runtime.instance = Some(instance);
        }
    }

    match errno {
        Some(e) => Err(e),
        None => Ok(0),
    }
}

/// `EG_WASM_GET_INFO(handle, out_ptr)`: 把状态 / 退出码 / gas 消耗写给用户.
///
/// # Errors
///
/// - 句柄不属于当前进程 → `EBADF`
/// - 写出失败 → `EFAULT`
pub fn get_info_syscall(handle: u64, out_ptr: u64) -> Result<usize, Errno> {
    if out_ptr == 0 {
        return Err(Errno::Efault);
    }
    let pid = current_pid()?;
    let h = owned_handle(pid, handle)?;

    let info = {
        let map = RUNTIMES.lock();
        let runtime = map.get(&h).ok_or(Errno::Ebadf)?;
        if runtime.owner != pid {
            return Err(Errno::Ebadf);
        }
        edgine_abi::wasm::EgWasmInfo {
            status: runtime.status,
            exit_code: runtime.exit_code,
            gas_used: runtime.gas_used,
        }
    };

    if crate::privileged::syscall::api::write_struct_to_user(out_ptr, &info) {
        Ok(0)
    } else {
        Err(Errno::Efault)
    }
}

/// `EG_WASM_UNLOAD(handle)`: 移除注册表条目并清零进程的载体句柄.
///
/// # Errors
///
/// - 句柄不属于当前进程 → `EBADF`
/// - 实例正在执行 → `EBUSY`
pub fn unload_syscall(handle: u64) -> Result<usize, Errno> {
    let pid = current_pid()?;
    let h = owned_handle(pid, handle)?;

    {
        let mut map = RUNTIMES.lock();
        let runtime = map.get(&h).ok_or(Errno::Ebadf)?;
        if runtime.owner != pid {
            return Err(Errno::Ebadf);
        }
        if runtime.instance.is_none() {
            return Err(Errno::Ebusy);
        }
        map.remove(&h);
    }

    let _ = process_with_mut(pid, |p| {
        if p.wasm_handle.load(Ordering::Acquire) == h {
            p.wasm_handle.store(0, Ordering::Release);
        }
    });
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::wasm::types::{
        ExportDesc, FuncType, FunctionBody, Limits, MemoryType, ValueType,
    };
    use crate::privileged::proc::PROCESS_TABLE;
    use crate::privileged::proc::raw;
    use crate::privileged::proc::scheduler::Scheduler;
    use alloc::vec;

    /// 安装当前临时进程 (含独立 fd 表), 闭包结束无条件拆除.
    fn with_temp_process<F: FnOnce()>(name: &str, f: F) {
        let Some(pid) = PROCESS_TABLE.allocate_pid() else {
            panic!("临时进程: 无空闲 pid");
        };
        let proc_ptr = raw::alloc_process(pid, name, None);
        if !PROCESS_TABLE.insert(proc_ptr) {
            raw::drop_boxed_process(proc_ptr);
            PROCESS_TABLE.free_pid(pid);
            panic!("临时进程: 插入进程表失败");
        }
        let prev_current = Scheduler::current();
        Scheduler::set_current(pid);
        f();
        Scheduler::set_current(prev_current.unwrap_or(0));
        let _ = PROCESS_TABLE.remove(pid);
        raw::drop_boxed_process(proc_ptr);
        PROCESS_TABLE.free_pid(pid);
    }

    /// 最小模块: 导出 `_start`, 返回 `i32.const 0`.
    fn start_module() -> WasmModule {
        WasmModule {
            types: vec![FuncType {
                params: vec![],
                results: vec![ValueType::I32],
            }],
            imports: vec![],
            functions: vec![0],
            tables: Vec::new(),
            memories: vec![MemoryType {
                limits: Limits {
                    min: 1,
                    max: Some(1),
                },
            }],
            globals: Vec::new(),
            exports: vec![ExportDesc {
                name: b"_start".to_vec(),
                kind: ExportKind::Function,
                index: 0,
            }],
            start_section: None,
            elements: Vec::new(),
            code: vec![FunctionBody {
                locals: Vec::new(),
                code: vec![0x41, 0x00, 0x0B],
            }],
            data: Vec::new(),
        }
    }

    fn read_u32(mem: &[u8], addr: usize) -> u32 {
        u32::from_le_bytes([mem[addr], mem[addr + 1], mem[addr + 2], mem[addr + 3]])
    }

    #[test]
    fn load_run_unload_lifecycle() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("wasm-lifecycle", || {
            let handle = load_module(start_module(), vec![], vec![]).expect("load");
            assert!(handle != 0);
            assert_eq!(
                process_with(current_pid().expect("pid"), |p| p
                    .wasm_handle
                    .load(Ordering::Acquire))
                .expect("process"),
                u32::try_from(handle).expect("句柄窄化")
            );

            assert_eq!(run_syscall(handle as u64), Ok(0), "一次跑完返回 0");
            let mut info = edgine_abi::wasm::EgWasmInfo {
                status: 0,
                exit_code: 0,
                gas_used: 0,
            };
            let info_ptr = core::ptr::from_mut(&mut info) as u64;
            assert_eq!(get_info_syscall(handle as u64, info_ptr), Ok(0));
            assert_eq!(info.status, edgine_abi::wasm::EG_WASM_STATUS_FINISHED);
            assert_eq!(info.exit_code, 0);
            assert!(info.gas_used > 0, "gas 计量应有读数");

            assert_eq!(
                run_syscall(handle as u64),
                Err(Errno::Einval),
                "已完成不可再跑"
            );
            assert_eq!(unload_syscall(handle as u64), Ok(0));
            assert_eq!(
                run_syscall(handle as u64),
                Err(Errno::Ebadf),
                "卸载后句柄失效"
            );
        });
    }

    #[test]
    fn second_load_is_busy() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("wasm-busy", || {
            let handle = load_module(start_module(), vec![], vec![]).expect("load");
            assert_eq!(
                load_module(start_module(), vec![], vec![]),
                Err(Errno::Ebusy)
            );
            assert_eq!(unload_syscall(handle as u64), Ok(0));
        });
    }

    #[test]
    fn foreign_handle_is_rejected() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        let mut foreign = 0usize;
        with_temp_process("wasm-owner", || {
            foreign = load_module(start_module(), vec![], vec![]).expect("load");
        });
        with_temp_process("wasm-thief", || {
            assert_eq!(run_syscall(foreign as u64), Err(Errno::Ebadf));
            assert_eq!(unload_syscall(foreign as u64), Err(Errno::Ebadf));
        });
        // 回收失效条目 (owner 进程已拆除)
        {
            let mut map = RUNTIMES.lock();
            reap_stale(&mut map);
            assert!(!map.contains_key(&u32::try_from(foreign).expect("句柄窄化")));
        }
    }

    #[test]
    fn run_writes_wasi_context_and_memory() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        with_temp_process("wasm-env", || {
            let handle = load_module(
                start_module(),
                vec!["prog".to_string(), "a".to_string()],
                vec![("K".to_string(), "V".to_string())],
            )
            .expect("load");
            assert_eq!(run_syscall(handle as u64), Ok(0));
            {
                let map = RUNTIMES.lock();
                let entry = map
                    .get(&u32::try_from(handle).expect("句柄窄化"))
                    .expect("entry");
                let instance = entry.instance.as_ref().expect("实例已放回");
                instance.with_context(|ctx| {
                    assert_eq!(ctx.args.len(), 2);
                    assert_eq!(ctx.env.len(), 1);
                });
                assert_eq!(entry.status, edgine_abi::wasm::EG_WASM_STATUS_FINISHED);
                assert_eq!(
                    read_u32(
                        instance
                            .interpreter()
                            .memory
                            .as_ref()
                            .expect("memory")
                            .data
                            .as_slice(),
                        0
                    ),
                    0
                );
            }
            assert_eq!(unload_syscall(handle as u64), Ok(0));
        });
    }
}
