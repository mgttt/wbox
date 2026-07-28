//! TLS 1.3 记录层（RFC 8446 §5）。
//!
//! 记录层负责把握手消息与应用数据切成记录、加解密、还原。TLS 1.3 的记录
//! 有两个容易踩的点，都在这里处理：
//!
//! 1. **真实的内容类型藏在密文里**。加密记录的外层类型永远写
//!    `application_data(23)`，真类型是明文末尾的最后一个非零字节。
//!    照外层类型分派会把所有加密握手消息都当成应用数据。
//! 2. **序号不随记录传输**，两端各自维护，用它与 IV 异或出每条记录的
//!    nonce。序号错位的表现是"认证失败"，指不到根因，所以两端的递增时机
//!    必须严格对齐（每加解密一条记录 +1，密钥切换时归零）。

use crate::aes::{AesGcm, AuthError};

/// 记录内容类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentType {
    ChangeCipherSpec,
    Alert,
    Handshake,
    ApplicationData,
}

impl ContentType {
    pub fn from_u8(v: u8) -> Option<ContentType> {
        match v {
            20 => Some(ContentType::ChangeCipherSpec),
            21 => Some(ContentType::Alert),
            22 => Some(ContentType::Handshake),
            23 => Some(ContentType::ApplicationData),
            _ => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            ContentType::ChangeCipherSpec => 20,
            ContentType::Alert => 21,
            ContentType::Handshake => 22,
            ContentType::ApplicationData => 23,
        }
    }
}

/// 单条记录的明文上限（RFC 8446 §5.1：2^14）。
pub const MAX_PLAINTEXT: usize = 1 << 14;
/// 密文上限 = 明文上限 + 内容类型 1 字节 + AEAD 标签 16 字节 + 余量。
pub const MAX_CIPHERTEXT: usize = MAX_PLAINTEXT + 256;

/// 一个方向上的加解密状态：密钥、IV 与序号。
pub struct Keys {
    aead: AesGcm,
    iv: [u8; 12],
    seq: u64,
}

impl Keys {
    pub fn new(key: &[u8], iv: &[u8]) -> Keys {
        let mut fixed = [0u8; 12];
        fixed.copy_from_slice(iv);
        Keys {
            aead: AesGcm::new(key),
            iv: fixed,
            seq: 0,
        }
    }

    /// 本条记录的 nonce：IV 的低 8 字节与序号异或（RFC 8446 §5.3）。
    fn nonce(&self) -> [u8; 12] {
        let mut n = self.iv;
        let s = self.seq.to_be_bytes();
        for i in 0..8 {
            n[4 + i] ^= s[i];
        }
        n
    }

    /// 加密一条记录，返回完整的线上字节（含 5 字节头）。
    pub fn seal(&mut self, ty: ContentType, plaintext: &[u8]) -> Vec<u8> {
        // 内层明文 = 数据 || 真实内容类型（不做填充：填充只防流量分析，
        // 而拉镜像的流量特征本来就藏不住）。
        let mut inner = Vec::with_capacity(plaintext.len() + 1);
        inner.extend_from_slice(plaintext);
        inner.push(ty.as_u8());

        let len = (inner.len() + 16) as u16;
        // AAD 就是记录头本身。
        let header = [
            ContentType::ApplicationData.as_u8(),
            0x03,
            0x03,
            (len >> 8) as u8,
            len as u8,
        ];
        let tag = self.aead.seal(&self.nonce(), &header, &mut inner);
        self.seq += 1;

        let mut out = Vec::with_capacity(5 + inner.len() + 16);
        out.extend_from_slice(&header);
        out.extend_from_slice(&inner);
        out.extend_from_slice(&tag);
        out
    }

    /// 解密一条记录体，返回（真实内容类型, 明文）。
    pub fn open(&mut self, header: &[u8; 5], body: &mut Vec<u8>) -> Result<ContentType, String> {
        if body.len() < 17 {
            return Err("TLS：加密记录过短".into());
        }
        let tag_start = body.len() - 16;
        let tag: [u8; 16] = body[tag_start..].try_into().unwrap();
        body.truncate(tag_start);
        self.aead
            .open(&self.nonce(), header, body, &tag)
            .map_err(|_: AuthError| "TLS：记录认证失败".to_string())?;
        self.seq += 1;

        // 真实类型是末尾最后一个非零字节；它之后全是填充零。
        while let Some(&0) = body.last() {
            body.pop();
        }
        let ty = body.pop().ok_or("TLS：记录里没有内容类型")?;
        ContentType::from_u8(ty).ok_or_else(|| format!("TLS：未知内容类型 {ty}"))
    }
}

/// 拼一条**明文**记录（握手初期还没有密钥时用）。
pub fn plaintext_record(ty: ContentType, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(ty.as_u8());
    // 记录层版本恒写 0x0303（TLS 1.2）——TLS 1.3 为穿过中间设备如此规定。
    out.extend_from_slice(&[0x03, 0x03]);
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (Keys, Keys) {
        let key = [0x2fu8; 16];
        let iv = [0x91u8; 12];
        (Keys::new(&key, &iv), Keys::new(&key, &iv))
    }

    #[test]
    fn seal_open_round_trip_preserves_inner_content_type() {
        // 加密记录的外层类型永远是 application_data，真类型藏在密文里。
        // 照外层分派会把所有加密握手消息都当成应用数据。
        let (mut tx, mut rx) = keys();
        let wire = tx.seal(ContentType::Handshake, b"hello handshake");
        assert_eq!(wire[0], 23, "外层类型必须是 application_data");

        let header: [u8; 5] = wire[..5].try_into().unwrap();
        let mut body = wire[5..].to_vec();
        let ty = rx.open(&header, &mut body).unwrap();
        assert_eq!(ty, ContentType::Handshake, "真实类型要还原出来");
        assert_eq!(body, b"hello handshake");
    }

    #[test]
    fn sequence_numbers_advance_together() {
        // 序号不随记录传输，两端各自维护。错位的表现是"认证失败"，
        // 指不到根因——所以这条要单独钉住。
        let (mut tx, mut rx) = keys();
        for i in 0..5u8 {
            let payload = [i; 8];
            let wire = tx.seal(ContentType::ApplicationData, &payload);
            let header: [u8; 5] = wire[..5].try_into().unwrap();
            let mut body = wire[5..].to_vec();
            let ty = rx.open(&header, &mut body).unwrap();
            assert_eq!(ty, ContentType::ApplicationData);
            assert_eq!(body, payload);
        }
        assert_eq!(tx.seq, 5);
        assert_eq!(rx.seq, 5);
    }

    #[test]
    fn out_of_order_records_fail_authentication() {
        // 跳过一条记录后，序号错位 → 必须认证失败而不是解出垃圾。
        let (mut tx, mut rx) = keys();
        let _skipped = tx.seal(ContentType::ApplicationData, b"first");
        let second = tx.seal(ContentType::ApplicationData, b"second");
        let header: [u8; 5] = second[..5].try_into().unwrap();
        let mut body = second[5..].to_vec();
        assert!(rx.open(&header, &mut body).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let (mut tx, mut rx) = keys();
        let wire = tx.seal(ContentType::Handshake, b"payload");
        // 改密文
        let mut bad = wire.clone();
        bad[7] ^= 1;
        let header: [u8; 5] = bad[..5].try_into().unwrap();
        let mut body = bad[5..].to_vec();
        assert!(rx.open(&header, &mut body).is_err());

        // 改记录头（AAD）也必须被抓住
        let (mut tx, mut rx) = keys();
        let wire = tx.seal(ContentType::Handshake, b"payload");
        let mut header: [u8; 5] = wire[..5].try_into().unwrap();
        header[3] ^= 1;
        let mut body = wire[5..].to_vec();
        assert!(rx.open(&header, &mut body).is_err());
    }

    #[test]
    fn rejects_short_and_empty_records() {
        let (_, mut rx) = keys();
        let header = [23u8, 3, 3, 0, 0];
        assert!(rx.open(&header, &mut Vec::new()).is_err());
        assert!(rx.open(&header, &mut vec![0u8; 16]).is_err());
    }

    #[test]
    fn strips_padding_to_find_content_type() {
        // 对端可能加填充；真实类型是最后一个非零字节。
        let key = [1u8; 16];
        let iv = [2u8; 12];
        let tx = Keys::new(&key, &iv);
        let mut rx = Keys::new(&key, &iv);

        // 手工构造带填充的内层明文：data || type || 0x00 * 3
        let mut inner = b"padded".to_vec();
        inner.push(ContentType::Handshake.as_u8());
        inner.extend_from_slice(&[0, 0, 0]);
        let len = (inner.len() + 16) as u16;
        let header = [23u8, 3, 3, (len >> 8) as u8, len as u8];
        let tag = tx.aead.seal(&tx.nonce(), &header, &mut inner);
        inner.extend_from_slice(&tag);

        let ty = rx.open(&header, &mut inner).unwrap();
        assert_eq!(ty, ContentType::Handshake);
        assert_eq!(inner, b"padded");
    }

    #[test]
    fn plaintext_record_uses_legacy_version() {
        // TLS 1.3 规定记录层版本恒写 0x0303，为的是穿过中间设备。
        let r = plaintext_record(ContentType::Handshake, b"abc");
        assert_eq!(&r[..5], &[22, 0x03, 0x03, 0x00, 0x03]);
        assert_eq!(&r[5..], b"abc");
    }

    #[test]
    fn nonce_xors_sequence_into_low_eight_bytes() {
        let k = Keys::new(&[0u8; 16], &[0u8; 12]);
        assert_eq!(k.nonce(), [0u8; 12]);
        let mut k = Keys::new(&[0u8; 16], &[0u8; 12]);
        k.seq = 0x0102;
        let n = k.nonce();
        // 前 4 字节不参与异或
        assert_eq!(&n[..4], &[0, 0, 0, 0]);
        assert_eq!(&n[4..], &[0, 0, 0, 0, 0, 0, 0x01, 0x02]);
    }
}
