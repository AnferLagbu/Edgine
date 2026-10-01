/// 文件操作命令: dir, cd, pwd, cat, mkdir, touch, del, cp, mv, save
use userlib::*;
use userlib::sys::*;

use super::{Cmd, as_str, path_arg};

fn print_nb(buf: &[u8]) { print(core::str::from_utf8(buf).unwrap_or("?").trim_end_matches('\0')); }

pub fn dir(cmd: &Cmd) {
    let p = path_arg(cmd).unwrap_or_else(|| { let mut d = [0u8; 256]; d[0]=b'/'; d[1]=0; d });
    let fd = file_open(&p, O_RDONLY);
    if fd < 0 { print("dir: '"); print_nb(&p); println("' not found"); return; }
    let mut count = 0;
    loop {
        let mut entry = UserDirEntry { node: 0, file_type: 0, name: [0; 256] };
        if fs_readdir(fd, &mut entry) <= 0 { break; }
        if entry.node != 0 {
            if entry.file_type == FT_DIR { print("  [D] "); } else { print("  [F] "); }
            println(core::str::from_utf8(&entry.name).unwrap_or("?").trim_end_matches('\0'));
            count += 1;
        }
    }
    fs_close(fd);
    if count == 0 { println("  (empty)"); }
}

pub fn cd(cmd: &Cmd) {
    if let Some(p) = path_arg(cmd) {
        if env_chdir(&p) < 0 { print("cd: '"); print_nb(&p); println("' not found"); }
    } else { println("cd: missing path"); }
}

pub fn pwd(_: &Cmd) {
    let mut cwd = [0u8; 128];
    if env_getcwd(&mut cwd) >= 0 {
        let s = core::str::from_utf8(&cwd).unwrap_or("/").trim_end_matches('\0');
        println(if s.is_empty() { "/" } else { s });
    } else { println("/"); }
}

pub fn cat(cmd: &Cmd) {
    if let Some(p) = path_arg(cmd) {
        let fd = file_open(&p, O_RDONLY);
        if fd < 0 { print("cat: '"); print_nb(&p); println("' not found"); return; }
        let mut buf = [0u8; 512];
        loop { let n = fs_read(fd, &mut buf[..511]); if n <= 0 { break; } buf[n as usize]=0;
            print(core::str::from_utf8(&buf[..n as usize]).unwrap_or("<binary>")); }
        fs_close(fd);
    } else { println("cat: missing file"); }
}

pub fn mkdir(cmd: &Cmd) {
    if let Some(p) = path_arg(cmd) {
        if fs_mkdir(&p) < 0 { print("mkdir: cannot create '"); print_nb(&p); println("'"); return; }
        print("Created: "); print_nb(&p); println("");
    } else { println("mkdir: missing directory name"); }
}

pub fn touch(cmd: &Cmd) {
    if let Some(p) = path_arg(cmd) {
        let fd = file_open(&p, O_CREAT | O_WRONLY);
        if fd < 0 { print("touch: cannot create '"); print_nb(&p); println("'"); return; }
        fs_close(fd); print("Created: "); print_nb(&p); println("");
    } else { println("touch: missing file name"); }
}

pub fn del(cmd: &Cmd) {
    if let Some(p) = path_arg(cmd) {
        // try unlink first, then rmdir
        if fs_unlink(&p) < 0 && fs_rmdir(&p) < 0 {
            print("del: cannot remove '"); print_nb(&p); println("'");
            return;
        }
        print("Removed: "); print_nb(&p); println("");
    } else { println("del: missing path"); }
}

pub fn cp(cmd: &Cmd) {
    if cmd.n < 3 { println("cp: usage: cp <src> <dst>"); return; }
    let src = path_arg(cmd).unwrap_or([0u8; 256]);
    let dst = as_str(cmd.get(2));
    let mut dst_buf = [0u8; 256]; let dbl = core::cmp::min(dst.len(), 255);
    dst_buf[..dbl].copy_from_slice(&dst.as_bytes()[..dbl]); dst_buf[dbl] = 0;

    let fd_src = file_open(&src, O_RDONLY);
    if fd_src < 0 { print("cp: '"); print_nb(&src); println("' not found"); return; }

    let fd_dst = file_open(&dst_buf, O_CREAT | O_WRONLY | O_TRUNC);
    if fd_dst < 0 { print("cp: cannot create '"); print(dst); println("'"); fs_close(fd_src); return; }

    let mut buf = [0u8; 512];
    let mut total = 0u64;
    loop {
        let n = fs_read(fd_src, &mut buf);
        if n <= 0 { break; }
        fs_write(fd_dst, &buf[..n as usize]);
        total += n as u64;
    }
    fs_close(fd_src); fs_close(fd_dst);
    print("Copied "); print_dec(total as i64); println(" bytes");
}

pub fn mv(cmd: &Cmd) {
    if cmd.n < 3 { println("mv: usage: mv <src> <dst>"); return; }
    let src = path_arg(cmd).unwrap_or([0u8; 256]);
    let dst = as_str(cmd.get(2));
    let mut dst_buf = [0u8; 256]; let dbl = core::cmp::min(dst.len(), 255);
    dst_buf[..dbl].copy_from_slice(&dst.as_bytes()[..dbl]); dst_buf[dbl] = 0;

    if fs_rename(&src, &dst_buf) < 0 {
        print("mv: cannot rename '"); print_nb(&src); println("'");
    } else {
        print("Moved: "); print_nb(&src); print(" -> "); println(dst);
    }
}

pub fn save(cmd: &Cmd) {
    if cmd.n < 3 { println("save: usage: save <file> <text>"); return; }
    let path = as_str(cmd.get(1)); let text_str = as_str(cmd.get(2));
    let mut p = [0u8; 256]; let pb = path.as_bytes(); let len = core::cmp::min(pb.len(), 255);
    p[..len].copy_from_slice(&pb[..len]); p[len] = 0;
    let fd = file_open(&p, O_CREAT | O_WRONLY | O_TRUNC);
    if fd < 0 { print("save: cannot open '"); print(path); println("'"); return; }
    let n = fs_write(fd, text_str.as_bytes()); fs_close(fd);
    print("Wrote "); print_dec(n as i64); println(" bytes");
}
