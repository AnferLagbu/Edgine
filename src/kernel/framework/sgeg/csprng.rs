use super::types::PWM_SALT_LEN;
#[cfg(target_arch = "x86_64")]
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;

#[cfg(target_arch = "x86_64")]
static RDRAND_AVAILABLE: AtomicBool = AtomicBool::new(false);
#[cfg(target_arch = "x86_64")]
static RDRAND_CHECKED: AtomicBool = AtomicBool::new(false);

#[cfg(target_arch = "x86_64")]
fn check_rdrand() -> bool {
    if RDRAND_CHECKED.load(Ordering::Relaxed) {
        return RDRAND_AVAILABLE.load(Ordering::Relaxed);
    }
    let (_, _, ecx, _) = crate::framework::cpu::cpuid::cpuid(1, 0);
    let available = ecx & (1 << 30) != 0;
    RDRAND_AVAILABLE.store(available, Ordering::Relaxed);
    RDRAND_CHECKED.store(true, Ordering::Relaxed);
    available
}

#[cfg(not(target_arch = "x86_64"))]
fn check_rdrand() -> bool {
    // AArch64 等架构暂不使用硬件 RNG，统一走 fallback
    false
}

#[cfg(target_arch = "x86_64")]
fn rdrand_u64() -> Option<u64> {
    // SAFETY: rdrand 是原子指令, options(nomem, nostack) 不污染内存;
    // 调用方契约: 硬件支持 rdrand (由 check_rdrand 保证)。
    unsafe {
        let mut ret: u64;
        let mut ok: u8;
        core::arch::asm!(
            "rdrand {}",
            "setc {}",
            lateout(reg) ret,
            lateout(reg_byte) ok,
            options(nomem, nostack),
        );
        if ok != 0 { Some(ret) } else { None }
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn rdrand_u64() -> Option<u64> {
    None
}

// 有意窄化: 显式收窄, 调用方保证值域
#[expect(clippy::cast_possible_truncation)]
#[expect(
    clippy::borrow_as_ptr,
    reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
)]
#[expect(
    clippy::unreadable_literal,
    reason = "unreadable_literal: 长数字常量无下划线分隔; 内核硬件常量 (MMIO 地址/位掩码) 已知精确值, 当前优先 expect"
)]
fn fallback_entropy_byte(idx: usize) -> u8 {
    use core::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0x5A3C_9E17_F2D8_4B61);
    let tsc = crate::arch!(timestamp());
    let stack_addr = &tsc as *const _ as u64;
    let counter = COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::AcqRel);
    let mut v = tsc
        .wrapping_mul(0x2545_F491_4F6C_DD1D)
        .wrapping_add(stack_addr)
        .wrapping_mul(0x1329_4A6B_3C7D_8E0F)
        .wrapping_add(counter)
        .wrapping_add(idx as u64 * 0x517CC1B727220A95);
    v = v
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(tsc.rotate_left(idx as u32));
    ((v >> 56) as u8) ^ ((v >> 40) as u8)
}

fn fill_random_bytes(buf: &mut [u8]) {
    if check_rdrand() {
        let mut i = 0;
        while i + 8 <= buf.len() {
            if let Some(val) = rdrand_u64() {
                buf[i..i + 8].copy_from_slice(&val.to_le_bytes());
                i += 8;
            } else {
                buf[i] = fallback_entropy_byte(i);
                i += 1;
            }
        }
        while i < buf.len() {
            buf[i] = fallback_entropy_byte(i);
            i += 1;
        }
    } else {
        for i in 0..buf.len() {
            buf[i] = fallback_entropy_byte(i);
        }
    }
}

pub fn generate_salt() -> [u8; PWM_SALT_LEN] {
    let mut salt = [0u8; PWM_SALT_LEN];
    fill_random_bytes(&mut salt);
    salt
}
