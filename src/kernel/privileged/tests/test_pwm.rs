use super::check;
use crate::privileged::sgeg::capability;
use crate::privileged::sgeg::engine;
use crate::privileged::sgeg::types::{CapBits, CapDomain, GrantRecord, PwmEntry, PwmFlags, PwmId};
use crate::privileged::tests::{TestResult, runner};
use crate::register_tests_inner;

// UT-07 (2026-09-26): pwm::sha256 / pwm::types / pwm::audit 三组注册副本已删 —
// 纯算法与纯类型断言分别以 privileged/sgeg/sha256.rs 与
// privileged/sgeg/types.rs 的 #[cfg(test)] 为唯一归属.

fn test_pwmentry_caps() -> TestResult {
    let entry = PwmEntry::new();
    check!(!entry.is_valid(), "new entry should not be valid");

    entry.pwm.store(123, core::sync::atomic::Ordering::Release);
    check!(entry.is_valid(), "entry with pwm should be valid");
    check!(entry.get_pwm() == PwmId(123), "get_pwm mismatch");

    let fs_caps = entry.load_caps(CapDomain::FS);
    check!(fs_caps == CapBits::NONE, "new entry fs caps should be NONE");

    entry.fetch_or_caps(
        CapDomain::FS,
        CapBits(capability::FS_CAP_READ | capability::FS_CAP_WRITE),
    );
    let after = entry.load_caps(CapDomain::FS);
    check!(
        after.contains(CapBits(capability::FS_CAP_READ)),
        "should have FS_READ"
    );
    check!(
        after.contains(CapBits(capability::FS_CAP_WRITE)),
        "should have FS_WRITE"
    );
    check!(
        !after.contains(CapBits(capability::FS_CAP_EXECUTE)),
        "should not have FS_EXEC"
    );

    check!(
        entry.has_capability(CapDomain::FS, CapBits(capability::FS_CAP_READ)),
        "has_capability should be true"
    );
    check!(
        !entry.has_capability(CapDomain::FS, CapBits(capability::FS_CAP_DELETE)),
        "has_capability DELETE should be false"
    );
    TestResult::Pass
}

fn test_pwmentry_flags() -> TestResult {
    let entry = PwmEntry::new();
    check!(
        !entry.has_flag(PwmFlags::DISABLED),
        "new entry should not be disabled"
    );

    // 2026-06-29 修复: 必须设置非零 pwm 才能测试 DISABLED 标志.
    // engine::check(0, ...) 走 bootstrap 路径直接返回 true, 绕过 flag 检查.
    entry.pwm.store(124, core::sync::atomic::Ordering::Release);
    check!(entry.is_valid(), "entry with pwm=124 should be valid");

    entry.add_flags(PwmFlags::DISABLED);
    check!(
        entry.has_flag(PwmFlags::DISABLED),
        "should be disabled after add"
    );

    check!(
        !engine::check(
            entry.pwm.load(core::sync::atomic::Ordering::Acquire),
            CapDomain::FS,
            CapBits::ALL
        ),
        "disabled entry should fail check"
    );

    entry.remove_flags(PwmFlags::DISABLED);
    check!(
        !entry.has_flag(PwmFlags::DISABLED),
        "should not be disabled after remove"
    );
    TestResult::Pass
}

fn test_grant_record() -> TestResult {
    let rec = GrantRecord {
        grantor_pwm: PwmId(1),
        grantee_pwm: PwmId(2),
        domain: CapDomain::FS,
        caps: CapBits(capability::FS_CAP_READ),
        granted_at: 100,
    };
    check!(!rec.is_empty(), "filled record should not be empty");

    let empty = GrantRecord::EMPTY;
    check!(empty.is_empty(), "EMPTY record should be empty");
    check!(
        empty.grantor_pwm == PwmId::ZERO,
        "EMPTY grantor should be ZERO"
    );
    TestResult::Pass
}

fn test_pwmentry_note() -> TestResult {
    // T4-1: 全 Atomic 化后 set_note 接受 &self, 验证用 note_equals
    let entry = PwmEntry::new();
    entry.set_note("test-identity");
    check!(entry.note_equals("test-identity"), "note mismatch");
    check!(!entry.note_equals("other"), "note should not match other");
    TestResult::Pass
}

fn test_viable_floor() -> TestResult {
    check!(
        capability::VIABLE_FLOOR[CapDomain::FS.as_usize()] != 0,
        "FS viable floor should be non-zero"
    );
    check!(
        capability::VIABLE_FLOOR[CapDomain::PROC.as_usize()] != 0,
        "PROC viable floor should be non-zero"
    );
    check!(
        capability::VIABLE_FLOOR[CapDomain::SYSTEM.as_usize()] == 0,
        "SYSTEM viable floor should be zero"
    );
    TestResult::Pass
}

#[cfg(target_arch = "x86_64")]
fn test_pwmentry_cow_bp() -> TestResult {
    use crate::functions::fs::unkfs::bp::UnkfsBlockPointer;
    use crate::functions::fs::unkfs::dmu::UnkfsDmuObject;

    let mut obj = UnkfsDmuObject::new_file(1, 0);
    let bp = UnkfsBlockPointer::null();
    obj.cow_bp(bp, 5);
    check!(obj.birth_txg == 5, "birth txg should be 5 after cow_bp");
    TestResult::Pass
}

pub fn register_pwm_tests() {
    let r = runner();
    register_tests_inner! { r:
        "pwm::entry": {
            "caps": test_pwmentry_caps,
            "flags": test_pwmentry_flags,
            "note": test_pwmentry_note,
        },
        "pwm::grant_record": {
            "basic": test_grant_record,
        },
        "pwm::capability": {
            "viable_floor": test_viable_floor,
        },
    }

    #[cfg(target_arch = "x86_64")]
    r.register("pwm::dmu", "cow_bp", test_pwmentry_cow_bp);
}
