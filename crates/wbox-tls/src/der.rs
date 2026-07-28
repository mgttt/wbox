//! DER（ASN.1 的可分辨编码规则）解析器。
//!
//! 只做**解析**，不做编码——证书是别人给的，我们只读。
//!
//! # 面对的是敌意输入
//!
//! 证书来自网络，可能是攻击者构造的。所以：
//!
//! - 每一步都检查长度，绝不 `unwrap` 或裸切片；
//! - **拒绝非最短形式的长度编码**（DER 要求长度用最短形式，BER 允许
//!   `0x81 0x05` 这种冗余写法）。宽松解析会让同一份逻辑内容有多种字节表示，
//!   而证书的签名是按字节算的——那正是各种"同一证书两种解读"漏洞的温床；
//! - 有嵌套深度上限，深层嵌套不能把递归打爆栈。

/// 一个 DER 元素（TLV）。
#[derive(Clone, Copy, Debug)]
pub struct Element<'a> {
    /// 标签字节（含类别与构造位）。
    pub tag: u8,
    /// 值（不含标签与长度）。
    pub value: &'a [u8],
    /// 含标签与长度的完整编码。签名是对**这一整段**算的，不是对 value。
    pub raw: &'a [u8],
}

/// 常见标签。
pub const TAG_BOOLEAN: u8 = 0x01;
pub const TAG_INTEGER: u8 = 0x02;
pub const TAG_BIT_STRING: u8 = 0x03;
pub const TAG_OCTET_STRING: u8 = 0x04;
pub const TAG_NULL: u8 = 0x05;
pub const TAG_OID: u8 = 0x06;
pub const TAG_UTF8_STRING: u8 = 0x0c;
pub const TAG_SEQUENCE: u8 = 0x30;
pub const TAG_SET: u8 = 0x31;
pub const TAG_PRINTABLE_STRING: u8 = 0x13;
pub const TAG_IA5_STRING: u8 = 0x16;
pub const TAG_UTC_TIME: u8 = 0x17;
pub const TAG_GENERALIZED_TIME: u8 = 0x18;

/// 嵌套深度上限。真实证书不超过十层，64 已宽松到不可能误伤。
const MAX_DEPTH: usize = 64;

pub type Result<T> = std::result::Result<T, String>;

/// 顺序读取 DER 元素的游标。
#[derive(Clone, Copy, Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader {
            data,
            pos: 0,
            depth: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// 剩余未读字节。
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }

    /// 读下一个元素（任意标签）。
    pub fn read(&mut self) -> Result<Element<'a>> {
        let start = self.pos;
        let d = self.data;
        if self.pos >= d.len() {
            return Err("DER：数据已读完".into());
        }
        let tag = d[self.pos];
        self.pos += 1;
        if self.pos >= d.len() {
            return Err("DER：缺少长度字节".into());
        }
        let first = d[self.pos];
        self.pos += 1;
        let len = if first < 0x80 {
            first as usize
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 {
                // 不定长（0x80）在 DER 里非法。
                return Err("DER：不允许不定长编码".into());
            }
            if n > 4 {
                return Err("DER：长度字段过长".into());
            }
            if self.pos + n > d.len() {
                return Err("DER：长度字段被截断".into());
            }
            let mut v = 0usize;
            for &b in &d[self.pos..self.pos + n] {
                v = (v << 8) | b as usize;
            }
            self.pos += n;
            // DER 要求最短形式：短形式装得下的不能用长形式，
            // 长形式的首字节不能为零。
            if v < 0x80 || d[self.pos - n] == 0 {
                return Err("DER：长度不是最短形式".into());
            }
            v
        };
        if self.pos + len > d.len() {
            return Err("DER：值被截断".into());
        }
        let value = &d[self.pos..self.pos + len];
        self.pos += len;
        Ok(Element {
            tag,
            value,
            raw: &d[start..self.pos],
        })
    }

    /// 读一个指定标签的元素。
    pub fn expect(&mut self, tag: u8) -> Result<Element<'a>> {
        let e = self.read()?;
        if e.tag != tag {
            return Err(format!("DER：期待标签 0x{tag:02x}，实际 0x{:02x}", e.tag));
        }
        Ok(e)
    }

    /// 读一个 SEQUENCE 并返回其内容的子游标。
    pub fn sequence(&mut self) -> Result<Reader<'a>> {
        self.nested(TAG_SEQUENCE)
    }

    /// 读一个指定标签的构造型元素并返回其内容的子游标。
    pub fn nested(&mut self, tag: u8) -> Result<Reader<'a>> {
        if self.depth + 1 > MAX_DEPTH {
            return Err("DER：嵌套过深".into());
        }
        let e = self.expect(tag)?;
        Ok(Reader {
            data: e.value,
            pos: 0,
            depth: self.depth + 1,
        })
    }

    /// 看一眼下一个元素的标签但不消费。
    pub fn peek_tag(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// 若下一个元素是该标签就读走，否则返回 `None`（用于可选字段）。
    pub fn take_if(&mut self, tag: u8) -> Result<Option<Element<'a>>> {
        if self.peek_tag() == Some(tag) {
            Ok(Some(self.read()?))
        } else {
            Ok(None)
        }
    }
}

impl<'a> Element<'a> {
    /// 把 INTEGER 的值取成无前导零的大端字节。
    ///
    /// DER 的 INTEGER 是有符号的，正数最高位为 1 时会补一个 0x00。
    /// **负数一律拒绝**：证书里的整数（模数、序列号、签名分量）都该是正的，
    /// 接受负数只会让语义变模糊。
    pub fn integer_bytes(&self) -> Result<&'a [u8]> {
        if self.tag != TAG_INTEGER {
            return Err("DER：不是 INTEGER".into());
        }
        let v = self.value;
        if v.is_empty() {
            return Err("DER：INTEGER 为空".into());
        }
        if v[0] & 0x80 != 0 {
            return Err("DER：不接受负 INTEGER".into());
        }
        // 最短形式：除非是为了表示正号，否则不允许前导 0x00。
        if v.len() > 1 && v[0] == 0 && v[1] & 0x80 == 0 {
            return Err("DER：INTEGER 有多余的前导零".into());
        }
        Ok(if v.len() > 1 && v[0] == 0 { &v[1..] } else { v })
    }

    /// 取 BIT STRING 的内容（要求未使用位数为 0）。
    pub fn bit_string(&self) -> Result<&'a [u8]> {
        if self.tag != TAG_BIT_STRING {
            return Err("DER：不是 BIT STRING".into());
        }
        let v = self.value;
        if v.is_empty() {
            return Err("DER：BIT STRING 为空".into());
        }
        if v[0] != 0 {
            // 公钥与签名都是整字节的，未使用位不为 0 说明结构不对。
            return Err("DER：BIT STRING 有未使用位".into());
        }
        Ok(&v[1..])
    }
}

/// 常用 OID 的字节编码（DER 里 OID 的值部分）。
pub mod oid {
    /// `1.2.840.113549.1.1.1` rsaEncryption
    pub const RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
    /// `1.2.840.113549.1.1.11` sha256WithRSAEncryption
    pub const SHA256_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
    /// `1.2.840.113549.1.1.12` sha384WithRSAEncryption
    pub const SHA384_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c];
    /// `1.2.840.113549.1.1.13` sha512WithRSAEncryption
    pub const SHA512_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d];
    /// `1.2.840.113549.1.1.10` RSASSA-PSS
    pub const RSA_PSS: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0a];
    /// `1.2.840.10045.2.1` id-ecPublicKey
    pub const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
    /// `1.2.840.10045.3.1.7` prime256v1 (P-256)
    pub const P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
    /// `1.3.132.0.34` secp384r1 (P-384)
    pub const P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
    /// `1.2.840.10045.4.3.2` ecdsa-with-SHA256
    pub const ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
    /// `1.2.840.10045.4.3.3` ecdsa-with-SHA384
    pub const ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];
    /// `2.5.4.3` commonName
    pub const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
    /// `2.5.29.17` subjectAltName
    pub const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
    /// `2.5.29.19` basicConstraints
    pub const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
    /// `2.5.29.15` keyUsage
    pub const KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_sequences() {
        // SEQUENCE { INTEGER 1, SEQUENCE { OCTET STRING "hi" }, NULL }
        let der = [
            0x30, 0x0b, 0x02, 0x01, 0x01, 0x30, 0x04, 0x04, 0x02, b'h', b'i', 0x05, 0x00,
        ];
        let mut r = Reader::new(&der);
        let mut seq = r.sequence().unwrap();
        assert_eq!(seq.expect(TAG_INTEGER).unwrap().integer_bytes().unwrap(), [1]);
        let mut inner = seq.sequence().unwrap();
        assert_eq!(inner.expect(TAG_OCTET_STRING).unwrap().value, b"hi");
        assert_eq!(seq.expect(TAG_NULL).unwrap().value, b"");
        assert!(seq.is_empty());
    }

    #[test]
    fn raw_includes_tag_and_length() {
        // 签名是对整个 TLV 算的，不是对 value——这条不变式弄错，
        // 所有证书都会验签失败，而且极难查。
        let der = [0x30, 0x03, 0x02, 0x01, 0x07];
        let mut r = Reader::new(&der);
        let e = r.read().unwrap();
        assert_eq!(e.raw, &der[..]);
        assert_eq!(e.value, &der[2..]);
    }

    #[test]
    fn long_form_lengths() {
        // 0x81 0x80 = 128 字节，是合法的最短长形式。
        let mut der = vec![0x04, 0x81, 0x80];
        der.extend_from_slice(&[0xaa; 128]);
        let mut r = Reader::new(&der);
        assert_eq!(r.read().unwrap().value.len(), 128);
    }

    #[test]
    fn rejects_non_minimal_and_malformed_lengths() {
        // 这一组是 DER 与 BER 的分界。宽松解析会让同一份逻辑内容有多种字节
        // 表示，而签名是按字节算的——"同一证书两种解读"就是这么来的。
        let cases: &[(&[u8], &str)] = &[
            (&[0x04, 0x81, 0x05, 1, 2, 3, 4, 5], "短形式装得下却用了长形式"),
            (&[0x04, 0x82, 0x00, 0x05, 1, 2, 3, 4, 5], "长形式首字节为零"),
            (&[0x04, 0x80], "不定长编码"),
            (&[0x04, 0x85, 1, 2, 3, 4, 5], "长度字段过长"),
            (&[0x04, 0x05, 1, 2], "值被截断"),
            (&[0x04], "缺长度字节"),
            (&[], "空输入"),
        ];
        for (der, why) in cases {
            assert!(Reader::new(der).read().is_err(), "应当拒绝：{why}");
        }
    }

    #[test]
    fn integer_rules() {
        let ok = |v: &[u8]| {
            let mut d = vec![0x02, v.len() as u8];
            d.extend_from_slice(v);
            let mut r = Reader::new(&d);
            r.read().unwrap().integer_bytes().map(|b| b.to_vec())
        };
        assert_eq!(ok(&[0x07]).unwrap(), vec![0x07]);
        // 正数最高位为 1 时补 0x00 是合法的，取值时要剥掉。
        assert_eq!(ok(&[0x00, 0xff]).unwrap(), vec![0xff]);
        // 负数拒绝。
        assert!(ok(&[0xff]).is_err());
        // 多余前导零拒绝（0x00 0x01 不是最短形式）。
        assert!(ok(&[0x00, 0x01]).is_err());
        // 空拒绝。
        assert!(ok(&[]).is_err());
    }

    #[test]
    fn bit_string_requires_whole_bytes() {
        let mk = |v: &[u8]| {
            let mut d = vec![0x03, v.len() as u8];
            d.extend_from_slice(v);
            let mut r = Reader::new(&d);
            r.read().unwrap().bit_string().map(|b| b.to_vec())
        };
        assert_eq!(mk(&[0x00, 0xde, 0xad]).unwrap(), vec![0xde, 0xad]);
        assert!(mk(&[0x03, 0xde]).is_err(), "未使用位不为 0 要拒绝");
        assert!(mk(&[]).is_err());
    }

    #[test]
    fn optional_fields_via_take_if() {
        let der = [0x30, 0x05, 0x02, 0x01, 0x09, 0x05, 0x00];
        let mut r = Reader::new(&der);
        let mut seq = r.sequence().unwrap();
        assert!(seq.take_if(TAG_BOOLEAN).unwrap().is_none());
        assert!(seq.take_if(TAG_INTEGER).unwrap().is_some());
        assert!(seq.take_if(TAG_NULL).unwrap().is_some());
        assert!(seq.is_empty());
    }

    /// 造一个真嵌套的 `SEQUENCE { SEQUENCE { ... } }`，深度为 `d`。
    fn nested_der(d: usize) -> Vec<u8> {
        let mut cur = vec![0x30, 0x00];
        for _ in 1..d {
            let mut next = vec![0x30];
            let n = cur.len();
            if n < 0x80 {
                next.push(n as u8);
            } else if n < 0x100 {
                next.extend_from_slice(&[0x81, n as u8]);
            } else {
                next.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]);
            }
            next.extend_from_slice(&cur);
            cur = next;
        }
        cur
    }

    #[test]
    fn depth_limit_stops_runaway_nesting() {
        // 深层嵌套的证书是敌意构造的典型形态，必须报错而不是打爆栈。
        let der = nested_der(MAX_DEPTH + 20);
        let mut cur = Reader::new(&der).sequence().unwrap();
        let mut depth = 1usize;
        while let Ok(next) = cur.sequence() {
            cur = next;
            depth += 1;
            assert!(depth <= MAX_DEPTH, "深度限制没有生效（已到 {depth} 层）");
        }
        assert_eq!(depth, MAX_DEPTH, "应当正好在 MAX_DEPTH 层被挡住");
    }

    #[test]
    fn accepts_nesting_within_the_limit() {
        // 上限之内要照常解析——只测"挡得住"会让一个恒返回错误的实现也变绿。
        let der = nested_der(10);
        let mut cur = Reader::new(&der).sequence().unwrap();
        for _ in 1..9 {
            cur = cur.sequence().unwrap();
        }
    }
}
