//! 根身份创建 — 交互式密码输入

use userlib::sys;
use userlib::{cmp, print, print_dec, println, read_line};

const MIN_PASSWORD_LEN: usize = 4;

pub fn create() -> i32 {
    println("");
    println("--- Step 4: Administrator PWID Setup ---");
    println("");
    println("Creating the first administrator identity.");
    println("This identity will have full system access.");
    println("");
    loop {
        print("Enter root password (min 4 chars): ");
        let mut pw1 = [0u8; 64];
        let len1 = read_line(&mut pw1);
        if len1 < MIN_PASSWORD_LEN {
            print("Password too short! Minimum ");
            print_dec(MIN_PASSWORD_LEN as i64);
            println(" characters required.");
            continue;
        }
        print("Confirm root password: ");
        let mut pw2 = [0u8; 64];
        let len2 = read_line(&mut pw2);
        if len1 != len2 || cmp(&pw1[..len1], &pw2[..len2]) != 0 {
            println("Passwords do not match! Please try again.");
            continue;
        }
        println("");
        println("Creating root identity...");
        let mut p = [0u8; 65];
        p[..len1].copy_from_slice(&pw1[..len1]);
        p[len1] = 0;
        let r = sys::auth_create_first(&p[..len1 + 1]);
        if r >= 0 {
            println("Root identity created successfully!");
            return 0;
        } else {
            print("Failed to create root identity (error: ");
            print_dec(r as i64);
            println("). Please try again.");
        }
    }
}
