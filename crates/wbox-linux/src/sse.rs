//! SSE / SSE2 子集。
//!
//! 为什么模拟器一定要有这一层：现代 libc 的 `strlen`/`memchr`/`memcmp`/`memcpy`
//! 全是 SSE2 实现的，只做整数指令的模拟器连 `puts("hi")` 都跑不到底。
//! 这里覆盖的正是那些字符串函数用到的整数向量指令：
//! `movdqu/movdqa`、`pxor`、`pcmpeqb`、`pmovmskb`、`pminub`、`psubb`、
//! `por/pand/pandn`、`pslldq/psrldq`、`punpck*`、`pshufd`、`movd/movq`。
//!
//! 浮点部分也在这里：标量与打包的 add/sub/mul/div/min/max/sqrt、
//! `ucomis*`/`comis*` 比较、`cmps*` 掩码比较，以及 int↔float 各向转换。
//!
//! 两处容易踩的语义差异，实现时特意绕开了 Rust 标准库的"更合理"行为：
//!
//! - `MIN*`/`MAX*` 在任一操作数为 NaN 时返回**第二个**操作数，而
//!   `f64::min` 会挑非 NaN 的那个（见 `x86_min_f64`）；
//! - 标量形式只写 lane 0，高位字节必须保留（编译器会在高位存活跃值）。
//!
//! **x87 尚未实现**（`fld1`/`fnstcw`/…），会照实报 `Undefined`。
//! 影响面：glibc 的 `long double` 路径，例如 `seq` 与 `printf "%f"`。
//! 缺口清单见 crate 文档与 `docs/rust-rewrite.md` §4。

use crate::core::{CoreResult, CoreState};
use crate::cpu::trunc;
use crate::exec::{Dec, Rm};

/// 取 128 位操作数：xmm 寄存器或 16 字节内存。
fn read_xmm_rm(m: &mut CoreState, d: &Dec, rm: Rm) -> CoreResult<[u8; 16]> {
    match m.fixup(d, rm) {
        Rm::Reg(r) => Ok(m.cpu.xmm[r]),
        Rm::Mem(a) => {
            let mut b = [0u8; 16];
            m.mem.read(a, &mut b)?;
            Ok(b)
        }
    }
}

fn write_xmm_rm(m: &mut CoreState, d: &Dec, rm: Rm, v: [u8; 16]) -> CoreResult<()> {
    match m.fixup(d, rm) {
        Rm::Reg(r) => m.cpu.xmm[r] = v,
        Rm::Mem(a) => m.mem.write(a, &v)?,
    }
    Ok(())
}

/// 读 n 字节操作数（`movd`/`movq`/`movss`/`movsd` 的窄形式）。
fn read_narrow(m: &mut CoreState, d: &Dec, rm: Rm, n: usize) -> CoreResult<[u8; 16]> {
    let mut out = [0u8; 16];
    match m.fixup(d, rm) {
        Rm::Reg(r) => out[..n].copy_from_slice(&m.cpu.xmm[r][..n]),
        Rm::Mem(a) => m.mem.read(a, &mut out[..n])?,
    }
    Ok(out)
}

fn write_narrow(m: &mut CoreState, d: &Dec, rm: Rm, v: &[u8], n: usize) -> CoreResult<()> {
    match m.fixup(d, rm) {
        Rm::Reg(r) => {
            // 寄存器目标：窄写只改低 n 字节，高位保留
            m.cpu.xmm[r][..n].copy_from_slice(&v[..n]);
        }
        Rm::Mem(a) => m.mem.write(a, &v[..n])?,
    }
    Ok(())
}

/// 逐字节二元运算。
fn zip(a: [u8; 16], b: [u8; 16], f: impl Fn(u8, u8) -> u8) -> [u8; 16] {
    let mut o = [0u8; 16];
    for i in 0..16 {
        o[i] = f(a[i], b[i]);
    }
    o
}

/// 按 lane 宽度做二元运算（lane = 1/2/4/8 字节）。
fn lanes(a: [u8; 16], b: [u8; 16], w: usize, f: impl Fn(u64, u64) -> u64) -> [u8; 16] {
    let mut o = [0u8; 16];
    for i in (0..16).step_by(w) {
        let mut x = 0u64;
        let mut y = 0u64;
        for j in 0..w {
            x |= (a[i + j] as u64) << (j * 8);
            y |= (b[i + j] as u64) << (j * 8);
        }
        let r = f(x, y);
        for j in 0..w {
            o[i + j] = (r >> (j * 8)) as u8;
        }
    }
    o
}

/// 浮点指令的四种形式，由前缀决定：
/// 无前缀 = packed single，66 = packed double，F3 = scalar single，F2 = scalar double。
#[derive(Clone, Copy, PartialEq)]
enum FForm {
    Ps,
    Pd,
    Ss,
    Sd,
}

fn fform(d: &Dec) -> FForm {
    match (d.rep, d.o16) {
        (0xf3, _) => FForm::Ss,
        (0xf2, _) => FForm::Sd,
        (_, true) => FForm::Pd,
        _ => FForm::Ps,
    }
}

fn get_f32(x: &[u8; 16], i: usize) -> f32 {
    f32::from_le_bytes(x[i * 4..i * 4 + 4].try_into().unwrap())
}
fn set_f32(x: &mut [u8; 16], i: usize, v: f32) {
    x[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
}
fn get_f64(x: &[u8; 16], i: usize) -> f64 {
    f64::from_le_bytes(x[i * 8..i * 8 + 8].try_into().unwrap())
}
fn set_f64(x: &mut [u8; 16], i: usize, v: f64) {
    x[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
}

/// 对 dst/src 施加一个浮点二元运算，按 `form` 决定作用的 lane。
///
/// 标量形式（Ss/Sd）**只写 lane 0，其余字节保留**——这是 x86 的规定，
/// 也是很多编译器生成代码的隐含前提（高位常存着别的活跃值）。
fn fbinop(
    dst: [u8; 16],
    src: [u8; 16],
    form: FForm,
    f32op: impl Fn(f32, f32) -> f32,
    f64op: impl Fn(f64, f64) -> f64,
) -> [u8; 16] {
    let mut o = dst;
    match form {
        FForm::Ss => set_f32(&mut o, 0, f32op(get_f32(&dst, 0), get_f32(&src, 0))),
        FForm::Sd => set_f64(&mut o, 0, f64op(get_f64(&dst, 0), get_f64(&src, 0))),
        FForm::Ps => {
            for i in 0..4 {
                set_f32(&mut o, i, f32op(get_f32(&dst, i), get_f32(&src, i)));
            }
        }
        FForm::Pd => {
            for i in 0..2 {
                set_f64(&mut o, i, f64op(get_f64(&dst, i), get_f64(&src, i)));
            }
        }
    }
    o
}

/// 执行一条 SSE 指令（`op` 是 0x0f 之后的字节）。
/// 认不出来就返回 `Undefined`，由上层报错。
pub fn exec(m: &mut CoreState, d: &mut Dec, op: u8) -> CoreResult<()> {
    match op {
        // ---- 浮点算术：add/mul/sub/min/div/max/sqrt
        0x58 | 0x59 | 0x5c | 0x5d | 0x5e | 0x5f => {
            let (reg, rm) = m.modrm(d)?;
            let form = fform(d);
            // 标量形式的内存操作数只有 4/8 字节，不能按 16 字节读，
            // 否则跨过映射边界时会误报 fault。
            let src = match form {
                FForm::Ss => read_narrow(m, d, rm, 4)?,
                FForm::Sd => read_narrow(m, d, rm, 8)?,
                _ => read_xmm_rm(m, d, rm)?,
            };
            let dst = m.cpu.xmm[reg];
            m.cpu.xmm[reg] = match op {
                0x58 => fbinop(dst, src, form, |a, b| a + b, |a, b| a + b),
                0x59 => fbinop(dst, src, form, |a, b| a * b, |a, b| a * b),
                0x5c => fbinop(dst, src, form, |a, b| a - b, |a, b| a - b),
                // MINPS/MAXPS 的 x86 语义：任一操作数是 NaN 就返回**第二个**
                // 操作数（src）。Rust 的 f32::min 会返回非 NaN 的那个，语义不同。
                0x5d => fbinop(dst, src, form, x86_min_f32, x86_min_f64),
                0x5e => fbinop(dst, src, form, |a, b| a / b, |a, b| a / b),
                _ => fbinop(dst, src, form, x86_max_f32, x86_max_f64),
            };
            m.next(d)
        }

        // sqrt
        0x51 => {
            let (reg, rm) = m.modrm(d)?;
            let form = fform(d);
            let src = match form {
                FForm::Ss => read_narrow(m, d, rm, 4)?,
                FForm::Sd => read_narrow(m, d, rm, 8)?,
                _ => read_xmm_rm(m, d, rm)?,
            };
            let dst = m.cpu.xmm[reg];
            // sqrt 是一元运算，借 fbinop 的 lane 分派：忽略第一个操作数
            m.cpu.xmm[reg] = fbinop(dst, src, form, |_, b| b.sqrt(), |_, b| b.sqrt());
            m.next(d)
        }

        // cvtsi2ss / cvtsi2sd：整数 -> 浮点（REX.W 决定源是 32 还是 64 位）
        0x2a => {
            let (reg, rm) = m.modrm(d)?;
            let size = if d.rex_w() { 8 } else { 4 };
            let raw = m.rm_read(d, rm, size)?;
            let v = crate::cpu::sext(raw, size) as i64;
            let mut o = m.cpu.xmm[reg];
            match d.rep {
                0xf3 => set_f32(&mut o, 0, v as f32),
                0xf2 => set_f64(&mut o, 0, v as f64),
                _ => return Err(m.undef(d)), // 无前缀/66 是 MMX 版，未实现
            }
            m.cpu.xmm[reg] = o;
            m.next(d)
        }

        // cvttss2si / cvttsd2si（截断）与 cvtss2si / cvtsd2si（按当前舍入模式）
        0x2c | 0x2d => {
            let (reg, rm) = m.modrm(d)?;
            let size = if d.rex_w() { 8 } else { 4 };
            let src = match d.rep {
                0xf3 => read_narrow(m, d, rm, 4)?,
                0xf2 => read_narrow(m, d, rm, 8)?,
                _ => return Err(m.undef(d)),
            };
            let f = if d.rep == 0xf3 {
                get_f32(&src, 0) as f64
            } else {
                get_f64(&src, 0)
            };
            // 0x2c 截断；0x2d 舍入到最近偶数（默认舍入模式，我们不建模 MXCSR）
            let r = if op == 0x2c {
                f.trunc()
            } else {
                round_half_even(f)
            };
            // 超范围/NaN 时 x86 返回"整数不定值"= 目标宽度的最小负数
            let out = if r.is_nan() {
                min_int(size)
            } else if size == 8 {
                if !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&r) {
                    min_int(8)
                } else {
                    r as i64 as u64
                }
            } else if !(-2147483648.0..2147483648.0).contains(&r) {
                min_int(4)
            } else {
                (r as i32) as u32 as u64
            };
            m.cpu.set_reg(reg, size, out, false);
            m.next(d)
        }

        // ucomiss/ucomisd（0x2e）与 comiss/comisd（0x2f）：比较并写标志
        0x2e | 0x2f => {
            let (reg, rm) = m.modrm(d)?;
            let n = if d.o16 { 8 } else { 4 };
            let src = read_narrow(m, d, rm, n)?;
            let dst = m.cpu.xmm[reg];
            let (a, b) = if d.o16 {
                (get_f64(&dst, 0), get_f64(&src, 0))
            } else {
                (get_f32(&dst, 0) as f64, get_f32(&src, 0) as f64)
            };
            let f = &mut m.cpu.flags;
            // 浮点比较写的是 ZF/PF/CF；OF/SF/AF 恒清。
            // 无序（有 NaN）时三者全置——这就是 `jp` 能检测 NaN 的原因。
            f.of = false;
            f.sf = false;
            f.af = false;
            if a.is_nan() || b.is_nan() {
                f.zf = true;
                f.pf = true;
                f.cf = true;
            } else {
                f.pf = false;
                f.zf = a == b;
                f.cf = a < b;
            }
            m.next(d)
        }

        // cvtss2sd / cvtsd2ss / cvtps2pd / cvtpd2ps
        0x5a => {
            let (reg, rm) = m.modrm(d)?;
            let mut o = m.cpu.xmm[reg];
            match (d.rep, d.o16) {
                (0xf3, _) => {
                    // single -> double（标量）
                    let src = read_narrow(m, d, rm, 4)?;
                    set_f64(&mut o, 0, get_f32(&src, 0) as f64);
                }
                (0xf2, _) => {
                    // double -> single（标量）
                    let src = read_narrow(m, d, rm, 8)?;
                    set_f32(&mut o, 0, get_f64(&src, 0) as f32);
                }
                (_, true) => {
                    // cvtpd2ps：两个 double -> 低两个 single，高 64 位清零
                    let src = read_xmm_rm(m, d, rm)?;
                    o = [0u8; 16];
                    for i in 0..2 {
                        set_f32(&mut o, i, get_f64(&src, i) as f32);
                    }
                }
                _ => {
                    // cvtps2pd：低两个 single -> 两个 double
                    let src = read_narrow(m, d, rm, 8)?;
                    for i in 0..2 {
                        let v = get_f32(&src, i) as f64;
                        set_f64(&mut o, i, v);
                    }
                }
            }
            m.cpu.xmm[reg] = o;
            m.next(d)
        }

        // cvtdq2ps（无前缀）/ cvtps2dq（66）/ cvttps2dq（F3）
        0x5b => {
            let (reg, rm) = m.modrm(d)?;
            let src = read_xmm_rm(m, d, rm)?;
            let mut o = [0u8; 16];
            if d.rep == 0 && !d.o16 {
                for i in 0..4 {
                    let iv = i32::from_le_bytes(src[i * 4..i * 4 + 4].try_into().unwrap());
                    set_f32(&mut o, i, iv as f32);
                }
            } else {
                let trunc_mode = d.rep == 0xf3;
                for i in 0..4 {
                    let f = get_f32(&src, i) as f64;
                    let r = if trunc_mode {
                        f.trunc()
                    } else {
                        round_half_even(f)
                    };
                    let iv = if r.is_nan() || !(-2147483648.0..2147483648.0).contains(&r) {
                        i32::MIN
                    } else {
                        r as i32
                    };
                    o[i * 4..i * 4 + 4].copy_from_slice(&iv.to_le_bytes());
                }
            }
            m.cpu.xmm[reg] = o;
            m.next(d)
        }

        // cvtdq2pd（F3）/ cvtpd2dq（F2）
        0xe6 => {
            let (reg, rm) = m.modrm(d)?;
            let mut o = [0u8; 16];
            if d.rep == 0xf3 {
                let src = read_narrow(m, d, rm, 8)?;
                for i in 0..2 {
                    let iv = i32::from_le_bytes(src[i * 4..i * 4 + 4].try_into().unwrap());
                    set_f64(&mut o, i, iv as f64);
                }
            } else if d.rep == 0xf2 {
                let src = read_xmm_rm(m, d, rm)?;
                for i in 0..2 {
                    let r = get_f64(&src, i).trunc();
                    let iv = if r.is_nan() || !(-2147483648.0..2147483648.0).contains(&r) {
                        i32::MIN
                    } else {
                        r as i32
                    };
                    o[i * 4..i * 4 + 4].copy_from_slice(&iv.to_le_bytes());
                }
            } else {
                return Err(m.undef(d));
            }
            m.cpu.xmm[reg] = o;
            m.next(d)
        }

        // cmpss/cmpsd/cmpps/cmppd：按谓词比较，结果是全 1 / 全 0 掩码
        0xc2 => {
            let (reg, rm) = m.modrm(d)?;
            let form = fform(d);
            let src = match form {
                FForm::Ss => read_narrow(m, d, rm, 4)?,
                FForm::Sd => read_narrow(m, d, rm, 8)?,
                _ => read_xmm_rm(m, d, rm)?,
            };
            let imm = m.f8(d)? & 7;
            let dst = m.cpu.xmm[reg];
            let pred = |a: f64, b: f64| -> bool {
                let un = a.is_nan() || b.is_nan();
                match imm {
                    0 => a == b,        // eq（有序）
                    1 => !un && a < b,  // lt
                    2 => !un && a <= b, // le
                    3 => un,            // unord
                    4 => un || a != b,  // neq
                    5 => un || a >= b,  // nlt
                    6 => un || a > b,   // nle
                    _ => !un,           // ord
                }
            };
            let mut o = dst;
            match form {
                FForm::Ss => {
                    let t = pred(get_f32(&dst, 0) as f64, get_f32(&src, 0) as f64);
                    o[0..4].copy_from_slice(&if t { [0xffu8; 4] } else { [0u8; 4] });
                }
                FForm::Sd => {
                    let t = pred(get_f64(&dst, 0), get_f64(&src, 0));
                    o[0..8].copy_from_slice(&if t { [0xffu8; 8] } else { [0u8; 8] });
                }
                FForm::Ps => {
                    for i in 0..4 {
                        let t = pred(get_f32(&dst, i) as f64, get_f32(&src, i) as f64);
                        o[i * 4..i * 4 + 4].copy_from_slice(&if t {
                            [0xffu8; 4]
                        } else {
                            [0u8; 4]
                        });
                    }
                }
                FForm::Pd => {
                    for i in 0..2 {
                        let t = pred(get_f64(&dst, i), get_f64(&src, i));
                        o[i * 8..i * 8 + 8].copy_from_slice(&if t {
                            [0xffu8; 8]
                        } else {
                            [0u8; 8]
                        });
                    }
                }
            }
            m.cpu.xmm[reg] = o;
            m.next(d)
        }
        // ---- 搬运
        // 0x10/0x11：movups(无前缀) / movupd(66) / movss(F3) / movsd(F2)
        0x10 | 0x11 => {
            let (reg, rm) = m.modrm(d)?;
            let n = match d.rep {
                0xf3 => 4, // movss：32 位
                0xf2 => 8, // movsd：64 位
                _ => 16,   // movups/movupd
            };
            if op == 0x10 {
                let v = read_narrow(m, d, rm, n)?;
                if n == 16 {
                    m.cpu.xmm[reg] = v;
                } else {
                    // 从内存加载标量时高位清零；xmm 之间搬运时高位保留
                    match m.fixup(d, rm) {
                        Rm::Mem(_) => m.cpu.xmm[reg] = v,
                        Rm::Reg(_) => m.cpu.xmm[reg][..n].copy_from_slice(&v[..n]),
                    }
                }
            } else {
                let v = m.cpu.xmm[reg];
                write_narrow(m, d, rm, &v, n)?;
            }
            m.next(d)
        }

        // movaps/movapd（对齐版；这里不强制检查对齐——模拟器不靠对齐取数，
        // 而真实 CPU 会因未对齐 #GP。guest 若依赖那个异常来做探测会有差异，
        // 实测中没有遇到）
        0x28 | 0x29 => {
            let (reg, rm) = m.modrm(d)?;
            if op == 0x28 {
                m.cpu.xmm[reg] = read_xmm_rm(m, d, rm)?;
            } else {
                let v = m.cpu.xmm[reg];
                write_xmm_rm(m, d, rm, v)?;
            }
            m.next(d)
        }

        // movdqa(66) / movdqu(F3)
        0x6f | 0x7f => {
            if d.rep != 0xf3 && !d.o16 {
                return Err(m.undef(d)); // 无前缀是 MMX 的 movq，未实现
            }
            let (reg, rm) = m.modrm(d)?;
            if op == 0x6f {
                m.cpu.xmm[reg] = read_xmm_rm(m, d, rm)?;
            } else {
                let v = m.cpu.xmm[reg];
                write_xmm_rm(m, d, rm, v)?;
            }
            m.next(d)
        }

        // 0x6e：movd/movq xmm, r/m（66 前缀，REX.W 决定 32/64）
        0x6e if d.o16 => {
            let (reg, rm) = m.modrm(d)?;
            let size = if d.rex_w() { 8 } else { 4 };
            let v = m.rm_read(d, rm, size)?;
            let mut x = [0u8; 16]; // 高位清零
            x[..size as usize].copy_from_slice(&v.to_le_bytes()[..size as usize]);
            m.cpu.xmm[reg] = x;
            m.next(d)
        }

        // 0x7e：66 -> movd/movq r/m, xmm；F3 -> movq xmm, xmm/m64
        0x7e => {
            let (reg, rm) = m.modrm(d)?;
            if d.rep == 0xf3 {
                let v = read_narrow(m, d, rm, 8)?;
                m.cpu.xmm[reg] = v; // 高 64 位清零
                m.next(d)
            } else if d.o16 {
                let size = if d.rex_w() { 8 } else { 4 };
                let x = m.cpu.xmm[reg];
                let mut b = [0u8; 8];
                b[..size as usize].copy_from_slice(&x[..size as usize]);
                let v = u64::from_le_bytes(b);
                m.rm_write(d, rm, size, trunc(v, size))?;
                m.next(d)
            } else {
                Err(m.undef(d))
            }
        }

        // movq r/m64, xmm（66 0F D6）
        0xd6 if d.o16 => {
            let (reg, rm) = m.modrm(d)?;
            let x = m.cpu.xmm[reg];
            match m.fixup(d, rm) {
                Rm::Mem(a) => m.mem.write(a, &x[..8])?,
                Rm::Reg(r) => {
                    let mut o = [0u8; 16]; // xmm 目标时高 64 位清零
                    o[..8].copy_from_slice(&x[..8]);
                    m.cpu.xmm[r] = o;
                }
            }
            m.next(d)
        }

        // movlps/movlpd（加载/存储低 64 位）
        0x12 | 0x13 => {
            let (reg, rm) = m.modrm(d)?;
            if op == 0x12 {
                let v = read_narrow(m, d, rm, 8)?;
                m.cpu.xmm[reg][..8].copy_from_slice(&v[..8]);
            } else {
                let v = m.cpu.xmm[reg];
                write_narrow(m, d, rm, &v, 8)?;
            }
            m.next(d)
        }
        // movhps/movhpd（高 64 位）
        0x16 | 0x17 => {
            let (reg, rm) = m.modrm(d)?;
            if op == 0x16 {
                let v = read_narrow(m, d, rm, 8)?;
                m.cpu.xmm[reg][8..].copy_from_slice(&v[..8]);
            } else {
                let hi: [u8; 8] = m.cpu.xmm[reg][8..].try_into().unwrap();
                write_narrow(m, d, rm, &hi, 8)?;
            }
            m.next(d)
        }

        // ---- 位运算（andps/andpd/andnps/orps/xorps 与整数版同语义）
        0x54..=0x57 => {
            let (reg, rm) = m.modrm(d)?;
            let b = read_xmm_rm(m, d, rm)?;
            let a = m.cpu.xmm[reg];
            m.cpu.xmm[reg] = match op {
                0x54 => zip(a, b, |x, y| x & y),
                0x55 => zip(a, b, |x, y| !x & y), // andn
                0x56 => zip(a, b, |x, y| x | y),
                _ => zip(a, b, |x, y| x ^ y),
            };
            m.next(d)
        }
        0xdb | 0xdf | 0xeb | 0xef => {
            // pand / pandn / por / pxor
            let (reg, rm) = m.modrm(d)?;
            let b = read_xmm_rm(m, d, rm)?;
            let a = m.cpu.xmm[reg];
            m.cpu.xmm[reg] = match op {
                0xdb => zip(a, b, |x, y| x & y),
                0xdf => zip(a, b, |x, y| !x & y),
                0xeb => zip(a, b, |x, y| x | y),
                _ => zip(a, b, |x, y| x ^ y),
            };
            m.next(d)
        }

        // ---- 比较：pcmpeqb/w/d，全 1 或全 0 填满 lane
        0x74..=0x76 => {
            let (reg, rm) = m.modrm(d)?;
            let b = read_xmm_rm(m, d, rm)?;
            let a = m.cpu.xmm[reg];
            let w = match op {
                0x74 => 1,
                0x75 => 2,
                _ => 4,
            };
            let mask = if w == 8 {
                u64::MAX
            } else {
                (1u64 << (w * 8)) - 1
            };
            m.cpu.xmm[reg] = lanes(a, b, w, |x, y| if x == y { mask } else { 0 });
            m.next(d)
        }
        // pcmpgtb/w/d（有符号大于）
        0x64..=0x66 => {
            let (reg, rm) = m.modrm(d)?;
            let b = read_xmm_rm(m, d, rm)?;
            let a = m.cpu.xmm[reg];
            let w: usize = match op {
                0x64 => 1,
                0x65 => 2,
                _ => 4,
            };
            let bits = w as u32 * 8;
            let mask = (1u64 << bits) - 1;
            m.cpu.xmm[reg] = lanes(a, b, w, |x, y| {
                let sx = ((x << (64 - bits)) as i64) >> (64 - bits);
                let sy = ((y << (64 - bits)) as i64) >> (64 - bits);
                if sx > sy {
                    mask
                } else {
                    0
                }
            });
            m.next(d)
        }

        // pmovmskb：每字节的最高位收集成 16 位掩码。
        // 这条是 SSE2 字符串函数的枢纽——比较结果靠它变成通用寄存器里的位图。
        0xd7 => {
            let (reg, rm) = m.modrm(d)?;
            let x = match m.fixup(d, rm) {
                Rm::Reg(r) => m.cpu.xmm[r],
                Rm::Mem(_) => return Err(m.undef(d)), // 源必须是 xmm
            };
            let mut mask = 0u64;
            for (i, b) in x.iter().enumerate() {
                if b & 0x80 != 0 {
                    mask |= 1 << i;
                }
            }
            let size = if d.rex_w() { 8 } else { 4 };
            m.cpu.set_reg(reg, size, mask, false);
            m.next(d)
        }

        // ---- 加减与最小最大（字符串函数用 pminub 找 NUL）
        0xd8 | 0xd9 | 0xda | 0xde | 0xe8 | 0xe9 | 0xec | 0xed => {
            let (reg, rm) = m.modrm(d)?;
            let b = read_xmm_rm(m, d, rm)?;
            let a = m.cpu.xmm[reg];
            m.cpu.xmm[reg] = match op {
                0xd8 => zip(a, b, |x, y| x.saturating_sub(y)), // psubusb
                0xd9 => lanes(a, b, 2, |x, y| (x as u16).saturating_sub(y as u16) as u64), // psubusw
                0xda => zip(a, b, |x, y| x.min(y)),                                        // pminub
                0xde => zip(a, b, |x, y| x.max(y)),                                        // pmaxub
                0xe8 => zip(a, b, |x, y| (x as i8).saturating_add(y as i8) as u8),         // paddsb
                0xe9 => lanes(a, b, 2, |x, y| {
                    ((x as u16 as i16).saturating_add(y as u16 as i16)) as u16 as u64
                }), // paddsw
                0xec => zip(a, b, |x, y| (x as i8).saturating_sub(y as i8) as u8), // 占位：psubsb
                _ => lanes(a, b, 2, |x, y| {
                    ((x as u16 as i16).saturating_sub(y as u16 as i16)) as u16 as u64
                }),
            };
            m.next(d)
        }
        // psubb/w/d/q 与 paddb/w/d/q（回绕语义）
        0xf8 | 0xf9 | 0xfa | 0xfb | 0xfc | 0xfd | 0xfe | 0xd4 => {
            let (reg, rm) = m.modrm(d)?;
            let b = read_xmm_rm(m, d, rm)?;
            let a = m.cpu.xmm[reg];
            let (w, add): (usize, bool) = match op {
                0xf8 => (1, false),
                0xf9 => (2, false),
                0xfa => (4, false),
                0xfb => (8, false),
                0xfc => (1, true),
                0xfd => (2, true),
                0xfe => (4, true),
                _ => (8, true),
            };
            let mask = if w == 8 {
                u64::MAX
            } else {
                (1u64 << (w * 8)) - 1
            };
            m.cpu.xmm[reg] = lanes(a, b, w, |x, y| {
                (if add {
                    x.wrapping_add(y)
                } else {
                    x.wrapping_sub(y)
                }) & mask
            });
            m.next(d)
        }

        // ---- 交错（punpcklbw 等；memset 用它把一个字节铺满向量）
        0x60 | 0x61 | 0x62 | 0x6c | 0x68 | 0x69 | 0x6a | 0x6d => {
            let (reg, rm) = m.modrm(d)?;
            let b = read_xmm_rm(m, d, rm)?;
            let a = m.cpu.xmm[reg];
            let w: usize = match op {
                0x60 | 0x68 => 1,
                0x61 | 0x69 => 2,
                0x62 | 0x6a => 4,
                _ => 8,
            };
            // 高/低半选择不能简单用 `op >= 0x68`：四字版的编码是
            // 0x6c=punpcklqdq（低）、0x6d=punpckhqdq（高），0x6c 落在
            // 0x68 之后但取的是**低**半。
            let high = matches!(op, 0x68 | 0x69 | 0x6a | 0x6d);
            let src = if high { 8 } else { 0 };
            let mut o = [0u8; 16];
            let mut k = 0;
            let mut i = src;
            while k < 16 {
                o[k..k + w].copy_from_slice(&a[i..i + w]);
                o[k + w..k + 2 * w].copy_from_slice(&b[i..i + w]);
                k += 2 * w;
                i += w;
            }
            m.cpu.xmm[reg] = o;
            m.next(d)
        }

        // 打包收窄：packsswb(0x63) / packuswb(0x67) / packssdw(0x6b)。
        // 结果低半来自 dst、高半来自 src，每个 lane 饱和收窄到一半宽度。
        0x63 | 0x67 | 0x6b => {
            let (reg, rm) = m.modrm(d)?;
            let src = read_xmm_rm(m, d, rm)?;
            let dst = m.cpu.xmm[reg];
            let mut o = [0u8; 16];
            if op == 0x6b {
                // 32 -> 16，有符号饱和
                for (half, input) in [dst, src].iter().enumerate() {
                    for i in 0..4 {
                        let v = i32::from_le_bytes(input[i * 4..i * 4 + 4].try_into().unwrap());
                        let s = v.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                        let at = half * 8 + i * 2;
                        o[at..at + 2].copy_from_slice(&s.to_le_bytes());
                    }
                }
            } else {
                // 16 -> 8；0x63 有符号饱和，0x67 无符号饱和
                let signed = op == 0x63;
                for (half, input) in [dst, src].iter().enumerate() {
                    for i in 0..8 {
                        let v = i16::from_le_bytes(input[i * 2..i * 2 + 2].try_into().unwrap());
                        o[half * 8 + i] = if signed {
                            v.clamp(i8::MIN as i16, i8::MAX as i16) as i8 as u8
                        } else {
                            v.clamp(0, 255) as u8
                        };
                    }
                }
            }
            m.cpu.xmm[reg] = o;
            m.next(d)
        }

        // pshufd(66) / pshuflw(F2) / pshufhw(F3)
        0x70 => {
            let (reg, rm) = m.modrm(d)?;
            let src = read_xmm_rm(m, d, rm)?;
            let imm = m.f8(d)?;
            let mut o = [0u8; 16];
            if d.o16 {
                // pshufd：4 个 32 位 lane 按 imm 的 2 位一组重排
                for i in 0..4 {
                    let sel = ((imm >> (i * 2)) & 3) as usize;
                    o[i * 4..i * 4 + 4].copy_from_slice(&src[sel * 4..sel * 4 + 4]);
                }
            } else if d.rep == 0xf2 {
                // pshuflw：低 4 个 16 位 lane 重排，高 64 位原样
                o.copy_from_slice(&src);
                for i in 0..4 {
                    let sel = ((imm >> (i * 2)) & 3) as usize;
                    o[i * 2..i * 2 + 2].copy_from_slice(&src[sel * 2..sel * 2 + 2]);
                }
            } else if d.rep == 0xf3 {
                o.copy_from_slice(&src);
                for i in 0..4 {
                    let sel = ((imm >> (i * 2)) & 3) as usize;
                    o[8 + i * 2..8 + i * 2 + 2].copy_from_slice(&src[8 + sel * 2..8 + sel * 2 + 2]);
                }
            } else {
                return Err(m.undef(d));
            }
            m.cpu.xmm[reg] = o;
            m.next(d)
        }

        // shufps（无前缀）/ shufpd（66）。ld.so 的启动路径会用到。
        0xc6 => {
            let (reg, rm) = m.modrm(d)?;
            let src = read_xmm_rm(m, d, rm)?;
            let imm = m.f8(d)?;
            let dst = m.cpu.xmm[reg];
            let mut o = [0u8; 16];
            if d.o16 {
                // shufpd：低 64 位从 dst 的两个 qword 里按 imm bit0 选，
                // 高 64 位从 src 里按 imm bit1 选。
                let lo = (imm & 1) as usize;
                let hi = ((imm >> 1) & 1) as usize;
                o[0..8].copy_from_slice(&dst[lo * 8..lo * 8 + 8]);
                o[8..16].copy_from_slice(&src[hi * 8..hi * 8 + 8]);
            } else {
                // shufps：结果低两个 dword 取自 dst，高两个取自 src，
                // 每个由 imm 的一组 2 位选出。
                for i in 0..2 {
                    let sel = ((imm >> (i * 2)) & 3) as usize;
                    o[i * 4..i * 4 + 4].copy_from_slice(&dst[sel * 4..sel * 4 + 4]);
                }
                for i in 2..4 {
                    let sel = ((imm >> (i * 2)) & 3) as usize;
                    o[i * 4..i * 4 + 4].copy_from_slice(&src[sel * 4..sel * 4 + 4]);
                }
            }
            m.cpu.xmm[reg] = o;
            m.next(d)
        }

        // 移位组：0x71 字，0x72 双字，0x73 四字/整字节
        0x71..=0x73 => {
            let (sub, rm) = m.modrm(d)?;
            let imm = m.f8(d)? as u32;
            let r = match m.fixup(d, rm) {
                Rm::Reg(r) => r,
                Rm::Mem(_) => return Err(m.undef(d)),
            };
            let x = m.cpu.xmm[r];
            let sub = sub as u8 & 7;
            let w: usize = match op {
                0x71 => 2,
                0x72 => 4,
                _ => 8,
            };
            let out = match (op, sub) {
                // pslldq / psrldq：整个 128 位按**字节**移，字符串函数常用
                (0x73, 7) => {
                    let mut o = [0u8; 16];
                    let n = (imm as usize).min(16);
                    o[n..].copy_from_slice(&x[..16 - n]);
                    o
                }
                (0x73, 3) => {
                    let mut o = [0u8; 16];
                    let n = (imm as usize).min(16);
                    o[..16 - n].copy_from_slice(&x[n..]);
                    o
                }
                (_, 2) => lanes(
                    x,
                    x,
                    w,
                    |v, _| {
                        if imm as usize >= w * 8 {
                            0
                        } else {
                            v >> imm
                        }
                    },
                ), // psrl
                (_, 6) => lanes(x, x, w, |v, _| {
                    let mask = if w == 8 {
                        u64::MAX
                    } else {
                        (1u64 << (w * 8)) - 1
                    };
                    if imm as usize >= w * 8 {
                        0
                    } else {
                        (v << imm) & mask
                    }
                }), // psll
                (_, 4) => lanes(x, x, w, |v, _| {
                    // psra：按 lane 宽度做算术右移
                    let bits = w as u32 * 8;
                    let sv = ((v << (64 - bits)) as i64) >> (64 - bits);
                    let sh = (imm).min(bits - 1);
                    ((sv >> sh) as u64) & if w == 8 { u64::MAX } else { (1u64 << bits) - 1 }
                }),
                _ => return Err(m.undef(d)),
            };
            m.cpu.xmm[r] = out;
            m.next(d)
        }

        _ => Err(m.undef(d)),
    }
}

// ---------------------------------------------------------------- 浮点辅助

/// x86 的 MINPS/MINSS 语义：任一操作数为 NaN 时返回**第二个**操作数。
/// Rust 的 `f32::min` 会挑非 NaN 的那个，语义不同，不能直接用。
fn x86_min_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        b
    } else if a < b {
        a
    } else {
        b
    }
}

fn x86_min_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        b
    } else if a < b {
        a
    } else {
        b
    }
}

fn x86_max_f32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        b
    } else if a > b {
        a
    } else {
        b
    }
}

fn x86_max_f64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        b
    } else if a > b {
        a
    } else {
        b
    }
}

/// 按"最近偶数"舍入（x86 的默认舍入模式）。
/// `f64::round` 是"远离零"，在 .5 上与 x86 不一致。
fn round_half_even(f: f64) -> f64 {
    let r = f.round();
    if (f - f.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        r - f.signum()
    } else {
        r
    }
}

/// 目标宽度的"整数不定值"：x86 在转换超范围或 NaN 时返回它。
fn min_int(size: u8) -> u64 {
    if size == 8 {
        i64::MIN as u64
    } else {
        i32::MIN as u32 as u64
    }
}
