//! 磁盘探测与选择
//!
//! 安全检测:
//!   - 阻止安装到安装介质自身 (单盘检测)
//!   - 最小磁盘容量检查 (128 MB)

use userlib::sys::{self, UserDiskInfo};
use userlib::{print, print_dec, println, read_line};

pub(crate) const MAX_DISKS: usize = 4;
const MIN_DISK_SECTORS: u64 = 262_144; // 128 MB (sector = 512 bytes)

// unsafe 依据单源 (探测状态面): 三个 static mut 只由本模块 detect / choose /
// selected 访问. install 是单线程向导 (全 crate 无 clone 或线程创建 syscall),
// 且 wizard::run 严格按 detect() → choose() → selected() 顺序、每步返 0 才继续,
// 故任一处访问时既无并发写入者, 也无跨函数存活的 DISK_LIST 借用.
// 下标上界单源: 内核 disk_list 返回 min(max_count, 设备数)
// (functions/sgeg/storage/disk.rs), 本模块传的 max_count 恒为 MAX_DISKS ⇒
// DISK_COUNT ≤ MAX_DISKS, 凡以它为界的下标都落在 DISK_LIST 界内.
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
    // SAFETY: 探测状态单源 (见 static 声明处) —— 只读取值拷贝, 不产生指向 static
    // 的引用. SELECTED_DISK 由 choose() 成功路径写在 [0, DISK_COUNT) 内; 初值 -1
    // 时下面 try_from 的 expect 先 panic, 不会把负值当下标用.
    let idx =
        usize::try_from(unsafe { SELECTED_DISK }).expect("selected() 仅在 choose() 成功后调用");
    // SAFETY: 单源 —— idx < DISK_COUNT ≤ MAX_DISKS 故在界内; 该共享借用只活到
    // 下面两个标量拷贝为止, 期间无 DISK_LIST 写入者 (单线程 + 向导顺序).
    let info = unsafe { &DISK_LIST[idx] };
    (info.disk_id, u64::from(info.sectors))
}

fn atoi(buf: &[u8], len: usize) -> i32 {
    let mut v: i32 = 0;
    for &c in &buf[..len] {
        if c.is_ascii_digit() {
            v = v * 10 + i32::from(c - b'0');
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
    // SAFETY: 探测状态单源 —— 即时写一个 i32, 此刻尚无任何 DISK_LIST 借用存活
    // (下面循环才开始取); count > 0 已判且 ≤ MAX_DISKS (内核按 max_count 截断).
    unsafe {
        DISK_COUNT = count;
    }
    println("");
    print("Detected ");
    print_dec(i64::from(count));
    println(" disk(s):");
    println("");
    let mut usable = 0;
    for i in 0..usize::try_from(count).expect("count 已确认大于 0") {
        // SAFETY: 单源 —— i < count ≤ MAX_DISKS 故在界内; 每轮迭代独占一个元素,
        // 上一轮的可变借用已随迭代结束, 无其它存活引用. 内核 disk_info 经
        // write_struct_to_user 写整个结构, 该入口自带 check_user_buf(ptr,
        // size_of::<UserDiskInfo>()) 全尺寸校验, 两侧结构同形 repr(C) ⇒ 写入不越
        // 本元素; 借用存活至 disk_info 返回及其后的打印读完为止.
        let info = unsafe { &mut DISK_LIST[i] };
        sys::disk_info(u32::try_from(disk_ids[i]).expect("磁盘 ID 超出 u32"), info);
        print("  [");
        print_dec(i64::try_from(i).expect("磁盘下标超出 i64"));
        print("] Disk ");
        print_dec(i64::from(info.disk_id));
        print(": ");
        let model = core::str::from_utf8(&info.model).unwrap_or("Unknown");
        print(model.trim_end_matches('\0'));
        print(" (");
        let size_mb = info.sectors / 2 / 1024;
        if size_mb >= 1024 {
            print_dec(i64::from(size_mb / 1024));
            print(" GB");
        } else {
            print_dec(i64::from(size_mb));
            print(" MB");
        }
        if u64::from(info.sectors) < MIN_DISK_SECTORS {
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
    // SAFETY: 探测状态单源 —— 只读取值拷贝, 不产生引用. choose() 只在 detect()
    // 返 0 后被调, 故 DISK_COUNT 已是 [1, MAX_DISKS] 内的实测设备数 (count <= 0
    // 那条路径 detect 返 -1, 向导已中止), 下面的 count - 1 不会下溢.
    let count = usize::try_from(unsafe { DISK_COUNT }).expect("choose() 在 detect() 成功之后调用");

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
    print_dec(i64::try_from(count - 1).expect("磁盘数超出 i64"));
    print("): ");
    let mut buf = [0u8; 8];
    let len = read_line(&mut buf);
    if len == 0 {
        println("  [ERROR] No selection made.");
        return -1;
    }
    let sel = atoi(&buf, len);
    if sel < 0 || usize::try_from(sel).expect("已排除负值") >= count {
        println("  [ERROR] Invalid selection.");
        return -1;
    }

    // 容量检查
    // SAFETY: 单源 —— sel 已通过上面的 `sel >= 0 && sel < count` 校验, 而
    // count = DISK_COUNT ≤ MAX_DISKS ⇒ 下标在界内; 该共享借用活到本函数结束,
    // 期间只写 SELECTED_DISK (另一个 static), 不写 DISK_LIST, 借用不被失效.
    let info = unsafe { &DISK_LIST[usize::try_from(sel).expect("已通过范围校验")] };
    if u64::from(info.sectors) < MIN_DISK_SECTORS {
        let model = core::str::from_utf8(&info.model).unwrap_or("Unknown");
        print("  [ERROR] Disk ");
        print(model.trim_end_matches('\0'));
        println(" is too small (need at least 128 MB).");
        return -1;
    }

    // SAFETY: 单源 —— 即时写一个 i32, 目标是与上面存活的 info 借用不同的 static,
    // 不构成别名冲突; sel 已范围校验, 写入后 selected() 的下标前提由此成立.
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
