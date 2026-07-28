//! ELF64 加载器（x86-64）。
//!
//! 职责与 Linux 的 `fs/binfmt_elf.c` 对齐：解析 program header、按 `PT_LOAD`
//! 建立映射、`ET_DYN` 加载偏置、有 `PT_INTERP` 时把动态链接器一起装进来并把
//! 入口指向它。ELF 之外的格式（`#!` 脚本）由 `syscall::execve` 处理。

use crate::mem::{Mem, PAGE_MASK, PAGE_SIZE, PROT_EXEC, PROT_READ, PROT_WRITE};

pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
pub const EM_X86_64: u16 = 62;

pub const PT_LOAD: u32 = 1;
pub const PT_INTERP: u32 = 3;
pub const PT_PHDR: u32 = 6;
pub const PT_TLS: u32 = 7;

const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// `ET_DYN`（静态 PIE 或动态可执行文件）的加载基址。
/// 与 Linux 的 `ELF_ET_DYN_BASE` 同一量级，便于对照 guest 自己打印的地址。
pub const ET_DYN_BASE: u64 = 0x5555_5555_4000;
/// 动态链接器的加载基址，与主程序拉开足够距离。
pub const INTERP_BASE: u64 = 0x7f00_0000_0000;

#[derive(Debug)]
pub struct LoadError(pub String);

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for LoadError {}

fn err<T>(msg: impl Into<String>) -> Result<T, LoadError> {
    Err(LoadError(msg.into()))
}

#[derive(Debug, Clone, Copy)]
pub struct Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

/// 加载结果，`stack::setup` 用它填 auxv。
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    /// 真正要跳过去的入口（有 interp 时是 interp 的入口）。
    pub entry: u64,
    /// 主程序自己的入口（`AT_ENTRY`）。
    pub prog_entry: u64,
    /// 主程序加载偏置。
    pub bias: u64,
    /// guest 里 program header 表的地址（`AT_PHDR`）。
    pub phdr_addr: u64,
    pub phentsize: u64,
    pub phnum: u64,
    /// 动态链接器基址（`AT_BASE`）；静态程序为 0。
    pub interp_base: u64,
    /// `PT_INTERP` 里的路径（guest 视角），静态程序为 None。
    pub interp_path: Option<String>,
    /// 所有 `PT_LOAD` 之后的最高地址，`brk` 从这里起。
    pub brk: u64,
}

fn rd16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}
fn rd32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
fn rd64(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

/// 校验 ELF 头并返回 (e_type, e_entry, phdrs)。
pub fn parse(image: &[u8]) -> Result<(u16, u64, Vec<Phdr>), LoadError> {
    if image.len() < 64 {
        return err("不是 ELF：文件短于 64 字节");
    }
    if &image[0..4] != b"\x7fELF" {
        return err("不是 ELF：魔数不匹配");
    }
    if image[4] != 2 {
        return err("只支持 ELFCLASS64（64 位）");
    }
    if image[5] != 1 {
        return err("只支持 ELFDATA2LSB（小端）");
    }
    let e_type = rd16(image, 16);
    let e_machine = rd16(image, 18);
    if e_machine != EM_X86_64 {
        return err(format!(
            "只支持 x86-64（EM_X86_64=62），该文件 e_machine={e_machine}"
        ));
    }
    if e_type != ET_EXEC && e_type != ET_DYN {
        return err(format!(
            "只支持 ET_EXEC/ET_DYN，该文件 e_type={e_type}"
        ));
    }
    let e_entry = rd64(image, 24);
    let e_phoff = rd64(image, 32) as usize;
    let e_phentsize = rd16(image, 54) as usize;
    let e_phnum = rd16(image, 56) as usize;
    if e_phentsize < 56 {
        return err(format!("e_phentsize={e_phentsize} 小于 ELF64 的 56"));
    }
    let need = e_phoff
        .checked_add(e_phnum * e_phentsize)
        .ok_or_else(|| LoadError("program header 表偏移溢出".into()))?;
    if need > image.len() {
        return err("program header 表越过文件末尾");
    }
    let mut phdrs = Vec::with_capacity(e_phnum);
    for i in 0..e_phnum {
        let o = e_phoff + i * e_phentsize;
        phdrs.push(Phdr {
            p_type: rd32(image, o),
            p_flags: rd32(image, o + 4),
            p_offset: rd64(image, o + 8),
            p_vaddr: rd64(image, o + 16),
            p_filesz: rd64(image, o + 32),
            p_memsz: rd64(image, o + 40),
            p_align: rd64(image, o + 48),
        });
    }
    Ok((e_type, e_entry, phdrs))
}

fn prot_of(p_flags: u32) -> u8 {
    let mut prot = 0;
    if p_flags & PF_R != 0 {
        prot |= PROT_READ;
    }
    if p_flags & PF_W != 0 {
        prot |= PROT_WRITE;
    }
    if p_flags & PF_X != 0 {
        prot |= PROT_EXEC;
    }
    prot
}

/// 把一个 ELF 镜像的 `PT_LOAD` 段映射进 `mem`，返回 (最高地址, phdr guest 地址)。
///
/// 两遍走：先按可写映射并填内容，再统一收紧权限。分两遍是必需的——
/// `.text` 是只读段，如果建立映射时就按最终权限设置，加载器自己都写不进去；
/// 而 ELF 允许 `p_vaddr` 不页对齐，同一个物理页可能被两个段共享
/// （典型是 `.rodata` 尾页与 `.data` 首页），所以第二遍要按**并集**算权限，
/// 否则先处理的只读段会把共享页打回只读，guest 一写就 fault。
fn map_segments(
    mem: &mut Mem,
    image: &[u8],
    phdrs: &[Phdr],
    bias: u64,
) -> Result<(u64, u64), LoadError> {
    let mut high = 0u64;
    let mut phdr_addr = 0u64;
    // page -> 最终权限（并集）
    let mut want: std::collections::HashMap<u64, u8> = std::collections::HashMap::new();

    for ph in phdrs {
        if ph.p_type == PT_PHDR {
            phdr_addr = ph.p_vaddr + bias;
        }
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        let vaddr = ph.p_vaddr + bias;
        let start = vaddr & !PAGE_MASK;
        let end = (vaddr + ph.p_memsz + PAGE_MASK) & !PAGE_MASK;

        mem.map(start, end - start, PROT_READ | PROT_WRITE);
        mem.zero(start, end - start);

        if ph.p_filesz > 0 {
            let fo = ph.p_offset as usize;
            let fs = ph.p_filesz as usize;
            let fend = fo
                .checked_add(fs)
                .ok_or_else(|| LoadError("PT_LOAD 文件范围溢出".into()))?;
            if fend > image.len() {
                return err(format!(
                    "PT_LOAD 段的文件范围 [{fo:#x},{fend:#x}) 越过文件末尾（{} 字节）",
                    image.len()
                ));
            }
            mem.write_raw(vaddr, &image[fo..fend]);
        }
        // p_memsz > p_filesz 的部分（.bss）已被上面的 zero 清零。

        high = high.max(end);
        let prot = prot_of(ph.p_flags);
        let mut page = start;
        while page < end {
            *want.entry(page).or_insert(0) |= prot;
            page += PAGE_SIZE;
        }
    }
    if high == 0 {
        return err("ELF 里没有可加载的 PT_LOAD 段");
    }
    for (page, prot) in want {
        mem.map(page, PAGE_SIZE, prot);
    }
    Ok((high, phdr_addr))
}

/// 加载主程序（以及必要时的动态链接器）。
///
/// `resolve_interp` 把 `PT_INTERP` 里的 guest 路径解析成宿主路径并读出内容；
/// 由调用方注入，这样 VFS 前缀翻译留在 `fs` 模块里，加载器不碰路径策略。
pub fn load(
    mem: &mut Mem,
    image: &[u8],
    mut resolve_interp: impl FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<Loaded, LoadError> {
    let (e_type, e_entry, phdrs) = parse(image)?;
    let bias = if e_type == ET_DYN { ET_DYN_BASE } else { 0 };

    let (high, mut phdr_addr) = map_segments(mem, image, &phdrs, bias)?;

    // 没有 PT_PHDR 时（少见）用第一个 PT_LOAD 推算 phdr 在 guest 里的地址。
    if phdr_addr == 0 {
        let e_phoff = rd64(image, 32);
        if let Some(first) = phdrs.iter().find(|p| p.p_type == PT_LOAD) {
            if e_phoff >= first.p_offset && e_phoff - first.p_offset < first.p_filesz {
                phdr_addr = first.p_vaddr + bias + (e_phoff - first.p_offset);
            }
        }
    }

    let mut out = Loaded {
        entry: e_entry + bias,
        prog_entry: e_entry + bias,
        bias,
        phdr_addr,
        phentsize: rd16(image, 54) as u64,
        phnum: rd16(image, 56) as u64,
        interp_base: 0,
        interp_path: None,
        brk: (high + PAGE_MASK) & !PAGE_MASK,
    };

    // PT_INTERP：动态可执行文件。内核的做法是把 ld.so 也映射进来，
    // 然后跳到 ld.so 的入口，由它自己完成重定位并转交主程序。
    if let Some(ph) = phdrs.iter().find(|p| p.p_type == PT_INTERP) {
        let o = ph.p_offset as usize;
        let n = ph.p_filesz as usize;
        if o + n > image.len() {
            return err("PT_INTERP 越过文件末尾");
        }
        let raw = &image[o..n + o];
        let path = String::from_utf8_lossy(raw.split(|&b| b == 0).next().unwrap_or(raw)).into_owned();
        let interp_image = resolve_interp(&path).map_err(LoadError)?;
        let (it, ie, iph) = parse(&interp_image)?;
        let ibias = if it == ET_DYN { INTERP_BASE } else { 0 };
        map_segments(mem, &interp_image, &iph, ibias)?;
        out.entry = ie + ibias;
        out.interp_base = ibias;
        out.interp_path = Some(path);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工拼一个最小 ELF64：一个可读可执行的 PT_LOAD。
    fn tiny_elf(e_type: u16, vaddr: u64, code: &[u8]) -> Vec<u8> {
        let ehsize = 64usize;
        let phentsize = 56usize;
        let phoff = ehsize;
        let code_off = phoff + phentsize;
        let mut v = vec![0u8; code_off];
        v[0..4].copy_from_slice(b"\x7fELF");
        v[4] = 2; // ELFCLASS64
        v[5] = 1; // little endian
        v[6] = 1; // EV_CURRENT
        v[16..18].copy_from_slice(&e_type.to_le_bytes());
        v[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        v[24..32].copy_from_slice(&(vaddr + code_off as u64).to_le_bytes()); // e_entry
        v[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
        v[52..54].copy_from_slice(&(ehsize as u16).to_le_bytes());
        v[54..56].copy_from_slice(&(phentsize as u16).to_le_bytes());
        v[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

        let ph = phoff;
        v[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        v[ph + 4..ph + 8].copy_from_slice(&(PF_R | PF_X).to_le_bytes());
        v[ph + 8..ph + 16].copy_from_slice(&0u64.to_le_bytes()); // p_offset
        v[ph + 16..ph + 24].copy_from_slice(&vaddr.to_le_bytes());
        let filesz = (code_off + code.len()) as u64;
        v[ph + 32..ph + 40].copy_from_slice(&filesz.to_le_bytes());
        v[ph + 40..ph + 48].copy_from_slice(&filesz.to_le_bytes());
        v[ph + 48..ph + 56].copy_from_slice(&PAGE_SIZE.to_le_bytes());
        v.extend_from_slice(code);
        v
    }

    fn no_interp(_: &str) -> Result<Vec<u8>, String> {
        Err("测试镜像不应该有 interp".into())
    }

    #[test]
    fn loads_et_exec_at_fixed_vaddr() {
        let img = tiny_elf(ET_EXEC, 0x40_0000, &[0x90, 0xc3]);
        let mut m = Mem::new();
        let l = load(&mut m, &img, no_interp).unwrap();
        assert_eq!(l.bias, 0);
        assert_eq!(l.entry, 0x40_0000 + 120);
        assert_eq!(l.interp_base, 0);
        // 入口处能取指，且拿到的就是写进去的 code
        assert_eq!(m.fetch(l.entry, 2).unwrap(), &[0x90, 0xc3]);
        assert!(l.brk >= 0x40_0000 + PAGE_SIZE);
    }

    #[test]
    fn et_dyn_gets_load_bias() {
        let img = tiny_elf(ET_DYN, 0, &[0xc3]);
        let mut m = Mem::new();
        let l = load(&mut m, &img, no_interp).unwrap();
        assert_eq!(l.bias, ET_DYN_BASE);
        assert_eq!(l.entry, ET_DYN_BASE + 120);
        assert_eq!(m.fetch(l.entry, 1).unwrap(), &[0xc3]);
    }

    #[test]
    fn text_segment_is_not_writable() {
        let img = tiny_elf(ET_EXEC, 0x40_0000, &[0xc3]);
        let mut m = Mem::new();
        let l = load(&mut m, &img, no_interp).unwrap();
        assert!(
            m.write_u8(l.entry, 0).is_err(),
            "PF_R|PF_X 段加载完必须收回写权限"
        );
    }

    #[test]
    fn rejects_non_elf_and_wrong_arch() {
        let mut m = Mem::new();
        assert!(load(&mut m, b"#!/bin/sh\n", no_interp).is_err());
        let mut img = tiny_elf(ET_EXEC, 0x40_0000, &[0xc3]);
        img[18..20].copy_from_slice(&183u16.to_le_bytes()); // EM_AARCH64
        let e = load(&mut m, &img, no_interp).unwrap_err().to_string();
        assert!(e.contains("x86-64"), "{e}");
    }

    #[test]
    fn rejects_segment_past_end_of_file() {
        let mut img = tiny_elf(ET_EXEC, 0x40_0000, &[0xc3]);
        // 把 p_filesz 改成远大于文件长度
        let ph = 64;
        img[ph + 32..ph + 40].copy_from_slice(&0x10_0000u64.to_le_bytes());
        let mut m = Mem::new();
        let e = load(&mut m, &img, no_interp).unwrap_err().to_string();
        assert!(e.contains("越过文件末尾"), "{e}");
    }

    #[test]
    fn bss_is_zeroed_and_writable() {
        // p_memsz 比 p_filesz 大 0x100，多出来的是 .bss
        let mut img = tiny_elf(ET_EXEC, 0x40_0000, &[0xc3]);
        let ph = 64;
        let filesz = 121u64;
        img[ph + 4..ph + 8].copy_from_slice(&(PF_R | PF_W).to_le_bytes());
        img[ph + 40..ph + 48].copy_from_slice(&(filesz + 0x100).to_le_bytes());
        let mut m = Mem::new();
        load(&mut m, &img, no_interp).unwrap();
        assert_eq!(m.read_u8(0x40_0000 + filesz + 8).unwrap(), 0);
        assert!(m.write_u8(0x40_0000 + filesz + 8, 0x5a).is_ok());
        assert_eq!(m.read_u8(0x40_0000 + filesz + 8).unwrap(), 0x5a);
    }
}
