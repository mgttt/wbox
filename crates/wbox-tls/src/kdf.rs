//! HKDF（RFC 5869）与 TLS 1.3 的密钥调度（RFC 8446 §7.1）。
//!
//! TLS 1.3 的整套密钥都由一棵 HKDF 派生树长出来。这个文件只做派生本身，
//! 谁在什么时候派生哪个密钥由 `handshake.rs` 决定。
//!
//! 最容易出错的是 `HKDF-Expand-Label` 的**标签编码**：标签前要加
//! `"tls13 "` 前缀，且长度字段是单字节。写错了握手会在 Finished 那一步
//! 失败，而那时你看到的只是"对端说 decrypt_error"，完全指不到这里。

use crate::hash::{hmac, HashAlg, Digest};

/// HKDF-Extract：`PRK = HMAC(salt, ikm)`。
pub fn extract(alg: HashAlg, salt: &[u8], ikm: &[u8]) -> Digest {
    hmac(alg, salt, ikm)
}

/// HKDF-Expand（RFC 5869 §2.3）。
pub fn expand(alg: HashAlg, prk: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let hash_len = alg.len();
    assert!(len <= 255 * hash_len, "HKDF-Expand 输出过长");
    let mut out = Vec::with_capacity(len);
    let mut prev: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while out.len() < len {
        let mut msg = Vec::with_capacity(prev.len() + info.len() + 1);
        msg.extend_from_slice(&prev);
        msg.extend_from_slice(info);
        msg.push(counter);
        let t = hmac(alg, prk, &msg);
        prev = t.to_vec();
        out.extend_from_slice(&prev);
        counter += 1;
    }
    out.truncate(len);
    out
}

/// `HKDF-Expand-Label`（RFC 8446 §7.1）。
///
/// ```text
/// struct {
///     uint16 length;
///     opaque label<7..255>  = "tls13 " + Label;
///     opaque context<0..255>;
/// } HkdfLabel;
/// ```
pub fn expand_label(alg: HashAlg, secret: &[u8], label: &str, context: &[u8], len: usize) -> Vec<u8> {
    let mut info = Vec::with_capacity(4 + 6 + label.len() + context.len());
    info.extend_from_slice(&(len as u16).to_be_bytes());
    // "tls13 " 前缀是规范的一部分，漏掉它握手会在 Finished 那步失败，
    // 而错误信息完全指不到这里。
    let full_label = format!("tls13 {label}");
    info.push(full_label.len() as u8);
    info.extend_from_slice(full_label.as_bytes());
    info.push(context.len() as u8);
    info.extend_from_slice(context);
    expand(alg, secret, &info, len)
}

/// `Derive-Secret(Secret, Label, Messages)`（RFC 8446 §7.1）。
pub fn derive_secret(alg: HashAlg, secret: &[u8], label: &str, transcript_hash: &[u8]) -> Vec<u8> {
    expand_label(alg, secret, label, transcript_hash, alg.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wbox_codec::sha256::hex;

    #[test]
    fn rfc5869_test_case_1() {
        // RFC 5869 附录 A.1（SHA-256）。
        let ikm = [0x0b; 22];
        let salt: Vec<u8> = (0..13).collect();
        let info: Vec<u8> = (0xf0..0xfa).collect();
        let prk = extract(HashAlg::Sha256, &salt, &ikm);
        assert_eq!(
            hex(&prk),
            "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"
        );
        let okm = expand(HashAlg::Sha256, &prk, &info, 42);
        assert_eq!(
            hex(&okm),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
    }

    #[test]
    fn rfc5869_test_case_3_empty_salt_and_info() {
        // 附录 A.3：salt 与 info 都为空——TLS 的 Early Secret 就是这种形状。
        let ikm = [0x0b; 22];
        let prk = extract(HashAlg::Sha256, &[], &ikm);
        assert_eq!(
            hex(&prk),
            "19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04"
        );
        let okm = expand(HashAlg::Sha256, &prk, &[], 42);
        assert_eq!(
            hex(&okm),
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
        );
    }

    #[test]
    fn expand_label_encoding_is_exact() {
        // 标签编码写错，握手会在 Finished 那步失败，而错误信息完全指不到
        // 这里。所以单独钉住 info 的字节形状。
        // 期望：len(2) || 0x0c || "tls13 hello" || 0x03 || "abc"
        let mut want = Vec::new();
        want.extend_from_slice(&16u16.to_be_bytes());
        want.push(11); // len("tls13 hello")
        want.extend_from_slice(b"tls13 hello");
        want.push(3);
        want.extend_from_slice(b"abc");

        // 用同一份 info 走原始 expand，结果必须与 expand_label 一致。
        let secret = [0x42u8; 32];
        let via_label = expand_label(HashAlg::Sha256, &secret, "hello", b"abc", 16);
        let via_raw = expand(HashAlg::Sha256, &secret, &want, 16);
        assert_eq!(via_label, via_raw);
        assert_eq!(via_label.len(), 16);
    }

    #[test]
    fn expand_produces_requested_length_across_block_boundaries() {
        // 输出跨多个 HMAC 块时计数器必须递增；不递增的话每块都一样，
        // 而"长度对了"这条断言发现不了。
        let prk = [1u8; 32];
        for len in [1usize, 31, 32, 33, 64, 100] {
            let out = expand(HashAlg::Sha256, &prk, b"ctx", len);
            assert_eq!(out.len(), len);
        }
        let long = expand(HashAlg::Sha256, &prk, b"ctx", 96);
        assert_ne!(&long[..32], &long[32..64], "各块必须不同");
        assert_ne!(&long[32..64], &long[64..96]);
    }

    #[test]
    fn sha384_path_works() {
        // TLS_AES_256_GCM_SHA384 走 SHA-384 的调度。
        let prk = extract(HashAlg::Sha384, &[], &[0u8; 48]);
        assert_eq!(prk.len(), 48);
        assert_eq!(derive_secret(HashAlg::Sha384, &prk, "derived", &[]).len(), 48);
    }
}
