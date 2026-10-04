//! 安装向导 — 6 步交互式系统安装 → 持久化到磁盘

mod probe;
mod prepare;
mod deploy;
mod auth;
mod config;
mod finish;

use userlib::{println, read_line};
use userlib::sys::{fs_mount, fs_unmount};

const MOUNT_POINT: &[u8] = b"/mnt\0";

fn mount_target() -> bool {
    let r = fs_mount(b"none\0", MOUNT_POINT, b"unkfs\0", b"defaults\0");
    r == 0
}

fn unmount_target() {
    fs_unmount(MOUNT_POINT);
}

fn welcome_page() {
    println("");
    println("========================================");
    println("        Edgine Installation Wizard");
    println("========================================");
    println("");
    println("Welcome to Edgine Operating System!");
    println("");
    println("This wizard will guide you through the");
    println("system installation process.");
    println("");
    println("Press ENTER to continue...");
    let mut buf = [0u8; 16];
    read_line(&mut buf);
}

pub fn run() {
    welcome_page();

    // Step 1: 磁盘探测与选择
    if probe::detect() != 0 {
        println("Installation aborted: No disks available.");
        println("Tip: Attach a virtual disk or physical drive and reboot.");
        return;
    }
    if probe::choose() != 0 {
        println("Installation cancelled by user.");
        return;
    }

    let (disk_id, sectors) = probe::selected();

    // Step 2: 分区与格式化 (一旦执行就无法撤销)
    if prepare::execute(disk_id, sectors) != 0 {
        println("Installation failed: Disk preparation error.");
        println("Tip: The disk may be in an inconsistent state.");
        println("     Re-run the installer or re-initialize the disk.");
        return;
    }

    // Step 3: 挂载目标文件系统
    println(""); println("Mounting target filesystem...");
    if !mount_target() {
        println("  [ERROR] Failed to mount UNKFS to /mnt");
        println("Installation failed: Unable to access target disk.");
        println("Tip: Disk was partitioned but filesystem may be corrupt.");
        println("     Re-run the installer to re-format the disk.");
        return;
    }
    println("  [OK] Target mounted at /mnt");

    // Step 4: 应用部署
    if deploy::deploy_all() != 0 {
        println("Installation failed: Application deployment error.");
        println("Tip: Check that source files exist on the install media.");
        unmount_target();
        return;
    }

    // Step 5: 根身份
    if auth::create() != 0 {
        println("Installation failed at root identity setup.");
        println("Tip: Re-run the installer — the disk will be re-used.");
        unmount_target();
        return;
    }

    // Step 6: 系统配置
    config::hostname();
    config::directory_tree();
    config::fstab();

    // Step 7: 完成
    if finish::execute() != 0 {
        println("Warning: Installation may be incomplete.");
        println("Tip: Re-run the installer to attempt recovery.");
    }

    println("Unmounting target filesystem...");
    unmount_target();
}

pub fn needed() -> bool {
    finish::check_marker()
}
