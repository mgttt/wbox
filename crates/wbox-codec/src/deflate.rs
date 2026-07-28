//! DEFLATE（RFC 1951）与 gzip（RFC 1952）。取代 `flate2` + `miniz_oxide`。
//!
//! 两个方向都要：**解压**用于 pull 时解 `tar+gzip` 层，**压缩**用于 push /
//! build 时打包层。
//!
//! # 解压这一侧必须容得下别人产生的一切合法码流
//!
//! 层是 docker/buildah/containerd 压出来的，动态 Huffman、任意块划分、
//! 32 KiB 窗口内的任意回溯都会出现。所以解码器是完整实现，三种块类型
//! （stored / fixed Huffman / dynamic Huffman）全支持。
//!
//! # 压缩这一侧只需要"合法且够用"
//!
//! 我们压出来的东西只有 registry 要读，它只要求是合法 DEFLATE。所以编码器
//! 走**固定 Huffman + 哈希链查最长匹配**这一档：实现小、无专利/无表构造，
//! 压缩率比 zlib 的 level 6 略差，速度相当。**不做动态 Huffman 编码**——
//! 那要额外实现一次 Huffman 树构造与码长限制，收益只是几个百分点的体积。
//!
//! 权衡的落点：解码器面对的是外部输入，必须完整且对畸形码流报错；编码器
//! 面对的是自己的输出，简单可靠优先。

use std::io::{self, Read, Write};

// ============================================================ 位读取

/// LSB-first 的位流读取器（DEFLATE 的位序）。
struct BitReader<'a> {
    data: &'a [u8],
    /// 下一个未读字节。
    pos: usize,
    /// 位缓冲，低位先出。
    bits: u64,
    /// `bits` 里有效的位数。
    n: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            bits: 0,
            n: 0,
        }
    }

    fn refill(&mut self) {
        while self.n <= 56 && self.pos < self.data.len() {
            self.bits |= (self.data[self.pos] as u64) << self.n;
            self.pos += 1;
            self.n += 8;
        }
    }

    fn need(&mut self, count: u32) -> io::Result<()> {
        if self.n < count {
            self.refill();
        }
        if self.n < count {
            return Err(err("DEFLATE 码流意外结束"));
        }
        Ok(())
    }

    fn take(&mut self, count: u32) -> io::Result<u32> {
        if count == 0 {
            return Ok(0);
        }
        self.need(count)?;
        let v = (self.bits & ((1u64 << count) - 1)) as u32;
        self.bits >>= count;
        self.n -= count;
        Ok(v)
    }

    /// 丢弃到字节边界（stored 块前要做）。
    fn align(&mut self) {
        let drop = self.n % 8;
        self.bits >>= drop;
        self.n -= drop;
    }

    /// 从缓冲里直接取整字节（stored 块的负载）。
    fn read_bytes(&mut self, out: &mut Vec<u8>, len: usize) -> io::Result<()> {
        for _ in 0..len {
            self.need(8)?;
            out.push((self.bits & 0xff) as u8);
            self.bits >>= 8;
            self.n -= 8;
        }
        Ok(())
    }
}

fn err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

// ============================================================ Huffman 解码

/// 规范 Huffman 解码表。
///
/// 用"按码长逐级比较"的经典算法（RFC 1951 附录给的那个），不建大查找表：
/// 层 tar 解压是一次性的 I/O 密集操作，查表带来的常数优化不值得多一份
/// 需要正确性论证的代码。
struct Huffman {
    /// `counts[l]` = 码长为 l 的符号个数。
    counts: [u16; MAX_BITS + 1],
    /// 按（码长, 符号值）排序后的符号表。
    symbols: Vec<u16>,
}

const MAX_BITS: usize = 15;

impl Huffman {
    /// 由码长数组构造。码长 0 表示该符号不参与编码。
    fn new(lengths: &[u8]) -> io::Result<Self> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &l in lengths {
            if l as usize > MAX_BITS {
                return Err(err("Huffman 码长越界"));
            }
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        // 校验码长集合是否可构成前缀码（不允许过完备）。
        let mut left = 1i32;
        for &count in counts.iter().take(MAX_BITS + 1).skip(1) {
            left <<= 1;
            left -= count as i32;
            if left < 0 {
                return Err(err("Huffman 码表过完备"));
            }
        }
        // `offs[l]` 是码长 l 的符号在 `symbols` 里的起始下标；填完之后
        // `symbols` 就是按（码长, 符号值）排好序的表，正是解码要的顺序。
        let mut offs = [0u16; MAX_BITS + 1];
        let mut total = 0u16;
        for l in 1..=MAX_BITS {
            offs[l] = total;
            total += counts[l];
        }
        let mut symbols = vec![0u16; total as usize];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, br: &mut BitReader<'_>) -> io::Result<u16> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..=MAX_BITS {
            code |= br.take(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(err("Huffman 码字非法"))
    }
}

// ============================================================ 解压

/// 长度码 257..=285 的基值与额外位。
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// 距离码 0..=29 的基值与额外位。
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// 码长字母表的读取顺序（RFC 1951 §3.2.7）。
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// 解压裸 DEFLATE 码流。
pub fn inflate(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 4);
    let mut br = BitReader::new(data);
    loop {
        let last = br.take(1)?;
        let btype = br.take(2)?;
        match btype {
            0 => {
                br.align();
                let len = br.take(16)? as usize;
                let nlen = br.take(16)? as usize;
                if len != (!nlen & 0xffff) {
                    return Err(err("stored 块的 LEN/NLEN 不互补"));
                }
                br.read_bytes(&mut out, len)?;
            }
            1 => {
                let (lit, dist) = fixed_tables()?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut br)?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            _ => return Err(err("DEFLATE 块类型非法（3）")),
        }
        if last == 1 {
            break;
        }
    }
    Ok(out)
}

fn fixed_tables() -> io::Result<(Huffman, Huffman)> {
    let mut lit = [0u8; 288];
    for (i, l) in lit.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    // 固定表的距离码全是 5 位，共 32 个；30/31 在规范里"不会出现"，但仍要
    // 占位——否则码表不完备，解码时非法码字会被错当成更长的码继续读下去。
    // 真正的拒绝在 `inflate_block` 里（`dsym >= 30`）。
    let dist = [5u8; 32];
    Ok((Huffman::new(&lit)?, Huffman::new(&dist)?))
}

fn dynamic_tables(br: &mut BitReader<'_>) -> io::Result<(Huffman, Huffman)> {
    let hlit = br.take(5)? as usize + 257;
    let hdist = br.take(5)? as usize + 1;
    let hclen = br.take(4)? as usize + 4;
    let mut clen = [0u8; 19];
    for &idx in CLEN_ORDER.iter().take(hclen) {
        clen[idx] = br.take(3)? as u8;
    }
    let clen_huff = Huffman::new(&clen)?;

    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = clen_huff.decode(br)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return Err(err("码长重复码出现在开头"));
                }
                let prev = lengths[i - 1];
                let n = 3 + br.take(2)? as usize;
                if i + n > lengths.len() {
                    return Err(err("码长重复越界"));
                }
                for _ in 0..n {
                    lengths[i] = prev;
                    i += 1;
                }
            }
            17 => {
                let n = 3 + br.take(3)? as usize;
                if i + n > lengths.len() {
                    return Err(err("码长零填充越界"));
                }
                i += n;
            }
            18 => {
                let n = 11 + br.take(7)? as usize;
                if i + n > lengths.len() {
                    return Err(err("码长零填充越界"));
                }
                i += n;
            }
            _ => return Err(err("码长字母表符号非法")),
        }
    }
    let lit = Huffman::new(&lengths[..hlit])?;
    let dist = Huffman::new(&lengths[hlit..])?;
    Ok((lit, dist))
}

fn inflate_block(
    br: &mut BitReader<'_>,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
) -> io::Result<()> {
    loop {
        let sym = lit.decode(br)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            257..=285 => {
                let idx = sym as usize - 257;
                let len = LEN_BASE[idx] as usize + br.take(LEN_EXTRA[idx] as u32)? as usize;
                let dsym = dist.decode(br)? as usize;
                if dsym >= 30 {
                    return Err(err("距离码非法"));
                }
                let d = DIST_BASE[dsym] as usize + br.take(DIST_EXTRA[dsym] as u32)? as usize;
                if d > out.len() {
                    return Err(err("回溯距离超出已解出的数据"));
                }
                // 重叠拷贝（d < len）是合法且常见的，必须逐字节。
                let start = out.len() - d;
                for k in 0..len {
                    let b = out[start + k];
                    out.push(b);
                }
            }
            _ => return Err(err("字面量/长度码非法")),
        }
    }
}

// ============================================================ 压缩

/// 压缩级别。语义只有"要不要找匹配"这一档区分，见模块注释。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// 不查匹配，全部按字面量编码（仍是合法 DEFLATE，只是不省体积）。
    None,
    /// 短哈希链，快。
    Fast,
    /// 长哈希链，体积更小。
    Default,
}

const WINDOW: usize = 32 * 1024;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: usize = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;

/// LSB-first 的位写入器。
struct BitWriter {
    out: Vec<u8>,
    cur: u32,
    n: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            cur: 0,
            n: 0,
        }
    }

    fn put(&mut self, value: u32, count: u32) {
        self.cur |= value << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push((self.cur & 0xff) as u8);
            self.cur >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman 码字是 MSB-first 定义的，写入时要按位翻转。
    fn put_code(&mut self, code: u32, count: u32) {
        let mut v = 0u32;
        for i in 0..count {
            v |= ((code >> (count - 1 - i)) & 1) << i;
        }
        self.put(v, count);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push((self.cur & 0xff) as u8);
        }
        self.out
    }
}

/// 固定 Huffman 表里字面量/长度符号的（码字, 位数）。
fn fixed_lit_code(sym: u16) -> (u32, u32) {
    match sym {
        0..=143 => (0x30 + sym as u32, 8),
        144..=255 => (0x190 + (sym as u32 - 144), 9),
        256..=279 => (sym as u32 - 256, 7),
        _ => (0xc0 + (sym as u32 - 280), 8),
    }
}

/// 用固定 Huffman 压缩成裸 DEFLATE 码流。
pub fn deflate(data: &[u8], level: Level) -> Vec<u8> {
    let mut bw = BitWriter::new();
    // 单个块，BFINAL=1，BTYPE=01（固定 Huffman）。
    bw.put(1, 1);
    bw.put(1, 2);

    let max_chain = match level {
        Level::None => 0,
        Level::Fast => 8,
        Level::Default => 128,
    };

    // 哈希链：head[h] 是最近一个哈希为 h 的位置，prev[i] 是 i 之前同哈希的位置。
    let mut head = vec![usize::MAX; HASH_SIZE];
    let mut prev = vec![usize::MAX; data.len().max(1)];

    let hash3 = |d: &[u8], i: usize| -> usize {
        ((d[i] as usize) << 10 ^ (d[i + 1] as usize) << 5 ^ (d[i + 2] as usize)) & (HASH_SIZE - 1)
    };

    let mut i = 0usize;
    while i < data.len() {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if max_chain > 0 && i + MIN_MATCH <= data.len() {
            let h = hash3(data, i);
            let mut cand = head[h];
            let mut chain = max_chain;
            while cand != usize::MAX && chain > 0 {
                let dist = i - cand;
                if dist > WINDOW {
                    break;
                }
                // 先比"能否超过当前最好长度"的那一位，绝大多数候选一比就淘汰。
                if best_len == 0 || data.get(cand + best_len) == data.get(i + best_len) {
                    let mut l = 0usize;
                    let limit = MAX_MATCH.min(data.len() - i);
                    while l < limit && data[cand + l] == data[i + l] {
                        l += 1;
                    }
                    if l > best_len {
                        best_len = l;
                        best_dist = dist;
                        if l >= limit {
                            break;
                        }
                    }
                }
                cand = prev[cand];
                chain -= 1;
            }
        }

        if best_len >= MIN_MATCH {
            let (lsym, lextra, lbits) = length_code(best_len);
            let (code, bits) = fixed_lit_code(lsym);
            bw.put_code(code, bits);
            bw.put(lextra, lbits);
            let (dsym, dextra, dbits) = distance_code(best_dist);
            bw.put_code(dsym as u32, 5);
            bw.put(dextra, dbits);
            // 匹配覆盖的每个位置都要进哈希链，否则后续匹配会漏掉。
            for k in 0..best_len {
                let p = i + k;
                if p + MIN_MATCH <= data.len() {
                    let h = hash3(data, p);
                    prev[p] = head[h];
                    head[h] = p;
                }
            }
            i += best_len;
        } else {
            let (code, bits) = fixed_lit_code(data[i] as u16);
            bw.put_code(code, bits);
            if i + MIN_MATCH <= data.len() {
                let h = hash3(data, i);
                prev[i] = head[h];
                head[h] = i;
            }
            i += 1;
        }
    }

    // 块结束符 256。
    let (code, bits) = fixed_lit_code(256);
    bw.put_code(code, bits);
    bw.finish()
}

/// 长度 -> （符号, 额外位值, 额外位数）。
fn length_code(len: usize) -> (u16, u32, u32) {
    debug_assert!((MIN_MATCH..=MAX_MATCH).contains(&len));
    let mut idx = 28;
    for (k, &base) in LEN_BASE.iter().enumerate() {
        if (len as u16) < base {
            idx = k - 1;
            break;
        }
    }
    let extra = len as u32 - LEN_BASE[idx] as u32;
    (257 + idx as u16, extra, LEN_EXTRA[idx] as u32)
}

/// 距离 -> （符号, 额外位值, 额外位数）。
fn distance_code(dist: usize) -> (u16, u32, u32) {
    debug_assert!((1..=WINDOW).contains(&dist));
    let mut idx = 29;
    for (k, &base) in DIST_BASE.iter().enumerate() {
        if (dist as u16) < base {
            idx = k - 1;
            break;
        }
    }
    let extra = dist as u32 - DIST_BASE[idx] as u32;
    (idx as u16, extra, DIST_EXTRA[idx] as u32)
}

// ============================================================ CRC32

/// CRC-32（IEEE 802.3，gzip 用）。查表按需生成，省掉一张常量表。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

// ============================================================ gzip

/// gzip 封装：10 字节固定头 + DEFLATE + CRC32 + 原始长度。
pub fn gzip_compress(data: &[u8], level: Level) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[
        0x1f, 0x8b, // magic
        0x08, // CM = deflate
        0x00, // FLG：无文件名、无注释
        0x00, 0x00, 0x00, 0x00, // MTIME = 0（不写时间：层的字节要可复现）
        match level {
            Level::Default => 0x00,
            _ => 0x04, // XFL：4 = 最快
        },
        0xff, // OS = 未知（同样为了可复现，不暴露宿主）
    ]);
    out.extend_from_slice(&deflate(data, level));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// gzip 解压。校验 CRC32 与长度——层的完整性不能只靠外层 digest，
/// 这里报错比把半截数据当成 rootfs 解出来好得多。
pub fn gzip_decompress(data: &[u8]) -> io::Result<Vec<u8>> {
    if data.len() < 18 {
        return Err(err("gzip 数据太短"));
    }
    if data[0] != 0x1f || data[1] != 0x8b {
        return Err(err("不是 gzip 数据（magic 不匹配）"));
    }
    if data[2] != 0x08 {
        return Err(err("gzip 压缩方法不是 deflate"));
    }
    let flg = data[3];
    let mut p = 10usize;
    if flg & 0x04 != 0 {
        // FEXTRA
        if p + 2 > data.len() {
            return Err(err("gzip FEXTRA 截断"));
        }
        let xlen = u16::from_le_bytes([data[p], data[p + 1]]) as usize;
        p += 2 + xlen;
    }
    if flg & 0x08 != 0 {
        p = skip_cstr(data, p)?; // FNAME
    }
    if flg & 0x10 != 0 {
        p = skip_cstr(data, p)?; // FCOMMENT
    }
    if flg & 0x02 != 0 {
        p += 2; // FHCRC
    }
    if p + 8 > data.len() {
        return Err(err("gzip 头部截断"));
    }
    let body = &data[p..data.len() - 8];
    let out = inflate(body)?;
    let tail = &data[data.len() - 8..];
    let want_crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    let want_len = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
    if crc32(&out) != want_crc {
        return Err(err("gzip CRC32 校验失败"));
    }
    if (out.len() as u32) != want_len {
        return Err(err("gzip 长度校验失败"));
    }
    Ok(out)
}

fn skip_cstr(data: &[u8], mut p: usize) -> io::Result<usize> {
    while p < data.len() && data[p] != 0 {
        p += 1;
    }
    if p >= data.len() {
        return Err(err("gzip 头部字符串未终止"));
    }
    Ok(p + 1)
}

// ============================================================ Read/Write 适配

/// 把一个 `Read` 里的 gzip 数据解出来。
///
/// 一次性读进内存再解——层最大受调用方的 `MAX_RESPONSE_BYTES` 约束，
/// 而流式解压要维护跨块的 32 KiB 窗口状态，复杂度不值得。
pub struct GzDecoder<R: Read> {
    inner: R,
    done: Option<std::io::Cursor<Vec<u8>>>,
}

impl<R: Read> GzDecoder<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, done: None }
    }
}

impl<R: Read> Read for GzDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done.is_none() {
            let mut raw = Vec::new();
            self.inner.read_to_end(&mut raw)?;
            self.done = Some(std::io::Cursor::new(gzip_decompress(&raw)?));
        }
        self.done.as_mut().unwrap().read(buf)
    }
}

/// 往里写原始字节，`finish()` 拿到 gzip 字节。
pub struct GzEncoder<W: Write> {
    inner: W,
    buf: Vec<u8>,
    level: Level,
    finished: bool,
}

impl<W: Write> GzEncoder<W> {
    pub fn new(inner: W, level: Level) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            level,
            finished: false,
        }
    }

    /// 压缩并写出，可重复调用（第二次起是空操作）。
    pub fn try_finish(&mut self) -> io::Result<()> {
        if !self.finished {
            let gz = gzip_compress(&self.buf, self.level);
            self.inner.write_all(&gz)?;
            self.finished = true;
        }
        Ok(())
    }

    /// 收尾并交回底层写入器。
    pub fn finish(mut self) -> io::Result<W> {
        self.try_finish()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for GzEncoder<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8], level: Level) {
        let c = deflate(data, level);
        let back = inflate(&c).expect("inflate 失败");
        assert_eq!(back, data, "level={level:?} len={}", data.len());
    }

    #[test]
    fn roundtrips_various_shapes() {
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            b"hello hello hello hello hello".to_vec(),
            vec![0u8; 100_000],                          // 极端可压缩
            (0..70_000u32).map(|i| (i % 256) as u8).collect(), // 周期性、超窗口
            {
                // 伪随机：几乎不可压缩，走的是字面量路径
                let mut v = Vec::with_capacity(50_000);
                let mut s = 12345u32;
                for _ in 0..50_000 {
                    s = s.wrapping_mul(1103515245).wrapping_add(12345);
                    v.push((s >> 16) as u8);
                }
                v
            },
        ];
        for c in cases {
            for lv in [Level::None, Level::Fast, Level::Default] {
                roundtrip(&c, lv);
            }
        }
    }

    #[test]
    fn actually_compresses() {
        // 不只是"能解回来"，还要真的变小——否则匹配查找其实没生效也会绿。
        let data = b"the quick brown fox jumps over the lazy dog. ".repeat(500);
        let c = deflate(&data, Level::Default);
        assert!(
            c.len() < data.len() / 8,
            "压缩率不合理：{} -> {}",
            data.len(),
            c.len()
        );
    }

    #[test]
    fn overlapping_match_copies_byte_by_byte() {
        // dist < len 的重叠回溯是 RLE 的常见形态，必须逐字节展开。
        let data = b"ababababababababababababababababab".to_vec();
        roundtrip(&data, Level::Default);
    }

    #[test]
    fn gzip_roundtrip_and_integrity() {
        let data = b"wbox gzip layer payload".repeat(100);
        let gz = gzip_compress(&data, Level::Default);
        assert_eq!(&gz[..3], &[0x1f, 0x8b, 0x08]);
        assert_eq!(gzip_decompress(&gz).unwrap(), data);

        // 篡改一个字节必须被 CRC 抓住，而不是解出半截数据。
        let mut bad = gz.clone();
        let n = bad.len();
        bad[n - 5] ^= 0xff;
        assert!(gzip_decompress(&bad).is_err());
    }

    #[test]
    fn gzip_is_reproducible() {
        // 层的字节要可复现：同样输入两次压出来必须一模一样（不写 mtime）。
        let data = b"reproducible".repeat(50);
        assert_eq!(
            gzip_compress(&data, Level::Default),
            gzip_compress(&data, Level::Default)
        );
    }

    #[test]
    fn rejects_malformed_streams() {
        assert!(inflate(&[]).is_err());
        assert!(inflate(&[0x06]).is_err()); // BTYPE=3
        assert!(gzip_decompress(&[0u8; 20]).is_err());
        assert!(gzip_decompress(b"short").is_err());
        // stored 块 LEN/NLEN 不互补
        let bad = [0x01, 0x05, 0x00, 0x00, 0x00, b'x'];
        assert!(inflate(&bad).is_err());
    }

    #[test]
    fn decodes_stored_blocks() {
        // 我们自己不产生 stored 块，但别人会。
        let payload = b"stored payload";
        let mut s = vec![0x01, payload.len() as u8, 0x00];
        s.push(!(payload.len() as u8));
        s.push(0xff);
        s.extend_from_slice(payload);
        assert_eq!(inflate(&s).unwrap(), payload);
    }

    #[test]
    fn decodes_dynamic_huffman_from_reference_encoder() {
        // **这条是解压侧最重要的一条**：真实镜像层是 zlib/libdeflate 压出来的
        // 动态 Huffman 块，而我们自己的编码器只产生固定 Huffman——只测
        // "自产自销"的往返，动态块那条路一行都不会被走到。
        //
        // 夹具由 CPython 的 zlib（level 9）产生，内容是一段中英混排文本，
        // 已确认块类型为 BTYPE=10（动态）。
        const GZ_B64: &str = concat!(
            "H4sIAAAAAAACAyXOvQ7BYBSA4b1XcRKzixCbxWildKhIv6aaMDb+wydFpKEqYpA0Ikykfnsxzmk/",
            "k1tATO/25E1ATWZ1Scr/kododsDAwmCLwRAfS3F04vUOrycxd6PBirgTuy3QmWEW5IoCIlzE/oD2",
            "Z5r7T6tB+yZ5PuRUrcRqVcCgj/cwnvpSStfTTDMLqqYYQBMOGSZDVi4rRfNrjMSaR94u8i4/7GK/",
            "bxzPK7IPeN28LFeE3ZdrE29Tr0P2+Dvwn01WVO0DxwgSjr4AAAA="
        );
        const RAW_B64: &str = concat!(
            "IyB3Ym94Cgpgd2JveGAg5piv5LiA5Liq5LiN5L6d6LWW56Gs5Lu26Jma5ouf5YyW55qEIHBvcnRh",
            "YmxlIOi/m+eoi+WuueWZqOOAguWug+WcqCBXaW5kb3dzIOS4iuS9v+eUqApBcHBDb250YWluZXIg",
            "5ZKMIEpvYiBPYmplY3Qg6L+Q6KGM5pys5py656iL5bqP77yM5Lmf5Y+v5Lul6YCa6L+H6ZqP5YyF",
            "5YiG5Y+R55qECmB3Ym94LWxpbg=="
        );
        let gz = crate::base64::decode(GZ_B64).unwrap();
        let raw = crate::base64::decode(RAW_B64).unwrap();
        // 先确认夹具真的是动态块（BFINAL=1, BTYPE=10 → 低三位 0b101）。
        assert_eq!(gz[10] & 0b111, 0b101, "夹具不是动态 Huffman 块");
        assert_eq!(gzip_decompress(&gz).unwrap(), raw);
    }

    #[test]
    fn read_write_adapters_pair_up() {
        let data = b"adapter payload".repeat(200);
        let mut enc = GzEncoder::new(Vec::new(), Level::Fast);
        enc.write_all(&data).unwrap();
        let gz = enc.finish().unwrap();
        let mut out = Vec::new();
        GzDecoder::new(&gz[..]).read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }
}
