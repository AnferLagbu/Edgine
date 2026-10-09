//! 磁盘探测与选择
//!
//! 安全检测:
//!   - 阻止安装到安装介质自身 (单盘检测)
//!   - 最小磁盘容量检查 (128 MB)

use userlib::sys::{self, UserDiskInfo};
use userlib::{print, print_dec, println, read_line};

pub(crate) const MAX_DISKS: usize = 4;
const MIN_DISK_SECTORS: u64 = 262144; // 128 MB (sector = 512 bytes)

static mut DISK_COUNT: i32 = 0;
static mut SELECTED_DISK: i32 = -1;
static mut DISK_LIST: [UserDiskInfo; MAX_DISKS] = [
    UserDiskInfo {
        disk_id: 0,
        present: 0,
        total_sectors: 0,
        sectors: 0,
        model: [0; 64],
    },
    UserDiskInfo {
        disk_id: 0,
        present: 0,
        total_sectors: 0,
        sectors: 0,
        model: [0; 64],
    },
    UserDiskInfo {
        disk_id: 0,
        present: 0,
        total_sectors: 0,
        sectors: 0,
        model: [0; 64],
    },
    UserDiskInfo {
        disk_id: 0,
        present: 0,
        total_sectors: 0,
        sectors: 0,
        model: [0; 64],
    },
];

pub fn selected() -> (u32, u64) {
    let idx = unsafe { SELECTED_DISK } as usize;
    let info = unsafe { &DISK_LIST[idx] };
    (info.disk_id, info.sectors as u64)
}

fn atoi(buf: &[u8], len: usize) -> i32 {
    let mut v: i32 = 0;
    for &c in &buf[..len] {
        if c.is_ascii_digit() {
            v = v * 10 + (c - b'0') as i32;
        }
    }
    v
}

pub fn detect() -> i32 {
    println("");
    println("--- Step 1: Disk Detection ---");
    println("");
    println("Scanning for available disks...");
    let mut disk_ids = [0u64; MAX_DISKS];
    let count = sys::disk_list(&mut disk_ids);
    if count <= 0 {
        println("  [ERROR] No disks detected!");
        return -1;
    }
    unsafe {
        DISK_COUNT = count;
    }
    println("");
    print("Detected ");
    print_dec(count as i64);
    println(" disk(s):");
    println("");
    let mut usable = 0;
    for i in 0..(count as usize) {
        let info = unsafe { &mut DISK_LIST[i] };
        sys::disk_info(disk_ids[i] as u32, info);
        print("  [");
        print_dec(i as i64);
        print("] Disk ");
        print_dec(info.disk_id as i64);
        print(": ");
        let model = core::str::from_utf8(&info.model).unwrap_or("Unknown");
        print(model.trim_end_matches('\0'));
        print(" (");
        let size_mb = info.sectors / 2 / 1024;
        if size_mb >= 1024 {
            print_dec((size_mb / 1024) as i64);
            print(" GB");
        } else {
            print_dec(size_mb as i64);
            print(" MB");
        }
        if (info.sectors as u64) < MIN_DISK_SECTORS {
            println(") [SKIP: too small, need >=128 MB]");
        } else {
            usable += 1;
            println(")");
        }
    }
    if usable == 0 {
        println("");
        println("  [ERROR] No usable disks (minimum 128 MB required).");
        return -1;
    }
    0
}

pub fn choose() -> i32 {
    println("");
    println("Select a disk for installation:");
    let count = unsafe { DISK_COUNT as usize };

    // 单盘警告: 安装介质与目标可能是同一磁盘
    if count == 1 {
        println("");
        println("  [WARNING] Only one disk detected!");
        println("  The installation media and target disk may be the SAME device.");
        println("  If you continue, the installer may overwrite itself.");
        println("  This is only safe on virtual machines with separate boot/disk images.");
        println("");
    }

    print("Enter disk number (0-");
    print_dec((count - 1) as i64);
    print("): ");
    let mut buf = [0u8; 8];
    let len = read_line(&mut buf);
    if len == 0 {
        println("  [ERROR] No selection made.");
        return -1;
    }
    let sel = atoi(&buf, len);
    if sel < 0 || sel as usize >= count {
        println("  [ERROR] Invalid selection.");
        return -1;
    }

    // 容量检查
    let info = unsafe { &DISK_LIST[sel as usize] };
    if (info.sectors as u64) < MIN_DISK_SECTORS {
        let model = core::str::from_utf8(&info.model).unwrap_or("Unknown");
        print("  [ERROR] Disk ");
        print(model.trim_end_matches('\0'));
        println(" is too small (need at least 128 MB).");
        return -1;
    }

    unsafe {
        SELECTED_DISK = sel;
    }
    println("");
    println("  [WARNING] ALL DATA ON THIS DISK WILL BE ERASED!");
    print("  Selected: ");
    let model = core::str::from_utf8(&info.model).unwrap_or("Unknown");
    println(model.trim_end_matches('\0'));
    println("");
    print("Type 'yes' to confirm: ");
    let mut confirm = [0u8; 8];
    read_line(&mut confirm);
    let y = (confirm[0] | 0x20) == b'y';
    let e = (confirm[1] | 0x20) == b'e';
    let s = (confirm[2] | 0x20) == b's';
    if !y || !e || !s {
        println("  Installation cancelled.");
        return -1;
    }
    0
}
