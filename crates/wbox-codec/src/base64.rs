//! Base64（RFC 4648 标准字母表，带 `=` 填充）。取代 `base64` crate。
//!
//! 用途：registry 的 `Authorization: Basic` 头，以及 TLS 证书 PEM 解码。
//! 解码器**严格**：非字母表字符、错误的填充、长度不是 4 的倍数一律报错——
//! 宽松解码在凭证与证书这两个场景里只会掩盖问题。

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 编码为标准 base64（带填充）。
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 解码标准 base64。字符表外的字符（含空白）一律拒绝。
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(4) {
        return Err(format!("base64 长度 {} 不是 4 的倍数", b.len()));
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    let mut i = 0;
    while i < b.len() {
        let quad = &b[i..i + 4];
        i += 4;
        // 填充只允许出现在最后一组的后两位。
        let pad = quad.iter().filter(|&&c| c == b'=').count();
        if pad > 0 && i != b.len() {
            return Err("base64 填充只能出现在结尾".to_string());
        }
        if pad > 2 || (pad > 0 && quad[3] != b'=') {
            return Err("base64 填充位置非法".to_string());
        }
        let mut n = 0u32;
        for (j, &c) in quad.iter().enumerate() {
            let v = if c == b'=' {
                if j < 4 - pad {
                    return Err("base64 填充位置非法".to_string());
                }
                0
            } else {
                sextet(c).ok_or_else(|| format!("base64 非法字符 {:?}", c as char))?
            };
            n = (n << 6) | v as u32;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

fn sextet(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(raw.as_bytes()), enc, "encode {raw:?}");
            assert_eq!(decode(enc).unwrap(), raw.as_bytes(), "decode {enc:?}");
        }
    }

    #[test]
    fn roundtrip_all_bytes() {
        let data: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode(&encode(&data)).unwrap(), data);
    }

    #[test]
    fn rejects_malformed() {
        // 长度不对齐、非法字符、填充在中间——都必须是 Err 而不是尽力而为。
        assert!(decode("Zm9vY").is_err());
        assert!(decode("Zm9v YmFy").is_err());
        assert!(decode("Zm=9dmFy").is_err());
        assert!(decode("Zg=v").is_err());
        assert!(decode("====").is_err());
    }
}
