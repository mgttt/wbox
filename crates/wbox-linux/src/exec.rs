//! 译码与执行。
//!
//! 结构上是「边译边执行」：`Dec` 是一条指令的译码游标，`step()` 里一个大
//! `match` 按 opcode 分派，需要 ModRM/立即数时才去取。不先生成中间表示是有意的
//! ——x86 的操作数形式和 opcode 绑得很紧，拆成通用 IR 反而要写更多胶水。
//!
//! 覆盖范围：完整的整数/逻辑/移位/位操作/串操作/控制流，加上 `sse.rs` 里的
//! SSE2 子集。未实现的 opcode 一律报 `Exception::Undefined` 并打印原始字节，
//! 绝不静默当 NOP —— 静默跳过会让 guest 以难以追查的方式跑错。

use crate::alu;
use crate::cpu::{sext, trunc, RAX, RCX, RDX};
use crate::machine::{ExecResult, Exception, Machine};
use crate::mem::PROT_EXEC;

/// 一条指令的译码状态。
pub struct Dec {
    /// 指令起始地址（RIP-relative 与异常报告都要用）。
    pub start: u64,
    /// 当前取字节位置。
    pub pc: u64,
    /// REX 字节（0 = 没有）。
    pub rex: u8,
    pub has_rex: bool,
    /// 0x66 操作数宽度前缀。
    pub o16: bool,
    /// 0x67 地址宽度前缀（32 位有效地址）。
    pub a32: bool,
    /// 0xf2 / 0xf3（REP 系列，也参与 SSE 的 opcode 选择）。
    pub rep: u8,
    pub lock: bool,
    /// fs/gs 段基址覆盖。
    pub seg: Option<u64>,
}

impl Dec {
    #[inline]
    pub fn rex_w(&self) -> bool {
        self.rex & 8 != 0
    }
    #[inline]
    pub fn rex_r(&self) -> usize {
        ((self.rex & 4) >> 2) as usize
    }
    #[inline]
    pub fn rex_x(&self) -> usize {
        ((self.rex & 2) >> 1) as usize
    }
    #[inline]
    pub fn rex_b(&self) -> usize {
        (self.rex & 1) as usize
    }
    /// 默认操作数宽度：REX.W -> 8，0x66 -> 2，否则 4。
    #[inline]
    pub fn opsize(&self) -> u8 {
        if self.rex_w() {
            8
        } else if self.o16 {
            2
        } else {
            4
        }
    }
    /// 栈操作与近跳转的宽度：长模式下默认 64 位，只有 0x66 能改成 16。
    #[inline]
    pub fn stacksize(&self) -> u8 {
        if self.o16 {
            2
        } else {
            8
        }
    }
}

/// ModRM 的 r/m 操作数。
#[derive(Debug, Clone, Copy)]
pub enum Rm {
    Reg(usize),
    Mem(u64),
}

/// 无 REX 时 r/m 或 reg 字段的 4..7 指的是 AH/CH/DH/BH。
#[inline]
fn bh(d: &Dec, idx: usize, size: u8) -> bool {
    size == 1 && !d.has_rex && (4..8).contains(&idx)
}

impl Machine {
    // ------------------------------------------------------------ 取指

    #[inline]
    pub(crate) fn f8(&mut self, d: &mut Dec) -> ExecResult<u8> {
        let b = self.mem.fetch_u8(d.pc)?;
        d.pc += 1;
        Ok(b)
    }

    pub(crate) fn f16(&mut self, d: &mut Dec) -> ExecResult<u16> {
        let a = self.f8(d)? as u16;
        let b = self.f8(d)? as u16;
        Ok(a | (b << 8))
    }

    pub(crate) fn f32(&mut self, d: &mut Dec) -> ExecResult<u32> {
        let mut v = 0u32;
        for i in 0..4 {
            v |= (self.f8(d)? as u32) << (i * 8);
        }
        Ok(v)
    }

    pub(crate) fn f64v(&mut self, d: &mut Dec) -> ExecResult<u64> {
        let mut v = 0u64;
        for i in 0..8 {
            v |= (self.f8(d)? as u64) << (i * 8);
        }
        Ok(v)
    }

    /// 按操作数宽度取立即数并符号扩展（`size==8` 时立即数仍是 32 位）。
    pub(crate) fn imm_sext(&mut self, d: &mut Dec, size: u8) -> ExecResult<u64> {
        Ok(match size {
            1 => sext(self.f8(d)? as u64, 1),
            2 => sext(self.f16(d)? as u64, 2),
            _ => sext(self.f32(d)? as u64, 4),
        })
    }

    // ------------------------------------------------------------ ModRM

    /// 译码 ModRM（含 SIB / 位移 / RIP-relative），返回 (reg 字段, r/m)。
    pub(crate) fn modrm(&mut self, d: &mut Dec) -> ExecResult<(usize, Rm)> {
        let m = self.f8(d)?;
        let md = m >> 6;
        let reg = ((m >> 3) & 7) as usize | (d.rex_r() << 3);
        let rm = (m & 7) as usize;

        if md == 3 {
            return Ok((reg, Rm::Reg(rm | (d.rex_b() << 3))));
        }

        let mut addr: u64;
        if rm == 4 {
            // SIB
            let sib = self.f8(d)?;
            let scale = 1u64 << (sib >> 6);
            let index = ((sib >> 3) & 7) as usize | (d.rex_x() << 3);
            let base = (sib & 7) as usize | (d.rex_b() << 3);

            // index == 4（rsp）且无 REX.X 表示「没有变址」
            let idx_val = if index == 4 {
                0
            } else {
                self.cpu.regs[index].wrapping_mul(scale)
            };
            // base == 5 且 mod == 0 表示「没有基址，跟 disp32」
            let base_val = if (base & 7) == 5 && md == 0 {
                self.f32(d)? as i32 as i64 as u64
            } else {
                self.cpu.regs[base]
            };
            addr = base_val.wrapping_add(idx_val);
        } else if rm == 5 && md == 0 {
            // RIP-relative：位移相对**下一条指令**的地址，所以要在把整条
            // 指令的剩余字节（立即数）取完之后才知道基准。这里先记下位移，
            // 由 rip_rel_fixup 在指令末尾补齐。
            let disp = self.f32(d)? as i32 as i64 as u64;
            return Ok((reg, Rm::Mem(RIP_REL_MARKER.wrapping_add(disp))));
        } else {
            addr = self.cpu.regs[rm | (d.rex_b() << 3)];
        }

        match md {
            1 => {
                let disp = self.f8(d)? as i8 as i64 as u64;
                addr = addr.wrapping_add(disp);
            }
            2 => {
                let disp = self.f32(d)? as i32 as i64 as u64;
                addr = addr.wrapping_add(disp);
            }
            _ => {}
        }

        if d.a32 {
            addr &= 0xffff_ffff;
        }
        if let Some(base) = d.seg {
            addr = addr.wrapping_add(base);
        }
        Ok((reg, Rm::Mem(addr)))
    }

    /// RIP-relative 的最终地址修正。必须在整条指令译码完成后调用。
    #[inline]
    pub(crate) fn fixup(&self, d: &Dec, rm: Rm) -> Rm {
        match rm {
            Rm::Mem(a) if is_rip_rel(a) => {
                let disp = a.wrapping_sub(RIP_REL_MARKER);
                let mut addr = d.pc.wrapping_add(disp);
                if let Some(base) = d.seg {
                    addr = addr.wrapping_add(base);
                }
                Rm::Mem(addr)
            }
            other => other,
        }
    }

    // ------------------------------------------------------ 操作数读写

    pub(crate) fn rm_read(&mut self, d: &Dec, rm: Rm, size: u8) -> ExecResult<u64> {
        Ok(match self.fixup(d, rm) {
            Rm::Reg(r) => self.cpu.get_reg(r, size, bh(d, r, size)),
            Rm::Mem(a) => self.mem.read_sized(a, size)?,
        })
    }

    pub(crate) fn rm_write(&mut self, d: &Dec, rm: Rm, size: u8, v: u64) -> ExecResult<()> {
        match self.fixup(d, rm) {
            Rm::Reg(r) => self.cpu.set_reg(r, size, v, bh(d, r, size)),
            Rm::Mem(a) => self.mem.write_sized(a, size, v)?,
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn reg_read(&self, d: &Dec, reg: usize, size: u8) -> u64 {
        self.cpu.get_reg(reg, size, bh(d, reg, size))
    }

    #[inline]
    pub(crate) fn reg_write(&mut self, d: &Dec, reg: usize, size: u8, v: u64) {
        self.cpu.set_reg(reg, size, v, bh(d, reg, size))
    }

    // ------------------------------------------------------------ 栈

    pub(crate) fn push(&mut self, size: u8, v: u64) -> ExecResult<()> {
        let sp = self.cpu.rsp().wrapping_sub(size as u64);
        self.mem.write_sized(sp, size, v)?;
        self.cpu.set_rsp(sp);
        Ok(())
    }

    pub(crate) fn pop(&mut self, size: u8) -> ExecResult<u64> {
        let sp = self.cpu.rsp();
        let v = self.mem.read_sized(sp, size)?;
        self.cpu.set_rsp(sp.wrapping_add(size as u64));
        Ok(v)
    }

    // ------------------------------------------------------------ 条件码

    /// x86 的 16 个条件码（Jcc/SETcc/CMOVcc 共用编号）。
    pub(crate) fn cond(&self, cc: u8) -> bool {
        let f = &self.cpu.flags;
        match cc {
            0x0 => f.of,                    // O
            0x1 => !f.of,                   // NO
            0x2 => f.cf,                    // B/C/NAE
            0x3 => !f.cf,                   // AE/NB/NC
            0x4 => f.zf,                    // E/Z
            0x5 => !f.zf,                   // NE/NZ
            0x6 => f.cf || f.zf,            // BE/NA
            0x7 => !f.cf && !f.zf,          // A/NBE
            0x8 => f.sf,                    // S
            0x9 => !f.sf,                   // NS
            0xa => f.pf,                    // P/PE
            0xb => !f.pf,                   // NP/PO
            0xc => f.sf != f.of,            // L/NGE
            0xd => f.sf == f.of,            // GE/NL
            0xe => f.zf || (f.sf != f.of),  // LE/NG
            _ => !f.zf && (f.sf == f.of),   // G/NLE
        }
    }

    // ------------------------------------------------------------ 主循环

    /// 执行一条指令。
    pub fn step(&mut self) -> ExecResult<()> {
        let start = self.cpu.rip;
        let mut d = Dec {
            start,
            pc: start,
            rex: 0,
            has_rex: false,
            o16: false,
            a32: false,
            rep: 0,
            lock: false,
            seg: None,
        };

        // ---- 前缀。REX 必须紧邻 opcode，所以一旦见到 REX 就停止收前缀。
        let op = loop {
            let b = self.f8(&mut d)?;
            match b {
                0x66 => d.o16 = true,
                0x67 => d.a32 = true,
                0xf0 => d.lock = true,
                0xf2 | 0xf3 => d.rep = b,
                0x2e | 0x36 | 0x3e | 0x26 => {} // cs/ss/ds/es 覆盖在长模式下无效
                0x64 => d.seg = Some(self.cpu.fs_base),
                0x65 => d.seg = Some(self.cpu.gs_base),
                0x40..=0x4f => {
                    d.rex = b & 0x0f;
                    d.has_rex = true;
                    // REX 之后只能是 opcode（或 0x0f 转义）
                    break self.f8(&mut d)?;
                }
                _ => break b,
            }
        };

        self.cpu.icount += 1;

        if op == 0x0f {
            let op2 = self.f8(&mut d)?;
            return self.exec_0f(&mut d, op2);
        }
        self.exec_1byte(&mut d, op)
    }

    /// 提交 RIP：正常顺序执行时指向下一条指令。
    #[inline]
    pub(crate) fn next(&mut self, d: &Dec) -> ExecResult<()> {
        self.cpu.rip = d.pc;
        Ok(())
    }

    pub(crate) fn undef(&mut self, d: &Dec) -> Exception {
        // 把指令起始处的原始字节抓出来（最长 16 字节），便于对照 objdump。
        let mut bytes = Vec::new();
        for i in 0..16u64 {
            match self.mem.fetch_u8(d.start + i) {
                Ok(b) => bytes.push(b),
                Err(_) => break,
            }
        }
        Exception::Undefined {
            rip: d.start,
            bytes,
        }
    }

    // -------------------------------------------------- 单字节 opcode

    fn exec_1byte(&mut self, d: &mut Dec, op: u8) -> ExecResult<()> {
        match op {
            // ---- 8 组 ALU 的规则编码：ADD/OR/ADC/SBB/AND/SUB/XOR/CMP
            // 每组 6 个形式：r/m8,r8 | r/m,r | r8,r/m8 | r,r/m | al,imm8 | eax,imm
            0x00..=0x3d if (op & 7) <= 5 && (op >> 3) <= 7 => {
                let group = op >> 3;
                let form = op & 7;
                self.alu_group(d, group, form)
            }

            0x50..=0x57 => {
                let r = (op & 7) as usize | (d.rex_b() << 3);
                let size = d.stacksize();
                let v = self.cpu.get_reg(r, size, false);
                self.push(size, v)?;
                self.next(d)
            }
            0x58..=0x5f => {
                let r = (op & 7) as usize | (d.rex_b() << 3);
                let size = d.stacksize();
                let v = self.pop(size)?;
                self.cpu.set_reg(r, size, v, false);
                self.next(d)
            }

            // MOVSXD r64, r/m32
            0x63 => {
                let (reg, rm) = self.modrm(d)?;
                let v = self.rm_read(d, rm, 4)?;
                let size = d.opsize();
                self.reg_write(d, reg, size, sext(v, 4));
                self.next(d)
            }

            0x68 => {
                let v = sext(self.f32(d)? as u64, 4);
                let size = d.stacksize();
                self.push(size, v)?;
                self.next(d)
            }
            0x6a => {
                let v = sext(self.f8(d)? as u64, 1);
                let size = d.stacksize();
                self.push(size, v)?;
                self.next(d)
            }

            // IMUL r, r/m, imm
            0x69 | 0x6b => {
                let (reg, rm) = self.modrm(d)?;
                let size = d.opsize();
                let a = self.rm_read(d, rm, size)?;
                let b = if op == 0x6b {
                    sext(self.f8(d)? as u64, 1)
                } else {
                    self.imm_sext(d, size.min(4))?
                };
                let (lo, _) = alu::imul(a, b, size, &mut self.cpu.flags);
                self.reg_write(d, reg, size, lo);
                self.next(d)
            }

            // Jcc rel8
            0x70..=0x7f => {
                let disp = self.f8(d)? as i8 as i64 as u64;
                let taken = self.cond(op & 0x0f);
                self.cpu.rip = if taken { d.pc.wrapping_add(disp) } else { d.pc };
                Ok(())
            }

            // Group 1：ALU r/m, imm
            0x80 | 0x81 | 0x83 => {
                let size = if op == 0x80 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let imm = if op == 0x83 {
                    sext(self.f8(d)? as u64, 1)
                } else if op == 0x80 {
                    self.f8(d)? as u64
                } else {
                    self.imm_sext(d, size.min(4))?
                };
                let a = self.rm_read(d, rm, size)?;
                let (r, store) = self.alu_op(reg as u8 & 7, a, imm, size);
                if store {
                    self.rm_write(d, rm, size, r)?;
                }
                self.next(d)
            }

            // TEST r/m, r
            0x84 | 0x85 => {
                let size = if op == 0x84 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let a = self.rm_read(d, rm, size)?;
                let b = self.reg_read(d, reg, size);
                alu::and(a, b, size, &mut self.cpu.flags);
                self.next(d)
            }

            // XCHG r/m, r
            0x86 | 0x87 => {
                let size = if op == 0x86 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let a = self.rm_read(d, rm, size)?;
                let b = self.reg_read(d, reg, size);
                self.rm_write(d, rm, size, b)?;
                self.reg_write(d, reg, size, a);
                self.next(d)
            }

            // MOV
            0x88 | 0x89 => {
                let size = if op == 0x88 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let v = self.reg_read(d, reg, size);
                self.rm_write(d, rm, size, v)?;
                self.next(d)
            }
            0x8a | 0x8b => {
                let size = if op == 0x8a { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let v = self.rm_read(d, rm, size)?;
                self.reg_write(d, reg, size, v);
                self.next(d)
            }

            // LEA：只算有效地址，不访存，也不加段基址
            0x8d => {
                let (reg, rm) = self.modrm(d)?;
                let addr = match self.fixup(d, rm) {
                    Rm::Mem(a) => a,
                    Rm::Reg(_) => return Err(self.undef(d)), // LEA reg,reg 非法
                };
                let size = d.opsize();
                self.reg_write(d, reg, size, trunc(addr, size));
                self.next(d)
            }

            // POP r/m
            0x8f => {
                let (_, rm) = self.modrm(d)?;
                let size = d.stacksize();
                let v = self.pop(size)?;
                self.rm_write(d, rm, size, v)?;
                self.next(d)
            }

            // NOP / XCHG rax,r（0x90 带 REX.B 时是真 XCHG）
            0x90 => {
                if d.rex_b() != 0 {
                    let size = d.opsize();
                    let a = self.cpu.get_reg(RAX, size, false);
                    let b = self.cpu.get_reg(8, size, false);
                    self.cpu.set_reg(RAX, size, b, false);
                    self.cpu.set_reg(8, size, a, false);
                }
                self.next(d)
            }
            0x91..=0x97 => {
                let r = (op & 7) as usize | (d.rex_b() << 3);
                let size = d.opsize();
                let a = self.cpu.get_reg(RAX, size, false);
                let b = self.cpu.get_reg(r, size, false);
                self.cpu.set_reg(RAX, size, b, false);
                self.cpu.set_reg(r, size, a, false);
                self.next(d)
            }

            // CWDE / CDQE：把 ax/eax 符号扩展成 eax/rax
            0x98 => {
                let size = d.opsize();
                let v = match size {
                    2 => sext(self.cpu.get_reg(RAX, 1, false), 1),
                    4 => sext(self.cpu.get_reg(RAX, 2, false), 2),
                    _ => sext(self.cpu.get_reg(RAX, 4, false), 4),
                };
                self.cpu.set_reg(RAX, size, v, false);
                self.next(d)
            }
            // CDQ / CQO：把 rax 的符号位铺满 rdx
            0x99 => {
                let size = d.opsize();
                let a = self.cpu.get_reg(RAX, size, false);
                let hi = if (sext(a, size) as i64) < 0 { u64::MAX } else { 0 };
                self.cpu.set_reg(RDX, size, hi, false);
                self.next(d)
            }

            0x9c => {
                let v = self.cpu.flags.pack();
                let size = d.stacksize();
                self.push(size, v)?;
                self.next(d)
            }
            0x9d => {
                let size = d.stacksize();
                let v = self.pop(size)?;
                self.cpu.flags.unpack(v);
                self.next(d)
            }
            // SAHF / LAHF
            0x9e => {
                let ah = self.cpu.get_reg(4, 1, true);
                let keep = self.cpu.flags.pack() & !0xff;
                self.cpu.flags.unpack(keep | (ah & 0xd5));
                self.next(d)
            }
            0x9f => {
                let v = self.cpu.flags.pack() & 0xff;
                self.cpu.set_reg(4, 1, v, true);
                self.next(d)
            }

            // 串操作
            0xa4 | 0xa5 | 0xa6 | 0xa7 | 0xaa | 0xab | 0xac | 0xad | 0xae | 0xaf => {
                self.string_op(d, op)
            }

            // TEST al/eax, imm
            0xa8 => {
                let imm = self.f8(d)? as u64;
                let a = self.cpu.get_reg(RAX, 1, false);
                alu::and(a, imm, 1, &mut self.cpu.flags);
                self.next(d)
            }
            0xa9 => {
                let size = d.opsize();
                let imm = self.imm_sext(d, size.min(4))?;
                let a = self.cpu.get_reg(RAX, size, false);
                alu::and(a, imm, size, &mut self.cpu.flags);
                self.next(d)
            }

            // MOV r8, imm8
            0xb0..=0xb7 => {
                let r = (op & 7) as usize | (d.rex_b() << 3);
                let imm = self.f8(d)? as u64;
                let byte_high = !d.has_rex && (4..8).contains(&r);
                self.cpu.set_reg(r, 1, imm, byte_high);
                self.next(d)
            }
            // MOV r, imm（REX.W 时是唯一的 imm64 形式 movabs）
            0xb8..=0xbf => {
                let r = (op & 7) as usize | (d.rex_b() << 3);
                let size = d.opsize();
                let imm = match size {
                    2 => self.f16(d)? as u64,
                    4 => self.f32(d)? as u64,
                    _ => self.f64v(d)?,
                };
                self.cpu.set_reg(r, size, imm, false);
                self.next(d)
            }

            // 移位组：imm8 / 1 / cl
            0xc0 | 0xc1 | 0xd0 | 0xd1 | 0xd2 | 0xd3 => {
                let size = if op & 1 == 0 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let cnt = match op {
                    0xc0 | 0xc1 => self.f8(d)? as u64,
                    0xd0 | 0xd1 => 1,
                    _ => self.cpu.get_reg(RCX, 1, false),
                };
                let a = self.rm_read(d, rm, size)?;
                let f = &mut self.cpu.flags;
                let r = match reg as u8 & 7 {
                    0 => alu::rol(a, cnt, size, f),
                    1 => alu::ror(a, cnt, size, f),
                    2 => alu::rcl(a, cnt, size, f),
                    3 => alu::rcr(a, cnt, size, f),
                    4 | 6 => alu::shl(a, cnt, size, f),
                    5 => alu::shr(a, cnt, size, f),
                    _ => alu::sar(a, cnt, size, f),
                };
                self.rm_write(d, rm, size, r)?;
                self.next(d)
            }

            // RET / RET imm16
            0xc2 | 0xc3 => {
                let extra = if op == 0xc2 { self.f16(d)? as u64 } else { 0 };
                let ret = self.pop(8)?;
                self.cpu.set_rsp(self.cpu.rsp().wrapping_add(extra));
                self.cpu.rip = ret;
                Ok(())
            }

            // MOV r/m, imm
            0xc6 | 0xc7 => {
                let size = if op == 0xc6 { 1 } else { d.opsize() };
                let (_, rm) = self.modrm(d)?;
                let imm = if op == 0xc6 {
                    self.f8(d)? as u64
                } else {
                    self.imm_sext(d, size.min(4))?
                };
                self.rm_write(d, rm, size, imm)?;
                self.next(d)
            }

            // LEAVE：mov rsp,rbp; pop rbp
            0xc9 => {
                let size = d.stacksize();
                self.cpu.set_rsp(self.cpu.regs[crate::cpu::RBP]);
                let v = self.pop(size)?;
                self.cpu.set_reg(crate::cpu::RBP, size, v, false);
                self.next(d)
            }

            0xcc => Err(Exception::Breakpoint { rip: d.start }),

            // CALL rel32
            0xe8 => {
                let disp = self.f32(d)? as i32 as i64 as u64;
                let ret = d.pc;
                self.push(8, ret)?;
                self.cpu.rip = d.pc.wrapping_add(disp);
                Ok(())
            }
            // JMP rel32 / rel8
            0xe9 => {
                let disp = self.f32(d)? as i32 as i64 as u64;
                self.cpu.rip = d.pc.wrapping_add(disp);
                Ok(())
            }
            0xeb => {
                let disp = self.f8(d)? as i8 as i64 as u64;
                self.cpu.rip = d.pc.wrapping_add(disp);
                Ok(())
            }

            0xf4 => Err(Exception::Exit(0)), // HLT：用户态 guest 不该执行，按停机处理
            0xf5 => {
                self.cpu.flags.cf = !self.cpu.flags.cf;
                self.next(d)
            }
            0xf8 => {
                self.cpu.flags.cf = false;
                self.next(d)
            }
            0xf9 => {
                self.cpu.flags.cf = true;
                self.next(d)
            }
            0xfc => {
                self.cpu.flags.df = false;
                self.next(d)
            }
            0xfd => {
                self.cpu.flags.df = true;
                self.next(d)
            }

            // Group 3：TEST/NOT/NEG/MUL/IMUL/DIV/IDIV
            0xf6 | 0xf7 => {
                let size = if op == 0xf6 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                match reg as u8 & 7 {
                    0 | 1 => {
                        let imm = if size == 1 {
                            self.f8(d)? as u64
                        } else {
                            self.imm_sext(d, size.min(4))?
                        };
                        let a = self.rm_read(d, rm, size)?;
                        alu::and(a, imm, size, &mut self.cpu.flags);
                    }
                    2 => {
                        let a = self.rm_read(d, rm, size)?;
                        self.rm_write(d, rm, size, trunc(!a, size))?; // NOT 不改标志
                    }
                    3 => {
                        let a = self.rm_read(d, rm, size)?;
                        let r = alu::neg(a, size, &mut self.cpu.flags);
                        self.rm_write(d, rm, size, r)?;
                    }
                    4 | 5 => {
                        let a = self.rm_read(d, rm, size)?;
                        let b = self.cpu.get_reg(RAX, size, false);
                        let signed = reg as u8 & 7 == 5;
                        let (lo, hi) = if signed {
                            alu::imul(b, a, size, &mut self.cpu.flags)
                        } else {
                            alu::mul(b, a, size, &mut self.cpu.flags)
                        };
                        if size == 1 {
                            // 8 位形式结果是 ax（不写 dl）
                            self.cpu.set_reg(RAX, 2, (lo & 0xff) | ((hi & 0xff) << 8), false);
                        } else {
                            self.cpu.set_reg(RAX, size, lo, false);
                            self.cpu.set_reg(RDX, size, hi, false);
                        }
                    }
                    _ => {
                        let signed = reg as u8 & 7 == 7;
                        let div = self.rm_read(d, rm, size)?;
                        let (hi, lo) = if size == 1 {
                            // 8 位形式被除数是 ax
                            let ax = self.cpu.get_reg(RAX, 2, false);
                            ((ax >> 8) & 0xff, ax & 0xff)
                        } else {
                            (
                                self.cpu.get_reg(RDX, size, false),
                                self.cpu.get_reg(RAX, size, false),
                            )
                        };
                        let out = if signed {
                            alu::idiv(hi, lo, div, size)
                        } else {
                            alu::div(hi, lo, div, size)
                        }
                        .ok_or(Exception::DivideError { rip: d.start })?;
                        if size == 1 {
                            self.cpu
                                .set_reg(RAX, 2, (out.quot & 0xff) | ((out.rem & 0xff) << 8), false);
                        } else {
                            self.cpu.set_reg(RAX, size, out.quot, false);
                            self.cpu.set_reg(RDX, size, out.rem, false);
                        }
                    }
                }
                self.next(d)
            }

            // Group 4/5：INC/DEC/CALL/JMP/PUSH r/m
            0xfe | 0xff => {
                let (reg, rm) = self.modrm(d)?;
                let sub = reg as u8 & 7;
                if op == 0xfe && sub > 1 {
                    return Err(self.undef(d));
                }
                match sub {
                    0 | 1 => {
                        let size = if op == 0xfe { 1 } else { d.opsize() };
                        let a = self.rm_read(d, rm, size)?;
                        let r = if sub == 0 {
                            alu::inc(a, size, &mut self.cpu.flags)
                        } else {
                            alu::dec(a, size, &mut self.cpu.flags)
                        };
                        self.rm_write(d, rm, size, r)?;
                        self.next(d)
                    }
                    2 => {
                        // CALL r/m64：目标要在压返回地址**之前**读出来，
                        // 否则 `call [rsp]` 之类会读到刚压进去的返回地址。
                        let target = self.rm_read(d, rm, 8)?;
                        self.push(8, d.pc)?;
                        self.cpu.rip = target;
                        Ok(())
                    }
                    4 => {
                        let target = self.rm_read(d, rm, 8)?;
                        self.cpu.rip = target;
                        Ok(())
                    }
                    6 => {
                        let size = d.stacksize();
                        let v = self.rm_read(d, rm, size)?;
                        self.push(size, v)?;
                        self.next(d)
                    }
                    _ => Err(self.undef(d)), // 远调用/远跳转：长模式用户态用不到
                }
            }

            _ => Err(self.undef(d)),
        }
    }

    /// 8 组规则 ALU 的公共译码。`group` 0..7，`form` 0..5。
    fn alu_group(&mut self, d: &mut Dec, group: u8, form: u8) -> ExecResult<()> {
        match form {
            0 | 1 => {
                let size = if form == 0 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let a = self.rm_read(d, rm, size)?;
                let b = self.reg_read(d, reg, size);
                let (r, store) = self.alu_op(group, a, b, size);
                if store {
                    self.rm_write(d, rm, size, r)?;
                }
            }
            2 | 3 => {
                let size = if form == 2 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let a = self.reg_read(d, reg, size);
                let b = self.rm_read(d, rm, size)?;
                let (r, store) = self.alu_op(group, a, b, size);
                if store {
                    self.reg_write(d, reg, size, r);
                }
            }
            _ => {
                let size = if form == 4 { 1 } else { d.opsize() };
                let imm = if size == 1 {
                    self.f8(d)? as u64
                } else {
                    self.imm_sext(d, size.min(4))?
                };
                let a = self.cpu.get_reg(RAX, size, false);
                let (r, store) = self.alu_op(group, a, imm, size);
                if store {
                    self.cpu.set_reg(RAX, size, r, false);
                }
            }
        }
        self.next(d)
    }

    /// 按组号求值。返回 (结果, 是否写回) —— CMP 只更新标志。
    pub(crate) fn alu_op(&mut self, group: u8, a: u64, b: u64, size: u8) -> (u64, bool) {
        let f = &mut self.cpu.flags;
        let cf = f.cf;
        match group {
            0 => (alu::add(a, b, size, false, f), true),
            1 => (alu::or(a, b, size, f), true),
            2 => (alu::add(a, b, size, cf, f), true),
            3 => (alu::sub(a, b, size, cf, f), true),
            4 => (alu::and(a, b, size, f), true),
            5 => (alu::sub(a, b, size, false, f), true),
            6 => (alu::xor(a, b, size, f), true),
            _ => (alu::sub(a, b, size, false, f), false), // CMP
        }
    }

    /// 串操作。REP 前缀时**每次 step 只做一轮**，rcx 未归零就不推进 RIP。
    /// 这样长串拷贝可以被信号和指令计数打断，不会在一条指令里卡住主循环。
    fn string_op(&mut self, d: &mut Dec, op: u8) -> ExecResult<()> {
        let size = if op & 1 == 0 { 1 } else { d.opsize() };
        let step: i64 = if self.cpu.flags.df {
            -(size as i64)
        } else {
            size as i64
        };

        // REP 且 rcx==0：整条指令退化为空操作。
        if d.rep != 0 && self.cpu.regs[RCX] == 0 {
            return self.next(d);
        }

        let si = self.cpu.regs[crate::cpu::RSI];
        let di = self.cpu.regs[crate::cpu::RDI];
        // 源操作数可以带段覆盖（`fs:`），目的操作数在 x86 里恒为 es:，长模式下基址 0。
        let si_eff = si.wrapping_add(d.seg.unwrap_or(0));

        let mut cmp_result_zf = None;
        match op {
            0xa4 | 0xa5 => {
                let v = self.mem.read_sized(si_eff, size)?;
                self.mem.write_sized(di, size, v)?;
                self.cpu.regs[crate::cpu::RSI] = si.wrapping_add(step as u64);
                self.cpu.regs[crate::cpu::RDI] = di.wrapping_add(step as u64);
            }
            0xa6 | 0xa7 => {
                let a = self.mem.read_sized(si_eff, size)?;
                let b = self.mem.read_sized(di, size)?;
                alu::sub(a, b, size, false, &mut self.cpu.flags);
                cmp_result_zf = Some(self.cpu.flags.zf);
                self.cpu.regs[crate::cpu::RSI] = si.wrapping_add(step as u64);
                self.cpu.regs[crate::cpu::RDI] = di.wrapping_add(step as u64);
            }
            0xaa | 0xab => {
                let v = self.cpu.get_reg(RAX, size, false);
                self.mem.write_sized(di, size, v)?;
                self.cpu.regs[crate::cpu::RDI] = di.wrapping_add(step as u64);
            }
            0xac | 0xad => {
                let v = self.mem.read_sized(si_eff, size)?;
                self.cpu.set_reg(RAX, size, v, false);
                self.cpu.regs[crate::cpu::RSI] = si.wrapping_add(step as u64);
            }
            _ => {
                // SCAS：al/eax 与 es:[rdi] 比较
                let a = self.cpu.get_reg(RAX, size, false);
                let b = self.mem.read_sized(di, size)?;
                alu::sub(a, b, size, false, &mut self.cpu.flags);
                cmp_result_zf = Some(self.cpu.flags.zf);
                self.cpu.regs[crate::cpu::RDI] = di.wrapping_add(step as u64);
            }
        }

        if d.rep == 0 {
            return self.next(d);
        }

        self.cpu.regs[RCX] = self.cpu.regs[RCX].wrapping_sub(1);

        // REPE/REPNE 在 CMPS/SCAS 上还要看 ZF
        let zf_stop = match cmp_result_zf {
            Some(zf) => (d.rep == 0xf3 && !zf) || (d.rep == 0xf2 && zf),
            None => false,
        };
        if self.cpu.regs[RCX] == 0 || zf_stop {
            self.next(d)
        } else {
            // RIP 留在原处，下一次 step 继续这条 REP 指令
            self.cpu.rip = d.start;
            Ok(())
        }
    }

    // --------------------------------------------------- 0x0f 双字节

    fn exec_0f(&mut self, d: &mut Dec, op: u8) -> ExecResult<()> {
        match op {
            // SYSCALL
            0x05 => crate::syscall::dispatch(self, d.pc),

            0x0b => Err(self.undef(d)), // UD2：guest 主动触发，照实报

            // prefetch* / NOP r/m / ENDBR64（F3 0F 1E FA）
            0x0d | 0x18 | 0x19 | 0x1a | 0x1b | 0x1c | 0x1d | 0x1e | 0x1f => {
                let _ = self.modrm(d)?;
                self.next(d)
            }

            // CMOVcc
            0x40..=0x4f => {
                let (reg, rm) = self.modrm(d)?;
                let size = d.opsize();
                let v = self.rm_read(d, rm, size)?;
                if self.cond(op & 0x0f) {
                    self.reg_write(d, reg, size, v);
                } else if size == 4 {
                    // 关键细节：条件不成立时 32 位形式**仍然**要把目标寄存器
                    // 的高 32 位清零（写 32 位寄存器的通用语义）。
                    let cur = self.reg_read(d, reg, 4);
                    self.reg_write(d, reg, 4, cur);
                }
                self.next(d)
            }

            // Jcc rel32
            0x80..=0x8f => {
                let disp = self.f32(d)? as i32 as i64 as u64;
                let taken = self.cond(op & 0x0f);
                self.cpu.rip = if taken { d.pc.wrapping_add(disp) } else { d.pc };
                Ok(())
            }

            // SETcc r/m8
            0x90..=0x9f => {
                let (_, rm) = self.modrm(d)?;
                let v = self.cond(op & 0x0f) as u64;
                self.rm_write(d, rm, 1, v)?;
                self.next(d)
            }

            0x31 => {
                // RDTSC：用已执行指令数当时间戳。单调递增即可满足 guest
                // 对"时间在走"的假设，也让同一 guest 的执行可复现。
                let t = self.cpu.icount;
                self.cpu.set_reg(RAX, 4, t & 0xffff_ffff, false);
                self.cpu.set_reg(RDX, 4, t >> 32, false);
                self.next(d)
            }

            0xa2 => {
                self.cpuid();
                self.next(d)
            }

            // BT/BTS/BTR/BTC r/m, r
            0xa3 | 0xab | 0xb3 | 0xbb => {
                let (reg, rm) = self.modrm(d)?;
                let size = d.opsize();
                let bit = self.reg_read(d, reg, size);
                self.bit_op(d, rm, size, bit, op)
            }
            // Group 8：BT/BTS/BTR/BTC r/m, imm8
            0xba => {
                let (reg, rm) = self.modrm(d)?;
                let size = d.opsize();
                let imm = self.f8(d)? as u64;
                let real = match reg as u8 & 7 {
                    4 => 0xa3,
                    5 => 0xab,
                    6 => 0xb3,
                    7 => 0xbb,
                    _ => return Err(self.undef(d)),
                };
                self.bit_op(d, rm, size, imm, real)
            }

            // SHLD / SHRD
            0xa4 | 0xa5 | 0xac | 0xad => {
                let (reg, rm) = self.modrm(d)?;
                let size = d.opsize();
                let cnt = if op & 1 == 0 {
                    self.f8(d)? as u64
                } else {
                    self.cpu.get_reg(RCX, 1, false)
                };
                let a = self.rm_read(d, rm, size)?;
                let b = self.reg_read(d, reg, size);
                let f = &mut self.cpu.flags;
                let r = if op < 0xac {
                    alu::shld(a, b, cnt, size, f)
                } else {
                    alu::shrd(a, b, cnt, size, f)
                };
                self.rm_write(d, rm, size, r)?;
                self.next(d)
            }

            // IMUL r, r/m
            0xaf => {
                let (reg, rm) = self.modrm(d)?;
                let size = d.opsize();
                let a = self.reg_read(d, reg, size);
                let b = self.rm_read(d, rm, size)?;
                let (lo, _) = alu::imul(a, b, size, &mut self.cpu.flags);
                self.reg_write(d, reg, size, lo);
                self.next(d)
            }

            // CMPXCHG r/m, r
            0xb0 | 0xb1 => {
                let size = if op == 0xb0 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let dst = self.rm_read(d, rm, size)?;
                let acc = self.cpu.get_reg(RAX, size, false);
                let src = self.reg_read(d, reg, size);
                alu::sub(acc, dst, size, false, &mut self.cpu.flags);
                if trunc(acc, size) == trunc(dst, size) {
                    self.rm_write(d, rm, size, src)?;
                } else {
                    self.cpu.set_reg(RAX, size, dst, false);
                }
                self.next(d)
            }

            // XADD
            0xc0 | 0xc1 => {
                let size = if op == 0xc0 { 1 } else { d.opsize() };
                let (reg, rm) = self.modrm(d)?;
                let a = self.rm_read(d, rm, size)?;
                let b = self.reg_read(d, reg, size);
                let sum = alu::add(a, b, size, false, &mut self.cpu.flags);
                self.reg_write(d, reg, size, a);
                self.rm_write(d, rm, size, sum)?;
                self.next(d)
            }

            // MOVZX / MOVSX
            0xb6 | 0xb7 | 0xbe | 0xbf => {
                let src_size = if op & 1 == 0 { 1 } else { 2 };
                let (reg, rm) = self.modrm(d)?;
                let v = self.rm_read(d, rm, src_size)?;
                let size = d.opsize();
                let v = if op >= 0xbe { sext(v, src_size) } else { v };
                self.reg_write(d, reg, size, trunc(v, size));
                self.next(d)
            }

            // POPCNT（F3 0F B8）
            0xb8 if d.rep == 0xf3 => {
                let (reg, rm) = self.modrm(d)?;
                let size = d.opsize();
                let v = self.rm_read(d, rm, size)?;
                let n = trunc(v, size).count_ones() as u64;
                self.reg_write(d, reg, size, n);
                let f = &mut self.cpu.flags;
                f.zf = n == 0;
                f.cf = false;
                f.of = false;
                f.sf = false;
                f.af = false;
                f.pf = false;
                self.next(d)
            }

            // BSF / BSR，以及 F3 前缀下的 TZCNT / LZCNT
            0xbc | 0xbd => {
                let (reg, rm) = self.modrm(d)?;
                let size = d.opsize();
                let v = trunc(self.rm_read(d, rm, size)?, size);
                let bits = size as u32 * 8;
                if d.rep == 0xf3 {
                    // TZCNT/LZCNT：源为 0 时返回宽度，CF 置位
                    let n = if op == 0xbc {
                        if v == 0 { bits } else { v.trailing_zeros() }
                    } else if v == 0 {
                        bits
                    } else {
                        v.leading_zeros() - (64 - bits)
                    };
                    self.reg_write(d, reg, size, n as u64);
                    self.cpu.flags.cf = v == 0;
                    self.cpu.flags.zf = n == 0;
                } else {
                    // BSF/BSR：源为 0 时 ZF 置位，目标**不定**（这里保持不变）
                    self.cpu.flags.zf = v == 0;
                    if v != 0 {
                        let n = if op == 0xbc {
                            v.trailing_zeros()
                        } else {
                            bits - 1 - (v.leading_zeros() - (64 - bits))
                        };
                        self.reg_write(d, reg, size, n as u64);
                    }
                }
                self.next(d)
            }

            // BSWAP
            0xc8..=0xcf => {
                let r = (op & 7) as usize | (d.rex_b() << 3);
                let size = d.opsize();
                let v = self.cpu.get_reg(r, size, false);
                let sw = match size {
                    2 => (v as u16).swap_bytes() as u64,
                    4 => (v as u32).swap_bytes() as u64,
                    _ => v.swap_bytes(),
                };
                self.cpu.set_reg(r, size, sw, false);
                self.next(d)
            }

            // Group 15：LFENCE/MFENCE/SFENCE 与 FXSAVE 族。
            // 单线程模拟器里屏障是空操作；FXSAVE 族没实现，照实报错。
            0xae => {
                let (reg, rm) = self.modrm(d)?;
                match (reg as u8 & 7, rm) {
                    (5..=7, Rm::Reg(_)) => self.next(d), // fence
                    _ => Err(self.undef(d)),
                }
            }

            _ => crate::sse::exec(self, d, op),
        }
    }

    /// BT/BTS/BTR/BTC 公共实现。
    fn bit_op(&mut self, d: &mut Dec, rm: Rm, size: u8, bit: u64, op: u8) -> ExecResult<()> {
        let bits = size as u64 * 8;
        match self.fixup(d, rm) {
            Rm::Reg(_) => {
                // 寄存器形式：位号按宽度取模
                let n = bit % bits;
                let v = self.rm_read(d, rm, size)?;
                self.cpu.flags.cf = (v >> n) & 1 != 0;
                let nv = match op {
                    0xab => v | (1 << n),
                    0xb3 => v & !(1 << n),
                    0xbb => v ^ (1 << n),
                    _ => v,
                };
                if op != 0xa3 {
                    self.rm_write(d, rm, size, trunc(nv, size))?;
                }
            }
            Rm::Mem(base) => {
                // 内存形式：位号是**带符号**的，可以跨出操作数所在的机器字。
                let signed = sext(bit, size) as i64;
                let byte_off = signed.div_euclid(8);
                let n = signed.rem_euclid(8) as u32;
                let addr = base.wrapping_add(byte_off as u64);
                let v = self.mem.read_u8(addr)?;
                self.cpu.flags.cf = (v >> n) & 1 != 0;
                let nv = match op {
                    0xab => v | (1 << n),
                    0xb3 => v & !(1 << n),
                    0xbb => v ^ (1 << n),
                    _ => v,
                };
                if op != 0xa3 {
                    self.mem.write_u8(addr, nv)?;
                }
            }
        }
        self.next(d)
    }

    /// CPUID。宣告的特性集必须与 `stack::HWCAP` 和 `sse.rs` 的实现一致。
    fn cpuid(&mut self) {
        let leaf = self.cpu.get_reg(RAX, 4, false) as u32;
        let (a, b, c, dx) = match leaf {
            // 最大基础 leaf + 厂商串 "GenuineIntel"（guest 的 libc 会按厂商
            // 选字符串函数实现；报 Intel 走的是最常见、最好验证的那条路）
            0 => (7u32, 0x756e_6547, 0x6c65_746e, 0x4965_6e69),
            1 => (
                0x0006_0f01, // family 6 model 15 stepping 1
                0,
                // ECX：只报 SSE3/SSSE3/SSE4.1/SSE4.2 之外的都不报。
                // 全 0 = 只有 SSE2，与 sse.rs 的实现范围吻合。
                0,
                // EDX：与 HWCAP 同源
                crate::stack::HWCAP as u32,
            ),
            7 => (0, 0, 0, 0), // 不报 AVX2/BMI 等扩展
            0x8000_0000 => (0x8000_0001, 0, 0, 0),
            // 0x8000_0001 EDX bit29 = 长模式
            0x8000_0001 => (0, 0, 0, 1 << 29),
            _ => (0, 0, 0, 0),
        };
        self.cpu.set_reg(RAX, 4, a as u64, false);
        self.cpu.set_reg(crate::cpu::RBX, 4, b as u64, false);
        self.cpu.set_reg(RCX, 4, c as u64, false);
        self.cpu.set_reg(RDX, 4, dx as u64, false);
    }
}

/// RIP-relative 位移的占位基准。
///
/// 选这个值是因为它**不是规范地址**（x86-64 要求 bit47..63 全同），所以任何
/// 合法 guest 地址都不可能落在它 ±2GB 的范围内，`is_rip_rel` 不会误判。
const RIP_REL_MARKER: u64 = 0xdead_8000_0000_0000;

#[inline]
fn is_rip_rel(a: u64) -> bool {
    // 位移是 i32，所以待修正的值一定落在 marker ±2GB 内。
    let off = a.wrapping_sub(RIP_REL_MARKER) as i64;
    off >= i32::MIN as i64 && off <= i32::MAX as i64
}

/// 供测试与 `main` 使用：把一段机器码放到可执行页并跑起来。
pub fn load_code(m: &mut Machine, addr: u64, code: &[u8]) {
    use crate::mem::{PROT_READ, PROT_WRITE};
    let len = (code.len() as u64 + 0xfff) & !0xfff;
    m.mem.map(addr, len.max(0x1000), PROT_READ | PROT_WRITE);
    m.mem.write_raw(addr, code);
    m.mem
        .map(addr, len.max(0x1000), PROT_READ | PROT_EXEC);
    m.cpu.rip = addr;
}
