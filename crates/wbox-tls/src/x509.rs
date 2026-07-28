//! X.509 证书解析与链校验（RFC 5280 的可用子集）。
//!
//! # 做什么、不做什么
//!
//! 做：解析证书、按 SAN（含通配符）匹配主机名、检查有效期与
//! `basicConstraints`、逐级验签直到内置根。
//!
//! **不做**，且都是有意的：
//!
//! - **不做吊销检查（CRL / OCSP）**。两者都要额外的网络请求：CRL 要下载
//!   可能几 MB 的列表，OCSP 要向 CA 发一次在线查询（还会泄露"你在访问谁"）。
//!   主流客户端也普遍软失败——查不到就放行，等于没查。如实写在这里，
//!   而不是假装做了。
//! - **不做名称约束（nameConstraints）**。它只对"受限中间 CA"这种少见部署
//!   有意义，registry 的链上不会出现。遇到标了 critical 的该扩展会**拒绝**，
//!   而不是忽略——见 [`Certificate::has_unsupported_critical_extension`]。
//! - **不认 CN 作为主机名**。RFC 6125 早已废弃这条，现代证书一律用 SAN。
//!   认 CN 会让"CN=evil.com 但 SAN 只有 good.com"这类证书被接受。
//!
//! # critical 扩展的规矩
//!
//! RFC 5280 §4.2：标了 critical 的扩展如果不认识，**必须拒绝整张证书**。
//! 这条容易被写成"忽略不认识的扩展"，那正好把 CA 想强制的约束绕过去了。

use crate::bigint::BigUint;
use crate::der::{self, oid, Reader};
use crate::ec;
use crate::hash::HashAlg;
use crate::rsa::RsaPublicKey;

pub type Result<T> = std::result::Result<T, String>;

/// 证书里的公钥。
#[derive(Clone, Debug)]
pub enum PublicKey {
    Rsa(RsaPublicKey),
    /// P-256 或 P-384 上的点。
    EcP256(ec::Point),
    EcP384(ec::Point),
    /// 支持不了的密钥（P-521、Ed25519、位数不足的 RSA……）。
    ///
    /// 与 [`SigAlg::Unsupported`] 同一个道理：**解析不失败、验签必失败**。
    /// 信任库里本来就有一些我们用不上的根（不同曲线、老算法），
    /// 让它们把整个信任库的加载搞崩是不对的；而任何真要用它验签的地方
    /// 都会拿到 `false`。
    Unsupported,
}

/// 签名算法（证书签名与 TLS 的 CertificateVerify 共用这个枚举）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigAlg {
    RsaPkcs1(HashAlg),
    RsaPss(HashAlg),
    Ecdsa(HashAlg),
    /// 认识不了的算法（最典型的是 `sha1WithRSAEncryption`）。
    ///
    /// **解析时不报错、验签时必失败**，这个分界是有意的：老根证书普遍
    /// 用 SHA-1 自签，而根的自签名在真实链校验里根本不验——信任来自它
    /// 在信任库里，不来自那个签名。为了能把这类根装进信任库就去支持
    /// SHA-1，等于为了一个用不上的地方引入一个已被攻破的哈希。
    Unsupported,
}

/// 解析后的证书。借用原始 DER，不拷贝。
pub struct Certificate<'a> {
    /// 整张证书的 DER。
    pub der: &'a [u8],
    /// `tbsCertificate` 的完整 DER —— **签名是对这一段算的**。
    pub tbs: &'a [u8],
    pub subject: &'a [u8],
    pub issuer: &'a [u8],
    pub not_before: i64,
    pub not_after: i64,
    pub public_key: PublicKey,
    pub sig_alg: SigAlg,
    pub signature: Vec<u8>,
    /// SAN 里的 dNSName 列表。
    pub dns_names: Vec<String>,
    pub is_ca: bool,
    /// `basicConstraints` 里的 pathLenConstraint。
    pub path_len: Option<u32>,
    /// 有不认识的 critical 扩展 → 整张证书必须拒绝。
    pub unsupported_critical: bool,
}

impl<'a> Certificate<'a> {
    /// 解析一张 DER 编码的证书。
    pub fn parse(der_bytes: &'a [u8]) -> Result<Certificate<'a>> {
        let mut top = Reader::new(der_bytes);
        let mut cert = top.sequence()?;

        // tbsCertificate —— 先原样留下它的 raw，签名对它算。
        let tbs_elem = {
            let mut probe = cert;
            probe.expect(der::TAG_SEQUENCE)?
        };
        let mut tbs = cert.sequence()?;

        // [0] version（可选，显式标签 0xa0）
        let version = match tbs.take_if(0xa0)? {
            Some(e) => {
                let mut r = Reader::new(e.value);
                let v = r.expect(der::TAG_INTEGER)?;
                let b = v.integer_bytes()?;
                if b.len() != 1 {
                    return Err("X.509：version 非法".into());
                }
                b[0]
            }
            None => 0,
        };
        // serialNumber
        let _serial = tbs.expect(der::TAG_INTEGER)?;
        // signature（TBS 里的算法标识，应与外层一致）
        let (_, inner_alg_raw) = parse_sig_alg_raw(&mut tbs)?;
        // issuer
        let issuer = tbs.expect(der::TAG_SEQUENCE)?;
        // validity
        let (not_before, not_after) = {
            let mut v = tbs.sequence()?;
            let nb = parse_time(&mut v)?;
            let na = parse_time(&mut v)?;
            (nb, na)
        };
        // subject
        let subject = tbs.expect(der::TAG_SEQUENCE)?;
        // subjectPublicKeyInfo
        let public_key = parse_spki(&mut tbs)?;

        // 可选：issuerUniqueID [1]、subjectUniqueID [2]、extensions [3]
        let _ = tbs.take_if(0x81)?;
        let _ = tbs.take_if(0x82)?;
        let mut dns_names = Vec::new();
        let mut is_ca = false;
        let mut path_len = None;
        let mut unsupported_critical = false;
        if let Some(ext_holder) = tbs.take_if(0xa3)? {
            if version != 2 {
                return Err("X.509：只有 v3 证书才能带扩展".into());
            }
            let mut holder = Reader::new(ext_holder.value);
            let mut exts = holder.sequence()?;
            while !exts.is_empty() {
                let mut ext = exts.sequence()?;
                let id = ext.expect(der::TAG_OID)?;
                let critical = match ext.take_if(der::TAG_BOOLEAN)? {
                    Some(e) => e.value.first().copied().unwrap_or(0) != 0,
                    None => false,
                };
                let body = ext.expect(der::TAG_OCTET_STRING)?;
                match id.value {
                    oid::SUBJECT_ALT_NAME => {
                        dns_names = parse_san(body.value)?;
                    }
                    oid::BASIC_CONSTRAINTS => {
                        let (ca, pl) = parse_basic_constraints(body.value)?;
                        is_ca = ca;
                        path_len = pl;
                    }
                    // keyUsage 解析出来但不强制——registry 的链上没见过因它
                    // 出问题的，而误判会把正常证书拒掉。标 critical 也放行，
                    // 因为我们**认识**它，只是选择不执行。
                    oid::KEY_USAGE => {}
                    _ => {
                        if critical {
                            // RFC 5280 §4.2：不认识的 critical 扩展必须拒绝
                            // 整张证书。写成"忽略"正好绕过 CA 想强制的约束。
                            unsupported_critical = true;
                        }
                    }
                }
            }
        }

        // 外层 signatureAlgorithm 与 signatureValue
        let (sig_alg, outer_alg_raw) = parse_sig_alg_raw(&mut cert)?;
        let sig = cert.expect(der::TAG_BIT_STRING)?;
        let signature = sig.bit_string()?.to_vec();

        // TBS 里的算法必须与外层**逐字节**一致，否则是"两种解读"的经典缺口。
        if inner_alg_raw != outer_alg_raw {
            return Err("X.509：内外层签名算法不一致".into());
        }

        Ok(Certificate {
            der: der_bytes,
            tbs: tbs_elem.raw,
            subject: subject.raw,
            issuer: issuer.raw,
            not_before,
            not_after,
            public_key,
            sig_alg,
            signature,
            dns_names,
            is_ca,
            path_len,
            unsupported_critical,
        })
    }

    /// 有不认识的 critical 扩展吗？
    pub fn has_unsupported_critical_extension(&self) -> bool {
        self.unsupported_critical
    }

    /// 在 `now`（Unix 秒）时是否处于有效期内。
    pub fn valid_at(&self, now: i64) -> bool {
        now >= self.not_before && now <= self.not_after
    }

    /// 主机名是否匹配本证书的 SAN。
    pub fn matches_hostname(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.dns_names.iter().any(|n| dns_name_matches(n, &host))
    }

    /// 用本证书的公钥验证一段数据的签名。
    pub fn verify_signature(&self, alg: SigAlg, msg: &[u8], sig: &[u8]) -> bool {
        verify_with_key(&self.public_key, alg, msg, sig)
    }
}

/// 用给定公钥验签。TLS 的 CertificateVerify 也走这里。
pub fn verify_with_key(key: &PublicKey, alg: SigAlg, msg: &[u8], sig: &[u8]) -> bool {
    match (key, alg) {
        (PublicKey::Rsa(k), SigAlg::RsaPkcs1(h)) => crate::rsa::verify_pkcs1v15(k, h, msg, sig),
        (PublicKey::Rsa(k), SigAlg::RsaPss(h)) => crate::rsa::verify_pss(k, h, msg, sig),
        (PublicKey::EcP256(pt), SigAlg::Ecdsa(h)) => ecdsa_verify(&ec::p256(), pt, h, msg, sig),
        (PublicKey::EcP384(pt), SigAlg::Ecdsa(h)) => ecdsa_verify(&ec::p384(), pt, h, msg, sig),
        // 认不出的算法、或密钥类型与签名算法对不上：一律拒绝，
        // 不做任何"聪明"的回退。
        _ => false,
    }
}

/// ECDSA 的签名是 DER 编码的 `SEQUENCE { r INTEGER, s INTEGER }`。
fn ecdsa_verify(curve: &ec::Curve, pt: &ec::Point, h: HashAlg, msg: &[u8], sig: &[u8]) -> bool {
    let mut r = Reader::new(sig);
    let Ok(mut seq) = r.sequence() else {
        return false;
    };
    let (Ok(re), Ok(se)) = (seq.expect(der::TAG_INTEGER), seq.expect(der::TAG_INTEGER)) else {
        return false;
    };
    // 签名后面多出来的字节要拒绝——那是可塑性（malleability）的入口。
    if !seq.is_empty() || !r.is_empty() {
        return false;
    }
    let (Ok(rb), Ok(sb)) = (re.integer_bytes(), se.integer_bytes()) else {
        return false;
    };
    let digest = h.digest(msg);
    ec::verify(
        curve,
        pt,
        &digest,
        &BigUint::from_bytes_be(rb),
        &BigUint::from_bytes_be(sb),
    )
}

/// 解析 AlgorithmIdentifier，返回算法与它的**原始字节**。
///
/// 原始字节用于"内外层一致"检查：比枚举更严——两个都认不出来的不同算法
/// 会被枚举归成同一个 `Unsupported`，而字节比对不会。
fn parse_sig_alg_raw<'a>(r: &mut Reader<'a>) -> Result<(SigAlg, &'a [u8])> {
    let mut probe = *r;
    let raw = probe.expect(der::TAG_SEQUENCE)?.raw;
    let alg = parse_sig_alg(r)?;
    Ok((alg, raw))
}

fn parse_sig_alg(r: &mut Reader<'_>) -> Result<SigAlg> {
    let mut alg = r.sequence()?;
    let id = alg.expect(der::TAG_OID)?;
    let params = alg.rest();
    match id.value {
        oid::SHA256_RSA => Ok(SigAlg::RsaPkcs1(HashAlg::Sha256)),
        oid::SHA384_RSA => Ok(SigAlg::RsaPkcs1(HashAlg::Sha384)),
        oid::SHA512_RSA => Ok(SigAlg::RsaPkcs1(HashAlg::Sha512)),
        oid::ECDSA_SHA256 => Ok(SigAlg::Ecdsa(HashAlg::Sha256)),
        oid::ECDSA_SHA384 => Ok(SigAlg::Ecdsa(HashAlg::Sha384)),
        oid::RSA_PSS => {
            // PSS 的哈希写在参数里。参数缺失时 RFC 4055 的默认是 SHA-1，
            // 而 SHA-1 已不可信 —— 明确拒绝而不是默默用它。
            parse_pss_hash(params).map(SigAlg::RsaPss)
        }
        // 认不出来不报错，交给 verify_with_key 去拒绝。见 SigAlg::Unsupported。
        _ => Ok(SigAlg::Unsupported),
    }
}

/// 从 RSASSA-PSS 参数里取哈希算法。
fn parse_pss_hash(params: &[u8]) -> Result<HashAlg> {
    let mut r = Reader::new(params);
    let mut p = r
        .sequence()
        .map_err(|_| "X.509：PSS 缺少参数".to_string())?;
    // [0] hashAlgorithm
    let Some(h) = p.take_if(0xa0)? else {
        return Err("X.509：PSS 未指定哈希（默认 SHA-1 不予接受）".into());
    };
    let mut hr = Reader::new(h.value);
    let mut alg = hr.sequence()?;
    let id = alg.expect(der::TAG_OID)?;
    match id.value {
        // 2.16.840.1.101.3.4.2.1 / .2 / .3
        [0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01] => Ok(HashAlg::Sha256),
        [0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02] => Ok(HashAlg::Sha384),
        [0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03] => Ok(HashAlg::Sha512),
        other => Err(format!("X.509：PSS 的哈希 OID 不支持 {other:02x?}")),
    }
}

fn parse_spki(r: &mut Reader<'_>) -> Result<PublicKey> {
    let mut spki = r.sequence()?;
    let (alg_oid, curve_oid) = {
        let mut alg = spki.sequence()?;
        let id = alg.expect(der::TAG_OID)?;
        let curve = alg.take_if(der::TAG_OID)?.map(|e| e.value);
        (id.value, curve)
    };
    let key_bits = spki.expect(der::TAG_BIT_STRING)?;
    let key = key_bits.bit_string()?;

    match alg_oid {
        oid::RSA_ENCRYPTION => {
            let mut kr = Reader::new(key);
            let mut seq = kr.sequence()?;
            let n = seq.expect(der::TAG_INTEGER)?.integer_bytes()?;
            let e = seq.expect(der::TAG_INTEGER)?.integer_bytes()?;
            let n = BigUint::from_bytes_be(n);
            // 太短的模数不接受。1024 位 RSA 早已不该出现在公网证书上。
            if n.bits() < 2048 {
                return Ok(PublicKey::Unsupported);
            }
            Ok(PublicKey::Rsa(RsaPublicKey {
                n,
                e: BigUint::from_bytes_be(e),
            }))
        }
        oid::EC_PUBLIC_KEY => match curve_oid {
            Some(oid::P256) => ec::p256()
                .parse_public_key(key)
                .map(PublicKey::EcP256)
                .ok_or_else(|| "X.509：P-256 公钥非法".to_string()),
            Some(oid::P384) => ec::p384()
                .parse_public_key(key)
                .map(PublicKey::EcP384)
                .ok_or_else(|| "X.509：P-384 公钥非法".to_string()),
            // P-521、brainpool 之类：留成 Unsupported，不让它拖垮解析。
            _ => Ok(PublicKey::Unsupported),
        },
        // Ed25519 等：同上。
        _ => Ok(PublicKey::Unsupported),
    }
}

/// 解析 UTCTime / GeneralizedTime 成 Unix 秒。
fn parse_time(r: &mut Reader<'_>) -> Result<i64> {
    let e = r.read()?;
    let s = std::str::from_utf8(e.value).map_err(|_| "X.509：时间不是 ASCII".to_string())?;
    let (year, rest) = match e.tag {
        der::TAG_UTC_TIME => {
            // YYMMDDHHMMSSZ；RFC 5280 §4.1.2.5.1：YY >= 50 归 19xx，否则 20xx。
            if s.len() != 13 || !s.ends_with('Z') {
                return Err(format!("X.509：UTCTime 格式非法 {s:?}"));
            }
            let yy: i64 = s[0..2].parse().map_err(|_| "X.509：年份非法".to_string())?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &s[2..])
        }
        der::TAG_GENERALIZED_TIME => {
            if s.len() != 15 || !s.ends_with('Z') {
                return Err(format!("X.509：GeneralizedTime 格式非法 {s:?}"));
            }
            let y: i64 = s[0..4].parse().map_err(|_| "X.509：年份非法".to_string())?;
            (y, &s[4..])
        }
        t => return Err(format!("X.509：不是时间类型（标签 0x{t:02x}）")),
    };
    let num = |a: usize, b: usize| -> Result<i64> {
        rest[a..b]
            .parse::<i64>()
            .map_err(|_| "X.509：时间字段非法".to_string())
    };
    let (mon, day, hh, mm, ss) = (num(0, 2)?, num(2, 4)?, num(4, 6)?, num(6, 8)?, num(8, 10)?);
    if !(1..=12).contains(&mon) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 {
        return Err("X.509：时间字段越界".into());
    }
    Ok(days_from_civil(year, mon, day) * 86400 + hh * 3600 + mm * 60 + ss)
}

/// 公历日期 → 自 1970-01-01 起的天数（Howard Hinnant 的 days_from_civil）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn parse_san(data: &[u8]) -> Result<Vec<String>> {
    let mut r = Reader::new(data);
    let mut seq = r.sequence()?;
    let mut names = Vec::new();
    while !seq.is_empty() {
        let e = seq.read()?;
        // [2] dNSName（IMPLICIT，所以是 0x82）
        if e.tag == 0x82 {
            let s = std::str::from_utf8(e.value)
                .map_err(|_| "X.509：SAN 的 dNSName 不是 ASCII".to_string())?;
            names.push(s.to_ascii_lowercase());
        }
        // 其它形式（IP、email、URI）不收——registry 只用 dNSName。
    }
    Ok(names)
}

fn parse_basic_constraints(data: &[u8]) -> Result<(bool, Option<u32>)> {
    let mut r = Reader::new(data);
    let mut seq = r.sequence()?;
    let ca = match seq.take_if(der::TAG_BOOLEAN)? {
        Some(e) => e.value.first().copied().unwrap_or(0) != 0,
        None => false,
    };
    let path_len = match seq.take_if(der::TAG_INTEGER)? {
        Some(e) => {
            let b = e.integer_bytes()?;
            if b.len() > 4 {
                return Err("X.509：pathLenConstraint 过大".into());
            }
            let mut v = 0u32;
            for &x in b {
                v = (v << 8) | x as u32;
            }
            Some(v)
        }
        None => None,
    };
    Ok((ca, path_len))
}

/// 主机名匹配，含通配符（RFC 6125）。
///
/// 通配符的规矩比想象中严：只允许出现在**最左一段**，只能是整段
/// （`*.example.com`，不是 `w*.example.com`），且**不跨点**——
/// `*.example.com` 不匹配 `a.b.example.com`。
fn dns_name_matches(pattern: &str, host: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // 通配符至少要覆盖两段以上的域，`*.com` 这种必须拒绝。
        if suffix.split('.').count() < 2 {
            return false;
        }
        // host 的第一段被通配符吃掉，剩下的必须完全相等。
        return match host.split_once('.') {
            Some((first, rest)) => !first.is_empty() && rest == suffix,
            None => false,
        };
    }
    pattern == host
}

/// 链校验的结果：验证通过时给出叶子证书的公钥（TLS 要用它验
/// `CertificateVerify`）。
pub struct Verified {
    pub public_key: PublicKey,
}

/// 校验证书链。
///
/// `chain[0]` 是叶子，其余是按序的中间证书（TLS 就是这个顺序）。
/// `roots` 是内置根证书的 DER 列表。
pub fn verify_chain(
    chain: &[&[u8]],
    roots: &[&[u8]],
    hostname: &str,
    now: i64,
) -> Result<Verified> {
    if chain.is_empty() {
        return Err("证书链为空".into());
    }
    // 链长上限：防止对端塞一条超长链把验签时间拖垮（每级都是一次公钥运算）。
    if chain.len() > 10 {
        return Err("证书链过长".into());
    }

    let certs: Vec<Certificate<'_>> = chain
        .iter()
        .map(|d| Certificate::parse(d))
        .collect::<Result<_>>()?;

    for (i, c) in certs.iter().enumerate() {
        if c.has_unsupported_critical_extension() {
            return Err(format!("证书 #{i} 带有不认识的 critical 扩展"));
        }
        if !c.valid_at(now) {
            return Err(format!(
                "证书 #{i} 不在有效期内（{} .. {}，现在 {now}）",
                c.not_before, c.not_after
            ));
        }
    }

    // 叶子证书要匹配主机名。
    let leaf = &certs[0];
    if !leaf.matches_hostname(hostname) {
        return Err(format!(
            "证书的 SAN {:?} 不匹配主机名 {hostname}",
            leaf.dns_names
        ));
    }
    if leaf.is_ca && certs.len() > 1 {
        // 叶子不该是 CA。这不是致命错，但值得拒绝：能签发别的证书的密钥
        // 不该同时用来当服务端身份。
        return Err("叶子证书带 CA 标志".into());
    }

    // 逐级：certs[i] 必须由 certs[i+1] 签发，最后一级由某个根签发。
    for i in 0..certs.len() {
        let child = &certs[i];
        if let Some(parent) = certs.get(i + 1) {
            if !parent.is_ca {
                return Err(format!("证书 #{} 不是 CA，不能签发下级", i + 1));
            }
            // pathLen 约束：parent 之下还能有多少级中间 CA。
            if let Some(limit) = parent.path_len {
                // i 级以下已经用掉 i 个中间层（不含叶子）。
                if (i as u32) > limit {
                    return Err(format!("证书 #{} 的 pathLen 约束被突破", i + 1));
                }
            }
            if child.issuer != parent.subject {
                return Err(format!("证书 #{i} 的 issuer 与上级 subject 不一致"));
            }
            if !parent.verify_signature(child.sig_alg, child.tbs, &child.signature) {
                return Err(format!("证书 #{i} 的签名验证失败"));
            }
        } else {
            // 最后一级：在根里找 subject == child.issuer 且能验签的。
            let mut ok = false;
            for rd in roots {
                let Ok(root) = Certificate::parse(rd) else {
                    continue;
                };
                if root.subject != child.issuer {
                    continue;
                }
                if !root.valid_at(now) {
                    continue;
                }
                if !root.is_ca {
                    continue;
                }
                if root.verify_signature(child.sig_alg, child.tbs, &child.signature) {
                    ok = true;
                    break;
                }
            }
            if !ok {
                return Err("证书链无法回溯到受信任的根".into());
            }
        }
    }

    Ok(Verified {
        public_key: certs[0].public_key.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matching_follows_rfc6125() {
        // 通配符只在最左一段、必须整段、不跨点。这三条任何一条放松，
        // 都会让攻击者用一张证书覆盖掉本不该覆盖的主机。
        assert!(dns_name_matches("example.com", "example.com"));
        assert!(dns_name_matches("*.example.com", "www.example.com"));
        assert!(dns_name_matches("*.example.com", "a-b.example.com"));

        // 不跨点
        assert!(!dns_name_matches("*.example.com", "a.b.example.com"));
        // 不匹配裸域
        assert!(!dns_name_matches("*.example.com", "example.com"));
        // 通配符不能只覆盖一段（*.com 太宽）
        assert!(!dns_name_matches("*.com", "example.com"));
        // 部分通配（w*.example.com）不支持
        assert!(!dns_name_matches("w*.example.com", "www.example.com"));
        // 通配符不在最左段
        assert!(!dns_name_matches("www.*.com", "www.example.com"));
        // 空的第一段
        assert!(!dns_name_matches("*.example.com", ".example.com"));
        // 不同域
        assert!(!dns_name_matches("example.com", "evil.com"));
    }

    #[test]
    fn hostname_matching_is_case_and_dot_insensitive() {
        let cert = Certificate {
            der: &[],
            tbs: &[],
            subject: &[],
            issuer: &[],
            not_before: 0,
            not_after: i64::MAX,
            public_key: PublicKey::Rsa(RsaPublicKey {
                n: BigUint::one(),
                e: BigUint::one(),
            }),
            sig_alg: SigAlg::RsaPkcs1(HashAlg::Sha256),
            signature: vec![],
            dns_names: vec!["registry-1.docker.io".into()],
            is_ca: false,
            path_len: None,
            unsupported_critical: false,
        };
        assert!(cert.matches_hostname("registry-1.docker.io"));
        assert!(cert.matches_hostname("Registry-1.Docker.IO"));
        // 末尾的根点要忽略
        assert!(cert.matches_hostname("registry-1.docker.io."));
        assert!(!cert.matches_hostname("evil.docker.io"));
    }

    #[test]
    fn civil_date_conversion_matches_known_epochs() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2024, 2, 29), 19782); // 闰日
        assert_eq!(days_from_civil(2038, 1, 19), 24855);
    }

    #[test]
    fn utc_time_two_digit_year_pivot() {
        // RFC 5280：YY >= 50 归 19xx。写错的话 2049 与 1950 会互换，
        // 而那意味着有效期判断整个反过来。
        let mk = |tag: u8, s: &str| {
            let mut d = vec![tag, s.len() as u8];
            d.extend_from_slice(s.as_bytes());
            let mut r = Reader::new(&d);
            parse_time(&mut r)
        };
        let y2049 = mk(der::TAG_UTC_TIME, "490101000000Z").unwrap();
        let y1950 = mk(der::TAG_UTC_TIME, "500101000000Z").unwrap();
        assert!(y2049 > 0, "2049 应当在纪元之后");
        assert!(y1950 < 0, "1950 应当在纪元之前");
        // GeneralizedTime 用四位年份
        let g = mk(der::TAG_GENERALIZED_TIME, "20240229123456Z").unwrap();
        assert_eq!(
            g,
            days_from_civil(2024, 2, 29) * 86400 + 12 * 3600 + 34 * 60 + 56
        );
    }

    #[test]
    fn rejects_malformed_times() {
        let mk = |tag: u8, s: &str| {
            let mut d = vec![tag, s.len() as u8];
            d.extend_from_slice(s.as_bytes());
            let mut r = Reader::new(&d);
            parse_time(&mut r)
        };
        assert!(mk(der::TAG_UTC_TIME, "24010100000Z").is_err(), "长度不对");
        assert!(mk(der::TAG_UTC_TIME, "240101000000").is_err(), "缺 Z");
        assert!(mk(der::TAG_UTC_TIME, "241301000000Z").is_err(), "月份 13");
        assert!(mk(der::TAG_UTC_TIME, "240100000000Z").is_err(), "日 0");
        assert!(mk(der::TAG_UTC_TIME, "240101250000Z").is_err(), "小时 25");
    }

    #[test]
    fn empty_chain_is_rejected() {
        assert!(verify_chain(&[], &[], "example.com", 0).is_err());
    }
}
