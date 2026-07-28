//! RSA 签名**验证**（RFC 8017 的 RSASSA-PKCS1-v1_5 与 RSASSA-PSS）。
//!
//! 只验不签。证书链上绝大多数签名是 RSA；TLS 1.3 的 `CertificateVerify`
//! 则强制用 PSS（`rsa_pss_rsae_sha256` 那一族），所以两种填充都要。
//!
//! # 验证的写法只有一种是对的
//!
//! PKCS#1 v1.5 的验证**必须是"重新编码再逐字节比对"**，绝不能反过来
//! "解析签名里的结构再取出哈希比"。后者是 Bleichenbacher'06 那一类
//! 伪造攻击的根源：宽松的解析会接受填充里塞了垃圾的签名，而对 e=3 的
//! 密钥这种签名可以直接构造出来。这里的实现是前者，且填充长度不足时
//! 直接拒绝。

use crate::bigint::BigUint;
use crate::hash::HashAlg;

/// RSA 公钥。只有模数与指数——验签不需要别的。
#[derive(Clone, Debug)]
pub struct RsaPublicKey {
    pub n: BigUint,
    pub e: BigUint,
}

impl RsaPublicKey {
    /// 模数的字节长度（签名长度必须与它相等）。
    pub fn size(&self) -> usize {
        self.n.bits().div_ceil(8)
    }

    /// 公钥运算 `sig^e mod n`，输出定长字节。
    fn raw(&self, sig: &[u8]) -> Option<Vec<u8>> {
        let k = self.size();
        // 长度不等一律拒绝：短签名靠前导零"等价"，但那是攻击者可控的
        // 变形，不该被接受。
        if sig.len() != k {
            return None;
        }
        let s = BigUint::from_bytes_be(sig);
        // s 必须落在 [0, n)。
        if s.ge(&self.n) {
            return None;
        }
        s.modpow(&self.e, &self.n).to_bytes_be(k)
    }
}

/// PKCS#1 v1.5 里各哈希算法的 DigestInfo 前缀（RFC 8017 §9.2 Notes）。
fn digest_info_prefix(alg: HashAlg) -> &'static [u8] {
    match alg {
        HashAlg::Sha256 => &[
            0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x01, 0x05, 0x00, 0x04, 0x20,
        ],
        HashAlg::Sha384 => &[
            0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x02, 0x05, 0x00, 0x04, 0x30,
        ],
        HashAlg::Sha512 => &[
            0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x03, 0x05, 0x00, 0x04, 0x40,
        ],
    }
}

/// 验证 RSASSA-PKCS1-v1_5 签名。
pub fn verify_pkcs1v15(key: &RsaPublicKey, alg: HashAlg, msg: &[u8], sig: &[u8]) -> bool {
    let k = key.size();
    let digest = alg.digest(msg);
    let prefix = digest_info_prefix(alg);

    // 重新编码一份期望的 EM，再逐字节比对——不是去解析签名里的结构。
    // EM = 0x00 || 0x01 || PS(0xff...) || 0x00 || DigestInfo
    let t_len = prefix.len() + digest.len();
    if k < t_len + 11 {
        return false; // 填充放不下，密钥太小
    }
    let mut expect = Vec::with_capacity(k);
    expect.push(0x00);
    expect.push(0x01);
    expect.resize(k - t_len - 1, 0xff);
    expect.push(0x00);
    expect.extend_from_slice(prefix);
    expect.extend_from_slice(&digest);
    debug_assert_eq!(expect.len(), k);

    match key.raw(sig) {
        Some(em) => em == expect,
        None => false,
    }
}

/// MGF1 掩码生成函数（RFC 8017 §B.2.1）。
fn mgf1(alg: HashAlg, seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + alg.len());
    let mut counter: u32 = 0;
    while out.len() < len {
        let mut h = alg.hasher();
        h.update(seed);
        h.update(&counter.to_be_bytes());
        out.extend_from_slice(&h.finish());
        counter += 1;
    }
    out.truncate(len);
    out
}

/// 验证 RSASSA-PSS 签名。盐长等于哈希长（TLS 1.3 的 `rsa_pss_*` 都是这样）。
pub fn verify_pss(key: &RsaPublicKey, alg: HashAlg, msg: &[u8], sig: &[u8]) -> bool {
    let h_len = alg.len();
    let s_len = h_len; // TLS 1.3 规定盐长 == 哈希长
    let em_bits = key.n.bits() - 1;
    let em_len = em_bits.div_ceil(8);

    let Some(raw) = key.raw(sig) else {
        return false;
    };
    // raw 是 k 字节；EM 只有 em_len 字节，差的那一字节必须是前导零。
    let k = key.size();
    if em_len > k {
        return false;
    }
    let em = &raw[k - em_len..];
    if raw[..k - em_len].iter().any(|&b| b != 0) {
        return false;
    }

    if em_len < h_len + s_len + 2 {
        return false;
    }
    // 尾字节必须是 0xbc。
    if em[em_len - 1] != 0xbc {
        return false;
    }
    let db_len = em_len - h_len - 1;
    let masked_db = &em[..db_len];
    let h = &em[db_len..db_len + h_len];

    // 最左 (8*em_len - em_bits) 位必须为零。
    let unused_bits = 8 * em_len - em_bits;
    if unused_bits > 0 && masked_db[0] >> (8 - unused_bits) != 0 {
        return false;
    }

    let db_mask = mgf1(alg, h, db_len);
    let mut db: Vec<u8> = masked_db
        .iter()
        .zip(db_mask.iter())
        .map(|(a, b)| a ^ b)
        .collect();
    if unused_bits > 0 {
        db[0] &= 0xff >> unused_bits;
    }

    // DB = PS(全 0) || 0x01 || salt
    let ps_len = db_len - s_len - 1;
    if db[..ps_len].iter().any(|&b| b != 0) || db[ps_len] != 0x01 {
        return false;
    }
    let salt = &db[ps_len + 1..];

    // H' = Hash(0x00*8 || mHash || salt)，与 H 比。
    let m_hash = alg.digest(msg);
    let mut hasher = alg.hasher();
    hasher.update(&[0u8; 8]);
    hasher.update(&m_hash);
    hasher.update(salt);
    hasher.finish() == h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// RFC 8017 附带的 1024 位测试密钥（pkcs1v15sign-vectors 的第一个）。
    fn key1024() -> RsaPublicKey {
        RsaPublicKey {
            n: BigUint::from_bytes_be(&unhex(concat!(
                "a56e4a0e701017589a5187dc7ea841d156f2ec0e36ad52a44dfeb1e61f7ad991",
                "d8c51056ffedb162b4c0f283a12a88a394dff526ab7291cbb307ceabfce0b1df",
                "d5cd9508096d5b2b8b6df5d671ef6377c0921cb23c270a70e2598e6ff89d19f1",
                "05acc2d3f0cb35f29280e1386b6f64c4ef22e1e1f20d0ce8cffb2249bd9a2137"
            ))),
            e: BigUint::from_u32(65537),
        }
    }

    #[test]
    fn pkcs1v15_encoding_is_exact() {
        // 重新编码那一步是验证的核心，单独钉住它的字节形状。
        // k=128, SHA-256 → T 长 19+32=51，PS 长 128-51-3=74。
        let key = key1024();
        let digest = HashAlg::Sha256.digest(b"hello");
        let prefix = digest_info_prefix(HashAlg::Sha256);
        let mut expect = vec![0x00, 0x01];
        expect.resize(128 - 51 - 1, 0xff);
        expect.push(0x00);
        expect.extend_from_slice(prefix);
        expect.extend_from_slice(&digest);
        assert_eq!(expect.len(), key.size());
        assert_eq!(&expect[..2], &[0x00, 0x01]);
        // PS 是 128-51-3 = 74 个 0xff，占下标 2..=75；76 是分隔的 0x00。
        assert!(expect[2..76].iter().all(|&b| b == 0xff));
        assert_eq!(expect[76], 0x00);
        assert_eq!(&expect[77..77 + 19], prefix);
    }

    #[test]
    fn pkcs1v15_accepts_a_real_signature() {
        // 用 RFC 8017 那把 1024 位测试密钥的**私钥指数**现签一条
        // （签名由 Python 的 pow(m, d, n) 给出），验证正向路径真的通。
        // 只有负向用例的话，一个"永远返回 false"的实现也能全绿。
        let key = key1024();
        let sig = unhex(concat!(
            "022d5061790ff569e2e120b4aed61ef06fae9fa203f6650f090fbde39e1038e7",
            "3240fa9cb6f659269d4c5eed1530eba331c283980b0986df72099ab0ad85fdf9",
            "887ba97371e8201c4b96cfc3954e758a192bbc585ff1d365bbfb11860c588ca1",
            "52e45ba1233c00ce08d838a03302b792b8b45f6651e42b3095a02e4a59476bdb"
        ));
        assert!(verify_pkcs1v15(&key, HashAlg::Sha256, b"hello", &sig));
        // 换一个字节的消息就必须失败。
        assert!(!verify_pkcs1v15(&key, HashAlg::Sha256, b"hellp", &sig));
        // 换算法也必须失败（DigestInfo 前缀不同）。
        assert!(!verify_pkcs1v15(&key, HashAlg::Sha384, b"hello", &sig));
        // 改一位签名必须失败。
        let mut bad = sig.clone();
        bad[64] ^= 1;
        assert!(!verify_pkcs1v15(&key, HashAlg::Sha256, b"hello", &bad));
    }

    #[test]
    fn pss_accepts_a_real_signature() {
        // 同一把测试密钥现签的 PSS（盐固定为 0x00..0x1f，便于复现）。
        // PSS 的验证路径与 v1.5 完全不同（MGF1 掩码 + DB 结构 + 0xbc 尾），
        // 必须单独有正向用例。
        let key = key1024();
        let sig = unhex(concat!(
            "2fe76e4317d7790e85d585968216b32012c874905736d292dfb79a81b34e9742",
            "4912b331fa97c7770e823fd6ca41115d946783361e57ebef3d0a3a6a1395b820",
            "c36bfe71405c11531955706c4fe86f8730be191c40c370ec381e434a3ee9bcd1",
            "6c8c093f0c8bc51ae504f27a735322b4652e0f9744273dc9b8790e907da17e30"
        ));
        assert!(verify_pss(&key, HashAlg::Sha256, b"hello", &sig));
        assert!(!verify_pss(&key, HashAlg::Sha256, b"hellp", &sig));
        let mut bad = sig.clone();
        bad[100] ^= 0x80;
        assert!(!verify_pss(&key, HashAlg::Sha256, b"hello", &bad));
        // 用 v1.5 去验一条 PSS 签名必须失败（填充方案不能混）。
        assert!(!verify_pkcs1v15(&key, HashAlg::Sha256, b"hello", &sig));
    }

    #[test]
    fn rejects_wrong_length_signature() {
        let key = key1024();
        // 短一字节 / 长一字节都必须拒绝——靠前导零"等价"的变形是攻击者可控的。
        assert!(!verify_pkcs1v15(&key, HashAlg::Sha256, b"m", &[0u8; 127]));
        assert!(!verify_pkcs1v15(&key, HashAlg::Sha256, b"m", &[0u8; 129]));
        assert!(!verify_pss(&key, HashAlg::Sha256, b"m", &[0u8; 127]));
    }

    #[test]
    fn rejects_signature_not_less_than_modulus() {
        let key = key1024();
        // s >= n 必须拒绝（RFC 8017 §5.2.2 第一步）。
        let big = key.n.to_bytes_be(128).unwrap();
        assert!(!verify_pkcs1v15(&key, HashAlg::Sha256, b"m", &big));
    }

    #[test]
    fn rejects_garbage() {
        let key = key1024();
        assert!(!verify_pkcs1v15(&key, HashAlg::Sha256, b"m", &[0xab; 128]));
        assert!(!verify_pss(&key, HashAlg::Sha256, b"m", &[0xab; 128]));
    }

    #[test]
    fn mgf1_matches_reference() {
        // MGF1(SHA-256, "bar", 50)，参考值由 Python 的等价实现给出。
        let out = mgf1(HashAlg::Sha256, b"bar", 50);
        assert_eq!(out.len(), 50);
        // 直接断言 MGF1 的输出字节（参考实现：SHA-256 计数器模式）。
        assert_eq!(
            wbox_codec::sha256::hex(&out),
            concat!(
                "382576a7841021cc28fc4c0948753fb8312090cea942ea4c4e735d10dc724b15",
                "5f9f6069f289d61daca0cb814502ef04eae1"
            )
        );
    }

    #[test]
    fn digest_info_prefixes_have_documented_lengths() {
        // 前缀写错会让验证恒失败（安全但不可用）或恒成功（灾难）。
        // 长度是它们最容易核对的不变式。
        assert_eq!(digest_info_prefix(HashAlg::Sha256).len(), 19);
        assert_eq!(digest_info_prefix(HashAlg::Sha384).len(), 19);
        assert_eq!(digest_info_prefix(HashAlg::Sha512).len(), 19);
        // 每个前缀末字节就是摘要长度。
        assert_eq!(*digest_info_prefix(HashAlg::Sha256).last().unwrap(), 32);
        assert_eq!(*digest_info_prefix(HashAlg::Sha384).last().unwrap(), 48);
        assert_eq!(*digest_info_prefix(HashAlg::Sha512).last().unwrap(), 64);
    }
}
