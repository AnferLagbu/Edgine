//! 应用部署 — 批量复制系统二进制文件至安装目标 (/mnt)
//!
//! 部署前预检所有源文件是否存在，避免部分写入后才发现缺失。

use userlib::fs;
use userlib::sys;
use userlib::{print, print_dec, println};

pub struct AppManifest {
    pub src: &'static [u8],
    pub dst_rel: &'static [u8],
    pub desc: &'static str,
}

static TARGET_PREFIX: &[u8] = b"/mnt";

static MANIFEST: &[AppManifest] = &[
    AppManifest {
        src: b"/boot/kernel.bin\0",
        dst_rel: b"cfg/boot/kernel.bin\0",
        desc: "Kernel",
    },
    AppManifest {
        src: b"/bin/init\0",
        dst_rel: b"app/sys/init\0",
        desc: "Init process",
    },
    AppManifest {
        src: b"/bin/eash\0",
        dst_rel: b"app/sys/eash\0",
        desc: "eash Shell",
    },
    AppManifest {
        src: b"/bin/install\0",
        dst_rel: b"app/sys/installguide\0",
        desc: "Install guide",
    },
];

fn build_dst<'a>(rel: &[u8], buf: &'a mut [u8; 64]) -> &'a [u8] {
    let mut pos = 0;
    for &b in TARGET_PREFIX {
        buf[pos] = b;
        pos += 1;
    }
    if !rel.starts_with(b"/") && pos < buf.len() {
        buf[pos] = b'/';
        pos += 1;
    }
    for &b in rel {
        if b == 0 {
            break;
        }
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    if pos < buf.len() {
        buf[pos] = 0;
        pos += 1;
    }
    &buf[..pos - 1]
}

pub fn deploy_all() -> i32 {
    println("");
    println("--- Step 3: Application Deployment ---");
    println("");

    // 预检: 所有源文件是否存在
    let mut missing = false;
    for m in MANIFEST {
        let fd = fs::file_open(m.src, sys::O_RDONLY);
        if fd < 0 {
            print("  [MISSING] ");
            println(m.desc);
            missing = true;
        } else {
            sys::fs_close(fd);
        }
    }
    if missing {
        println("");
        println("  [ABORT] One or more source files are missing.");
        println("  The install media may be incomplete or corrupted.");
        return -1;
    }

    // 执行复制
    println("  All source files verified. Copying...");
    println("");
    let mut ok = 0u32;
    let mut fail = 0u32;
    for m in MANIFEST {
        print("  ");
        print(m.desc);
        let mut dst_buf = [0u8; 64];
        let dst = build_dst(m.dst_rel, &mut dst_buf);
        if fs::file_copy(m.src, dst) {
            println(" ... OK");
            ok += 1;
        } else {
            println(" ... FAIL");
            print("      src: ");
            let src_str = core::str::from_utf8(m.src)
                .unwrap_or("?")
                .trim_end_matches('\0');
            println(src_str);
            fail += 1;
        }
    }
    println("");
    print("  Deployed: ");
    print_dec(ok as i64);
    print(" / ");
    print_dec((ok + fail) as i64);
    println("");
    if fail > 0 {
        println("  [ERROR] Some files could not be written to disk.");
        println("  Check that the target filesystem has enough space.");
        return -1;
    }
    0
}
