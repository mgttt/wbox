//! ALU 与移位/位运算，含精确的标志位语义。
//!
//! 全部走 u128 中间结果，这样 1/2/4/8 字节四种宽度用同一份代码算 CF，
//! 不需要为 8 字节单独处理 u64 溢出。

use crate::cpu::{parity, sign_bit, trunc, Flags};

#[inline]
pub fn mask_of(size: u8) -> u64 {
    match size {
        1 => 0xff,
        2 => 0xffff,
        4 => 0xffff_ffff,
        _ => u64::MAX,
    }
}

/// 设置 ZF/SF/PF —— 所有逻辑与算术指令共用的部分。
#[inline]
fn set_zsp(f: &mut Flags, r: u64, size: u8) {
    f.zf = r == 0;
    f.sf = r & sign_bit(size) != 0;
    f.pf = parity(r);
}

/// ADD / ADC（`carry` 为进位输入）。
pub fn add(a: u64, b: u64, size: u8, carry: bool, f: &mut Flags) -> u64 {
    let x = trunc(a, size) as u128;
    let y = trunc(b, size) as u128;
    let s = x + y + carry as u128;
    let mask = mask_of(size) as u128;
    let r = (s & mask) as u64;
    f.cf = s > mask;
    // OF：两个操作数同号而结果异号
    f.of = (!(a ^ b) & (a ^ r) & sign_bit(size)) != 0;
    f.af = ((a ^ b ^ r) & 0x10) != 0;
    set_zsp(f, r, size);
    r
}

/// SUB / SBB / CMP（`borrow` 为借位输入）。
pub fn sub(a: u64, b: u64, size: u8, borrow: bool, f: &mut Flags) -> u64 {
    let x = trunc(a, size) as u128;
    let y = trunc(b, size) as u128 + borrow as u128;
    let mask = mask_of(size) as u128;
    let r = ((x.wrapping_sub(y)) & mask) as u64;
    f.cf = x < y;
    // OF：操作数异号且结果与被减数异号
    f.of = ((a ^ b) & (a ^ r) & sign_bit(size)) != 0;
    f.af = ((a ^ b ^ r) & 0x10) != 0;
    set_zsp(f, r, size);
    r
}

/// 逻辑运算共用尾部：CF/OF/AF 清零。
#[inline]
fn logic_flags(f: &mut Flags, r: u64, size: u8) -> u64 {
    f.cf = false;
    f.of = false;
    f.af = false;
    set_zsp(f, r, size);
    r
}

pub fn and(a: u64, b: u64, size: u8, f: &mut Flags) -> u64 {
    logic_flags(f, trunc(a & b, size), size)
}

pub fn or(a: u64, b: u64, size: u8, f: &mut Flags) -> u64 {
    logic_flags(f, trunc(a | b, size), size)
}

pub fn xor(a: u64, b: u64, size: u8, f: &mut Flags) -> u64 {
    logic_flags(f, trunc(a ^ b, size), size)
}

/// INC / DEC：与 ADD/SUB 1 相同，但**不动 CF**。
pub fn inc(a: u64, size: u8, f: &mut Flags) -> u64 {
    let cf = f.cf;
    let r = add(a, 1, size, false, f);
    f.cf = cf;
    r
}

pub fn dec(a: u64, size: u8, f: &mut Flags) -> u64 {
    let cf = f.cf;
    let r = sub(a, 1, size, false, f);
    f.cf = cf;
    r
}

/// NEG：等价于 0 - a，CF = (a != 0)。
pub fn neg(a: u64, size: u8, f: &mut Flags) -> u64 {
    let r = sub(0, a, size, false, f);
    f.cf = trunc(a, size) != 0;
    r
}

// ---------------------------------------------------------------- 移位

/// 移位计数按 x86 规则掩码：64 位用低 6 位，其余用低 5 位。
#[inline]
pub fn shift_count(cnt: u64, size: u8) -> u32 {
    (if size == 8 { cnt & 0x3f } else { cnt & 0x1f }) as u32
}

pub fn shl(a: u64, cnt: u64, size: u8, f: &mut Flags) -> u64 {
    let n = shift_count(cnt, size);
    if n == 0 {
        return trunc(a, size); // 计数为 0 时标志位完全不变
    }
    let bits = size as u32 * 8;
    let v = trunc(a, size);
    let r = trunc(if n >= bits { 0 } else { v << n }, size);
    // CF = 最后移出的位
    f.cf = if n <= bits {
        (v >> (bits - n)) & 1 != 0
    } else {
        false
    };
    // OF 只在移 1 位时有定义：CF 与结果符号位不同则置位
    f.of = (r & sign_bit(size) != 0) != f.cf;
    f.af = false;
    set_zsp(f, r, size);
    r
}

pub fn shr(a: u64, cnt: u64, size: u8, f: &mut Flags) -> u64 {
    let n = shift_count(cnt, size);
    if n == 0 {
        return trunc(a, size);
    }
    let bits = size as u32 * 8;
    let v = trunc(a, size);
    let r = if n >= bits { 0 } else { v >> n };
    f.cf = if n <= bits {
        (v >> (n - 1)) & 1 != 0
    } else {
        false
    };
    // SHR 的 OF = 原操作数的符号位
    f.of = v & sign_bit(size) != 0;
    f.af = false;
    set_zsp(f, r, size);
    r
}

pub fn sar(a: u64, cnt: u64, size: u8, f: &mut Flags) -> u64 {
    let n = shift_count(cnt, size);
    if n == 0 {
        return trunc(a, size);
    }
    let bits = size as u32 * 8;
    let v = crate::cpu::sext(a, size) as i64;
    // 移满或移过宽度时结果是符号位的复制（算术右移饱和）。
    let r = trunc((v >> n.min(bits - 1)) as u64, size);
    // CF = 最后移出的那一位；移过宽度时所有位都移出了，剩下的就是符号位。
    f.cf = if n >= bits {
        v < 0
    } else {
        (v >> (n - 1)) & 1 != 0
    };
    f.of = false; // SAR 的 OF 恒清
    f.af = false;
    set_zsp(f, r, size);
    r
}

pub fn rol(a: u64, cnt: u64, size: u8, f: &mut Flags) -> u64 {
    let bits = size as u32 * 8;
    let n = shift_count(cnt, size) % bits;
    let v = trunc(a, size);
    if n == 0 {
        return v;
    }
    let r = trunc((v << n) | (v >> (bits - n)), size);
    f.cf = r & 1 != 0;
    f.of = f.cf != (r & sign_bit(size) != 0);
    r
}

pub fn ror(a: u64, cnt: u64, size: u8, f: &mut Flags) -> u64 {
    let bits = size as u32 * 8;
    let n = shift_count(cnt, size) % bits;
    let v = trunc(a, size);
    if n == 0 {
        return v;
    }
    let r = trunc((v >> n) | (v << (bits - n)), size);
    f.cf = r & sign_bit(size) != 0;
    // OF = 结果最高两位异或
    let msb = r & sign_bit(size) != 0;
    let msb2 = r & (sign_bit(size) >> 1) != 0;
    f.of = msb != msb2;
    r
}

/// RCL：带 CF 参与的循环左移（`bits+1` 位循环）。
pub fn rcl(a: u64, cnt: u64, size: u8, f: &mut Flags) -> u64 {
    let bits = size as u32 * 8;
    let n = shift_count(cnt, size) % (bits + 1);
    if n == 0 {
        return trunc(a, size);
    }
    let mut v = trunc(a, size);
    for _ in 0..n {
        let msb = v & sign_bit(size) != 0;
        v = trunc((v << 1) | f.cf as u64, size);
        f.cf = msb;
    }
    f.of = f.cf != (v & sign_bit(size) != 0);
    v
}

pub fn rcr(a: u64, cnt: u64, size: u8, f: &mut Flags) -> u64 {
    let bits = size as u32 * 8;
    let n = shift_count(cnt, size) % (bits + 1);
    if n == 0 {
        return trunc(a, size);
    }
    let mut v = trunc(a, size);
    for _ in 0..n {
        let lsb = v & 1 != 0;
        v = trunc((v >> 1) | ((f.cf as u64) << (bits - 1)), size);
        f.cf = lsb;
    }
    let msb = v & sign_bit(size) != 0;
    let msb2 = v & (sign_bit(size) >> 1) != 0;
    f.of = msb != msb2;
    v
}

/// SHLD：把 `b` 的高位移进 `a` 的低位。
pub fn shld(a: u64, b: u64, cnt: u64, size: u8, f: &mut Flags) -> u64 {
    let bits = size as u32 * 8;
    let n = shift_count(cnt, size);
    if n == 0 || n >= bits {
        return trunc(a, size);
    }
    let v = trunc(a, size);
    let r = trunc((v << n) | (trunc(b, size) >> (bits - n)), size);
    f.cf = (v >> (bits - n)) & 1 != 0;
    f.of = (r & sign_bit(size) != 0) != f.cf;
    set_zsp(f, r, size);
    r
}

/// SHRD：把 `b` 的低位移进 `a` 的高位。
pub fn shrd(a: u64, b: u64, cnt: u64, size: u8, f: &mut Flags) -> u64 {
    let bits = size as u32 * 8;
    let n = shift_count(cnt, size);
    if n == 0 || n >= bits {
        return trunc(a, size);
    }
    let v = trunc(a, size);
    let r = trunc((v >> n) | (trunc(b, size) << (bits - n)), size);
    f.cf = (v >> (n - 1)) & 1 != 0;
    f.of = (r & sign_bit(size) != 0) != (v & sign_bit(size) != 0);
    set_zsp(f, r, size);
    r
}

// ---------------------------------------------------------------- 乘除

/// 无符号乘：返回 (低半, 高半)。CF/OF = 高半非零。
pub fn mul(a: u64, b: u64, size: u8, f: &mut Flags) -> (u64, u64) {
    let x = trunc(a, size) as u128;
    let y = trunc(b, size) as u128;
    let p = x * y;
    let bits = size as u32 * 8;
    let lo = trunc(p as u64, size);
    let hi = trunc((p >> bits) as u64, size);
    f.cf = hi != 0;
    f.of = f.cf;
    set_zsp(f, lo, size);
    (lo, hi)
}

/// 有符号乘：返回 (低半, 高半)。CF/OF = 结果无法用单宽度表示。
pub fn imul(a: u64, b: u64, size: u8, f: &mut Flags) -> (u64, u64) {
    let x = crate::cpu::sext(a, size) as i64 as i128;
    let y = crate::cpu::sext(b, size) as i64 as i128;
    let p = x * y;
    let bits = size as u32 * 8;
    let lo = trunc(p as u64, size);
    let hi = trunc((p >> bits) as u64, size);
    // 低半符号扩展回来若等于完整乘积，说明没溢出
    f.of = (crate::cpu::sext(lo, size) as i64 as i128) != p;
    f.cf = f.of;
    set_zsp(f, lo, size);
    (lo, hi)
}

/// 除法结果；`None` 表示应触发 #DE（除零或商溢出）。
pub struct DivOut {
    pub quot: u64,
    pub rem: u64,
}

/// 无符号除：`hi:lo / b`。
pub fn div(hi: u64, lo: u64, b: u64, size: u8) -> Option<DivOut> {
    let d = trunc(b, size) as u128;
    if d == 0 {
        return None;
    }
    let bits = size as u32 * 8;
    let n = ((trunc(hi, size) as u128) << bits) | trunc(lo, size) as u128;
    let q = n / d;
    if q > mask_of(size) as u128 {
        return None; // 商溢出，x86 同样报 #DE
    }
    Some(DivOut {
        quot: q as u64,
        rem: (n % d) as u64,
    })
}

/// 有符号除：`hi:lo / b`，商向零取整，余数符号跟被除数。
pub fn idiv(hi: u64, lo: u64, b: u64, size: u8) -> Option<DivOut> {
    let d = crate::cpu::sext(b, size) as i64 as i128;
    if d == 0 {
        return None;
    }
    let bits = size as u32 * 8;
    let n_raw = ((trunc(hi, size) as u128) << bits) | trunc(lo, size) as u128;
    // 把 2*size 宽的被除数符号扩展成 i128
    let n = if size == 8 {
        n_raw as i128
    } else {
        let w = bits * 2;
        let sign = 1u128 << (w - 1);
        if n_raw & sign != 0 {
            (n_raw | !((1u128 << w) - 1)) as i128
        } else {
            n_raw as i128
        }
    };
    let q = n.wrapping_div(d);
    let lim_hi = (mask_of(size) >> 1) as i128; // 最大正商
    let lim_lo = -lim_hi - 1;
    if q > lim_hi || q < lim_lo {
        return None;
    }
    Some(DivOut {
        quot: trunc(q as u64, size),
        rem: trunc(n.wrapping_rem(d) as u64, size),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f() -> Flags {
        Flags::default()
    }

    #[test]
    fn add_sets_carry_and_overflow_per_width() {
        let mut fl = f();
        // 8 位：0xff + 1 = 0，CF 置，无符号回绕
        assert_eq!(add(0xff, 1, 1, false, &mut fl), 0);
        assert!(fl.cf && fl.zf && !fl.of);
        // 8 位：0x7f + 1 = 0x80，有符号溢出
        assert_eq!(add(0x7f, 1, 1, false, &mut fl), 0x80);
        assert!(fl.of && fl.sf && !fl.cf);
        // 64 位：u64::MAX + 1
        assert_eq!(add(u64::MAX, 1, 8, false, &mut fl), 0);
        assert!(fl.cf && fl.zf);
    }

    #[test]
    fn adc_includes_carry_in() {
        let mut fl = f();
        assert_eq!(add(1, 1, 4, true, &mut fl), 3);
        assert_eq!(add(0xffff_ffff, 0, 4, true, &mut fl), 0);
        assert!(fl.cf);
    }

    #[test]
    fn sub_borrow_and_overflow() {
        let mut fl = f();
        // 0 - 1 = -1，CF（借位）置
        assert_eq!(sub(0, 1, 4, false, &mut fl), 0xffff_ffff);
        assert!(fl.cf && fl.sf && !fl.zf);
        // 8 位：0x80 - 1 = 0x7f，有符号溢出
        assert_eq!(sub(0x80, 1, 1, false, &mut fl), 0x7f);
        assert!(fl.of && !fl.sf);
        // 相等 -> ZF
        assert_eq!(sub(5, 5, 4, false, &mut fl), 0);
        assert!(fl.zf && !fl.cf);
    }

    #[test]
    fn inc_dec_preserve_carry() {
        let mut fl = f();
        fl.cf = true;
        assert_eq!(inc(0xff, 1, &mut fl), 0);
        assert!(fl.cf, "INC 不得改 CF");
        fl.cf = false;
        assert_eq!(dec(0, 1, &mut fl), 0xff);
        assert!(!fl.cf, "DEC 不得改 CF");
    }

    #[test]
    fn logic_clears_carry_overflow() {
        let mut fl = f();
        fl.cf = true;
        fl.of = true;
        assert_eq!(and(0xf0, 0x3c, 1, &mut fl), 0x30);
        assert!(!fl.cf && !fl.of);
        assert_eq!(xor(0xaa, 0xaa, 1, &mut fl), 0);
        assert!(fl.zf);
        assert_eq!(or(0xf0, 0x0f, 1, &mut fl), 0xff);
        assert!(fl.sf);
    }

    #[test]
    fn neg_sets_carry_when_nonzero() {
        let mut fl = f();
        assert_eq!(neg(1, 4, &mut fl), 0xffff_ffff);
        assert!(fl.cf);
        assert_eq!(neg(0, 4, &mut fl), 0);
        assert!(!fl.cf && fl.zf);
    }

    #[test]
    fn shift_count_masks_by_width() {
        assert_eq!(shift_count(65, 8), 1, "64 位取低 6 位");
        assert_eq!(shift_count(33, 4), 1, "32 位取低 5 位");
    }

    #[test]
    fn shl_shr_sar_semantics() {
        let mut fl = f();
        assert_eq!(shl(1, 4, 4, &mut fl), 0x10);
        // 移出的位进 CF
        assert_eq!(shl(0x8000_0000, 1, 4, &mut fl), 0);
        assert!(fl.cf && fl.zf);
        assert_eq!(shr(0x10, 4, 4, &mut fl), 1);
        assert_eq!(shr(1, 1, 4, &mut fl), 0);
        assert!(fl.cf);
        // SAR 复制符号位
        assert_eq!(sar(0xffff_ffff, 4, 4, &mut fl), 0xffff_ffff);
        assert_eq!(sar(0x8000_0000, 31, 4, &mut fl), 0xffff_ffff);
        assert_eq!(sar(0x40, 2, 1, &mut fl), 0x10);
    }

    #[test]
    fn shift_by_zero_leaves_flags_untouched() {
        let mut fl = f();
        fl.cf = true;
        fl.zf = true;
        let before = fl;
        assert_eq!(shl(0, 0, 4, &mut fl), 0);
        assert_eq!(fl, before, "移位计数为 0 时标志位必须原样");
        assert_eq!(shr(0xff, 0, 4, &mut fl), 0xff);
        assert_eq!(fl, before);
    }

    #[test]
    fn rotates_roundtrip() {
        let mut fl = f();
        assert_eq!(rol(0x8000_0000, 1, 4, &mut fl), 1);
        assert!(fl.cf);
        assert_eq!(ror(1, 1, 4, &mut fl), 0x8000_0000);
        assert!(fl.cf);
        // rol 后 ror 回原值
        let v = 0x1234_5678u64;
        let a = rol(v, 7, 4, &mut fl);
        assert_eq!(ror(a, 7, 4, &mut fl), v);
    }

    #[test]
    fn rcl_rcr_use_carry_as_extra_bit() {
        let mut fl = f();
        fl.cf = true;
        // 33 位循环：CF 进最低位
        assert_eq!(rcl(0, 1, 4, &mut fl), 1);
        assert!(!fl.cf);
        fl.cf = true;
        assert_eq!(rcr(0, 1, 4, &mut fl), 0x8000_0000);
        assert!(!fl.cf);
    }

    #[test]
    fn shld_shrd_move_bits_across() {
        let mut fl = f();
        assert_eq!(shld(0x0000_0000, 0x8000_0000, 1, 4, &mut fl), 1);
        assert_eq!(shrd(0x0000_0000, 0x0000_0001, 1, 4, &mut fl), 0x8000_0000);
    }

    #[test]
    fn mul_imul_high_half_and_flags() {
        let mut fl = f();
        assert_eq!(mul(0x1_0000, 0x1_0000, 4, &mut fl), (0, 1));
        assert!(fl.cf, "高半非零 -> CF");
        assert_eq!(mul(3, 4, 4, &mut fl), (12, 0));
        assert!(!fl.cf);
        // -1 * -1 = 1
        assert_eq!(imul(u64::MAX, u64::MAX, 4, &mut fl).0, 1);
        assert!(!fl.of);
        // 8 位：0x7f * 2 = 0xfe 溢出有符号范围
        let (lo, _) = imul(0x7f, 2, 1, &mut fl);
        assert_eq!(lo, 0xfe);
        assert!(fl.of);
    }

    #[test]
    fn div_and_idiv_basics() {
        let d = div(0, 100, 7, 4).unwrap();
        assert_eq!((d.quot, d.rem), (14, 2));
        // 有符号：-100 / 7 = -14 余 -2（向零取整）
        let n = (-100i32) as u32 as u64;
        let d = idiv(0xffff_ffff, n, 7, 4).unwrap();
        assert_eq!(d.quot as u32 as i32, -14);
        assert_eq!(d.rem as u32 as i32, -2);
    }

    #[test]
    fn div_by_zero_and_quotient_overflow_are_faults() {
        assert!(div(0, 1, 0, 4).is_none(), "除零");
        // hi >= divisor 时商必然溢出
        assert!(div(1, 0, 1, 4).is_none(), "商溢出");
        assert!(idiv(0, 1, 0, 4).is_none());
    }
}
