# 幻影挂载面评估

> 结论：`mount(2)` 的返回语义与「后端是否存在」脱钩。当前实现对至少两类文件系统（Ext2 / ExFAT）会返回成功，但挂载表中只留下「路径 + 类型名 + used 标记」，没有绑定任何 `FileSystem` trait object；其后所有 I/O 必然失败。同时 Ext2 / ExFAT 在 `functions/fs` 中**没有注册点**，因此即使改走 trait object 路径也无后端可用。DevFS 具备后端，但挂载调用只存在于测试里。

**快照与基线**：HEAD `a7b6d87b`（2026-11-13 20:26 +0800），取证日期 2026-11-14。全程只读。

## 一、挂载路径的完整链路

入口 `functions/fs/vfs_mount.rs:23` 的 `vfs_mount_internal(path, fs_name)`，先按 `fs_name` 解析出 `FsType`，随后进入一个两段式结构：

第一段（按类型预处理），`vfs_mount.rs:48`：

```rust
FsType::DevFs | FsType::Ext2 | FsType::ExFat => {}
```

该臂体为空。紧邻的注释声称「ext2 / exfat 挂载分别由 `Ext2FileSystem` / `ExfatFileSystem::fs_mount` 处理」，但该臂没有调用任何 `fs_mount`。

第二段（绑定 trait object），`vfs_mount.rs:68-79`：

```rust
let fs: &'static dyn FileSystem = match fs_type {
    FsType::RamFs => ramfs_fs(),
    FsType::Egfs => match egfs_fs() { ... },
    FsType::DevFs => &DEVFS_DATA,
    FsType::TmpFs | FsType::OverlayFs => match current_fs_backend().resolve_fs(fs_name) { ... },
    _ => return VFS_MANAGER.mount(path, fs_name).as_i32(),
};
VFS_MANAGER.mount_with_fs(path, fs_name, fs).as_i32()
```

Ext2 / ExFAT 落入 `_` 分支，走 `VFS_MANAGER.mount(path, fs_name)`，而不是下方的 `mount_with_fs`。

## 二、`VFS_MANAGER.mount` 的真实语义

`functions/fs/vfs_manager.rs:310-330`：

```rust
pub fn mount(&self, path: &str, fs_name: &str) -> Result<(), KernelError> {
    let mut mounts = self.mounts.lock();
    for mount in mounts.iter() {
        if mount.used && mount.get_path() == path {
            return Err(KernelError::AlreadyExists);
        }
    }
    for mount in mounts.iter_mut() {
        if !mount.used {
            mount.set_path(path);
            mount.set_fs_type(fs_name);
            mount.used = true;
            return Ok(());
        }
    }
    Err(KernelError::NoSpace)
}
```

该函数只做三件事：判重、占槽、写入路径与类型名。它**不调用** `fs_mount`，**不解析**后端，**不绑定** trait object。返回值 `Ok(())` 的含义是「挂载表里记了一条」，而不是「该路径现在可访问」。

对照 `mount_with_fs`（`vfs_manager.rs:336` 起）才是带后端的真实路径：它接收 `&'static dyn FileSystem` 并保存。

## 三、后端根本不存在

在 `src/kernel/functions/fs` 下检索 `Ext2Fs` / `ExfatFs` / `ext2::` / `exfat::`，**零命中**——没有任何注册点把这两个实现接入 `current_fs_backend()` 或挂载分派。目录与代码确实存在（`ext2/` 约 2,455 行、`exfat/` 约 1,009 行），但它们是**孤立实现**：既不在启动编排里注册，也不在 `vfs_mount_internal` 的任何可达分支里被引用。

因此 Ext2 / ExFAT 的问题是双重的：

1. 分派层把它们送到一个只记账的 `mount`；
2. 即便改送 `mount_with_fs`，也没有可解析的后端实例。

## 四、DevFS 的相邻问题

DevFS 在分派层有真实后端（`vfs_mount.rs:73` 的 `FsType::DevFs => &DEVFS_DATA`），走 `mount_with_fs`，属正确形态。但**生产代码中没有挂载调用**：`DEVFS_DATA` 唯一与挂载相关的调用出现在 `functions/fs/devfs.rs:873-875` 的单元测试 `test_devfs_mount` 中，断言 `devfs.mount("/dev") == 0`。

启动编排实际挂载的是 ramfs / EGFS（无盘时回退 ramfs），`/dev` 在生产启动路径上是否就绪取决于别的初始化路径，而不是 `mount(2)`。这意味着「devfs 可用」这一能力宣称在测试里成立、在启动后不一定成立。

## 五、测试为什么没发现

现有测试的口径是「挂载表状态」而不是「挂载后可 I/O」：

- 没有一条测试执行「`mount()` 返回 0 之后立刻 `open()` + `read()`」的往返断言；
- Ext2 / ExFAT 无磁盘镜像测试，也无 RAM-backed 块设备 mock 的挂载测试；
- `mount()` 的单元测试只检查 `used` / `path` / `fs_type` 三个字段被写入。

因此幻影挂载在测试面上是「全绿」的：函数返回了它被设计返回的值，只是这个值不承载调用方需要的语义。

## 六、后果

对外部用户态是直接阻断，且表现具有误导性：

1. `mount("ext2", "/mnt", ...)` 返回 0 → 程序认为挂载成功；
2. 随后 `open("/mnt/file")` 失败 → 程序归因为「文件不存在」或「权限问题」；
3. 真实原因（挂载点无后端）在任何返回值与日志里都不出现。

这与「返回 `-ENODEV` / `-ENOSYS`」的差别是本质性的：后者让程序在第一步就知道不支持，前者把故障推迟到第一次 I/O，并且污染了错误归因。

## 七、与其它缺口的关系

幻影挂载与三处相邻缺陷同源——「返回成功」优先于「语义正确」：

- 假成功 syscall（`select` / `newfstatat`）是同一模式在参数层的表现；
- EGFS 的目录项持久化把对象号硬写 0，是同一模式在序列化层的表现；
- `mount` 只记账不绑后端，是同一模式在资源管理层的表现。

三者共享同一个检测难点：它们都「返回了看起来正确的值」。

## 八、对计划的意义

幻影挂载的收口标准不应该是「`mount` 分支补上 `fs_mount`」，而应该是**把挂载成功与后端存在性绑定为同一个事实**：无法解析后端时 fail-closed 返回错误，而不是占一个槽位。配套的验收必须是一条行为级往返（mount → open → read），否则同类缺陷会以新的文件系统名字再次出现。

Ext2 / ExFAT 的处置是独立的第二个决策：要么注册进后端并纳入磁盘测试，要么从能力面移除（连同代码），不能保持「有实现、无注册、可被 mount 返回成功」的中间态。
