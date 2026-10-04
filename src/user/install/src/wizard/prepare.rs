//! 磁盘准备: 分区表创建、FAT16/UNKFS 格式化、引导器安装
//!
//! 任意步骤失败即中止，避免在半初始化的磁盘上继续操作。

use userlib::{print, println, print_dec};
use userlib::sys;

pub fn execute(disk_id: u32, sectors: u64) -> i32 {
    println(""); println("--- Step 2: Disk Partitioning & Formatting ---"); println("");

    // 2a. 分区表
    print("  Creating partition table (FAT16 boot + UNKFS)...");
    let r = sys::disk_partition(disk_id, sectors);
    if r != 0 {
        print(" FAILED (error "); print_dec(r as i64); println(")");
        println("  [ABORT] Cannot proceed without valid partition table.");
        return -1;
    }
    println(" [OK]");

    // 2b. FAT16 引导分区
    print("  Formatting boot partition (FAT16)...");
    let r = sys::fat_format(disk_id);
    if r != 0 {
        print(" FAILED (error "); print_dec(r as i64); println(")");
        println("  [ABORT] Boot partition format failed. Disk may need re-initialization.");
        return -1;
    }
    println(" [OK]");

    // 2c. UNKFS 系统分区
    print("  Formatting system partition (UNKFS)...");
    let r = sys::disk_format(disk_id);
    if r != 0 {
        print(" FAILED (error "); print_dec(r as i64); println(")");
        println("  [ABORT] System partition format failed. Disk may need re-initialization.");
        return -1;
    }
    println(" [OK]");

    // 2d. 引导器
    println("  Installing bootloader...");
    let r = sys::boot_install(disk_id);
    if r != 0 {
        print("  [ERROR] Bootloader installation failed (error: ");
        print_dec(r); println(")");
        return -1;
    }
    println("  [OK] Bootloader installed (Stage1 + kernel)");
    0
}
