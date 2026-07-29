//! CPU 状态：通用寄存器、RIP、标志位、XMM、段基址。
//!
//! 标志位用**即时求值**（每条指令算完就写 CF/ZF/SF/OF/PF/AF），不做 blink 的
//! 延迟标志。延迟标志快，但要为每类运算记住操作数和运算种类，是模拟器里最容易
//! 出错的地方之一；先把语义做对。

/// 通用寄存器编号（ModRM/REX 的 4 位编码）。
pub const RAX: usize = 0;
pub const RCX: usize = 1;
pub const RDX: usize = 2;
pub const RBX: usize = 3;
pub const RSP: usize = 4;
pub const RBP: usize = 5;
pub const RSI: usize = 6;
pub const RDI: usize = 7;
pub const R8: usize = 8;
pub const R9: usize = 9;
pub const R10: usize = 10;
pub const R11: usize = 11;

pub const REG_NAMES: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

/// 标志寄存器。只保留模拟器真正会读写的位。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub cf: bool,
    pub pf: bool,
    pub af: bool,
    pub zf: bool,
    pub sf: bool,
    pub of: bool,
    /// 方向标志：影响 `movs`/`stos`/`cmps`/`lods`/`scas` 的步进方向。
    pub df: bool,
}

impl Flags {
    /// 打包成 `pushfq`/`lahf` 看到的 RFLAGS 值。bit1 恒为 1（保留位）。
    pub fn pack(&self) -> u64 {
        let mut v: u64 = 1 << 1;
        v |= self.cf as u64;
        v |= (self.pf as u64) << 2;
        v |= (self.af as u64) << 4;
        v |= (self.zf as u64) << 6;
        v |= (self.sf as u64) << 7;
        v |= 1 << 9; // IF：用户态永远开中断
        v |= (self.df as u64) << 10;
        v |= (self.of as u64) << 11;
        v
    }

    /// 从 `popfq`/`sahf` 的值恢复。
    pub fn unpack(&mut self, v: u64) {
        self.cf = v & (1 << 0) != 0;
        self.pf = v & (1 << 2) != 0;
        self.af = v & (1 << 4) != 0;
        self.zf = v & (1 << 6) != 0;
        self.sf = v & (1 << 7) != 0;
        self.df = v & (1 << 10) != 0;
        self.of = v & (1 << 11) != 0;
    }
}

/// 按宽度取符号位。`size` 是字节数。
#[inline]
pub fn sign_bit(size: u8) -> u64 {
    1u64 << (size as u32 * 8 - 1)
}

/// 按宽度截断。
#[inline]
pub fn trunc(v: u64, size: u8) -> u64 {
    match size {
        1 => v & 0xff,
        2 => v & 0xffff,
        4 => v & 0xffff_ffff,
        _ => v,
    }
}

/// 按宽度符号扩展到 u64（当作 i64 的位模式）。
#[inline]
pub fn sext(v: u64, size: u8) -> u64 {
    match size {
        1 => v as u8 as i8 as i64 as u64,
        2 => v as u16 as i16 as i64 as u64,
        4 => v as u32 as i32 as i64 as u64,
        _ => v,
    }
}

/// 低 8 位的偶校验（PF 只看结果的低字节）。
#[inline]
pub fn parity(v: u64) -> bool {
    (v as u8).count_ones().is_multiple_of(2)
}

/// `Clone` 是 `fork` 的基础：快照式 fork 直接克隆整个 CPU 状态。
#[derive(Clone)]
pub struct Cpu {
    pub regs: [u64; 16],
    pub rip: u64,
    pub flags: Flags,
    /// XMM0-15，按字节存。SSE 的整数/浮点语义各自解释这 16 字节。
    pub xmm: [[u8; 16]; 16],
    /// SSE 控制/状态寄存器。当前浮点运算由宿主完成，但 guest 仍会通过
    /// FXSAVE/LDMXCSR 保存恢复这个可见状态。
    pub mxcsr: u32,
    /// `arch_prctl(ARCH_SET_FS)` 设的 TLS 基址；`fs:` 前缀访存加它。
    pub fs_base: u64,
    pub gs_base: u64,
    /// 已执行指令数（诊断/限步用）。
    pub icount: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            regs: [0; 16],
            rip: 0,
            flags: Flags::default(),
            xmm: [[0; 16]; 16],
            mxcsr: 0x1f80,
            fs_base: 0,
            gs_base: 0,
            icount: 0,
        }
    }

    /// 按宽度读寄存器。`byte_high` 表示无 REX 时 reg 4-7 指的是 AH/CH/DH/BH。
    #[inline]
    pub fn get_reg(&self, reg: usize, size: u8, byte_high: bool) -> u64 {
        if size == 1 && byte_high {
            return (self.regs[reg - 4] >> 8) & 0xff;
        }
        trunc(self.regs[reg], size)
    }

    /// 按宽度写寄存器。x86-64 的关键语义：
    /// - 32 位写**零扩展**到 64 位；
    /// - 8/16 位写只改低位，保留高位。
    #[inline]
    pub fn set_reg(&mut self, reg: usize, size: u8, val: u64, byte_high: bool) {
        if size == 1 && byte_high {
            let r = reg - 4;
            self.regs[r] = (self.regs[r] & !0xff00) | ((val & 0xff) << 8);
            return;
        }
        match size {
            1 => self.regs[reg] = (self.regs[reg] & !0xff) | (val & 0xff),
            2 => self.regs[reg] = (self.regs[reg] & !0xffff) | (val & 0xffff),
            4 => self.regs[reg] = val & 0xffff_ffff,
            _ => self.regs[reg] = val,
        }
    }

    #[inline]
    pub fn rsp(&self) -> u64 {
        self.regs[RSP]
    }

    #[inline]
    pub fn set_rsp(&mut self, v: u64) {
        self.regs[RSP] = v;
    }

    /// 一行寄存器转储，崩溃诊断用。
    pub fn dump(&self) -> String {
        let mut s = format!("rip={:#018x} ", self.rip);
        for (name, v) in REG_NAMES.iter().zip(self.regs.iter()) {
            s.push_str(&format!("{name}={v:#x} "));
        }
        s.push_str(&format!(
            "flags=[{}{}{}{}{}{}]",
            if self.flags.cf { "C" } else { "-" },
            if self.flags.pf { "P" } else { "-" },
            if self.flags.af { "A" } else { "-" },
            if self.flags.zf { "Z" } else { "-" },
            if self.flags.sf { "S" } else { "-" },
            if self.flags.of { "O" } else { "-" },
        ));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write32_zero_extends() {
        let mut c = Cpu::new();
        c.regs[RAX] = 0xffff_ffff_ffff_ffff;
        c.set_reg(RAX, 4, 0x1234_5678, false);
        assert_eq!(c.regs[RAX], 0x1234_5678, "32 位写必须清高 32 位");
    }

    #[test]
    fn write16_and_write8_preserve_high_bits() {
        let mut c = Cpu::new();
        c.regs[RAX] = 0xffff_ffff_ffff_ffff;
        c.set_reg(RAX, 2, 0x1234, false);
        assert_eq!(c.regs[RAX], 0xffff_ffff_ffff_1234);
        c.set_reg(RAX, 1, 0xab, false);
        assert_eq!(c.regs[RAX], 0xffff_ffff_ffff_12ab);
    }

    #[test]
    fn byte_high_registers_target_bits_8_15() {
        let mut c = Cpu::new();
        c.regs[RAX] = 0x0000_0000_0000_00ff;
        // 无 REX 时 reg=4 是 AH
        c.set_reg(4, 1, 0x12, true);
        assert_eq!(c.regs[RAX], 0x12ff);
        assert_eq!(c.get_reg(4, 1, true), 0x12);
    }

    #[test]
    fn flags_pack_unpack_roundtrip() {
        let mut f = Flags {
            cf: true,
            pf: false,
            af: true,
            zf: false,
            sf: true,
            of: true,
            df: true,
        };
        let packed = f.pack();
        let mut g = Flags::default();
        g.unpack(packed);
        assert_eq!(f, g);
        // 保留位 bit1 与 IF 恒置
        assert_ne!(packed & 0b10, 0);
        assert_ne!(packed & (1 << 9), 0);
        f.df = false;
        assert_eq!(f.pack() & (1 << 10), 0);
    }

    #[test]
    fn sext_and_trunc() {
        assert_eq!(sext(0xff, 1), u64::MAX);
        assert_eq!(sext(0x7f, 1), 0x7f);
        assert_eq!(sext(0xffff_ffff, 4), u64::MAX);
        assert_eq!(trunc(0x1234_5678_9abc_def0, 2), 0xdef0);
    }

    #[test]
    fn parity_counts_low_byte_only() {
        assert!(parity(0x00)); // 0 个 1 -> 偶
        assert!(!parity(0x01)); // 1 个 1 -> 奇
        assert!(parity(0x03)); // 2 个 1 -> 偶
        assert!(parity(0xff00)); // 低字节 0 -> 偶
    }
}
