//! AES-128 / AES-256 分组密码与 GCM 认证加密（FIPS 197 + NIST SP 800-38D）。
//!
//! TLS 1.3 里 `TLS_AES_128_GCM_SHA256` 是**必须实现**的套件，所有 registry
//! 都支持它；`TLS_AES_256_GCM_SHA384` 一并做了，成本只是多几轮密钥扩展。
//!
//! # 关于常量时间
//!
//! **这个实现用查表 S-box，不是常量时间的**，理论上可被缓存计时侧信道攻击。
//! 这一点在 `docs/rust-rewrite.md` §5.1 里如实写明了：wbox 的威胁模型是
//! "从 registry 拉镜像"，攻击者要在同一台机器上与 wbox 争抢缓存才谈得上
//! 利用，而那种情形下他已经能直接读 wbox 的内存了。用比特切片换常量时间
//! 会让实现复杂一个量级，不值得。**不要把这个实现用到别的地方去。**

// ============================================================ AES 分组密码

/// AES S-box（FIPS 197 Figure 7）。
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// 轮常量。AES-256 最多用到 `RCON[6]`。
const RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

/// GF(2^8) 上乘 2（x），模 0x11b。
fn xtime(b: u8) -> u8 {
    (b << 1) ^ (((b >> 7) & 1) * 0x1b)
}

/// GF(2^8) 乘法。只在 MixColumns 里用到小常数，写成通用形式便于核对。
fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    p
}

/// 展开后的 AES 轮密钥。GCM 只需要加密方向（CTR 模式），故不做解密。
pub struct Aes {
    /// 每轮 16 字节，最多 15 轮（AES-256 是 14 轮 + 初始轮）。
    round_keys: [[u8; 16]; 15],
    rounds: usize,
}

impl Aes {
    /// 由 16 字节（AES-128）或 32 字节（AES-256）密钥构造。
    pub fn new(key: &[u8]) -> Self {
        let (nk, rounds) = match key.len() {
            16 => (4usize, 10usize),
            32 => (8, 14),
            n => panic!("AES 密钥长度必须是 16 或 32 字节，收到 {n}"),
        };
        let total_words = 4 * (rounds + 1);
        let mut w = vec![[0u8; 4]; total_words];
        for i in 0..nk {
            w[i].copy_from_slice(&key[i * 4..i * 4 + 4]);
        }
        for i in nk..total_words {
            let mut temp = w[i - 1];
            if i % nk == 0 {
                // RotWord + SubWord + Rcon
                temp = [
                    SBOX[temp[1] as usize] ^ RCON[i / nk],
                    SBOX[temp[2] as usize],
                    SBOX[temp[3] as usize],
                    SBOX[temp[0] as usize],
                ];
            } else if nk > 6 && i % nk == 4 {
                // AES-256 专有的一步；漏掉它 AES-128 照样对，AES-256 全错。
                temp = [
                    SBOX[temp[0] as usize],
                    SBOX[temp[1] as usize],
                    SBOX[temp[2] as usize],
                    SBOX[temp[3] as usize],
                ];
            }
            for j in 0..4 {
                w[i][j] = w[i - nk][j] ^ temp[j];
            }
        }
        let mut round_keys = [[0u8; 16]; 15];
        for r in 0..=rounds {
            for c in 0..4 {
                round_keys[r][c * 4..c * 4 + 4].copy_from_slice(&w[r * 4 + c]);
            }
        }
        Self { round_keys, rounds }
    }

    /// 就地加密一个 16 字节分组。
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        xor_into(block, &self.round_keys[0]);
        for r in 1..self.rounds {
            sub_bytes(block);
            shift_rows(block);
            mix_columns(block);
            xor_into(block, &self.round_keys[r]);
        }
        // 最后一轮没有 MixColumns。
        sub_bytes(block);
        shift_rows(block);
        xor_into(block, &self.round_keys[self.rounds]);
    }
}

fn xor_into(dst: &mut [u8; 16], src: &[u8; 16]) {
    for i in 0..16 {
        dst[i] ^= src[i];
    }
}

fn sub_bytes(s: &mut [u8; 16]) {
    for b in s.iter_mut() {
        *b = SBOX[*b as usize];
    }
}

/// 状态按列优先存放：`s[r + 4c]`。第 r 行左移 r 格。
fn shift_rows(s: &mut [u8; 16]) {
    let t = *s;
    for r in 1..4 {
        for c in 0..4 {
            s[r + 4 * c] = t[r + 4 * ((c + r) % 4)];
        }
    }
}

fn mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let col = [s[4 * c], s[4 * c + 1], s[4 * c + 2], s[4 * c + 3]];
        s[4 * c] = gmul(col[0], 2) ^ gmul(col[1], 3) ^ col[2] ^ col[3];
        s[4 * c + 1] = col[0] ^ gmul(col[1], 2) ^ gmul(col[2], 3) ^ col[3];
        s[4 * c + 2] = col[0] ^ col[1] ^ gmul(col[2], 2) ^ gmul(col[3], 3);
        s[4 * c + 3] = gmul(col[0], 3) ^ col[1] ^ col[2] ^ gmul(col[3], 2);
    }
}

// ============================================================ GHASH

/// GF(2^128) 上的乘法（GCM 的位序：最高位在字节 0 的最高位）。
fn gf_mul(x: &[u8; 16], y: &[u8; 16]) -> [u8; 16] {
    let mut z = [0u8; 16];
    let mut v = *y;
    for i in 0..128 {
        // 从 x 的最高位开始逐位扫描。
        if (x[i / 8] >> (7 - (i % 8))) & 1 == 1 {
            for k in 0..16 {
                z[k] ^= v[k];
            }
        }
        // v <<= 1，若溢出则异或约化多项式 R = 0xe1 || 0^120。
        let lsb = v[15] & 1;
        let mut carry = 0u8;
        for b in v.iter_mut() {
            let new_carry = *b & 1;
            *b = (*b >> 1) | (carry << 7);
            carry = new_carry;
        }
        if lsb == 1 {
            v[0] ^= 0xe1;
        }
    }
    z
}

/// GHASH：对 16 字节分组序列做多项式求值。不足一块的尾巴补零。
fn ghash(h: &[u8; 16], data: &[u8], out: &mut [u8; 16]) {
    for chunk in data.chunks(16) {
        let mut block = [0u8; 16];
        block[..chunk.len()].copy_from_slice(chunk);
        for k in 0..16 {
            out[k] ^= block[k];
        }
        *out = gf_mul(out, h);
    }
}

// ============================================================ AES-GCM

/// AES-GCM 认证加密。TLS 1.3 固定用 12 字节 nonce 与 16 字节 tag。
pub struct AesGcm {
    aes: Aes,
    /// 哈希子密钥 H = E_K(0^128)。
    h: [u8; 16],
}

/// 认证失败。**故意不带任何细节**——区分"tag 不对"与"长度不对"会给攻击者
/// 额外信息，而调用方对这两种情况的处理是一样的：断开连接。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthError;

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AEAD 认证失败")
    }
}

impl std::error::Error for AuthError {}

impl AesGcm {
    pub fn new(key: &[u8]) -> Self {
        let aes = Aes::new(key);
        let mut h = [0u8; 16];
        aes.encrypt_block(&mut h);
        Self { aes, h }
    }

    /// 由 12 字节 nonce 生成初始计数块 J0 = nonce || 0x00000001。
    fn j0(nonce: &[u8; 12]) -> [u8; 16] {
        let mut j = [0u8; 16];
        j[..12].copy_from_slice(nonce);
        j[15] = 1;
        j
    }

    fn ctr_xor(&self, j0: &[u8; 16], data: &mut [u8]) {
        let mut counter = *j0;
        for chunk in data.chunks_mut(16) {
            // 计数器是最后 4 字节的大端整数，从 J0+1 开始。
            let mut c = u32::from_be_bytes([counter[12], counter[13], counter[14], counter[15]]);
            c = c.wrapping_add(1);
            counter[12..].copy_from_slice(&c.to_be_bytes());
            let mut ks = counter;
            self.aes.encrypt_block(&mut ks);
            for (b, k) in chunk.iter_mut().zip(ks.iter()) {
                *b ^= k;
            }
        }
    }

    /// 算认证标签：GHASH(AAD || 0* || C || 0* || len(AAD) || len(C)) ⊕ E_K(J0)。
    fn tag(&self, j0: &[u8; 16], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
        let mut s = [0u8; 16];
        ghash(&self.h, aad, &mut s);
        ghash(&self.h, ciphertext, &mut s);
        let mut lens = [0u8; 16];
        lens[..8].copy_from_slice(&((aad.len() as u64) * 8).to_be_bytes());
        lens[8..].copy_from_slice(&((ciphertext.len() as u64) * 8).to_be_bytes());
        for k in 0..16 {
            s[k] ^= lens[k];
        }
        s = gf_mul(&s, &self.h);
        let mut ek = *j0;
        self.aes.encrypt_block(&mut ek);
        for k in 0..16 {
            s[k] ^= ek[k];
        }
        s
    }

    /// 就地加密，返回 16 字节标签。
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], buf: &mut [u8]) -> [u8; 16] {
        let j0 = Self::j0(nonce);
        self.ctr_xor(&j0, buf);
        self.tag(&j0, aad, buf)
    }

    /// 校验标签并就地解密。**先验签再解密**，标签不对时 `buf` 保持密文原样。
    pub fn open(
        &self,
        nonce: &[u8; 12],
        aad: &[u8],
        buf: &mut [u8],
        tag: &[u8],
    ) -> Result<(), AuthError> {
        if tag.len() != 16 {
            return Err(AuthError);
        }
        let j0 = Self::j0(nonce);
        let want = self.tag(&j0, aad, buf);
        // 常量时间比较：提前返回会把"前几字节对了"泄露给攻击者，
        // 那足以逐字节爆破出一个合法标签。
        let mut diff = 0u8;
        for i in 0..16 {
            diff |= want[i] ^ tag[i];
        }
        if diff != 0 {
            return Err(AuthError);
        }
        self.ctr_xor(&j0, buf);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wbox_codec::sha256::hex;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn fips197_aes128_block() {
        // FIPS 197 附录 B/C.1 的标准向量。
        let key = unhex("000102030405060708090a0b0c0d0e0f");
        let mut block = [0u8; 16];
        block.copy_from_slice(&unhex("00112233445566778899aabbccddeeff"));
        Aes::new(&key).encrypt_block(&mut block);
        assert_eq!(hex(&block), "69c4e0d86a7b0430d8cdb78070b4c55a");
    }

    #[test]
    fn fips197_aes256_block() {
        // AES-256 的密钥扩展多一步 SubWord（i % nk == 4），漏了这条才会红。
        let key = unhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let mut block = [0u8; 16];
        block.copy_from_slice(&unhex("00112233445566778899aabbccddeeff"));
        Aes::new(&key).encrypt_block(&mut block);
        assert_eq!(hex(&block), "8ea2b7ca516745bfeafc49904b496089");
    }

    #[test]
    fn nist_gcm_test_case_2() {
        // NIST SP 800-38D 附录的标准 GCM 向量（全零密钥、全零明文）。
        let key = [0u8; 16];
        let nonce = [0u8; 12];
        let mut buf = [0u8; 16];
        let tag = AesGcm::new(&key).seal(&nonce, &[], &mut buf);
        assert_eq!(hex(&buf), "0388dace60b6a392f328c2b971b2fe78");
        assert_eq!(hex(&tag), "ab6e47d42cec13bdf53a67b21257bddf");
    }

    #[test]
    fn nist_gcm_test_case_4_with_aad() {
        // 带 AAD 且密文长度不是 16 的倍数——TLS 记录就是这种形状。
        let key = unhex("feffe9928665731c6d6a8f9467308308");
        let nonce_v = unhex("cafebabefacedbaddecaf888");
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&nonce_v);
        let aad = unhex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let mut buf = unhex(concat!(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72",
            "1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39"
        ));
        let tag = AesGcm::new(&key).seal(&nonce, &aad, &mut buf);
        assert_eq!(
            hex(&buf),
            concat!(
                "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e",
                "21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091"
            )
        );
        assert_eq!(hex(&tag), "5bc94fbc3221a5db94fae95ae7121a47");
    }

    #[test]
    fn seal_open_round_trip() {
        let key = [7u8; 32]; // AES-256 那条路也要走到
        let nonce = [9u8; 12];
        let aad = b"tls13 record header";
        let plain = b"the quick brown fox jumps over the lazy dog".to_vec();
        let gcm = AesGcm::new(&key);

        let mut buf = plain.clone();
        let tag = gcm.seal(&nonce, aad, &mut buf);
        assert_ne!(buf, plain, "密文不该等于明文");

        gcm.open(&nonce, aad, &mut buf, &tag).unwrap();
        assert_eq!(buf, plain);
    }

    #[test]
    fn open_rejects_tampering_and_leaves_buffer_untouched() {
        let gcm = AesGcm::new(&[1u8; 16]);
        let nonce = [2u8; 12];
        let aad = b"aad";
        let mut buf = b"secret payload".to_vec();
        let tag = gcm.seal(&nonce, aad, &mut buf);
        let sealed = buf.clone();

        // 改一位密文 → 认证必须失败，且 buf 不得被解密成半截明文。
        let mut bad = sealed.clone();
        bad[0] ^= 1;
        assert_eq!(gcm.open(&nonce, aad, &mut bad, &tag), Err(AuthError));
        assert_eq!(bad[1..], sealed[1..], "认证失败时不该动过缓冲区");

        // 改 tag / 改 AAD / 改 nonce 都必须失败。
        let mut t = tag;
        t[15] ^= 1;
        assert!(gcm.open(&nonce, aad, &mut sealed.clone(), &t).is_err());
        assert!(gcm
            .open(&nonce, b"other", &mut sealed.clone(), &tag)
            .is_err());
        assert!(gcm
            .open(&[3u8; 12], aad, &mut sealed.clone(), &tag)
            .is_err());
        // 短 tag 直接拒绝，不做截断比较。
        assert!(gcm
            .open(&nonce, aad, &mut sealed.clone(), &tag[..8])
            .is_err());
    }

    #[test]
    fn empty_plaintext_still_authenticates_aad() {
        // TLS 的某些记录负载为空，AAD 仍要被认证。
        let gcm = AesGcm::new(&[5u8; 16]);
        let nonce = [6u8; 12];
        let mut buf: Vec<u8> = Vec::new();
        let tag = gcm.seal(&nonce, b"header", &mut buf);
        assert!(gcm.open(&nonce, b"header", &mut Vec::new(), &tag).is_ok());
        assert!(gcm.open(&nonce, b"HEADER", &mut Vec::new(), &tag).is_err());
    }

    #[test]
    fn counter_advances_across_many_blocks() {
        // 超过一个分组的负载要确认计数器真的在递增——不递增的话
        // 每块都用同一段密钥流，是个致命但往返测试发现不了的错误。
        let gcm = AesGcm::new(&[0u8; 16]);
        let mut buf = vec![0u8; 64];
        gcm.seal(&[0u8; 12], &[], &mut buf);
        let first = &buf[..16];
        assert!(
            buf[16..32] != *first && buf[32..48] != *first,
            "各分组的密钥流必须不同"
        );
    }
}
