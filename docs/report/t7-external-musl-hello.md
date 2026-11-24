# T7-c 外部 Linux 程序验收 — 静态 musl hello（双架构）

日期：2026-11-24
关联：`docs/plan/syscall-followup.md` T7-c

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

## 边界与未覆盖

- 本腿只覆盖**静态 musl**；动态 musl（需 `ld-musl` 与 `PT_INTERP`）、glibc、
  vDSO、TLS 变体、带复杂重定位的 PIE 仍未跑。
- 同批尝试的 `busybox_ECHO`（1.35.0 x86_64 musl，sha256 `b9d380e2…9ff5`）在
  x86_64 上 `[EXEC] FAIL: cwd=0 root=0`，未进入 `[EXEC] PASS`。该载荷不作为
  通过证据；它暴露的是 busybox 启动序列/段布局与当前装载器的兼容缺口，需单独
  归因，不能算作 T7-c 已全绿。
- 验收使用临时 cpio/ISO，未入库为 CI 腿；复现需 Zig 工具链与 QEMU。若要把
  静态腿变成门禁，需先把 fixture 与构建步骤固化进 `scripts/`，再接入
  `ci/audit.sh`（当前刻意不做，避免把外部工具链下载变成默认门槛）。
