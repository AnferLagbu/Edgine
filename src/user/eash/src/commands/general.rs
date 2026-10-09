/// Shell 内置命令: help, clear, echo, exit
use userlib::{print, println};

use super::{Cmd, as_str};

pub fn help(cmd: &Cmd) {
    let filter = if cmd.n > 1 { as_str(cmd.get(1)) } else { "" };

    let categories: &[(&str, &str)] = &[
        ("file", "\n\u{25bc} 文件操作"),
        ("sys", "\n\u{25bc} 系统"),
        ("id", "\n\u{25bc} 身份"),
        ("shell", "\n\u{25bc} Shell 内置"),
    ];

    for (cat_key, cat_title) in categories {
        if !filter.is_empty() && filter != *cat_key {
            continue;
        }
        println(cat_title);
        for entry in super::TABLE {
            if entry.category == *cat_key {
                println(entry.help_line);
            }
        }
    }

    if filter.is_empty() {
        println("\nEdgine Shell — deep & lightweight");
        println("Use 'help <category>' for details");
    }
}

pub fn clear(_: &Cmd) {
    // ANSI 清屏
    print("\x1b[2J\x1b[H");
}

pub fn echo(cmd: &Cmd) {
    for i in 1..cmd.n {
        if i > 1 {
            print(" ");
        }
        print(as_str(cmd.get(i)));
    }
    println("");
}

pub fn exit(_: &Cmd) {
    // 由 shell 主循环处理
    crate::MAIN_EXIT.store(true, core::sync::atomic::Ordering::SeqCst);
}
