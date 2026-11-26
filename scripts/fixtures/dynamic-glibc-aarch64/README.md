# glibc aarch64 动态装载 fixture

D7 A1 载荷：aarch64 真实 glibc 动态 `true`、带 stdout 的 `hello`、启动
syscall `g2`、时间路径 `time`、pthread/TLS `glibc_pthread`、clone3 高级字段
`glibc_clone3` 与 dlopen `glibc_dlopen` + `libdlprobe.so`，配合上游
`ld-linux-aarch64.so.1` + `libc.so.6`，把 D7 G0–G4/C1/L1 的验收面从 x86_64
扩到 aarch64。

## 来源与构建

- `glibc_true`：Debian 13 (trixie) `coreutils 9.7-3` arm64 的 `/usr/bin/true`
- 六个探针：直接复用 `../dynamic-glibc-x86_64/` 下的同名 `.c`（未改源码），
  用 Fedora 45 交叉 `gcc-aarch64-linux-gnu 16.2.1` + Debian 13 arm64 sysroot
  (`libc6/libc6-dev/linux-libc-dev 2.41-12+deb13u5`、`libatomic1/libgcc-s1
  14.2.0-19`) 编译：

  ```bash
  CC="aarch64-linux-gnu-gcc --sysroot=<sysroot> \
      -isystem <sysroot>/usr/include/aarch64-linux-gnu \
      -L<sysroot>/usr/lib/aarch64-linux-gnu -fno-link-libatomic"
  $CC -O2 -s -fPIE -pie -Wl,--dynamic-linker=/lib/ld-linux-aarch64.so.1 \
      glibc_hello.c -o glibc_hello
  # glibc_pthread 追加 -pthread；其余探针同参数。
  ```

  `-fno-link-libatomic` 只用于绕开 Fedora 交叉 GCC specs 对
  `libatomic_asneeded` 的命名假设，探针没有原子运行库依赖。
- `libdlprobe.so`：`libdlprobe.c` 用
  `-O2 -s -fPIC -fno-stack-protector -shared -nostdlib -Wl,-soname,libdlprobe.so`
  编译，`NEEDED` 为空，导出 `probe_counter`/`probe_add`/`probe_get`
- 解释器：Debian 13 `/lib/ld-linux-aarch64.so.1`（glibc 2.41）
- libc：Debian 13 `/lib/aarch64-linux-gnu/libc.so.6`（glibc 2.41）
- 探针均 `strip --strip-all`；`glibc_true` 为 Debian 原始 stripped 产物。

## 读数

| 文件 | 大小 | sha256 |
|---|---:|---|
| `glibc_true` | 68,048 B | `c5884f055e6a99450a4712bc03cc6551f8e28c80c81a3787174e78737860c530` |
| `glibc_hello` | 68,040 B | `6f6ac22781594d4eee834fc85202ec1f39a02dbcf5b2451d65dd40904ae183aa` |
| `glibc_g2` | 68,080 B | `57dcc30fdb82f2115e72936fa59bad550a520c64c91c41c7d255e7f3871fbb27` |
| `glibc_time` | 68,112 B | `ef84e4b1d40654eb52b5bc39e32534f8e79e175a07cae2299edc4fc634539182` |
| `glibc_pthread` | 68,208 B | `3ec65429d31bf7a4255b5fca2813bc9fc5ce993327365d475923daf782d19ce5` |
| `glibc_clone3` | 68,136 B | `39bf30a54ca108c5a9bd0cb044ac9f8518ac524572fbb724b397c9b65858d1a5` |
| `glibc_dlopen` | 68,096 B | `047a745a79ba4fa5b3b6ecb6fa5ad60548c380d1719ec87a58efb094f947c13d` |
| `libdlprobe.so` | 66,888 B | `2cc08c905349caf322b3b8521aa3e16ed69d1884af6d658c24b15d9967aff37c` |
| `ld-linux-aarch64.so.1` | 201,344 B | `8839358cf3489b3399256c8ad6daff5147e61023cc3dd584966dfb3eb1928035` |
| `libc.so.6` | 1,716,616 B | `8ca3645e66bacafe8f1f0d3a1919ec2b1f3931ec99015beec432a88ec5a4704e` |

关键 ELF 事实：

```text
glibc_true: PT_INTERP=/lib/ld-linux-aarch64.so.1, NEEDED=libc.so.6 + ld-linux-aarch64.so.1
glibc_hello: ET_DYN PIE, PT_INTERP=/lib/ld-linux-aarch64.so.1, NEEDED=libc.so.6
glibc_g2: ET_DYN PIE, PT_INTERP=/lib/ld-linux-aarch64.so.1, NEEDED=libc.so.6 + ld-linux-aarch64.so.1
glibc_time: ET_DYN PIE, PT_INTERP=/lib/ld-linux-aarch64.so.1, NEEDED=libc.so.6
glibc_pthread: ET_DYN PIE, PT_INTERP=/lib/ld-linux-aarch64.so.1, NEEDED=libc.so.6 + ld-linux-aarch64.so.1
glibc_clone3: ET_DYN PIE, PT_INTERP=/lib/ld-linux-aarch64.so.1, NEEDED=libc.so.6
glibc_dlopen: ET_DYN PIE, PT_INTERP=/lib/ld-linux-aarch64.so.1, NEEDED=libc.so.6
libdlprobe.so: ET_DYN, SONAME=libdlprobe.so, NEEDED=<empty>
ld-linux-aarch64.so.1: ET_DYN, BIND_NOW
libc.so.6: ET_DYN, SONAME=libc.so.6
```

## initramfs 布局

```text
/bin/glibc_true
/bin/glibc_hello
/bin/glibc_g2
/bin/glibc_time
/bin/glibc_pthread
/bin/glibc_clone3
/bin/glibc_dlopen
/lib/ld-linux-aarch64.so.1
/lib/libc.so.6
/lib64/libdlprobe.so
```

aarch64 glibc 的默认库搜索路径是 `/lib`，故 libc 与 loader 同放 `/lib`；
`glibc_dlopen` 的绝对路径 `/lib64/libdlprobe.so` 与 x86_64 保持一致。

## 复现

宿主侧用 qemu-user 做冒烟（注意 `glibc_g2` 的 `rseq` 腿依赖 Edgine 内核语义，
qemu-user 不能作为该腿判据；`glibc_clone3` 的 `set_tid`/`cgroup` 期望 `ENOSYS`
也不应在普通 Linux 上跑）：

```bash
qemu-aarch64-static -L <sysroot> ./glibc_true
qemu-aarch64-static -L <sysroot> ./glibc_hello
qemu-aarch64-static -L <sysroot> ./glibc_time
qemu-aarch64-static -L <sysroot> ./glibc_pthread
```

Edgine 内核腿由 init 的 `glibc_probe()` 分别执行上表七个 `/bin/glibc_*`，
按 `wstatus == 0` 与各探针 `PASS` 行判定；QEMU aarch64 门禁要求
`[GLIBC] PASS`/`HELLO PASS`/`G2 PASS`/`TIME PASS`/`PTHREAD PASS`/`CLONE3 PASS`/
`DLOPEN PASS` 全部出现且无 `FAIL`。
