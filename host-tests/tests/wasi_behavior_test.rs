// SPDX-License-Identifier: MPL-2.0
//! W-9 行为级收口 — WASI 公共面端到端回归锁.
//!
//! 遵循 §8「测用户可见行为」: 每个用例都经真实 [`WasmInstance`] 跑一段
//! 导出函数 `f`, 由 `f` 调用 `wasi_snapshot_preview1` 宿主函数, 断言宿主
//! 写回线性内存的结果与返回的 errno, 而非逐内部函数堆内联单测.
//!
//! 覆盖面: `args_sizes_get` / `args_get` / `environ_get` / `clock_time_get`
//! (合法 + 非法) / `random_get` / `proc_exit` / `sched_yield` / `fd_read` /
//! `fd_seek` + `fd_tell` / `fd_close` (动态 + stdio) / `fd_prestat_get` +
//! `fd_prestat_dir_name` / `path_open` (存在 + 缺失). `fd_write` 与
//! `environ_sizes_get` 已分别由 `instance.rs` / `inject.rs` 内联端到端覆盖,
//! 此处不重复.
//!
//! 不在本批射程: `sock_*` 网络族 (需真实协议栈载体, 重装备, 另立专项);
//! `fd_pread` / `fd_pwrite` / `fd_readdir` 等变体与 `path_*` 其余成员同属
//! 已覆盖核心行为的派生路径, 按「量较小」口径暂不逐一铺开.
//!
//! 宿主调用参数序: 模块按声明顺序压 arg0..argN, 解释器逆序重压使 arg0
//! 位于栈顶, 宿主先弹 arg0 (与 `inject.rs` 的参数序回归锁同口径).

use std::sync::Mutex;

use edgine::kernel::functions::fs;
use edgine::kernel::functions::fs::ramfs_core::{self, RAMFS_DATA};
use edgine::kernel::functions::wasm::instance::WasmInstance;
use edgine::kernel::functions::wasm::runtime::InterpreterConfig;
use edgine::kernel::functions::wasm::types::ValueType::{self, I32, I64};
use edgine::kernel::functions::wasm::types::{
    DataSegment, ExportDesc, ExportKind, FuncType, FunctionBody, ImportDesc, ImportKind, Limits,
    MemoryType, Value, WasmError, WasmModule,
};
use edgine::kernel::functions::wasm::wasi::{
    WASI_STDIN, WasiContext, WasiFdEntry, WasiFileType, WasiRights,
};
use edgine::kernel::privileged::proc::{PROCESS_TABLE, Process, Scheduler};

/// 全局 FS / 进程表互斥 (host 测试并行会互相抹掉挂载与当前进程).
static GLOBAL: Mutex<()> = Mutex::new(());

/// 临时当前进程: 真实 `Process::new` 插入全局表, Drop 时回收.
struct TestProc {
    pid: u32,
}

impl TestProc {
    fn new(name: &str) -> Self {
        let pid = PROCESS_TABLE
            .allocate_pid()
            .expect("PROCESS_TABLE.allocate_pid 失败");
        let proc = Box::new(Process::new(pid, name, None));
        PROCESS_TABLE.insert(Box::into_raw(proc));
        Scheduler::set_current(pid);
        Self { pid }
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
        Scheduler::set_current(0);
    }
}

/// 初始化 fs / ramfs 并挂载根目录; 同一进程内重复调用无副作用.
fn init_fs_once() {
    fs::init();
    ramfs_core::init();
    let _mount: i32 = fs::vfs_mount_safe("/", "ramfs");
}

/// 把字节写入 ramfs 文件 (测试用最小 VFS 写入链).
fn write_ramfs_file(name: &str, bytes: &[u8]) {
    let created = RAMFS_DATA.lock().create_file("/", name, 0);
    assert!(created.is_some(), "create_file /{name} 失败");
    let path = format!("/{name}");
    let fd = fs::vfs_open_safe(&path, 1, 0);
    assert!(fd >= 0, "open {path} 应成功");
    let written = fs::vfs_write_safe(fd.cast_unsigned(), bytes);
    assert_eq!(
        usize::try_from(written).expect("写入字节数"),
        bytes.len(),
        "写入 {path} 应完整"
    );
    assert_eq!(fs::vfs_close_safe(fd.cast_unsigned()), 0);
}

/// 函数签名简写.
fn sig(params: &[ValueType], results: &[ValueType]) -> FuncType {
    FuncType {
        params: params.to_vec(),
        results: results.to_vec(),
    }
}

/// 构造 import 若干 WASI 函数、导出 `f() -> i32` 的最小模块.
///
/// `f` 的函数体由 `f_body` 给定 (以 `end` 收尾), 其返回值取宿主压回的
/// errno; `payload` 非空时作为数据段初始化线性内存偏移 0 起的字节.
fn module_with_imports(
    imports: &[(&str, FuncType)],
    f_body: Vec<u8>,
    payload: Vec<u8>,
) -> WasmModule {
    let n = u32::try_from(imports.len()).expect("import 数量");
    let mut types: Vec<FuncType> = imports.iter().map(|(_, s)| s.clone()).collect();
    types.push(FuncType {
        params: Vec::new(),
        results: vec![I32],
    });
    let import_descs = imports
        .iter()
        .enumerate()
        .map(|(i, (name, _))| ImportDesc {
            module: b"wasi_snapshot_preview1".to_vec(),
            name: name.as_bytes().to_vec(),
            desc: ImportKind::Function(u32::try_from(i).expect("import 下标")),
        })
        .collect();
    let data = if payload.is_empty() {
        Vec::new()
    } else {
        vec![DataSegment {
            memory_index: 0,
            offset: vec![0x41, 0x00, 0x0B], // i32.const 0; end
            data: payload,
        }]
    };
    WasmModule {
        types,
        imports: import_descs,
        functions: vec![n],
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
            index: n,
        }],
        start_section: None,
        elements: Vec::new(),
        code: vec![FunctionBody {
            locals: Vec::new(),
            code: f_body,
        }],
        data,
    }
}

/// 建实例 → 装配上下文 → 跑 `f`, 返回 (调用结果, 实例) 供事后检查内存/上下文.
fn run_instance(
    imports: &[(&str, FuncType)],
    f_body: Vec<u8>,
    payload: Vec<u8>,
    setup: impl FnOnce(&mut WasiContext),
) -> (Result<Option<Value>, WasmError>, WasmInstance) {
    let mut ctx = WasiContext::new();
    setup(&mut ctx);
    let mut instance = WasmInstance::new(
        module_with_imports(imports, f_body, payload),
        InterpreterConfig::default(),
        ctx,
    );
    let ret = instance.call("f", &[]);
    (ret, instance)
}

/// 读实例线性内存的 u32.
fn mem_u32(inst: &WasmInstance, addr: u32) -> u32 {
    inst.interpreter()
        .memory
        .as_ref()
        .expect("线性内存")
        .read_u32(addr)
        .expect("读 u32")
}

/// 读实例线性内存的字节切片.
fn mem_bytes(inst: &WasmInstance, addr: u32, len: u32) -> Vec<u8> {
    inst.interpreter()
        .memory
        .as_ref()
        .expect("线性内存")
        .get_slice(addr, len)
        .expect("读切片")
        .to_vec()
}

/// `args_sizes_get` + `args_get`: 计数/缓冲大小与 argv 指针数组 + 字符串落位.
#[test]
fn args_sizes_and_get_roundtrip() {
    // 内存布局: count@0, buf_size@4, argv 数组@8 (2 项), 字符串 buf@16.
    let imports = [
        ("args_sizes_get", sig(&[I32, I32], &[I32])),
        ("args_get", sig(&[I32, I32], &[I32])),
    ];
    let f_body = vec![
        0x41, 0x00, 0x41, 0x04, 0x10, 0x00, 0x1A, // args_sizes_get(0, 4); drop
        0x41, 0x08, 0x41, 0x10, 0x10, 0x01, // args_get(8, 16)
        0x0B,
    ];
    let (ret, inst) = run_instance(&imports, f_body, vec![], |ctx| {
        ctx.args.push(String::from("prog"));
        ctx.args.push(String::from("--flag"));
    });
    assert_eq!(ret, Ok(Some(Value::I32(0))));
    assert_eq!(mem_u32(&inst, 0), 2, "argc 应为 2");
    assert_eq!(mem_u32(&inst, 4), 12, "argv buf 总长 = 5 + 7");
    assert_eq!(mem_u32(&inst, 8), 16, "argv[0] 指向 buf 起点");
    assert_eq!(mem_u32(&inst, 12), 21, "argv[1] 偏移 = 16 + 5");
    assert_eq!(mem_bytes(&inst, 16, 4), b"prog".to_vec());
    assert_eq!(mem_bytes(&inst, 21, 6), b"--flag".to_vec());
}

/// `environ_get`: 指针数组与 "key=value\0" 字符串落位.
#[test]
fn environ_get_roundtrip() {
    let imports = [("environ_get", sig(&[I32, I32], &[I32]))];
    // 内存布局: environ 数组@0 (1 项), 字符串 buf@4.
    let f_body = vec![0x41, 0x00, 0x41, 0x04, 0x10, 0x00, 0x0B];
    let (ret, inst) = run_instance(&imports, f_body, vec![], |ctx| {
        ctx.env.push((String::from("KEY"), String::from("val")));
    });
    assert_eq!(ret, Ok(Some(Value::I32(0))));
    assert_eq!(mem_u32(&inst, 0), 4, "environ[0] 指向 buf@4");
    assert_eq!(mem_bytes(&inst, 4, 7), b"KEY=val".to_vec());
}

/// `args_get`/`environ_get` 必须**显式**写出 NUL 终止符, 不依赖线性内存零初始化.
///
/// 以 `0xFF` 预填整段线性内存: 若宿主只在写字符串字节后前进 `len+1` 而漏写 `0`,
/// 预留槽会保留 `0xFF`, 该断言即失败 —— 从而区分「显式写入」与「零初始化副作用」.
#[test]
fn args_environ_get_explicitly_write_nul() {
    let prefill = vec![0xFFu8; 32];

    // environ: 单条 "KEY=val" (7 字节), NUL 落在 buf@4 + 7 = 11.
    let imports = [("environ_get", sig(&[I32, I32], &[I32]))];
    let f_body = vec![0x41, 0x00, 0x41, 0x04, 0x10, 0x00, 0x0B];
    let (ret, inst) = run_instance(&imports, f_body, prefill.clone(), |ctx| {
        ctx.env.push((String::from("KEY"), String::from("val")));
    });
    assert_eq!(ret, Ok(Some(Value::I32(0))));
    assert_eq!(mem_u32(&inst, 0), 4, "environ[0] 指向 buf@4");
    assert_eq!(mem_bytes(&inst, 4, 7), b"KEY=val".to_vec());
    assert_eq!(
        mem_bytes(&inst, 11, 1),
        vec![0u8],
        "NUL 终止符应被显式写入 (预填 0xFF 证明非零初始化副作用)"
    );

    // args: 单条 "prog" (4 字节), NUL 落在 buf@4 + 4 = 8.
    let imports = [("args_get", sig(&[I32, I32], &[I32]))];
    let f_body = vec![0x41, 0x00, 0x41, 0x04, 0x10, 0x00, 0x0B];
    let (ret, inst) = run_instance(&imports, f_body, prefill, |ctx| {
        ctx.args.push(String::from("prog"));
    });
    assert_eq!(ret, Ok(Some(Value::I32(0))));
    assert_eq!(mem_u32(&inst, 0), 4, "argv[0] 指向 buf@4");
    assert_eq!(mem_bytes(&inst, 4, 4), b"prog".to_vec());
    assert_eq!(
        mem_bytes(&inst, 8, 1),
        vec![0u8],
        "NUL 终止符应被显式写入 (预填 0xFF 证明非零初始化副作用)"
    );
}

/// `clock_time_get`: 合法时钟 success 并写结果; 非法 `clock_id` 返 Inval.
#[test]
fn clock_time_get_valid_and_invalid() {
    let imports = [("clock_time_get", sig(&[I32, I64, I32], &[I32]))];
    // 合法: clock_id=1 (MONOTONIC), precision=0, result_ptr=0.
    let valid_body = vec![0x41, 0x01, 0x42, 0x00, 0x41, 0x00, 0x10, 0x00, 0x0B];
    let (ret, inst) = run_instance(&imports, valid_body, vec![], |_| {});
    assert_eq!(ret, Ok(Some(Value::I32(0))), "合法时钟应 success");
    // host 时基未校准 ⇒ get_time_ns() 折算 0, 结果槽应被写为 0.
    assert_eq!(mem_u32(&inst, 0), 0, "host 未校准, 纳秒读数折算 0");

    // 非法: clock_id=2 (非 0/1) ⇒ Inval(28).
    let invalid_body = vec![0x41, 0x02, 0x42, 0x00, 0x41, 0x00, 0x10, 0x00, 0x0B];
    let (ret2, _) = run_instance(&imports, invalid_body, vec![], |_| {});
    assert_eq!(ret2, Ok(Some(Value::I32(28))), "非法 clock_id 应 Inval");
}

/// `random_get`: 填充缓冲区返 success (不断言随机内容, 避免 flaky).
#[test]
fn random_get_fills_buffer() {
    let imports = [("random_get", sig(&[I32, I32], &[I32]))];
    let f_body = vec![0x41, 0x00, 0x41, 0x10, 0x10, 0x00, 0x0B]; // random_get(0, 16)
    let (ret, inst) = run_instance(&imports, f_body, vec![], |_| {});
    assert_eq!(ret, Ok(Some(Value::I32(0))), "random_get 应 success");
    assert_eq!(mem_bytes(&inst, 0, 16).len(), 16, "缓冲区应可读回 16 字节");
}

/// `proc_exit`: 设置退出码并以 Terminated 终止实例.
#[test]
fn proc_exit_sets_code_and_terminates() {
    let imports = [("proc_exit", sig(&[I32], &[]))];
    let f_body = vec![0x41, 0x2A, 0x10, 0x00, 0x0B]; // proc_exit(42)
    let (ret, inst) = run_instance(&imports, f_body, vec![], |_| {});
    assert_eq!(ret, Err(WasmError::Terminated), "proc_exit 应终止实例");
    assert_eq!(inst.interpreter().exit_code, 42, "退出码应落 42");
}

/// `sched_yield`: 单线程解释器下立即 success.
#[test]
fn sched_yield_returns_success() {
    let imports = [("sched_yield", sig(&[], &[I32]))];
    let f_body = vec![0x10, 0x00, 0x0B]; // sched_yield()
    let (ret, _) = run_instance(&imports, f_body, vec![], |_| {});
    assert_eq!(ret, Ok(Some(Value::I32(0))));
}

/// `fd_read`: 经真实 VFS 把文件字节读入 iovec 缓冲区.
#[test]
fn fd_read_from_vfs() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    init_fs_once();
    let _proc = TestProc::new("w9-read");
    write_ramfs_file("w9_read_t", b"hello");
    let file_fd = fs::vfs_open_safe("/w9_read_t", 0, 0);
    assert!(file_fd >= 0, "open /w9_read_t 应成功");

    // 内存布局: iovec@0 {buf:16, len:5}, nread@8, buf@16.
    let mut payload = vec![16, 0, 0, 0, 5, 0, 0, 0];
    payload.extend_from_slice(&[0u8; 8]); // nread@8
    payload.extend_from_slice(&[0u8; 5]); // buf@16
    let imports = [("fd_read", sig(&[I32, I32, I32, I32], &[I32]))];
    // fd=0 (stdin 映射到文件), iovs_ptr=0, iovs_len=1, nread_ptr=8.
    let f_body = vec![
        0x41, 0x00, 0x41, 0x00, 0x41, 0x01, 0x41, 0x08, 0x10, 0x00, 0x0B,
    ];
    let (ret, inst) = run_instance(&imports, f_body, payload, |ctx| {
        ctx.fd_table
            .get_mut(WASI_STDIN)
            .expect("stdin 预置")
            .inner_fd = file_fd;
    });
    assert_eq!(ret, Ok(Some(Value::I32(0))));
    assert_eq!(mem_u32(&inst, 8), 5, "nread 应为 5");
    assert_eq!(mem_bytes(&inst, 16, 5), b"hello".to_vec());
}

/// `fd_seek` + `fd_tell`: seek 落新偏移, tell 读回当前位置.
#[test]
fn fd_seek_and_tell() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    init_fs_once();
    let _proc = TestProc::new("w9-seek");
    write_ramfs_file("w9_seek_t", b"hello");
    let file_fd = fs::vfs_open_safe("/w9_seek_t", 0, 0);
    assert!(file_fd >= 0, "open /w9_seek_t 应成功");

    let imports = [
        ("fd_seek", sig(&[I32, I64, I32, I32], &[I32])),
        ("fd_tell", sig(&[I32, I32], &[I32])),
    ];
    // 内存布局: new_offset@0, tell_offset@4.
    let f_body = vec![
        0x41, 0x00, 0x42, 0x02, 0x41, 0x00, 0x41, 0x00, 0x10, 0x00,
        0x1A, // seek(0,2,SET,0); drop
        0x41, 0x00, 0x41, 0x04, 0x10, 0x01, // tell(0, 4)
        0x0B,
    ];
    let (ret, inst) = run_instance(&imports, f_body, vec![], |ctx| {
        ctx.fd_table
            .get_mut(WASI_STDIN)
            .expect("stdin 预置")
            .inner_fd = file_fd;
    });
    assert_eq!(ret, Ok(Some(Value::I32(0))));
    assert_eq!(mem_u32(&inst, 0), 2, "seek 到 2 应返回新偏移 2");
    assert_eq!(mem_u32(&inst, 4), 2, "tell 应读回当前位置 2");
}

/// `fd_close`: 动态 fd 关闭成功并释放; stdio fd 不可关闭返 Badf.
#[test]
fn fd_close_dynamic_and_stdio() {
    let imports = [("fd_close", sig(&[I32], &[I32]))];
    // 动态 fd 4 (inner_fd=-1, 无 VFS 后端, 不触进程 fd 表).
    let close4 = vec![0x41, 0x04, 0x10, 0x00, 0x0B];
    let (ret, inst) = run_instance(&imports, close4, vec![], |ctx| {
        let fd = ctx
            .fd_table
            .alloc(WasiFdEntry {
                file_type: WasiFileType::RegularFile,
                rights: WasiRights::FILE,
                inner_fd: -1,
                socket: None,
                path: None,
            })
            .expect("alloc 动态 fd");
        assert_eq!(fd, 4, "首个动态 fd 应为 4");
    });
    assert_eq!(ret, Ok(Some(Value::I32(0))), "关闭动态 fd 应 success");
    assert!(
        inst.with_context(|c| c.fd_table.get(4).is_err()),
        "fd 4 应已释放"
    );

    // stdio fd 1 不可关闭 ⇒ Badf(8).
    let close1 = vec![0x41, 0x01, 0x10, 0x00, 0x0B];
    let (ret2, _) = run_instance(&imports, close1, vec![], |_| {});
    assert_eq!(ret2, Ok(Some(Value::I32(8))), "关闭 stdio 应 Badf");
}

/// `fd_prestat_get` + `fd_prestat_dir_name`: preopen 路径长度与目录名; stdio 返 Badf.
#[test]
fn fd_prestat_get_and_dir_name() {
    let imports = [
        ("fd_prestat_get", sig(&[I32, I32], &[I32])),
        ("fd_prestat_dir_name", sig(&[I32, I32, I32], &[I32])),
    ];
    // 内存布局: path_len@0, name_buf@4.
    let f_body = vec![
        0x41, 0x03, 0x41, 0x00, 0x10, 0x00, 0x1A, // prestat_get(3, 0); drop
        0x41, 0x03, 0x41, 0x04, 0x41, 0x08, 0x10, 0x01, // prestat_dir_name(3, 4, 8)
        0x0B,
    ];
    let (ret, inst) = run_instance(&imports, f_body, vec![], |_| {});
    assert_eq!(ret, Ok(Some(Value::I32(0))));
    assert_eq!(mem_u32(&inst, 0), 1, "preopen 路径 \"/\" 长度 1");
    assert_eq!(mem_bytes(&inst, 4, 1), b"/".to_vec());

    // stdio fd 1 无 preopen 路径 ⇒ fd_prestat_get Badf(8).
    let bad_body = vec![0x41, 0x01, 0x41, 0x00, 0x10, 0x00, 0x0B];
    let (ret2, _) = run_instance(
        &[("fd_prestat_get", sig(&[I32, I32], &[I32]))],
        bad_body,
        vec![],
        |_| {},
    );
    assert_eq!(ret2, Ok(Some(Value::I32(8))), "stdio 无 preopen 应 Badf");
}

/// `path_open`: 经 preopen 解析打开已存在文件成功; 缺失文件返 Noent.
#[test]
fn path_open_existing_and_missing() {
    let _guard = GLOBAL.lock().expect("全局测试锁");
    init_fs_once();
    let _proc = TestProc::new("w9-open");
    write_ramfs_file("w9_open_t", b"x");

    let imports = [(
        "path_open",
        sig(&[I32, I32, I32, I32, I32, I64, I64, I32, I32], &[I32]),
    )];
    // 内存布局: 路径串@0 "w9_open_t" (11 字节), fd_ptr@16.
    let mut payload = b"w9_open_t".to_vec();
    payload.resize(16, 0);
    payload.extend_from_slice(&[0u8; 4]);
    // dirfd=3 (preopen "/"), o_flags=0 (只读已存在文件, 不触创建语义).
    let f_body = vec![
        0x41, 0x03, 0x41, 0x00, 0x41, 0x00, 0x41,
        0x0B, // 压栈 dirfd=3、dirflags=0、path_ptr=0、path_len=11
        0x41, 0x00, 0x42, 0x00, 0x42, 0x00, 0x41,
        0x00, // 压栈 o_flags=0、rights_base=0、rights_inh=0、fd_flags=0
        0x41, 0x10, 0x10, 0x00, 0x0B, // 压栈 fd_ptr=16; call 导入 0; end
    ];
    let (ret, inst) = run_instance(&imports, f_body, payload, |_| {});
    assert_eq!(ret, Ok(Some(Value::I32(0))), "打开已存在文件应 success");
    let new_fd = mem_u32(&inst, 16);
    assert!(new_fd >= 4, "新 fd 应 >= 4, 实测 {new_fd}");
    assert!(
        inst.with_context(|c| c.fd_table.get(new_fd).is_ok()),
        "新 fd 应已登记"
    );

    // 缺失文件 "no_such_file" (12 字节) ⇒ Noent(44).
    let mut bad_payload = b"no_such_file".to_vec();
    bad_payload.resize(16, 0);
    bad_payload.extend_from_slice(&[0u8; 4]);
    let bad_body = vec![
        0x41, 0x03, 0x41, 0x00, 0x41, 0x00, 0x41, 0x0C, // path_len=12
        0x41, 0x00, 0x42, 0x00, 0x42, 0x00, 0x41, 0x00, 0x41, 0x10, 0x10, 0x00, 0x0B,
    ];
    let (ret2, _) = run_instance(&imports, bad_body, bad_payload, |_| {});
    assert_eq!(ret2, Ok(Some(Value::I32(44))), "缺失文件应 Noent");
}
