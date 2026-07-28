//! tar（USTAR / GNU）。取代 `tar` crate。
//!
//! API 形状刻意与 `tar` crate 对齐（`Builder` / `Archive` / `Header` /
//! `EntryType`），因为调用方有几十处，换实现时**只改 `use` 行**比重写调用
//! 点安全得多。
//!
//! # 读这一侧要吃下真实镜像层
//!
//! OCI 层是 docker/buildah 打的，会用到：GNU 长路径（`L` 类型）、长链接目标
//! （`K` 类型）、PAX 扩展头（`x`/`g`）、目录/符号链接/硬链接/字符设备等类型。
//! 这些都支持；PAX 里只取 `path` 与 `linkpath` 两个键（其余是 mtime/uid 之类
//! 的元数据，wbox 不用）。
//!
//! # 写这一侧只需要"标准 tar 能读"
//!
//! 我们写出去的层要能被 docker 拉回去解开，所以走 USTAR + GNU 长名扩展。
//!
//! # 安全
//!
//! 解包**不在这里做**路径校验——调用方（`oci::image`）有一整套 whiteout /
//! 越权路径 / 符号链接逃逸的规则，比"tar 库里顺手挡一下"严格得多。这里只
//! 保证：不会因为畸形头部而 panic，size/类型字段是校验过的。

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const BLOCK: usize = 512;

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

// ============================================================ EntryType

/// tar 条目类型。取值即 typeflag 的字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    Regular,
    Link,
    Symlink,
    Char,
    Block,
    Directory,
    Fifo,
    /// GNU 长路径（下一条目的名字放在本条目的数据里）。
    GnuLongName,
    /// GNU 长链接目标。
    GnuLongLink,
    /// PAX 扩展头（`x` 单条 / `g` 全局）。
    XHeader,
    XGlobalHeader,
    /// 其它未知 typeflag，原样保留。
    Other(u8),
}

impl EntryType {
    pub fn from_byte(b: u8) -> Self {
        match b {
            b'0' | 0 => EntryType::Regular,
            b'1' => EntryType::Link,
            b'2' => EntryType::Symlink,
            b'3' => EntryType::Char,
            b'4' => EntryType::Block,
            b'5' => EntryType::Directory,
            b'6' => EntryType::Fifo,
            b'L' => EntryType::GnuLongName,
            b'K' => EntryType::GnuLongLink,
            b'x' => EntryType::XHeader,
            b'g' => EntryType::XGlobalHeader,
            other => EntryType::Other(other),
        }
    }

    pub fn as_byte(self) -> u8 {
        match self {
            EntryType::Regular => b'0',
            EntryType::Link => b'1',
            EntryType::Symlink => b'2',
            EntryType::Char => b'3',
            EntryType::Block => b'4',
            EntryType::Directory => b'5',
            EntryType::Fifo => b'6',
            EntryType::GnuLongName => b'L',
            EntryType::GnuLongLink => b'K',
            EntryType::XHeader => b'x',
            EntryType::XGlobalHeader => b'g',
            EntryType::Other(b) => b,
        }
    }

    pub fn is_dir(self) -> bool {
        self == EntryType::Directory
    }

    pub fn is_symlink(self) -> bool {
        self == EntryType::Symlink
    }

    pub fn is_hard_link(self) -> bool {
        self == EntryType::Link
    }

    pub fn is_file(self) -> bool {
        self == EntryType::Regular
    }
}

// ============================================================ Header

/// 512 字节的 tar 头部。字段偏移见 POSIX ustar 定义。
#[derive(Clone)]
pub struct Header {
    bytes: [u8; BLOCK],
}

impl Default for Header {
    fn default() -> Self {
        Self::new_gnu()
    }
}

impl Header {
    /// 新建一个 GNU 风格的头部（`magic` = `ustar  \0`）。
    pub fn new_gnu() -> Self {
        let mut bytes = [0u8; BLOCK];
        bytes[257..265].copy_from_slice(b"ustar  \0");
        let mut h = Header { bytes };
        h.set_mode(0o644);
        h.set_entry_type(EntryType::Regular);
        h
    }

    fn field(&self, off: usize, len: usize) -> &[u8] {
        &self.bytes[off..off + len]
    }

    fn set_str(&mut self, off: usize, len: usize, s: &str) {
        let b = s.as_bytes();
        let n = b.len().min(len);
        self.bytes[off..off + len].fill(0);
        self.bytes[off..off + n].copy_from_slice(&b[..n]);
    }

    /// 八进制数字字段：右对齐补零，末尾一个 NUL（GNU/ustar 通用写法）。
    fn set_octal(&mut self, off: usize, len: usize, v: u64) {
        let s = format!("{:0width$o}", v, width = len - 1);
        self.bytes[off..off + len - 1].copy_from_slice(&s.as_bytes()[s.len() + 1 - len..]);
        self.bytes[off + len - 1] = 0;
    }

    fn get_octal(&self, off: usize, len: usize) -> io::Result<u64> {
        let f = self.field(off, len);
        // GNU 的 base-256 扩展：最高位置 1 时后面是大端二进制。大文件
        // （>8 GiB）的 size 会用到。
        if f[0] & 0x80 != 0 {
            let mut v: u64 = 0;
            for &b in &f[f.len().saturating_sub(8)..] {
                v = (v << 8) | b as u64;
            }
            return Ok(v);
        }
        let mut v: u64 = 0;
        let mut seen = false;
        for &b in f {
            match b {
                b'0'..=b'7' => {
                    v = v
                        .checked_mul(8)
                        .and_then(|x| x.checked_add((b - b'0') as u64))
                        .ok_or_else(|| err("tar 八进制字段溢出"))?;
                    seen = true;
                }
                b' ' | 0 => {
                    if seen {
                        break;
                    }
                }
                _ => return Err(err("tar 八进制字段含非法字符")),
            }
        }
        Ok(v)
    }

    fn cstr(&self, off: usize, len: usize) -> String {
        let f = self.field(off, len);
        let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
        String::from_utf8_lossy(&f[..end]).into_owned()
    }

    /// 条目路径（含 ustar 的 prefix 字段拼接）。
    pub fn path(&self) -> PathBuf {
        let name = self.cstr(0, 100);
        let prefix = self.cstr(345, 155);
        if prefix.is_empty() {
            PathBuf::from(name)
        } else {
            PathBuf::from(format!("{prefix}/{name}"))
        }
    }

    /// 设置路径。超过 100 字节的部分由 `Builder` 用 GNU `L` 条目承载，
    /// 这里只写得下多少写多少。
    pub fn set_path(&mut self, p: &str) {
        self.set_str(0, 100, p);
        self.set_str(345, 155, "");
    }

    pub fn link_name(&self) -> Option<PathBuf> {
        let s = self.cstr(157, 100);
        if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(s))
        }
    }

    pub fn set_link_name(&mut self, p: &str) {
        self.set_str(157, 100, p);
    }

    pub fn mode(&self) -> u32 {
        self.get_octal(100, 8).unwrap_or(0o644) as u32
    }

    pub fn set_mode(&mut self, mode: u32) {
        // 只保留权限位：类型位由 typeflag 表达，混进来会让别的实现困惑。
        self.set_octal(100, 8, (mode & 0o7777) as u64);
    }

    pub fn size(&self) -> io::Result<u64> {
        self.get_octal(124, 12)
    }

    pub fn set_size(&mut self, size: u64) {
        self.set_octal(124, 12, size);
    }

    pub fn set_uid(&mut self, uid: u64) {
        self.set_octal(108, 8, uid);
    }

    pub fn set_gid(&mut self, gid: u64) {
        self.set_octal(116, 8, gid);
    }

    pub fn set_mtime(&mut self, mtime: u64) {
        self.set_octal(136, 12, mtime);
    }

    pub fn entry_type(&self) -> EntryType {
        EntryType::from_byte(self.bytes[156])
    }

    pub fn set_entry_type(&mut self, t: EntryType) {
        self.bytes[156] = t.as_byte();
    }

    /// 重算校验和。**必须在所有字段设完之后调用**——这是 tar 头部里唯一
    /// 会被静默写错的字段（写错了别的实现直接判为"不是 tar"）。
    pub fn set_cksum(&mut self) {
        self.bytes[148..156].fill(b' ');
        let sum: u32 = self.bytes.iter().map(|&b| b as u32).sum();
        let s = format!("{:06o}\0 ", sum);
        self.bytes[148..156].copy_from_slice(s.as_bytes());
    }

    /// 校验和是否正确。空块（全 0）不算合法头部，由调用方先判。
    fn checksum_ok(&self) -> bool {
        let Ok(want) = self.get_octal(148, 8) else {
            return false;
        };
        let mut unsigned: u32 = 0;
        let mut signed: i32 = 0;
        for (i, &b) in self.bytes.iter().enumerate() {
            let v = if (148..156).contains(&i) { b' ' } else { b };
            unsigned += v as u32;
            signed += v as i8 as i32;
        }
        want == unsigned as u64 || want as i64 == signed as i64
    }

    fn is_zero(&self) -> bool {
        self.bytes.iter().all(|&b| b == 0)
    }
}

// ============================================================ Builder

/// tar 写入器。
pub struct Builder<W: Write> {
    inner: Option<W>,
    follow: bool,
    finished: bool,
}

impl<W: Write> Builder<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner: Some(inner),
            follow: true,
            finished: false,
        }
    }

    /// 是否跟随符号链接（`false` = 符号链接原样入 tar）。
    pub fn follow_symlinks(&mut self, follow: bool) {
        self.follow = follow;
    }

    fn w(&mut self) -> &mut W {
        self.inner.as_mut().expect("Builder 已经 finish")
    }

    fn write_block(&mut self, block: &[u8; BLOCK]) -> io::Result<()> {
        self.w().write_all(block)
    }

    /// 写数据并补齐到 512 边界。
    fn write_padded(&mut self, data: &[u8]) -> io::Result<()> {
        self.w().write_all(data)?;
        let rem = data.len() % BLOCK;
        if rem != 0 {
            let pad = vec![0u8; BLOCK - rem];
            self.w().write_all(&pad)?;
        }
        Ok(())
    }

    /// 路径超过 100 字节 / 链接目标超过 100 字节时，先写一个 GNU 扩展条目。
    fn write_long_name(&mut self, kind: EntryType, value: &str) -> io::Result<()> {
        let mut h = Header::new_gnu();
        h.set_path("././@LongLink");
        h.set_entry_type(kind);
        h.set_mode(0o644);
        h.set_size(value.len() as u64 + 1);
        h.set_cksum();
        self.write_block(&h.bytes)?;
        let mut data = value.as_bytes().to_vec();
        data.push(0);
        self.write_padded(&data)
    }

    /// 写一个条目：头部里的路径以 `path` 为准（覆盖 header 里已有的）。
    pub fn append_data<P: AsRef<Path>, R: Read>(
        &mut self,
        header: &mut Header,
        path: P,
        mut data: R,
    ) -> io::Result<()> {
        let mut buf = Vec::new();
        data.read_to_end(&mut buf)?;
        let mut data = buf.as_slice();
        let p = normalize(path.as_ref());
        if p.len() > 100 {
            self.write_long_name(EntryType::GnuLongName, &p)?;
        }
        header.set_path(&p);
        // 只有普通文件才带数据；目录/链接的 size 必须是 0，否则解包方会
        // 把后面的字节当成本条目的内容读走，整个归档就错位了。
        if !matches!(header.entry_type(), EntryType::Regular) {
            data = &[];
            header.set_size(0);
        } else {
            header.set_size(data.len() as u64);
        }
        header.set_cksum();
        self.write_block(&header.bytes)?;
        self.write_padded(data)
    }

    /// 写一个已经设好路径的条目（不覆盖 header 里的路径）。
    ///
    /// 头部里的 `size` 也按原样使用——这是"我自己算好了全部字段"的入口，
    /// 测试用它构造刻意畸形的归档。
    pub fn append<R: Read>(&mut self, header: &Header, mut data: R) -> io::Result<()> {
        let mut buf = Vec::new();
        data.read_to_end(&mut buf)?;
        let mut h = header.clone();
        h.set_cksum();
        self.write_block(&h.bytes)?;
        if matches!(h.entry_type(), EntryType::Regular) {
            self.write_padded(&buf)
        } else {
            Ok(())
        }
    }

    /// 写一个目录条目（内容不递归，调用方自己决定要不要往下走）。
    pub fn append_dir<P: AsRef<Path>, Q: AsRef<Path>>(
        &mut self,
        path: P,
        src: Q,
    ) -> io::Result<()> {
        let md = fs::metadata(src.as_ref())?;
        let mut h = Header::new_gnu();
        h.set_entry_type(EntryType::Directory);
        h.set_mode(file_mode(&md));
        h.set_mtime(0);
        let name = normalize(path.as_ref());
        let name = if name.ends_with('/') {
            name
        } else {
            format!("{name}/")
        };
        self.append_data(&mut h, name, &[][..])
    }

    /// 写一个链接条目（符号链接或硬链接）。
    pub fn append_link<P: AsRef<Path>, T: AsRef<Path>>(
        &mut self,
        header: &mut Header,
        path: P,
        target: T,
    ) -> io::Result<()> {
        let t = normalize_link(target.as_ref());
        if t.len() > 100 {
            self.write_long_name(EntryType::GnuLongLink, &t)?;
        }
        header.set_link_name(&t);
        header.set_size(0);
        // 类型没设过就默认符号链接——硬链接调用方会显式设 `Link`。
        if matches!(header.entry_type(), EntryType::Regular) {
            header.set_entry_type(EntryType::Symlink);
        }
        self.append_data(header, path, &[][..])
    }

    /// 把宿主上的一个文件/目录/符号链接按 `name` 写进归档。
    pub fn append_path_with_name<P: AsRef<Path>, N: AsRef<Path>>(
        &mut self,
        src: P,
        name: N,
    ) -> io::Result<()> {
        let src = src.as_ref();
        let name = normalize(name.as_ref());
        let name = name.as_str();
        let md = if self.follow {
            fs::metadata(src)?
        } else {
            fs::symlink_metadata(src)?
        };
        let mut h = Header::new_gnu();
        h.set_mode(file_mode(&md));
        h.set_mtime(0);
        if md.file_type().is_symlink() {
            let target = fs::read_link(src)?;
            h.set_entry_type(EntryType::Symlink);
            return self.append_link(&mut h, name, target);
        }
        if md.is_dir() {
            h.set_entry_type(EntryType::Directory);
            let name = if name.ends_with('/') {
                name.to_string()
            } else {
                format!("{name}/")
            };
            return self.append_data(&mut h, name, &[][..]);
        }
        let data = fs::read(src)?;
        h.set_entry_type(EntryType::Regular);
        self.append_data(&mut h, name, data.as_slice())
    }

    /// 写归档结尾（两个全零块）。
    pub fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let zero = [0u8; BLOCK];
        self.write_block(&zero)?;
        self.write_block(&zero)?;
        self.w().flush()
    }

    /// 收尾并交回底层写入器。
    pub fn into_inner(mut self) -> io::Result<W> {
        self.finish()?;
        Ok(self.inner.take().expect("Builder 已经 finish"))
    }
}

#[cfg(unix)]
fn file_mode(md: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    md.mode() & 0o7777
}

#[cfg(not(unix))]
fn file_mode(md: &fs::Metadata) -> u32 {
    // Windows 没有 unix 权限位。目录 755、可执行位无从判断，普通文件按
    // 只读与否给 644/444——与 `tar` crate 的做法一致。
    if md.is_dir() {
        0o755
    } else if md.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

/// tar 里的路径一律用 `/`，且不带前导 `./` 与盘符。
fn normalize(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("./").unwrap_or(&s).to_string();
    s
}

fn normalize_link(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ============================================================ Archive

/// tar 读取器。
pub struct Archive<R: Read> {
    inner: R,
}

impl<R: Read> Archive<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    /// 顺序遍历条目。
    ///
    /// 与 `tar` crate 一样返回迭代器，但**条目内容是预读好的**（`Entry`
    /// 持有 `Vec<u8>`）——层里的单个文件不会大到放不下，换来的是调用方
    /// 不必按顺序消费数据流，少一类"忘了读完就下一条"的错位缺陷。
    pub fn entries(&mut self) -> io::Result<Entries<'_, R>> {
        Ok(Entries {
            inner: &mut self.inner,
            done: false,
            pending_path: None,
            pending_link: None,
        })
    }
}

/// 一个 tar 条目。
pub struct Entry {
    header: Header,
    path: PathBuf,
    link: Option<PathBuf>,
    data: Vec<u8>,
}

impl Entry {
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// 条目路径（已合并 GNU 长名 / PAX `path`）。
    pub fn path(&self) -> io::Result<std::borrow::Cow<'_, Path>> {
        Ok(std::borrow::Cow::Borrowed(&self.path))
    }

    /// 链接目标（已合并 GNU 长链接 / PAX `linkpath`）。
    pub fn link_name(&self) -> io::Result<Option<std::borrow::Cow<'_, Path>>> {
        Ok(self.link.as_deref().map(std::borrow::Cow::Borrowed))
    }

    pub fn size(&self) -> u64 {
        self.data.len() as u64
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// 把条目落到 `dest`。
    ///
    /// **不做任何路径校验**：调用方要先决定 `dest` 是否安全（见模块注释）。
    pub fn unpack(&self, dest: &Path) -> io::Result<()> {
        match self.header.entry_type() {
            EntryType::Directory => {
                fs::create_dir_all(dest)?;
                set_mode(dest, self.header.mode())
            }
            EntryType::Symlink => {
                let target = self
                    .link
                    .clone()
                    .ok_or_else(|| err("符号链接条目没有目标"))?;
                symlink(&target, dest)
            }
            EntryType::Link => {
                let target = self.link.clone().ok_or_else(|| err("硬链接条目没有目标"))?;
                fs::hard_link(&target, dest)
            }
            EntryType::Regular => {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(dest, &self.data)?;
                set_mode(dest, self.header.mode())
            }
            // 设备节点、FIFO 之类：普通用户建不出来，也不该出现在 rootfs 的
            // 使用路径上。明确跳过而不是假装成功建了个空文件。
            _ => Ok(()),
        }
    }
}

impl Read for Entry {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data.drain(..n);
        Ok(n)
    }
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_p: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &Path, dest: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, dest)
}

#[cfg(windows)]
fn symlink(target: &Path, dest: &Path) -> io::Result<()> {
    // Windows 建符号链接要开发者模式或管理员权限；失败要如实报错，
    // 不能悄悄降级成拷贝——那会让 rootfs 与镜像声明的不一致。
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, dest)
    } else {
        std::os::windows::fs::symlink_file(target, dest)
    }
}

/// 条目迭代器。
pub struct Entries<'a, R: Read> {
    inner: &'a mut R,
    done: bool,
    /// GNU `L` / PAX `path` 攒下来的、给下一条真实条目用的路径。
    pending_path: Option<String>,
    pending_link: Option<String>,
}

impl<R: Read> Entries<'_, R> {
    fn read_block(&mut self) -> io::Result<Option<[u8; BLOCK]>> {
        let mut b = [0u8; BLOCK];
        let mut got = 0;
        while got < BLOCK {
            match self.inner.read(&mut b[got..])? {
                0 => break,
                n => got += n,
            }
        }
        if got == 0 {
            return Ok(None);
        }
        if got < BLOCK {
            return Err(err("tar 头部截断"));
        }
        Ok(Some(b))
    }

    fn read_data(&mut self, size: u64) -> io::Result<Vec<u8>> {
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        let mut buf = vec![0u8; padded as usize];
        let mut got = 0usize;
        while got < buf.len() {
            match self.inner.read(&mut buf[got..])? {
                0 => return Err(err("tar 条目数据截断")),
                n => got += n,
            }
        }
        buf.truncate(size as usize);
        Ok(buf)
    }

    fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        loop {
            let Some(block) = self.read_block()? else {
                self.done = true;
                return Ok(None);
            };
            let header = Header { bytes: block };
            if header.is_zero() {
                // 结尾标记。后面即使还有字节也不再读（GNU tar 的行为）。
                self.done = true;
                return Ok(None);
            }
            if !header.checksum_ok() {
                return Err(err("tar 头部校验和不匹配"));
            }
            let size = header.size()?;
            let data = self.read_data(size)?;

            match header.entry_type() {
                EntryType::GnuLongName => {
                    self.pending_path = Some(cstr_from(&data));
                    continue;
                }
                EntryType::GnuLongLink => {
                    self.pending_link = Some(cstr_from(&data));
                    continue;
                }
                EntryType::XHeader | EntryType::XGlobalHeader => {
                    // PAX：`<len> <key>=<value>\n`。只取路径两键。
                    for (k, v) in parse_pax(&data) {
                        match k.as_str() {
                            "path" => self.pending_path = Some(v),
                            "linkpath" => self.pending_link = Some(v),
                            _ => {}
                        }
                    }
                    continue;
                }
                _ => {}
            }

            let path = match self.pending_path.take() {
                Some(p) => PathBuf::from(p),
                None => header.path(),
            };
            let link = match self.pending_link.take() {
                Some(l) => Some(PathBuf::from(l)),
                None => header.link_name(),
            };
            return Ok(Some(Entry {
                header,
                path,
                link,
                data,
            }));
        }
    }
}

impl<R: Read> Iterator for Entries<'_, R> {
    type Item = io::Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_entry() {
            Ok(Some(e)) => Some(Ok(e)),
            Ok(None) => None,
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

fn cstr_from(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).into_owned()
}

/// 解析 PAX 记录：`<十进制长度> <键>=<值>\n`。
fn parse_pax(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let Some(sp) = data[i..].iter().position(|&b| b == b' ') else {
            break;
        };
        let Ok(len) = std::str::from_utf8(&data[i..i + sp])
            .unwrap_or("")
            .parse::<usize>()
        else {
            break;
        };
        if len == 0 || i + len > data.len() {
            break;
        }
        let rec = &data[i + sp + 1..i + len];
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(eq) = rec.iter().position(|&b| b == b'=') {
            out.push((
                String::from_utf8_lossy(&rec[..eq]).into_owned(),
                String::from_utf8_lossy(&rec[eq + 1..]).into_owned(),
            ));
        }
        i += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(entries: &[(&str, EntryType, &str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut b = Builder::new(&mut out);
            for (path, ty, link, data) in entries {
                let mut h = Header::new_gnu();
                h.set_entry_type(*ty);
                h.set_mode(0o644);
                if link.is_empty() {
                    b.append_data(&mut h, path, *data).unwrap();
                } else {
                    b.append_link(&mut h, path, link).unwrap();
                }
            }
            b.finish().unwrap();
        }
        out
    }

    fn read_all(data: &[u8]) -> Vec<(String, EntryType, Option<String>, Vec<u8>)> {
        let mut ar = Archive::new(data);
        ar.entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.path().unwrap().to_string_lossy().into_owned(),
                    e.header().entry_type(),
                    e.link_name()
                        .unwrap()
                        .map(|p| p.to_string_lossy().into_owned()),
                    e.data().to_vec(),
                )
            })
            .collect()
    }

    #[test]
    fn round_trips_regular_dir_and_links() {
        let tar = build(&[
            ("a.txt", EntryType::Regular, "", b"hello"),
            ("d/", EntryType::Directory, "", b""),
            ("d/b.bin", EntryType::Regular, "", &[0u8, 1, 2, 255]),
            ("link", EntryType::Symlink, "a.txt", b""),
            ("hard", EntryType::Link, "a.txt", b""),
        ]);
        let got = read_all(&tar);
        assert_eq!(got.len(), 5);
        assert_eq!(got[0].0, "a.txt");
        assert_eq!(got[0].3, b"hello");
        assert_eq!(got[1].1, EntryType::Directory);
        assert_eq!(got[2].3, vec![0u8, 1, 2, 255]);
        assert_eq!(got[3].1, EntryType::Symlink);
        assert_eq!(got[3].2.as_deref(), Some("a.txt"));
        assert_eq!(got[4].1, EntryType::Link);
    }

    #[test]
    fn archive_is_block_aligned_and_terminated() {
        // 别的实现按 512 块读；长度不对齐或缺结尾块会被判成损坏。
        let tar = build(&[("x", EntryType::Regular, "", b"1234567890")]);
        assert_eq!(tar.len() % BLOCK, 0);
        assert!(tar[tar.len() - 1024..].iter().all(|&b| b == 0));
    }

    #[test]
    fn long_paths_use_gnu_extension() {
        let long = format!("{}/f.txt", "d".repeat(150));
        let tar = build(&[(&long, EntryType::Regular, "", b"x")]);
        let got = read_all(&tar);
        assert_eq!(got.len(), 1, "L 头部不该作为独立条目露给调用方");
        assert_eq!(got[0].0, long);
        assert_eq!(got[0].3, b"x");
    }

    #[test]
    fn long_link_targets_use_gnu_extension() {
        let target = format!("/{}/t", "x".repeat(140));
        let tar = build(&[("l", EntryType::Symlink, &target, b"")]);
        let got = read_all(&tar);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].2.as_deref(), Some(target.as_str()));
    }

    #[test]
    fn non_regular_entries_carry_no_data() {
        // 目录/链接带 size 会让后续条目整体错位——这是 tar 最典型的一类坏法。
        let tar = build(&[
            ("d/", EntryType::Directory, "", b"should be dropped"),
            ("after.txt", EntryType::Regular, "", b"ok"),
        ]);
        let got = read_all(&tar);
        assert_eq!(got.len(), 2);
        assert!(got[0].3.is_empty());
        assert_eq!(got[1].0, "after.txt");
        assert_eq!(got[1].3, b"ok");
    }

    #[test]
    fn reads_ustar_prefix_field() {
        // 别的实现会把长路径拆成 prefix + name，而不是用 GNU L 条目。
        let mut h = Header::new_gnu();
        h.set_entry_type(EntryType::Regular);
        h.set_mode(0o644);
        h.set_size(2);
        h.set_str(0, 100, "name.txt");
        h.set_str(345, 155, "some/prefix");
        h.set_cksum();
        let mut tar = h.bytes.to_vec();
        tar.extend_from_slice(b"hi");
        tar.extend_from_slice(&[0u8; BLOCK - 2]);
        tar.extend_from_slice(&[0u8; BLOCK * 2]);
        let got = read_all(&tar);
        assert_eq!(got[0].0, "some/prefix/name.txt");
    }

    #[test]
    fn reads_pax_path_records() {
        let long = "p".repeat(120);
        let body = format!("path={long}\n");
        // PAX 记录的长度字段把它自己也算进去，所以要解一个不动点。
        let mut total = body.len() + 2;
        while total.to_string().len() + 1 + body.len() != total {
            total = total.to_string().len() + 1 + body.len();
        }
        let rec = format!("{total} {body}");
        let mut out = Vec::new();
        {
            let mut b = Builder::new(&mut out);
            let mut h = Header::new_gnu();
            h.set_entry_type(EntryType::XHeader);
            // XHeader 不是 Regular，append_data 会清掉数据，所以手写 append
            h.set_path("PaxHeaders/0");
            h.set_size(rec.len() as u64);
            h.set_cksum();
            b.write_block(&h.bytes).unwrap();
            b.write_padded(rec.as_bytes()).unwrap();

            let mut h2 = Header::new_gnu();
            h2.set_entry_type(EntryType::Regular);
            b.append_data(&mut h2, "short", &b"data"[..]).unwrap();
            b.finish().unwrap();
        }
        let got = read_all(&out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, long, "PAX 的 path 应当覆盖头部里的短名");
    }

    #[test]
    fn rejects_corrupt_checksum() {
        let mut tar = build(&[("a", EntryType::Regular, "", b"x")]);
        tar[0] = b'Z'; // 改名字但不重算校验和
        let mut ar = Archive::new(&tar[..]);
        let first = ar.entries().unwrap().next().unwrap();
        assert!(first.is_err(), "校验和不匹配必须报错");
    }

    #[test]
    fn reads_base256_size_field() {
        // GNU 对 >8 GiB 的文件用 base-256 编码 size。这里只验证解析，
        // 不真的造一个那么大的归档。
        let mut h = Header::new_gnu();
        h.bytes[124] = 0x80;
        h.bytes[124 + 11] = 0x2a;
        assert_eq!(h.size().unwrap(), 42);
    }

    #[test]
    fn unpack_writes_files_and_links() {
        let dir = std::env::temp_dir().join(format!("wbox-tar-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let tar = build(&[
            ("f.txt", EntryType::Regular, "", b"content"),
            ("sub/", EntryType::Directory, "", b""),
            ("l", EntryType::Symlink, "f.txt", b""),
        ]);
        let mut ar = Archive::new(&tar[..]);
        for e in ar.entries().unwrap() {
            let e = e.unwrap();
            e.unpack(&dir.join(e.path().unwrap())).unwrap();
        }
        assert_eq!(fs::read(dir.join("f.txt")).unwrap(), b"content");
        assert!(dir.join("sub").is_dir());
        assert_eq!(fs::read(dir.join("l")).unwrap(), b"content");
        fs::remove_dir_all(&dir).unwrap();
    }
}
