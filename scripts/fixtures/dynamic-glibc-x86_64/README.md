# glibc x86_64 动态装载 fixture

D7 G0/G1 载荷：真实 glibc 动态 `true` 与带 stdout 的 `hello` + 上游 `ld-linux`
+ `libc.so.6`，用于让 glibc 解释器在 Edgine 上完成装载、重定位、`exit(0)`
与 `printf` 输出路径，暴露 glibc 启动面缺口。

## 来源

- `glibc_true`：Fedora 45 `/bin/true`（coreutils，ELF64 x86_64 PIE）
- `glibc_hello`：本目录 `glibc_hello.c` 用宿主 GCC 16.2.1 / glibc 2.44
  `-O2 -s -fPIE -pie` 编译，`NEEDED=libc.so.6`
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
| `ld-linux-x86-64.so.2` | 260,320 B | `6c3d7bdd7c5b6155fb6f7660bd49955f993580aac951e16bbe6eb004f87429e4` |
| `libc.so.6` | 2,187,808 B | `958416ed2a6831d8c739c75da15ebe0ed570299a768967dcff3fc736ca62aadb` |

关键 ELF 事实：

```text
glibc_true: PT_INTERP=/lib64/ld-linux-x86-64.so.2, NEEDED=libc.so.6
glibc_hello: ET_DYN PIE, PT_INTERP=/lib64/ld-linux-x86-64.so.2, NEEDED=libc.so.6
ld-linux-x86-64.so.2: ET_DYN, BIND_NOW, RELR
libc.so.6: ET_DYN, SONAME=libc.so.6
```

`libc.so.6` 只由上游 loader 经 `mmap` 读取，内核不整文件读它；内核只读主程序
（36 KB/11 KB）与解释器（260 KB），均在 `ELF_MAX_SIZE` 内。

## initramfs 布局

```text
/bin/glibc_true
/bin/glibc_hello
/lib64/ld-linux-x86-64.so.2
/lib64/libc.so.6
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
```

`0` 表示主程序、loader、libc 三者 ABI 自洽；Edgine 内核腿由 init 的
`glibc_probe()` 分别执行 `/bin/glibc_true` 与 `/bin/glibc_hello`，均以
`wstatus == 0` 判定，并额外检查串口出现 `hello dynamic glibc`。
