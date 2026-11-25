# T7-c 外部 Linux 程序验收 — 静态与动态 musl hello（双架构）

日期：2026-11-24（静态）；2026-11-25 补记动态
关联：`docs/plan/syscall-followup.md` T7-c

> 本文前半保留 2026-11-24 静态腿原始读数；动态 musl 与失败路径结果见
> “动态 musl 补记”一节，边界结论以补记后的内容为准。

## 目的

用**非本仓编写、非本仓工具链**的 Linux 用户态程序验证 Edgine 的 Linux syscall ABI，
而不是用 Edgine 自己的 `exectest` 自证。判据是外部程序真正执行并输出，且 `execve`
两条路径（cwd 相对名 / chroot 视图绝对名）的父进程 `wstatus == 0`。

## 工具链与载荷

- 编译器：Zig 0.13.0 `zig cc`，目标 `x86_64-linux-musl` / `aarch64-linux-musl`
- 链接：`-static -O2 -s`，musl libc 静态链接
- 源码：[`scripts/fixtures/t7_external_hello.c`](../../scripts/fixtures/t7_external_hello.c)
- 输出：`hello`（x86_64，5,160 B，sha256 `524ba56c…f08c`）、
  `hello_arm`（aarch64，6,208 B，sha256 `c8a869f4…4cc3`）

两条载荷均为 `ET_EXEC`，入口分别为 `0x10012a0`（x86_64）与 `0x1010290`（aarch64），
不依赖动态链接器、vDSO 或 glibc。

## 注入方式

不修改内核与用户库代码：把现有 initramfs 产物目录复制到临时目录，仅把
`bin/exectest` 的源文件替换为外部 hello，再用仓内 `make_initramfs_cpio.py` 重打
cpio。`init` 的既有 `exec_probe` 会以两条腿各执行一次 `/bin/exectest`，因此外部
hello 被真实 `execve` 两次（cwd 腿 + root 腿）。

- x86_64：GRUB `multiboot2 + module2` ISO，`-m 512 -smp 2`
- aarch64：`-M virt,gic-version=3 -cpu max -m 512 -smp 2 -kernel Image -initrd cpio`

## 实测读数

x86_64 串口日志：

```text
[ELF] verify_elf OK, entry=0x10012a0
[ELF] self.create OK
hello from external musl
[ELF] verify_elf OK, entry=0x10012a0
[ELF] self.create OK
hello from external musl
[EXEC] PASS: cwd+root argv+envp
```

aarch64 串口日志：

```text
[ELF] verify_elf OK, entry=0x1010290
[ELF] self.create OK
hello from external musl
[ELF] verify_elf OK, entry=0x1010290
[ELF] self.create OK
hello from external musl
[EXEC] PASS: cwd+root argv+envp
```

结论：静态 musl 外部程序在双架构上均通过 `execve` 装载、用户态执行、`write`
输出与正常退出；两条 `execve` 视图解析腿的 `wstatus` 均为 0。

## 动态 musl 补记（2026-11-25）

### 载荷与路径

- x86_64：`scripts/fixtures/dynamic-musl-x86_64/hello_dyn`，`PT_INTERP=/lib/ld-musl-x86_64.so.1`，
  `NEEDED=libc.musl-x86_64.so.1`。
- aarch64：Alpine 3.20.3 minirootfs 的真实 `busybox`，动态 PIE，
  `PT_INTERP=/lib/ld-musl-aarch64.so.1`，`NEEDED=libc.musl-aarch64.so.1`；
  init 以 `/bin/busybox` + applet `echo` 的 argv 执行 `/bin/dyntest`。
- 两架构 loader/libc 均由 `make_initramfs_cpio.py` 打入 initramfs；
  `elfld` 改写链与旧工具已退役（`d6505490`），`PT_INTERP` 保持原样。

### 实测读数

x86_64 与 aarch64 串口均出现：

```text
hello dynamic musl
[DYN] PASS: dynamic musl execve
[DYN] NEG PASS: missing interp -> ENOENT
[DYN] NEG PASS: bad interp rejected, child survived
[DYN] FMT PASS: relative interp -> ENOEXEC
[DYN] FMT PASS: malformed interp rejected, child survived
```

- 正向腿证明上游 `ld-musl` 完成解释器装载、libc 解析、RELRO 与用户态退出。
- `badinterp_*.dat`（合法 ELF 头 + `PT_INTERP` 指向不存在路径）证明内核在读取
  解释器阶段返回 `ENOENT`，子进程存活。
- `dyntest_badfmt` 由构建期把主载荷 `PT_INTERP` 首字节改为相对路径；内核在
  `interp_path` 解析阶段返回 `ENOEXEC`，不再静默退化为静态装载。无 NUL、
  越界与加法回绕由 `interp_path` 单测覆盖。
- `scripts/qemu_boot_test.sh` 对两架构均 fail-closed 要求上述 `PASS` 标记、
  禁止对应 `FAIL`；`ci/audit.sh full` 的双架构 QEMU 腿通过。

### musl 的 libc 缺失不是独立失败面

Alpine 的 `libc.musl-<arch>.so.1` 与 `ld-musl-<arch>.so.1` 是同一对象（镜像中用
符号链接表示；cpio 不支持符号链接，仓内保存同字节副本）。实测把 libc 从根中
移除、只保留 loader 后，动态程序仍能运行——loader 自身即 libc。因此“libc 缺失”
在 musl 下退化为“loader 缺失”，已由 `[DYN] NEG PASS` 的 `ENOENT` 腿覆盖；
真正的独立 `libc.so.6` 缺失场景留给 glibc D7。

## 边界与未覆盖

- 静态腿只覆盖静态 musl；动态 musl 已由补记覆盖，glibc、vDSO、TLS 变体、
  带复杂重定位的 PIE 仍未跑。
- 同批尝试的 `busybox_ECHO`（1.35.0 x86_64 musl，sha256 `b9d380e2…9ff5`）在
  x86_64 上 `[EXEC] FAIL: cwd=0 root=0`，未进入 `[EXEC] PASS`。该载荷不作为
  通过证据；它暴露的是 busybox 启动序列/段布局与当前装载器的兼容缺口，需单独
  归因，不能算作 T7-c 已全绿。
- 验收使用临时 cpio/ISO，未入库为 CI 腿；复现需 Zig 工具链与 QEMU。若要把
  静态腿变成门禁，需先把 fixture 与构建步骤固化进 `scripts/`，再接入
  `ci/audit.sh`（当前刻意不做，避免把外部工具链下载变成默认门槛）。
