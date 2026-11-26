# glibc x86_64 动态装载 fixture

D7 G0/G1/G2/G3/G4/C1/L1 载荷：真实 glibc 动态 `true`、带 stdout 的 `hello`、启动
syscall 探针 `g2`、时间路径探针 `time`、pthread/TLS 探针 `glibc_pthread`、clone3
高级字段探针 `glibc_clone3` 与 dlopen 多共享对象探针 `glibc_dlopen` + 独立
`libdlprobe.so`，配合上游 `ld-linux` + `libc.so.6`，用于验证 glibc 解释器在
Edgine 上的装载、重定位、`exit(0)`、`printf`、启动 syscall、
`AT_SYSINFO_EHDR=0` 下的时间 fallback、`pthread_create`/TLS/`pthread_join`、
`clone3` 的 `exit_signal`/`CLONE_PIDFD`/短结构/`set_tid`/`cgroup` 边界，以及
第二个共享对象的 `dlopen`/`dlsym`/`dlclose`/重新装载。

## 来源

- `glibc_true`：Fedora 45 `/bin/true`（coreutils，ELF64 x86_64 PIE）
- `glibc_hello`：本目录 `glibc_hello.c` 用宿主 GCC 16.2.1 / glibc 2.44
  `-O2 -s -fPIE -pie` 编译，`NEEDED=libc.so.6`
- `glibc_g2`：本目录 `glibc_g2.c` 同编译器参数编译，直接调
  `SYS_set_tid_address`/`SYS_prlimit64`/`SYS_getrlimit`/`SYS_rseq`，逐项对比
  `getrlimit` 与 `prlimit64` 的 16 个资源读数，并打印原始返回码
- `glibc_time`：本目录 `glibc_time.c` 同编译器参数编译，调用
  `clock_gettime`/`gettimeofday`/`time`/`clock_getres`，验证无 VDSO 的
  syscall fallback
- `glibc_pthread`：本目录 `glibc_pthread.c` 同编译器参数加 `-pthread` 编译，
  调用 `pthread_create`/TLS/`pthread_join`，验证 glibc 线程创建路径（含
  `clone3` 栈参数换算）与 TLS 隔离
- `glibc_clone3`：本目录 `glibc_clone3.c` 同编译器参数编译，覆盖
  `exit_signal=SIGUSR1` + `waitpid(__WALL)`、`CLONE_PIDFD` 写回/`CLOEXEC`/退出后
  零超时 `poll`（Edgine 的通用 poll 当前为单次快照，不消费 timeout）、
  `size=64` 短结构、`set_tid`/`cgroup` 期望 `ENOSYS`、`exit_signal=65` 期望 `EINVAL`
- `glibc_dlopen`：本目录 `glibc_dlopen.c` 同编译器参数编译，`RTLD_NOW|RTLD_LOCAL`
  装载 `/lib64/libdlprobe.so`，解析 `probe_add`/`probe_get`，验证首次调用返回 12、
  缺失符号返回 `NULL`、`dlclose` 后重新 `dlopen` 时 `probe_counter` 重新初始化为 5
- `libdlprobe.so`：本目录 `libdlprobe.c` 用
  `gcc -O2 -s -fPIC -fno-stack-protector -shared -nostdlib -Wl,-soname,libdlprobe.so`
  编译，`NEEDED` 为空，导出 `probe_counter`/`probe_add`/`probe_get`
- 解释器：Fedora 45 `/lib64/ld-linux-x86-64.so.2`（glibc 2.44）
- libc：Fedora 45 `/lib64/libc.so.6`（glibc 2.44）
- 所有二进制均 `strip --strip-all`，与仓内 musl fixture 同口径：只保留运行所需
  `PT_LOAD`/动态段/重定位，避免 Fedora 未剥离调试信息把解释器顶过
  `ELF_MAX_SIZE = 1 MiB`。

## 读数

| 文件 | 大小 | sha256 |
|---|---:|---|
| `glibc_true` | 36,424 B | `684bc05bc973d1bfec940279475ece99740a78765696fcd3ea2dbd540df16fb5` |
| `glibc_hello` | 11,176 B | `9aafb5ee74847ad39d6704447e6197f0545b8894964bd53f9815b1c763c58709` |
| `glibc_g2` | 11,200 B | `8562e680e9c5b5c241f801dc0637f570eef46b9395f14ff3a258bad3df56ae12` |
| `glibc_time` | 11,232 B | `1676f0f12b4090a06c687d7d6e6fdc57d57d7b215dfda240a95b731a78d77394` |
| `glibc_pthread` | 11,344 B | `466bd228a8f0c7648a1e73b736c35c96697563168ae4aa08de39b3d271868c08` |
| `glibc_clone3` | 11,272 B | `4fb861c111430dc7eb181598c0ca5c1ccc97276d550db5e9c58a3329bb48fc88` |
| `glibc_dlopen` | 11,232 B | `2626224240d46afdf950a38eec5a18c0c4ed0e78743ab972da0fafeb9a636ea9` |
| `libdlprobe.so` | 9,424 B | `22c71de1d00e41469b418da82770b940598645ff3f2464beba67e1f1f97f4f73` |
| `ld-linux-x86-64.so.2` | 260,320 B | `6c3d7bdd7c5b6155fb6f7660bd49955f993580aac951e16bbe6eb004f87429e4` |
| `libc.so.6` | 2,187,808 B | `958416ed2a6831d8c739c75da15ebe0ed570299a768967dcff3fc736ca62aadb` |

关键 ELF 事实：

```text
glibc_true: PT_INTERP=/lib64/ld-linux-x86-64.so.2, NEEDED=libc.so.6
glibc_hello: ET_DYN PIE, PT_INTERP=/lib64/ld-linux-x86-64.so.2, NEEDED=libc.so.6
glibc_g2: ET_DYN PIE, PT_INTERP=/lib64/ld-linux-x86-64.so.2, NEEDED=libc.so.6
glibc_time: ET_DYN PIE, PT_INTERP=/lib64/ld-linux-x86-64.so.2, NEEDED=libc.so.6
glibc_pthread: ET_DYN PIE, PT_INTERP=/lib64/ld-linux-x86-64.so.2, NEEDED=libc.so.6
glibc_clone3: ET_DYN PIE, PT_INTERP=/lib64/ld-linux-x86-64.so.2, NEEDED=libc.so.6
glibc_dlopen: ET_DYN PIE, PT_INTERP=/lib64/ld-linux-x86-64.so.2, NEEDED=libc.so.6
libdlprobe.so: ET_DYN, SONAME=libdlprobe.so, NEEDED=<empty>
ld-linux-x86-64.so.2: ET_DYN, BIND_NOW, RELR
libc.so.6: ET_DYN, SONAME=libc.so.6
```

`libc.so.6` 只由上游 loader 经 `mmap` 读取，内核不整文件读它；内核只读主程序
（36 KB/11 KB/11 KB/11 KB）与解释器（260 KB），均在 `ELF_MAX_SIZE` 内。

## initramfs 布局

```text
/bin/glibc_true
/bin/glibc_hello
/bin/glibc_g2
/bin/glibc_time
/bin/glibc_pthread
/bin/glibc_clone3
/bin/glibc_dlopen
/lib64/ld-linux-x86-64.so.2
/lib64/libc.so.6
/lib64/libdlprobe.so
```

`ld-linux` 的默认搜索路径包含 `/lib64`，因此不需要 `ld.so.cache`；cpio 不支持
符号链接，故 libc 按独立文件放入。

## 复现

宿主侧可直接用 loader 验证 fixture：

```bash
ld-linux-x86-64.so.2 --library-path . ./glibc_true
echo "true_rc=$?"
ld-linux-x86-64.so.2 --library-path . ./glibc_hello
echo "hello_rc=$?"
ld-linux-x86-64.so.2 --library-path . ./glibc_g2
echo "g2_rc=$?"
ld-linux-x86-64.so.2 --library-path . ./glibc_time
echo "time_rc=$?"
ld-linux-x86-64.so.2 --library-path . ./glibc_pthread
echo "pthread_rc=$?"
```

`glibc_dlopen` 的目标路径固定为 `/lib64/libdlprobe.so`，只能在 initramfs 的
`/lib64` 布局下直接跑；宿主侧可把 `libdlprobe.so` 放到测试根目录的 `/lib64`
（或用容器 bind mount）后再执行。

`glibc_clone3` 是内核语义探针：`set_tid`/`cgroup` 期望 `ENOSYS`，不应在普通
Linux 宿主上直接跑（宿主内核对这两项有实现）；其验收腿只在 Edgine QEMU 串口。

`0` 表示主程序、loader、libc 三者 ABI 自洽；Edgine 内核腿由 init 的
`glibc_probe()` 分别执行 `/bin/glibc_true`、`/bin/glibc_hello`、`/bin/glibc_g2`、
`/bin/glibc_time`、`/bin/glibc_pthread`、`/bin/glibc_clone3` 与
`/bin/glibc_dlopen`。前两腿按 `wstatus == 0` 判定并检查
`hello dynamic glibc`；G2 腿要求 `set_tid_address` 返回当前 TID、`prlimit64` 返回成功，
`rseq` 预期 `ENOSYS`（glibc 可 fallback）；G3 腿要求时间函数在
`AT_SYSINFO_EHDR=0` 下全部返回成功；G4 腿要求 `pthread_create`/TLS/`pthread_join`
成功并打印 `[PTHREAD] PASS`；L1 腿要求 `[DLOPEN] PASS` 且 `dlopen`/`dlsym`/
`dlclose`/重装载全部成功，由 QEMU 门禁逐行断言。
