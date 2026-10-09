#![no_std]
#![no_main]

use userlib::sys::*;
use userlib::*;

const BUF_SIZE: usize = 2048;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    print("[httpsrv] PANIC: ");
    if let Some(loc) = info.location() {
        print(loc.file());
        print(":");
        print_dec(loc.line() as i64);
    }
    print("\n");
    proc_exit(1);
}

fn u16_to_be(val: u16) -> u16 {
    ((val & 0xFF) << 8) | ((val >> 8) & 0xFF)
}

fn make_addr(port: u16) -> SockaddrIn {
    SockaddrIn {
        sin_family: AF_INET as u16,
        sin_port: u16_to_be(port),
        sin_addr: InAddr { s_addr: [0u8; 4] },
        sin_zero: [0u8; 8],
    }
}

fn read_request(fd: i32, buf: &mut [u8]) -> Option<usize> {
    let n = recv(fd, buf.as_mut_ptr(), buf.len(), 0);
    if n <= 0 {
        return None;
    }
    Some(n as usize)
}

fn send_response(fd: i32, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    send(fd, data.as_ptr(), data.len(), 0);
}

fn write_header_and_body(
    resp: &mut [u8],
    pos: &mut usize,
    status: u16,
    content_type: &[u8],
    body: &[u8],
) {
    let status_line: &[u8] = match status {
        200 => b"HTTP/1.0 200 OK\r\n",
        404 => b"HTTP/1.0 404 Not Found\r\n",
        _ => b"HTTP/1.0 500 Internal Server Error\r\n",
    };
    for &b in status_line {
        if *pos < BUF_SIZE {
            resp[*pos] = b;
            *pos += 1;
        }
    }

    let server = b"Server: Edgine-httpsrv/0.1\r\nContent-Type: ";
    for &b in server {
        if *pos < BUF_SIZE {
            resp[*pos] = b;
            *pos += 1;
        }
    }
    for &b in content_type {
        if *pos < BUF_SIZE {
            resp[*pos] = b;
            *pos += 1;
        }
    }
    for &b in b"\r\nConnection: close\r\n" {
        if *pos < BUF_SIZE {
            resp[*pos] = b;
            *pos += 1;
        }
    }

    let bl = body.len();
    let cl = b"Content-Length: ";
    for &b in cl {
        if *pos < BUF_SIZE {
            resp[*pos] = b;
            *pos += 1;
        }
    }
    write_usize(resp, pos, bl);
    for &b in b"\r\n\r\n" {
        if *pos < BUF_SIZE {
            resp[*pos] = b;
            *pos += 1;
        }
    }

    for &b in body {
        if *pos < BUF_SIZE {
            resp[*pos] = b;
            *pos += 1;
        }
    }
}

fn write_usize(buf: &mut [u8], pos: &mut usize, n: usize) {
    let mut num = n;
    let mut tmp = [0u8; 16];
    let mut ti = 0;
    if num == 0 {
        tmp[0] = b'0';
        ti = 1;
    } else {
        while num > 0 {
            tmp[ti] = b'0' + (num % 10) as u8;
            num /= 10;
            ti += 1;
        }
    }
    let mut k = ti;
    while k > 0 {
        k -= 1;
        if *pos < BUF_SIZE {
            buf[*pos] = tmp[k];
            *pos += 1;
        }
    }
}

fn starts_with(buf: &[u8], prefix: &[u8]) -> bool {
    buf.len() >= prefix.len() && &buf[..prefix.len()] == prefix
}

fn extract_path(req: &[u8]) -> &[u8] {
    let n = req.len();
    if n < 5 || !starts_with(req, b"GET ") {
        return b"/";
    }
    let start = 4;
    let mut end = start;
    while end < n && req[end] != b' ' && req[end] != b'\r' && req[end] != b'\n' {
        end += 1;
    }
    if end <= start {
        return b"/";
    }
    &req[start..end]
}

fn handle_client(fd: i32, resp: &mut [u8]) {
    let mut req_buf = [0u8; 1024];
    let n = match read_request(fd, &mut req_buf) {
        Some(n) => n,
        None => {
            close_socket(fd);
            return;
        }
    };

    let path = extract_path(&req_buf[..n]);
    print("[httpsrv] GET ");

    fn print_path(b: &[u8]) {
        for &c in b {
            if (0x20..=0x7F).contains(&c) {
                print_char(c);
            }
        }
    }
    print_path(path);
    print("\n");

    let mut pos: usize = 0;

    if path == b"/" {
        write_header_and_body(
            resp,
            &mut pos,
            200,
            b"text/html; charset=utf-8",
            b"<!DOCTYPE html>
<html><head><title>Edgine HTTP Server</title></head>
<body>
<h1>Edgine HTTP Server</h1>
<p>Welcome! This is a minimal HTTP server running on the Edgine kernel.</p>
<ul>
<li><a href=\"/about\">About</a></li>
</ul>
</body></html>",
        );
    } else if starts_with(path, b"/about") {
        write_header_and_body(
            resp,
            &mut pos,
            200,
            b"text/html; charset=utf-8",
            b"<!DOCTYPE html>
<html><head><title>About - Edgine</title></head>
<body>
<h1>About Edgine</h1>
<p>Edgine is a microkernel operating system written in Rust and C.</p>
<p>This HTTP server is a demonstration of the networking stack built on smoltcp.</p>
<p><a href=\"/\">Back</a></p>
</body></html>",
        );
    } else {
        write_header_and_body(
            resp,
            &mut pos,
            404,
            b"text/html; charset=utf-8",
            b"<!DOCTYPE html>
<html><head><title>404 - Edgine</title></head>
<body><h1>404 Not Found</h1>
<p>The requested resource was not found.</p>
<p><a href=\"/\">Back to home</a></p>
</body></html>",
        );
    }

    send_response(fd, &resp[..pos]);
    close_socket(fd);
}

#[unsafe(no_mangle)]
pub fn _start() -> ! {
    print("[httpsrv] Starting HTTP server on port 80...\n");

    let sockfd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if sockfd < 0 {
        print("[httpsrv] socket() failed: ");
        print_dec(sockfd as i64);
        print("\n");
        proc_exit(1);
    }

    {
        let opt: i32 = 1;
        setsockopt(
            sockfd,
            SOL_SOCKET,
            SO_REUSEADDR,
            &opt as *const i32 as *const u8,
            4,
        );
    }

    let addr = make_addr(80);
    let bind_ret = bind(
        sockfd,
        &addr as *const SockaddrIn as *const u8,
        core::mem::size_of::<SockaddrIn>() as u32,
    );
    if bind_ret < 0 {
        print("[httpsrv] bind() failed: ");
        print_dec(bind_ret as i64);
        print("\n");
        close_socket(sockfd);
        proc_exit(1);
    }

    let listen_ret = listen(sockfd, 5);
    if listen_ret < 0 {
        print("[httpsrv] listen() failed: ");
        print_dec(listen_ret as i64);
        print("\n");
        close_socket(sockfd);
        proc_exit(1);
    }

    print("[httpsrv] Listening on 0.0.0.0:80\n");

    let mut resp_buf = [0u8; BUF_SIZE];

    loop {
        let client_fd = accept(sockfd, core::ptr::null_mut(), core::ptr::null_mut());
        if client_fd < 0 {
            print("[httpsrv] accept() failed: ");
            print_dec(client_fd as i64);
            print("\n");
            continue;
        }
        print("[httpsrv] Connection accepted\n");
        handle_client(client_fd, &mut resp_buf);
    }
}
