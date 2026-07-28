//! 极小 PEM 解析：只取 `CERTIFICATE` 块，用于 `SSL_CERT_FILE` 追加根证书。
//!
//! 不做通用 PEM（不认加密私钥、不认头部属性）——这里唯一的用途是读一个
//! CA bundle。**坏块跳过而不是整体失败**：bundle 里混进一个我们不认识的
//! 块（比如 `TRUSTED CERTIFICATE`）不该让其余几百个根证书全部失效。

const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";

/// 取出 PEM 文本里所有证书的 DER 字节。
pub fn certificates(pem: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = pem;
    while let Some(start) = rest.find(BEGIN) {
        let after = &rest[start + BEGIN.len()..];
        let Some(end) = after.find(END) else { break };
        let b64: String = after[..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if let Ok(der) = wbox_codec::base64::decode(&b64) {
            out.push(der);
        }
        rest = &after[end + END.len()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_multiple_certificates() {
        let pem = format!("# 注释\n{BEGIN}\nAAEC\n{END}\n杂项\n{BEGIN}\nAwQF\n{END}\n");
        let got = certificates(&pem);
        assert_eq!(got, vec![vec![0, 1, 2], vec![3, 4, 5]]);
    }

    #[test]
    fn skips_bad_blocks_without_dropping_good_ones() {
        // bundle 里混进坏块时，其余根证书不能跟着一起失效。
        let pem = format!("{BEGIN}\n!!!not base64!!!\n{END}\n{BEGIN}\nAAEC\n{END}\n");
        assert_eq!(certificates(&pem), vec![vec![0, 1, 2]]);
    }

    #[test]
    fn ignores_unterminated_block() {
        assert!(certificates(&format!("{BEGIN}\nAAEC\n")).is_empty());
        assert!(certificates("完全没有 PEM").is_empty());
    }
}
