#![no_std]

pub mod fs;
pub mod io;
pub mod str;
pub mod sys;

pub use fs::{file_copy, file_open};
pub use io::{print, print_char, print_dec, print_hex, println, read_line};
pub use str::{cmp, parse_args};

pub fn delay_loop(count: u64) {
    for _ in 0..count {
        core::hint::spin_loop();
    }
}
