//! SHA-256（FIPS 180-4）。取代 `sha2` crate。
//!
//! 用途只有两处，但都不能出错：OCI blob 的 digest 校验（拉下来的层是不是
//! 我们要的那一份）与构建缓存键。实现是教科书式的直译，没有任何平台相关
//! 代码，因此两个宿主上逐位一致。
//!
//! 正确性靠 FIPS 的官方测试向量 + 长消息（跨多个块、含所有 padding 分支）
//! 钉住，见文件末尾的测试。

/// SHA-256 轮常量（前 64 个素数立方根小数部分的前 32 位）。
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// 初始哈希值（前 8 个素数平方根小数部分的前 32 位）。
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// 增量式 SHA-256。
///
/// 增量而不是一次性，是因为构建缓存键要把「上一步的键 + 指令 + 若干文件内容」
/// 顺序喂进同一个哈希，文件可能很大，不该先拼成一个 `Vec`。
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    /// 未满一块（64 字节）的尾巴。
    buf: [u8; 64],
    buf_len: usize,
    /// 已吸收的总字节数，padding 时要按位数写进末尾 8 字节。
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: H0,
            buf: [0u8; 64],
            buf_len: 0,
            total: 0,
        }
    }

    /// 吸收一段数据。可以任意次调用，与一次性喂入等价。
    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        // 先补齐上次剩下的半块。
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        // 整块直接压缩，不经过缓冲区。
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            let mut b = [0u8; 64];
            b.copy_from_slice(block);
            self.compress(&b);
            data = rest;
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    /// 收尾并输出 32 字节摘要。
    pub fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total.wrapping_mul(8);
        // padding：0x80，然后补 0 到 56 mod 64，最后 8 字节是大端位长。
        self.update_raw(&[0x80]);
        while self.buf_len != 56 {
            self.update_raw(&[0x00]);
        }
        self.update_raw(&bit_len.to_be_bytes());
        debug_assert_eq!(self.buf_len, 0);

        let mut out = [0u8; 32];
        for (i, w) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
        }
        out
    }

    /// padding 用：不计入 `total`（位长在进入 padding 前就定死了）。
    fn update_raw(&mut self, data: &[u8]) {
        for &b in data {
            self.buf[self.buf_len] = b;
            self.buf_len += 1;
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        let add = [a, b, c, d, e, f, g, h];
        for (s, v) in self.state.iter_mut().zip(add) {
            *s = s.wrapping_add(v);
        }
    }
}

/// 一次性摘要。
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// 小写十六进制字符串（不带 `sha256:` 前缀）。
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// `sha256(data)` 的十六进制。
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

/// HMAC-SHA256（RFC 2104）。TLS 的 HKDF 要它。
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(msg);
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner);
    outer.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fips_vectors() {
        // FIPS 180-4 附录的三个标准向量。
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn million_a() {
        // 跨大量块，且长度不是 64 的倍数——padding 的两条分支都会走到。
        let mut h = Sha256::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(
            hex(&h.finalize()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn incremental_matches_oneshot() {
        // 分片喂入必须与一次性等价，任意切点。
        let data: Vec<u8> = (0u32..5000).map(|i| (i % 251) as u8).collect();
        let expect = sha256(&data);
        for chunk in [1usize, 7, 63, 64, 65, 127, 4096] {
            let mut h = Sha256::new();
            for part in data.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(h.finalize(), expect, "chunk={chunk}");
        }
    }

    #[test]
    fn block_boundary_lengths() {
        // 55/56/57 是 padding 是否要多一块的分界，逐个盯住。
        let expect = [
            (
                55usize,
                "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
            ),
            (
                56,
                "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
            ),
            (
                57,
                "f13b2d724659eb3bf47f2dd6af1accc87b81f09f59f2b75e5c0bed6589dfe8c6",
            ),
            (
                64,
                "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
            ),
        ];
        for (n, want) in expect {
            assert_eq!(sha256_hex(&vec![b'a'; n]), want, "n={n}");
        }
    }

    #[test]
    fn hmac_rfc4231_vectors() {
        // RFC 4231 test case 1 与 2。
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // key 超过一个块（131 字节）时要先哈希——RFC 4231 test case 6。
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
