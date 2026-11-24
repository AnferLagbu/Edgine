# uaccess 写前预检设计

## 背景

x86_64 在 UEFI(OVMF) 下 `CR0.WP=1`，ring0 对**存在的只读页**做写会触发 `#PF`（`e=0003`）。
fork 把用户页置 COW 只读后，内核若直接写用户指针（典型：`clone` 的 `CLONE_PARENT_SETTID`
回写 TID），就会在核态缺页，旧 `#PF` 处理器把保护违例一律 `Panic`，该 CPU 停机。

UEFI U-1 的实测链：`clone_vm` 完成 `alloc/inherit/join_group/process_insert/adopt` 后，
在 `write_struct_to_user(parent_tidptr)` 处 `#PF e=0003 CR2=0x40C0D0`，`clone_thread`
永不返回，helper TCB 虽已登记但未入运行队列，5 s 后 `reap-timeout`。

## 方案：预检 + 窄兜底

核态 uaccess 不再依赖“访问中捕获异常再跳恢复点”（旧恢复点标签在 `setup_recovery`
函数体内，无法安全跨函数返回，且此前无人消费）。改为在进入 store/load 前把用户可控的
缺页消掉：

1. **读路径**：`validate_user_buf` 逐页确认已映射后才读。
2. **写路径**：`validate_user_writable` 逐页确认描述符可写；若只读但带 fork COW 帧标记，
   先调用与 EL0 缺页同一入口 `cow_handle_fault` 破 COW，破后复验可写；真只读/未映射直接
   返回 `false`。调用方转成 `EFAULT`，不进入裸 store。
3. **`#PF` 兜底**：x86 核态写、present、用户半区、用户 leaf/只读/帧带 COW 标记时，破 COW
   并返回重试；不命中仍按原语义 `Panic`（防止把未映射/真只读竞态静默吞掉）。

预检与 store 之间仍可能有另一线程 `mprotect` 的竞态；该窄兜底只负责 COW 竞态，非 COW 竞态
属内核缺陷，保持 panic 暴露。

## 覆盖面

- `userptr::write_struct_to_user`
- `syscall::raw::write_struct` / `write_u64` / `write_u64_pair`
- `copy_user::copy_to_user` / `clear_user`
- `copy_from_user` 读前映射预检；`copy_string_from_user` / `strlen_user` 逐页映射预检

## clone 语义

`vm_child_register` 把 TID 回写移到 `join_thread_group` / `process_insert` / 资源接管之前：
写失败时 TCB 尚未进进程表、未入组，可直接释放，调用方归还 PID，返回 `EFAULT`。
不再出现“TID 没写进去却已登记/入队”的半成品线程。

## 回归锁

- `[UACCESS] PASS: readonly clone EFAULT`：`clone` 的 `parent_tid` 指向 `.text` 只读页，
  必须返回 `-EFAULT`，不得停机。
- x86 UEFI `[THREAD] PASS` 连续 5/5；BIOS 与 aarch64 同镜像 PASS。
- host 测试锁定 `kernel_uaccess_cow_candidate_x86` 的 present/write/supervisor/用户半区判据。
