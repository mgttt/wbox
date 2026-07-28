//! 哈希算法的统一入口。
//!
//! 证书链、签名验证、TLS 密钥调度都要按**运行期**决定的算法去哈希
//! （证书里写着 `sha384WithRSAEncryption`，代码不能在编译期就定死）。
//! 所以这里用一个小枚举把 SHA-256/384/512 包起来，而不是各处写 match。
//!
//! SHA-256 复用 `wbox-codec` 里那一份，不另写第二份——同一件事两份实现
//! 必然慢慢漂开。

use crate::sha512::Sha512;

/// 支持的哈希算法。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlg {
    Sha256,
    Sha384,
    Sha512,
}

/// 增量哈希器（按算法分派）。
pub enum Hasher {
    Sha256(wbox_codec::Sha256),
    Sha384(Box<Sha512<48>>),
    Sha512(Box<Sha512<64>>),
}

/// 摘要值。最长 64 字节，用定长数组 + 长度避免堆分配。
#[derive(Clone, Copy)]
pub struct Digest {
    bytes: [u8; 64],
    len: usize,
}

impl std::ops::Deref for Digest {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl PartialEq<[u8]> for Digest {
    fn eq(&self, other: &[u8]) -> bool {
        &**self == other
    }
}

impl PartialEq<&[u8]> for Digest {
    fn eq(&self, other: &&[u8]) -> bool {
        &**self == *other
    }
}

impl std::fmt::Debug for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", wbox_codec::sha256::hex(self))
    }
}

impl HashAlg {
    /// 摘要字节长度。
    pub fn len(self) -> usize {
        match self {
            HashAlg::Sha256 => 32,
            HashAlg::Sha384 => 48,
            HashAlg::Sha512 => 64,
        }
    }

    pub fn is_empty(self) -> bool {
        false
    }

    pub fn hasher(self) -> Hasher {
        match self {
            HashAlg::Sha256 => Hasher::Sha256(wbox_codec::Sha256::new()),
            HashAlg::Sha384 => Hasher::Sha384(Box::default()),
            HashAlg::Sha512 => Hasher::Sha512(Box::default()),
        }
    }

    /// 一次性摘要。
    pub fn digest(self, data: &[u8]) -> Digest {
        let mut h = self.hasher();
        h.update(data);
        h.finish()
    }
}

/// 增量哈希的统一接口。
pub trait Hash {
    fn update(&mut self, data: &[u8]);
    fn finish(self) -> Digest;
}

impl Hasher {
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha384(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    pub fn finish(self) -> Digest {
        let mut bytes = [0u8; 64];
        let len = match self {
            Hasher::Sha256(h) => {
                bytes[..32].copy_from_slice(&h.finalize());
                32
            }
            Hasher::Sha384(h) => {
                bytes[..48].copy_from_slice(&h.finalize());
                48
            }
            Hasher::Sha512(h) => {
                bytes[..64].copy_from_slice(&h.finalize());
                64
            }
        };
        Digest { bytes, len }
    }

    /// 克隆当前状态。TLS 的握手转录哈希要在不结束的前提下取快照
    /// （Finished 之后还要继续往里喂消息）。
    pub fn snapshot(&self) -> Hasher {
        match self {
            Hasher::Sha256(h) => Hasher::Sha256(h.clone()),
            Hasher::Sha384(h) => Hasher::Sha384(h.clone()),
            Hasher::Sha512(h) => Hasher::Sha512(h.clone()),
        }
    }
}

impl Hash for Hasher {
    fn update(&mut self, data: &[u8]) {
        Hasher::update(self, data)
    }
    fn finish(self) -> Digest {
        Hasher::finish(self)
    }
}

/// HMAC，按算法分派。TLS 的 HKDF 与 Finished 都要它。
pub fn hmac(alg: HashAlg, key: &[u8], msg: &[u8]) -> Digest {
    let mut bytes = [0u8; 64];
    let len = match alg {
        HashAlg::Sha256 => {
            bytes[..32].copy_from_slice(&wbox_codec::sha256::hmac_sha256(key, msg));
            32
        }
        HashAlg::Sha384 => {
            bytes[..48].copy_from_slice(&crate::sha512::hmac_sha384(key, msg));
            48
        }
        // TLS 1.3 的套件只用到 SHA-256/384；SHA-512 的 HMAC 留着不实现，
        // 走到这里说明有人加了套件却没补这一处。
        HashAlg::Sha512 => unreachable!("TLS 1.3 没有基于 SHA-512 的套件"),
    };
    Digest { bytes, len }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wbox_codec::sha256::hex;

    #[test]
    fn dispatches_to_the_right_algorithm() {
        assert_eq!(
            hex(&HashAlg::Sha256.digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&HashAlg::Sha384.digest(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(HashAlg::Sha512.digest(b"abc").len(), 64);
        assert_eq!(HashAlg::Sha256.len(), 32);
    }

    #[test]
    fn incremental_equals_oneshot() {
        let mut h = HashAlg::Sha384.hasher();
        h.update(b"ab");
        h.update(b"c");
        assert_eq!(&*h.finish(), &*HashAlg::Sha384.digest(b"abc"));
    }

    #[test]
    fn snapshot_does_not_disturb_the_running_hash() {
        // TLS 的转录哈希要在中途取快照后继续喂——快照若影响原状态，
        // Finished 会算错，而那时已经很难查了。
        let mut h = HashAlg::Sha256.hasher();
        h.update(b"transcript-so-far");
        let mid = h.snapshot().finish();
        h.update(b"-more");
        let end = h.finish();
        assert_eq!(&*mid, &*HashAlg::Sha256.digest(b"transcript-so-far"));
        assert_eq!(&*end, &*HashAlg::Sha256.digest(b"transcript-so-far-more"));
    }

    #[test]
    fn hmac_dispatches() {
        assert_eq!(
            hex(&hmac(HashAlg::Sha256, &[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac(HashAlg::Sha384, &[0x0b; 20], b"Hi There")),
            "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6"
        );
    }
}
