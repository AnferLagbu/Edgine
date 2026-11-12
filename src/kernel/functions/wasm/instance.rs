#![deny(unsafe_code)]
#![deny(missing_docs)]
//! WASM 实例 — 解释器与 WASI 上下文的生命周期归属
//!
//! 解释器 (`Interpreter`) 只认识 host 闭包, 不认识 WASI; `WasiContext`
//! 与宿主闭包通过 `Arc<Mutex<WasiContext>>` 共享, 由本结构持有并随实例释放。
//! 这样 runtime 不反向依赖 WASI, 而 WASI 的 fd 表 / 参数 / 环境变量仍有
//! 明确的落点 (W-6 起由装载路径填充)。
//!
//! `Arc<Mutex>` 而非 `Rc<RefCell>`: W-7 的进程级载体把实例放进 functions 侧
//! 全局注册表 (需 `Send`), 宿主调用期间以锁保护上下文; 实例本身被"取出执行、
//! 执行完放回", 同一实例不会并发执行。

use alloc::sync::Arc;

use super::interpreter::Interpreter;
use super::runtime::InterpreterConfig;
use super::types::{Value, WasmError, WasmModule};
use super::wasi::WasiContext;
use super::wasi::inject::register_wasi_functions;
use crate::privileged::sync::Mutex;

/// 单个 WASM 实例: 解释器 + 本实例独占的 WASI 上下文
pub struct WasmInstance {
    interp: Interpreter,
    ctx: Arc<Mutex<WasiContext>>,
}

impl WasmInstance {
    /// 由模块与配置构造实例, 注入全部 WASI 宿主函数并按 import 段装配下标.
    ///
    /// `ctx` 随实例生命周期结束; 之后的调用与上下文的交互都经过同一句柄。
    #[must_use]
    pub fn new(module: WasmModule, config: InterpreterConfig, ctx: WasiContext) -> Self {
        let ctx = Arc::new(Mutex::named("WASI_CTX", ctx));
        let mut interp = Interpreter::new(module, config);
        register_wasi_functions(&mut interp, &ctx);
        interp.auto_register_wasi();
        Self { interp, ctx }
    }

    /// 调用导出函数.
    ///
    /// # Errors
    ///
    /// 导出名不存在、参数不匹配或执行 trap 时返回对应 [`WasmError`]。
    pub fn call(&mut self, name: &str, args: &[Value]) -> Result<Option<Value>, WasmError> {
        self.interp.call(name, args)
    }

    /// 解释器只读视图 (线性内存 / 退出码检查)
    #[must_use]
    pub fn interpreter(&self) -> &Interpreter {
        &self.interp
    }

    /// 只读访问本实例的 WASI 上下文
    pub fn with_context<R>(&self, f: impl FnOnce(&WasiContext) -> R) -> R {
        f(&self.ctx.lock())
    }

    /// 可变访问本实例的 WASI 上下文
    pub fn with_context_mut<R>(&mut self, f: impl FnOnce(&mut WasiContext) -> R) -> R {
        f(&mut self.ctx.lock())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::wasm::types::{
        DataSegment, ExportDesc, ExportKind, FuncType, FunctionBody, ImportDesc, ImportKind,
        Limits, MemoryType, ValueType,
    };
    use crate::functions::wasm::wasi::WASI_STDOUT;
    use crate::privileged::proc::scheduler::Scheduler;
    use crate::privileged::proc::{PROCESS_TABLE, raw};
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    fn empty_module() -> WasmModule {
        WasmModule {
            types: vec![FuncType {
                params: Vec::new(),
                results: Vec::new(),
            }],
            imports: Vec::new(),
            functions: vec![0],
            tables: Vec::new(),
            memories: Vec::new(),
            globals: Vec::new(),
            exports: Vec::new(),
            start_section: None,
            elements: Vec::new(),
            code: vec![FunctionBody {
                locals: Vec::new(),
                code: vec![0x0B],
            }],
            data: Vec::new(),
        }
    }

    /// 只 import `fd_write`、导出 `f` 的模块: `f` 以 fd 1 写出数据段的 "hello",
    /// 并把 `fd_write` 的 errno 作为返回值.
    fn stdout_module() -> WasmModule {
        // 数据段布局: 0..8 = iovec {buf:16, len:5}; 8..16 = nwritten 落点;
        // 16..21 = 载荷 "hello".
        let mut payload = vec![16, 0, 0, 0, 5, 0, 0, 0];
        payload.extend_from_slice(&[0u8; 8]);
        payload.extend_from_slice(b"hello");
        WasmModule {
            types: vec![
                FuncType {
                    params: vec![ValueType::I32; 4],
                    results: vec![ValueType::I32],
                },
                FuncType {
                    params: Vec::new(),
                    results: vec![ValueType::I32],
                },
            ],
            imports: vec![ImportDesc {
                module: b"wasi_snapshot_preview1".to_vec(),
                name: b"fd_write".to_vec(),
                desc: ImportKind::Function(0),
            }],
            functions: vec![1],
            tables: Vec::new(),
            memories: vec![MemoryType {
                limits: Limits {
                    min: 1,
                    max: Some(1),
                },
            }],
            globals: Vec::new(),
            exports: vec![ExportDesc {
                name: b"f".to_vec(),
                kind: ExportKind::Function,
                index: 1,
            }],
            start_section: None,
            elements: Vec::new(),
            code: vec![FunctionBody {
                locals: Vec::new(),
                code: vec![
                    0x41, 0x01, // arg0: fd = 1 (标准输出)
                    0x41, 0x00, // arg1: iovs 指针 = 0
                    0x41, 0x01, // arg2: iovec 数量 = 1
                    0x41, 0x08, // arg3: nwritten 落点 = 8
                    0x10, 0x00, // 调用导入函数 fd_write
                    0x0B, // 函数体结束
                ],
            }],
            data: vec![DataSegment {
                memory_index: 0,
                offset: vec![0x41, 0x00, 0x0B],
                data: payload,
            }],
        }
    }

    /// 临时安装当前进程 (含独立 FdTable), 执行闭包后无条件拆除现场.
    ///
    /// `fd_write` 经 `vfs_write_safe` 落当前进程 fd 表, 而 host 侧共享测试集
    /// 没有当前进程 ⇒ open / write 恒失败, 故先装临时进程 (与 fs 模块测试同口径).
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

    /// W-6 端到端回归锁: "模块写 stdout 得到字节" — `fd_write` 经真实 VFS 落盘.
    ///
    /// 宿主把 WASI fd 1 的 `inner_fd` 指向已打开文件的进程 fd; 模块数据段
    /// 预置 iovec 与 "hello". 跑 `f` 后应得到 errno 0 / nwritten=5, 且文件
    /// 内容为 "hello".
    #[test]
    fn module_writes_stdout_through_vfs() {
        let _lock = crate::functions::fs::FS_GLOBAL_TEST_LOCK.lock();
        crate::functions::fs::init();
        crate::functions::fs::ramfs_core::init();
        let _ = crate::functions::fs::api::vfs_mount_safe("/", "ramfs");

        with_temp_process("wasi-stdout-t", || {
            let created = crate::functions::fs::ramfs_core::RAMFS_DATA
                .lock()
                .create_file("/", "wasi_stdout_t", 0);
            assert!(created.is_some(), "create_file /wasi_stdout_t 失败");
            let out_fd = crate::functions::fs::api::vfs_open_safe("/wasi_stdout_t", 0, 0);
            assert!(out_fd >= 0, "open /wasi_stdout_t 应成功");

            let mut ctx = WasiContext::new();
            ctx.fd_table
                .get_mut(WASI_STDOUT)
                .expect("WASI stdout 应由 fd 表预置")
                .inner_fd = out_fd;
            let mut instance =
                WasmInstance::new(stdout_module(), InterpreterConfig::default(), ctx);

            assert_eq!(
                instance.call("f", &[]),
                Ok(Some(Value::I32(0))),
                "fd_write 应返回 errno 0 (success)"
            );
            {
                let mem = instance.interpreter().memory.as_ref().expect("线性内存");
                assert_eq!(mem.read_u32(8).expect("读回 nwritten"), 5);
            }

            let fd = out_fd.cast_unsigned();
            assert_eq!(
                crate::functions::fs::api::vfs_seek_safe(fd, 0, 0),
                0,
                "seek 回文件头应成功"
            );
            let mut buf = [0u8; 5];
            assert_eq!(
                crate::functions::fs::api::vfs_read_safe(fd, &mut buf),
                5,
                "应读回 5 字节"
            );
            assert_eq!(&buf, b"hello", "模块写 stdout 的字节应落真实 VFS");
        });
    }

    /// 上下文句柄跨调用保持, 且与实例同生命周期
    #[test]
    fn context_survives_across_calls() {
        let mut ctx = WasiContext::new();
        ctx.args.push(String::from("prog"));
        let mut instance = WasmInstance::new(empty_module(), InterpreterConfig::default(), ctx);

        assert_eq!(instance.with_context(|c| c.args.len()), 1);
        instance.with_context_mut(|c| c.args.push(String::from("--flag")));
        assert_eq!(instance.with_context(|c| c.args.len()), 2);
        assert!(instance.with_context(|c| c.args[0] == "prog"));
    }
}
