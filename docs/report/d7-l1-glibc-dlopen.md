# D7 L1：glibc dlopen 多共享对象收口

> 日期：2026-11-26
> 状态：x86_64 已收口；aarch64 无载体，SKIP
> 范围：上游 `ld-linux` 装载第二个 `.so`，覆盖 `dlopen`/`dlsym`/`dlclose` 与重新装载

## 1. 结论

L1 已在 x86_64 真实 glibc 上跑通。QEMU 串口读数：

```text
[DLOPEN] begin
[DLOPEN] first=12 value=12
[DLOPEN] PASS
[GLIBC] DLOPEN PASS: glibc dlopen
```

同时 G0–G4 全部保持通过：

```text
[GLIBC] PASS: dynamic glibc execve
[GLIBC] HELLO PASS: dynamic glibc hello
[GLIBC] G2 PASS: startup syscalls
[GLIBC] TIME PASS: no-vdso fallback
[GLIBC] PTHREAD PASS: glibc pthread
[GLIBC] CLONE3 PASS: glibc clone3
```

`bash scripts/qemu_boot_test.sh x86_64` 返回 `QEMU_RC=0`；`make test-kernel-host` 为
`640 passed / 0 failed`（新增 2 条 VMA 回归单测）；`./ci/build.sh all` 为
`Passed: 6 / Failed: 0`；`bash ci/audit.sh full` 为 `AUDIT_RC=0`，双架构 QEMU
`2/2`，本轮因环境缺失未执行腿数 `0`。

## 2. 根因

首轮现象不是 `dlopen` 失败，而是 glibc 在 loader 内部调用 `malloc/free` 时打印
`malloc(): corrupted top size`。逐层缩小后确认两个独立根因。

### 2.1 `find_free_range` 把 mmap 游标回退到低位 VMA 末尾

`MmStruct::find_free_range` 从 `mmap_base` 开始搜索，但遍历 VMA 时无条件执行
`cursor = vma.end`。当低位已有 PIE/库 VMA 时，游标被拉回 `main_end`，hint-less
`mmap` 于是从 `main_end` 分配。glibc loader 把 `libc.so.6` 映射到该地址，正好覆盖
未来 brk 堆区；后续 `brk` 扩展与这个 `FileBacked` VMA 重叠，`insert_vma` 返回
`VMA overlap with incompatible mapping`。

修复前调试日志可直接看到：

```text
[VMA] ov kind=FileBacked/Heap flags=PageFlags(PRESENT | USER | NX)/PageFlags(PRESENT | WRITABLE | USER)
       ex=5555555f3000..55555566d000 new=5555555f3000..5555555f4000
[BRK] insert-fail VMA overlap with incompatible mapping
```

修复：遍历时跳过已经整体落在 `cursor` 之下的 VMA，禁止把游标回退；只有跨过
`cursor` 的 VMA 才推进游标。抽出 `find_free_range_from(start, size)` 供单测复用，
新增回归测试锁住“hint-less mmap 必须从 `mmap_base` 之上分配”。

### 2.2 `set_brk` 把页对齐后的值写回 `mm->brk`

`set_brk` 原先把 `page_aligned` 同时写入 `mm.brk` 并作为返回值。Linux 的
`mm->brk` 保存调用方请求的精确 break，只有 VMA 覆盖范围按页对齐；glibc `sbrk`
依赖返回的精确旧 break 推导堆布局。页对齐回写让 glibc 的 top chunk 记账错位，
即使堆区不再被库占用，`malloc(64); free()` 仍会报 `corrupted top size`。

修复后 `set_brk`：

- 拒绝 `new_brk < start_brk`；
- VMA 仍按页对齐扩展/收缩；
- `mm.brk` 保存并返回精确 `new_brk`；
- 收缩时只拆除页对齐区间，保留当前页内仍有效的堆页。

回归实测：直接 `brk(0)` 后请求 `b0 + 0x123`，修复前返回 `-1`，修复后返回精确
`b0 + 0x123`；连续两轮 `malloc(64)/free` 通过。

### 2.3 `dispatch_fs_io` 缺少 `SYS_PREAD64` import

首轮更早的失败是 init 在 `[UACCESS] PASS` 后 `#GP`，串口出现 `pipe() rc=-22`、
`timerfd create fd=-14`。原因是 `dispatch_fs_io` 新增了 `SYS_PREAD64 => ...` 分支，
但 `use` 列表未导入该常量；Rust 把未导入的大写标识符解释成变量绑定，分支变成
catch-all，拦走所有进入 `dispatch_fs_io` 的调用。补 import 后恢复。该问题已随 L1
代码提交，并在报告中保留，避免再次以“缺 import 的 match 分支”形式回归。

## 3. 实现

- 内核新增 `pread64_syscall`：`buf == 0`/范围非法 → `EFAULT`，`pos < 0` →
  `EINVAL`，`count == 0` → `0`；正常路径委托 `vfs_pread`，不改变 fd 当前偏移。
- `dispatch_fs_io` 增加 `SYS_PREAD64` arm 与 import。
- 新增 `scripts/fixtures/dynamic-glibc-x86_64/glibc_dlopen.c`：`RTLD_NOW|RTLD_LOCAL`
  装载 `/lib64/libdlprobe.so`，`dlsym` 解析 `probe_add`/`probe_get`，首次调用期望
  `12`，缺失符号期望 `NULL`，`dlclose` 后重新 `dlopen` 期望 `probe_counter` 回到
  `5`。
- 新增 `libdlprobe.c` / `libdlprobe.so`：`ET_DYN`、`SONAME=libdlprobe.so`、
  `NEEDED` 为空，导出 `probe_counter`/`probe_add`/`probe_get`。
- `Makefile` 增加 `USER_GLIBC_DLOPEN`/`USER_LIBDLPROBE`/`GLIBC_CPIO_DEPS`，并用
  显式 `GLIBC_PREBUILT` 目标阻止 make 内置 `%: %.c` 调用宿主不存在的交叉编译器。
- `make_initramfs_cpio.py` 增加 `bin/glibc_dlopen`、`lib64/libdlprobe.so`。
- init 增加 `GLIBC_DLOPEN_PATH`，在 `glibc_probe()` 中执行并打印
  `[GLIBC] DLOPEN PASS: glibc dlopen`。
- `qemu_boot_test.sh` 增加 `[GLIBC] DLOPEN PASS`/`[DLOPEN] PASS` 正断言与
  `[DLOPEN] FAIL` 反断言。

## 4. 边界登记

- `pread64` 的无效 fd 与底层读错误仍经 `vfs_pread` 的 `-1` 泛化，当前落到
  `EPERM`，不是 POSIX 的 `EBADF`/`EIO`；`count > u32::MAX` 仍按 `u32` 窄化。
- `libdlprobe.so` 无 `NEEDED`，不覆盖“第二个 `.so` 自己依赖 libc/其他 `.so`”的
  依赖链；L1 只证明单层第二共享对象装载、符号解析、`GLOB_DAT` 重定位与卸载后
  重新初始化。
- 探针使用 `RTLD_NOW|RTLD_LOCAL`；未覆盖 `RTLD_GLOBAL`、符号插入/抢占、构造/析构
  函数、TLS 析构、`dlclose` 后代码页是否真正 `munmap`。
- `dlclose` 成功只以返回值和重新 `dlopen` 后 `probe_counter=5` 判定；glibc 可以
  保留映射或延迟卸载，本腿不要求内核证明物理页已回收。
- `glibc_dlopen` 目标路径固定 `/lib64/libdlprobe.so`；宿主直跑需要对应的 `/lib64`
  布局，QEMU 腿以 initramfs 为准。
- aarch64 无 glibc 载体，L1 仅 x86_64；aarch64 探针打印 SKIP。
- 内核体积 `_kernel_end_phys=0x3ff000`，距 K-08 上限 `0x400000` 仅 4 KiB；后续
  不得在该切片上叠加无界格式化调试。

## 5. 证据

- `other/build/log/qemu_boot_x86_64.log`
- `/tmp/qemu_final_l1b.log`
- `scripts/fixtures/dynamic-glibc-x86_64/README.md`
- `scripts/fixtures/dynamic-glibc-x86_64/SHA256SUMS`
- `/tmp/test_host_l1c.log`
- `/tmp/build_all_l1.log`
- `/tmp/audit_l1.log`
