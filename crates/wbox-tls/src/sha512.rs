//! SHA-384 / SHA-512（FIPS 180-4）。
//!
//! SHA-256 已经在 `wbox-codec` 里了，这里只补 64 位那一族。为什么需要它：
//! 证书链上大量签名是 `sha384WithRSAEncryption` 或 `ecdsa-with-SHA384`
//! （DigiCert 的 P-384 根就是），TLS 1.3 的 `TLS_AES_256_GCM_SHA384` 套件
//! 的密钥调度也走 SHA-384。
//!
//! SHA-384 与 SHA-512 是同一套压缩函数，只差初始向量与输出截断长度。

/// SHA-512 轮常量（前 80 个素数立方根小数部分的前 64 位）。
const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

const H512: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

const H384: [u64; 8] = [
    0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939,
    0x67332667ffc00b31, 0x8eb44a8768581511, 0xdb0c2e0d64f98fa7, 0x47b5481dbefa4fa4,
];

/// 增量式 SHA-512 家族。`OUT` 是输出字节数：48 = SHA-384，64 = SHA-512。
#[derive(Clone)]
pub struct Sha512<const OUT: usize> {
    state: [u64; 8],
    buf: [u8; 128],
    buf_len: usize,
    /// 已吸收的总字节数。SHA-512 的长度字段是 128 位，但 wbox 不会哈希
    /// 超过 2^64 字节的东西，所以高 64 位恒为 0。
    total: u64,
}

impl<const OUT: usize> Default for Sha512<OUT> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const OUT: usize> Sha512<OUT> {
    pub fn new() -> Self {
        // 输出长度决定初始向量，这是 SHA-384 与 SHA-512 唯一的两处差异之一。
        let state = match OUT {
            48 => H384,
            64 => H512,
            _ => panic!("SHA-512 家族只支持 48（SHA-384）与 64（SHA-512）字节输出"),
        };
        Self {
            state,
            buf: [0u8; 128],
            buf_len: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (128 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 128 {
                let b = self.buf;
                self.compress(&b);
                self.buf_len = 0;
            }
        }
        while data.len() >= 128 {
            let (block, rest) = data.split_at(128);
            let mut b = [0u8; 128];
            b.copy_from_slice(block);
            self.compress(&b);
            data = rest;
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    pub fn finalize(mut self) -> [u8; OUT] {
        let bit_len = self.total.wrapping_mul(8);
        self.raw(&[0x80]);
        // 补零到 112 mod 128，末尾 16 字节是 128 位大端位长。
        while self.buf_len != 112 {
            self.raw(&[0x00]);
        }
        self.raw(&[0u8; 8]); // 位长高 64 位恒为 0
        self.raw(&bit_len.to_be_bytes());
        debug_assert_eq!(self.buf_len, 0);

        let mut full = [0u8; 64];
        for (i, w) in self.state.iter().enumerate() {
            full[i * 8..i * 8 + 8].copy_from_slice(&w.to_be_bytes());
        }
        let mut out = [0u8; OUT];
        out.copy_from_slice(&full[..OUT]);
        out
    }

    fn raw(&mut self, data: &[u8]) {
        for &b in data {
            self.buf[self.buf_len] = b;
            self.buf_len += 1;
            if self.buf_len == 128 {
                let blk = self.buf;
                self.compress(&blk);
                self.buf_len = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 128]) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut v = [0u8; 8];
            v.copy_from_slice(&block[i * 8..i * 8 + 8]);
            w[i] = u64::from_be_bytes(v);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
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
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
}

/// SHA-384 一次性摘要。
pub fn sha384(data: &[u8]) -> [u8; 48] {
    let mut h = Sha512::<48>::new();
    h.update(data);
    h.finalize()
}

/// SHA-512 一次性摘要。
pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut h = Sha512::<64>::new();
    h.update(data);
    h.finalize()
}

/// HMAC-SHA384（RFC 2104）。块长 128 字节。
pub fn hmac_sha384(key: &[u8], msg: &[u8]) -> [u8; 48] {
    let mut k = [0u8; 128];
    if key.len() > 128 {
        k[..48].copy_from_slice(&sha384(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 128];
    let mut opad = [0x5cu8; 128];
    for i in 0..128 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha512::<48>::new();
    inner.update(&ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha512::<48>::new();
    outer.update(&opad);
    outer.update(&inner);
    outer.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wbox_codec::sha256::hex;

    #[test]
    fn fips_vectors_sha384() {
        assert_eq!(
            hex(&sha384(b"")),
            "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b"
        );
        assert_eq!(
            hex(&sha384(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
    }

    #[test]
    fn fips_vectors_sha512() {
        assert_eq!(
            hex(&sha512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(
            hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn long_message_crosses_many_blocks() {
        // 跨块 + padding 需要额外一块的分支都会走到。
        let mut h = Sha512::<64>::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(
            hex(&h.finalize()),
            "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
        );
    }

    #[test]
    fn incremental_matches_oneshot() {
        let data: Vec<u8> = (0u32..3000).map(|i| (i % 251) as u8).collect();
        let expect = sha384(&data);
        for chunk in [1usize, 13, 127, 128, 129, 4096] {
            let mut h = Sha512::<48>::new();
            for part in data.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(h.finalize(), expect, "chunk={chunk}");
        }
    }

    #[test]
    fn padding_boundary_lengths() {
        // 111/112/113 是 SHA-512 的 padding 分界：112 起就要多压一块。
        // 这几个长度的官方向量不好找，改成钉住"分片喂入 == 一次性"这条
        // 不变式——padding 分支写错了两条路必然不一致。
        for n in [110usize, 111, 112, 113, 127, 128, 129, 240] {
            let data = vec![b'x'; n];
            let mut h = Sha512::<64>::new();
            for b in data.chunks(7) {
                h.update(b);
            }
            assert_eq!(h.finalize(), sha512(&data), "n={n}");
        }
    }

    #[test]
    fn hmac_rfc4231_vectors() {
        // RFC 4231 test case 1 与 2（SHA-384 那两行）。
        assert_eq!(
            hex(&hmac_sha384(&[0x0b; 20], b"Hi There")),
            "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6"
        );
        assert_eq!(
            hex(&hmac_sha384(b"Jefe", b"what do ya want for nothing?")),
            "af45d2e376484031617f78d2b58a6b1b9c7ef464f5a01b47e42ec3736322445e8e2240ca5e69e2c78b3239ecfab21649"
        );
    }
}
