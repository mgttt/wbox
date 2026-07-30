//! Guest 地址空间。
//!
//! blink 的做法是在宿主里 reserve 一整块 VA 窗口（`WBOX_VA_BITS`，默认 1TB）
//! 再把 guest 线性地址按位映射进去。那套方案性能好，但依赖宿主的
//! reserve/commit 语义（Win32 `MEM_RESERVE` vs mmap `PROT_NONE`），也是
//! WIN32-PORT.md 里一半崩溃的来源。
//!
//! 这里改成**稀疏页表**：`page number -> Page`。代价是每次访存多一次哈希查找
//! （先把语义做对，缓存留给后续的性能里程碑），换来的是：
//!
//! - 完全平台无关，Windows/Linux 同一套代码，没有 VA 窗口布局问题；
//! - guest 的 64 位地址空间不需要宿主真的预留同等 VA；
//! - 越权访问一定命中"页不存在"，而不是踩到宿主自己的内存。

use std::collections::HashMap;

pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
pub const PAGE_MASK: u64 = PAGE_SIZE - 1;

pub const PROT_NONE: u8 = 0;
pub const PROT_READ: u8 = 1;
pub const PROT_WRITE: u8 = 2;
pub const PROT_EXEC: u8 = 4;

/// guest 访存失败。带上地址和意图，`main.rs` 据此打印诊断并按
/// 128+SIGSEGV 退出（与 blink 的行为一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    pub addr: u64,
    pub prot: u8,
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.prot {
            PROT_READ => "read",
            PROT_WRITE => "write",
            PROT_EXEC => "exec",
            _ => "access",
        };
        write!(f, "guest {} fault at {:#x}", what, self.addr)
    }
}

pub type MemResult<T> = Result<T, Fault>;

#[derive(Clone)]
struct Page {
    data: Box<[u8; PAGE_SIZE as usize]>,
    prot: u8,
}

/// guest 地址空间。
///
/// `Clone` 是快照式 `fork` 的基础：整张稀疏页表按值复制一份，父子从此互不
/// 影响。代价是 fork 的开销与已映射内存成正比（真内核是 COW），对
/// `sh -c 'a && b'` 这类用法完全够用。
#[derive(Clone)]
pub struct Mem {
    pages: HashMap<u64, Page>,
    /// mmap 的自底向上分配游标（`MAP_FIXED` 不走它）。
    mmap_next: u64,
    /// `brk` 当前值与 `brk` 起点，供 `sys_brk` 使用。
    pub brk: u64,
    pub brk_start: u64,
}

/// mmap 区域的默认起点。选在 128TB 用户空间的低端但远离可执行文件加载基址，
/// 与 Linux 的 mmap_base 习惯接近，方便对照 guest 自己打印的地址。
pub const MMAP_BASE: u64 = 0x0000_7f00_0000_0000;

impl Default for Mem {
    fn default() -> Self {
        Self::new()
    }
}

impl Mem {
    pub fn new() -> Self {
        Mem {
            pages: HashMap::new(),
            mmap_next: MMAP_BASE,
            brk: 0,
            brk_start: 0,
        }
    }

    #[inline]
    fn pn(addr: u64) -> u64 {
        addr >> PAGE_SHIFT
    }

    /// 已映射页数（诊断与测试用）。
    pub fn mapped_pages(&self) -> usize {
        self.pages.len()
    }

    /// 建立映射。已存在的页保留内容、只改权限——`mprotect` 依赖这一点；
    /// 需要清零的调用方自己 `zero`（`sys_mmap` 的匿名映射会做）。
    pub fn map(&mut self, addr: u64, len: u64, prot: u8) {
        let start = Self::pn(addr);
        let end = Self::pn(addr + len.saturating_sub(1)) + if len == 0 { 0 } else { 1 };
        for pn in start..end.max(start) {
            match self.pages.get_mut(&pn) {
                Some(p) => p.prot = prot,
                None => {
                    self.pages.insert(
                        pn,
                        Page {
                            data: Box::new([0u8; PAGE_SIZE as usize]),
                            prot,
                        },
                    );
                }
            }
        }
    }

    /// 撤销映射。整页粒度；未映射的页静默跳过（`munmap` 的 Linux 语义）。
    pub fn unmap(&mut self, addr: u64, len: u64) {
        if len == 0 {
            return;
        }
        let start = Self::pn(addr);
        let end = Self::pn(addr + len - 1) + 1;
        for pn in start..end {
            self.pages.remove(&pn);
        }
    }

    /// 是否整段都已映射（`mremap`/`munmap` 的前置检查）。
    pub fn is_mapped(&self, addr: u64, len: u64) -> bool {
        if len == 0 {
            return true;
        }
        (Self::pn(addr)..=Self::pn(addr + len - 1)).all(|pn| self.pages.contains_key(&pn))
    }

    /// 把区间清零（要求已映射；未映射的页跳过）。
    pub fn zero(&mut self, addr: u64, len: u64) {
        let mut a = addr;
        let end = addr.wrapping_add(len);
        while a < end {
            let off = (a & PAGE_MASK) as usize;
            let n = ((PAGE_SIZE as usize - off) as u64).min(end - a) as usize;
            if let Some(p) = self.pages.get_mut(&Self::pn(a)) {
                p.data[off..off + n].fill(0);
            }
            a += n as u64;
        }
    }

    /// 为 `mmap(NULL, ...)` 找一段连续的空闲地址。
    pub fn find_free(&mut self, len: u64) -> u64 {
        let want = (len + PAGE_MASK) & !PAGE_MASK;
        let npages = want >> PAGE_SHIFT;
        let mut base = self.mmap_next;
        'outer: loop {
            let start = Self::pn(base);
            for i in 0..npages {
                if self.pages.contains_key(&(start + i)) {
                    // 跳到冲突页之后重试，避免线性一页一页爬。
                    base = (start + i + 1) << PAGE_SHIFT;
                    continue 'outer;
                }
            }
            self.mmap_next = base + want;
            return base;
        }
    }

    /// 权限检查后返回页。`prot` 是本次访问需要的权限位。
    #[inline]
    fn page(&self, addr: u64, prot: u8) -> MemResult<&Page> {
        match self.pages.get(&Self::pn(addr)) {
            Some(p) if p.prot & prot == prot => Ok(p),
            _ => Err(Fault { addr, prot }),
        }
    }

    #[inline]
    fn page_mut(&mut self, addr: u64, prot: u8) -> MemResult<&mut Page> {
        match self.pages.get_mut(&Self::pn(addr)) {
            Some(p) if p.prot & prot == prot => Ok(p),
            _ => Err(Fault { addr, prot }),
        }
    }

    /// 读一段（可跨页）。
    pub fn read(&self, addr: u64, buf: &mut [u8]) -> MemResult<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let a = addr.checked_add(done as u64).ok_or(Fault {
                addr,
                prot: PROT_READ,
            })?;
            let off = (a & PAGE_MASK) as usize;
            let n = (PAGE_SIZE as usize - off).min(buf.len() - done);
            let p = self.page(a, PROT_READ)?;
            buf[done..done + n].copy_from_slice(&p.data[off..off + n]);
            done += n;
        }
        Ok(())
    }

    /// 写一段（可跨页）。
    pub fn write(&mut self, addr: u64, buf: &[u8]) -> MemResult<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let a = addr.checked_add(done as u64).ok_or(Fault {
                addr,
                prot: PROT_WRITE,
            })?;
            let off = (a & PAGE_MASK) as usize;
            let n = (PAGE_SIZE as usize - off).min(buf.len() - done);
            let p = self.page_mut(a, PROT_WRITE)?;
            p.data[off..off + n].copy_from_slice(&buf[done..done + n]);
            done += n;
        }
        Ok(())
    }

    /// 忽略权限位地读。
    ///
    /// `MAP_SHARED` 的回写要用它：guest 完全可以先 `mprotect(PROT_NONE)` 再
    /// `munmap`，而内核的页缓存并不因为映射不可读就丢掉内容——按 `PROT_READ`
    /// 去读会失败，那段写入就静默丢了（`t_mmap` 的 file-shared-persist 正是
    /// 这么写的）。未映射的页读成 0。
    pub fn read_raw(&self, addr: u64, buf: &mut [u8]) {
        let mut done = 0usize;
        while done < buf.len() {
            let a = addr.wrapping_add(done as u64);
            let off = (a & PAGE_MASK) as usize;
            let n = (PAGE_SIZE as usize - off).min(buf.len() - done);
            match self.pages.get(&Self::pn(a)) {
                Some(p) => buf[done..done + n].copy_from_slice(&p.data[off..off + n]),
                None => buf[done..done + n].fill(0),
            }
            done += n;
        }
    }

    /// 忽略权限位地写（加载器与栈初始化用：那时还没设最终 prot）。
    pub fn write_raw(&mut self, addr: u64, buf: &[u8]) {
        let mut done = 0usize;
        while done < buf.len() {
            let a = addr + done as u64;
            let off = (a & PAGE_MASK) as usize;
            let n = (PAGE_SIZE as usize - off).min(buf.len() - done);
            let pn = Self::pn(a);
            let p = self.pages.entry(pn).or_insert_with(|| Page {
                data: Box::new([0u8; PAGE_SIZE as usize]),
                prot: PROT_READ | PROT_WRITE,
            });
            p.data[off..off + n].copy_from_slice(&buf[done..done + n]);
            done += n;
        }
    }

    /// 取指。单独一个入口是为了让缺 `PROT_EXEC` 的错误能报成 exec fault
    /// （guest 跳到不可执行页时的诊断信息比"读失败"有用得多）。
    ///
    /// 返回从 `addr` 起最多 `n` 字节的**页内**切片；跨页时短读，
    /// 解码器负责在跨页处退回逐字节读。
    pub fn fetch(&self, addr: u64, n: usize) -> MemResult<&[u8]> {
        let p = match self.pages.get(&Self::pn(addr)) {
            Some(p) if p.prot & PROT_EXEC != 0 => p,
            _ => {
                return Err(Fault {
                    addr,
                    prot: PROT_EXEC,
                })
            }
        };
        let off = (addr & PAGE_MASK) as usize;
        let end = (off + n).min(PAGE_SIZE as usize);
        Ok(&p.data[off..end])
    }

    /// 取指专用的单字节读（跨页时用）。
    pub fn fetch_u8(&self, addr: u64) -> MemResult<u8> {
        let p = match self.pages.get(&Self::pn(addr)) {
            Some(p) if p.prot & PROT_EXEC != 0 => p,
            _ => {
                return Err(Fault {
                    addr,
                    prot: PROT_EXEC,
                })
            }
        };
        Ok(p.data[(addr & PAGE_MASK) as usize])
    }

    pub fn read_u8(&self, addr: u64) -> MemResult<u8> {
        // 单字节不会跨页，直接取，省掉 read() 的循环。
        let p = self.page(addr, PROT_READ)?;
        Ok(p.data[(addr & PAGE_MASK) as usize])
    }

    pub fn write_u8(&mut self, addr: u64, v: u8) -> MemResult<()> {
        let p = self.page_mut(addr, PROT_WRITE)?;
        p.data[(addr & PAGE_MASK) as usize] = v;
        Ok(())
    }

    pub fn read_u16(&self, addr: u64) -> MemResult<u16> {
        let mut b = [0u8; 2];
        self.read(addr, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }

    pub fn read_u32(&self, addr: u64) -> MemResult<u32> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    pub fn read_u64(&self, addr: u64) -> MemResult<u64> {
        let mut b = [0u8; 8];
        self.read(addr, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    pub fn write_u16(&mut self, addr: u64, v: u16) -> MemResult<()> {
        self.write(addr, &v.to_le_bytes())
    }

    pub fn write_u32(&mut self, addr: u64, v: u32) -> MemResult<()> {
        self.write(addr, &v.to_le_bytes())
    }

    pub fn write_u64(&mut self, addr: u64, v: u64) -> MemResult<()> {
        self.write(addr, &v.to_le_bytes())
    }

    /// 按宽度（1/2/4/8 字节）读，零扩展到 u64。
    pub fn read_sized(&self, addr: u64, size: u8) -> MemResult<u64> {
        Ok(match size {
            1 => self.read_u8(addr)? as u64,
            2 => self.read_u16(addr)? as u64,
            4 => self.read_u32(addr)? as u64,
            _ => self.read_u64(addr)?,
        })
    }

    /// 按宽度写（取 `v` 的低位）。
    pub fn write_sized(&mut self, addr: u64, size: u8, v: u64) -> MemResult<()> {
        match size {
            1 => self.write_u8(addr, v as u8),
            2 => self.write_u16(addr, v as u16),
            4 => self.write_u32(addr, v as u32),
            _ => self.write_u64(addr, v),
        }
    }

    /// 读 NUL 结尾的 guest 字符串。`max` 是安全上限（`PATH_MAX` 之类）。
    pub fn read_cstr(&self, addr: u64, max: usize) -> MemResult<Vec<u8>> {
        let mut out = Vec::new();
        let mut a = addr;
        while out.len() < max {
            let b = self.read_u8(a)?;
            if b == 0 {
                return Ok(out);
            }
            out.push(b);
            a = a.checked_add(1).ok_or(Fault {
                addr: a,
                prot: PROT_READ,
            })?;
        }
        // 超上限按截断处理而不是报错：guest 传了超长路径时，
        // 后续 open 会自然拿到 ENAMETOOLONG，比这里报 fault 更贴近 Linux。
        Ok(out)
    }

    /// 读 NULL 结尾的指针数组（`execve` 的 argv/envp）。
    pub fn read_ptr_array(&self, addr: u64, max: usize) -> MemResult<Vec<u64>> {
        let mut out = Vec::new();
        let mut a = addr;
        while out.len() < max {
            let p = self.read_u64(a)?;
            if p == 0 {
                break;
            }
            out.push(p);
            a += 8;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_read_write_roundtrip() {
        let mut m = Mem::new();
        m.map(0x1000, PAGE_SIZE, PROT_READ | PROT_WRITE);
        m.write_u64(0x1008, 0xdead_beef_cafe_babe).unwrap();
        assert_eq!(m.read_u64(0x1008).unwrap(), 0xdead_beef_cafe_babe);
    }

    #[test]
    fn unmapped_access_faults() {
        let m = Mem::new();
        assert_eq!(
            m.read_u8(0x2000).unwrap_err(),
            Fault {
                addr: 0x2000,
                prot: PROT_READ
            }
        );
    }

    #[test]
    fn write_to_readonly_page_faults() {
        let mut m = Mem::new();
        m.map(0x1000, PAGE_SIZE, PROT_READ);
        assert!(m.read_u8(0x1000).is_ok());
        assert_eq!(m.write_u8(0x1000, 1).unwrap_err().prot, PROT_WRITE);
    }

    #[test]
    fn access_spanning_two_pages_works() {
        let mut m = Mem::new();
        m.map(0x1000, PAGE_SIZE * 2, PROT_READ | PROT_WRITE);
        // 8 字节跨 0x1000/0x2000 页边界
        m.write_u64(0x1ffc, 0x1122_3344_5566_7788).unwrap();
        assert_eq!(m.read_u64(0x1ffc).unwrap(), 0x1122_3344_5566_7788);
    }

    #[test]
    fn span_faults_when_second_page_missing() {
        let mut m = Mem::new();
        m.map(0x1000, PAGE_SIZE, PROT_READ | PROT_WRITE);
        assert!(m.write_u64(0x1ffc, 0).is_err());
    }

    #[test]
    fn mprotect_keeps_page_contents() {
        let mut m = Mem::new();
        m.map(0x1000, PAGE_SIZE, PROT_READ | PROT_WRITE);
        m.write_u32(0x1000, 0xaabb_ccdd).unwrap();
        m.map(0x1000, PAGE_SIZE, PROT_READ); // = mprotect
        assert_eq!(m.read_u32(0x1000).unwrap(), 0xaabb_ccdd);
        assert!(m.write_u32(0x1000, 0).is_err());
    }

    #[test]
    fn find_free_skips_existing_mappings() {
        let mut m = Mem::new();
        m.map(MMAP_BASE, PAGE_SIZE * 4, PROT_READ);
        let a = m.find_free(PAGE_SIZE);
        assert!(a >= MMAP_BASE + PAGE_SIZE * 4, "got {a:#x}");
        assert!(!m.is_mapped(a, PAGE_SIZE));
    }

    #[test]
    fn fetch_requires_exec_and_short_reads_at_page_end() {
        let mut m = Mem::new();
        m.map(0x1000, PAGE_SIZE, PROT_READ | PROT_WRITE);
        assert!(m.fetch(0x1000, 4).is_err(), "no PROT_EXEC");
        m.map(0x1000, PAGE_SIZE, PROT_READ | PROT_EXEC);
        assert_eq!(m.fetch(0x1000, 15).unwrap().len(), 15);
        // 页尾只剩 3 字节，短读而不是跨页
        assert_eq!(m.fetch(0x1ffd, 15).unwrap().len(), 3);
    }

    #[test]
    fn cstr_and_ptr_array() {
        let mut m = Mem::new();
        m.map(0x1000, PAGE_SIZE, PROT_READ | PROT_WRITE);
        m.write(0x1000, b"/bin/sh\0").unwrap();
        assert_eq!(m.read_cstr(0x1000, 256).unwrap(), b"/bin/sh");
        m.write_u64(0x1100, 0x1000).unwrap();
        m.write_u64(0x1108, 0).unwrap();
        assert_eq!(m.read_ptr_array(0x1100, 16).unwrap(), vec![0x1000]);
    }

    #[test]
    fn unmap_then_access_faults() {
        let mut m = Mem::new();
        m.map(0x1000, PAGE_SIZE, PROT_READ | PROT_WRITE);
        assert!(m.is_mapped(0x1000, PAGE_SIZE));
        m.unmap(0x1000, PAGE_SIZE);
        assert!(!m.is_mapped(0x1000, PAGE_SIZE));
        assert!(m.read_u8(0x1000).is_err());
    }
}
