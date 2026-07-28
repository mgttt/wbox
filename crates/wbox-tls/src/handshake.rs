//! TLS 1.3 客户端握手（RFC 8446）。
//!
//! # 范围：只做 1.3，且只做一条路
//!
//! 只支持 **TLS 1.3**、**X25519** 密钥协商、**TLS_AES_128_GCM_SHA256** 与
//! **TLS_AES_256_GCM_SHA384** 两个套件。不做 TLS 1.2 回退、不做会话恢复
//! （PSK / 0-RTT）、不做客户端证书。
//!
//! 这不是偷懒，是**减少攻击面**：TLS 的历史漏洞里有很大一部分出在版本回退
//! 与旧套件上（FREAK、Logjam、POODLE 都是）。registry 全都支持 1.3，
//! 没有回退的必要；不实现就不可能被降级到它。对端不支持 1.3 时**明确报错**，
//! 而不是悄悄退回一个更弱的协议。
//!
//! # 握手流程
//!
//! ```text
//! Client                                    Server
//! ClientHello (key_share: X25519)  -------->
//!                                  <-------- ServerHello (key_share)
//!            ==== 从这里起用 handshake 密钥加密 ====
//!                                  <-------- EncryptedExtensions
//!                                  <-------- Certificate
//!                                  <-------- CertificateVerify
//!                                  <-------- Finished
//! Finished                         -------->
//!            ==== 从这里起用 application 密钥 ====
//! ```
//!
//! # 三处必须做对的验证
//!
//! 1. **ServerHello 的版本要看 supported_versions 扩展**，不是记录层版本，
//!    也不是 legacy_version 字段——后两者在 1.3 里恒为 0x0303。
//! 2. **CertificateVerify 的签名对象有固定前缀**（64 个 0x20 + 上下文串 +
//!    0x00 + 转录哈希）。少一段就验不过，且错误信息指不到这里。
//! 3. **Finished 是对转录哈希的 HMAC**，密钥由 `finished_key` 派生。
//!    它是整个握手的完整性校验——不验它，中间人可以任意篡改前面的消息。

use crate::hash::{hmac, HashAlg};
use crate::kdf;
use crate::record::{self, ContentType, Keys};
use crate::x509::{self, SigAlg};
use crate::{rand, x25519};

/// 握手消息类型。
const HS_CLIENT_HELLO: u8 = 1;
const HS_SERVER_HELLO: u8 = 2;
const HS_NEW_SESSION_TICKET: u8 = 4;
const HS_ENCRYPTED_EXTENSIONS: u8 = 8;
const HS_CERTIFICATE: u8 = 11;
const HS_CERTIFICATE_VERIFY: u8 = 15;
const HS_FINISHED: u8 = 20;

/// 支持的套件。值即 RFC 8446 的编号。
const TLS_AES_128_GCM_SHA256: u16 = 0x1301;
const TLS_AES_256_GCM_SHA384: u16 = 0x1302;

/// X25519 的 NamedGroup 编号。
const GROUP_X25519: u16 = 0x001d;

/// 我们声明支持的签名算法（RFC 8446 §4.2.3 的 SignatureScheme）。
const SIG_ECDSA_P256_SHA256: u16 = 0x0403;
const SIG_ECDSA_P384_SHA384: u16 = 0x0503;
const SIG_RSA_PSS_RSAE_SHA256: u16 = 0x0804;
const SIG_RSA_PSS_RSAE_SHA384: u16 = 0x0805;
const SIG_RSA_PSS_RSAE_SHA512: u16 = 0x0806;
const SIG_RSA_PKCS1_SHA256: u16 = 0x0401;
const SIG_RSA_PKCS1_SHA384: u16 = 0x0501;

pub type Result<T> = std::result::Result<T, String>;

/// 协商出来的套件参数。
#[derive(Clone, Copy)]
struct Suite {
    hash: HashAlg,
    key_len: usize,
}

fn suite_params(id: u16) -> Option<Suite> {
    match id {
        TLS_AES_128_GCM_SHA256 => Some(Suite {
            hash: HashAlg::Sha256,
            key_len: 16,
        }),
        TLS_AES_256_GCM_SHA384 => Some(Suite {
            hash: HashAlg::Sha384,
            key_len: 32,
        }),
        _ => None,
    }
}

/// 往字节流里写 TLS 的变长向量。
struct Writer(Vec<u8>);

impl Writer {
    fn new() -> Writer {
        Writer(Vec::new())
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.0.extend_from_slice(v);
    }
    /// 写一个带 1 字节长度前缀的块。
    fn block_u8(&mut self, f: impl FnOnce(&mut Writer)) {
        let at = self.0.len();
        self.0.push(0);
        f(self);
        let n = self.0.len() - at - 1;
        self.0[at] = n as u8;
    }
    /// 写一个带 2 字节长度前缀的块。
    fn block_u16(&mut self, f: impl FnOnce(&mut Writer)) {
        let at = self.0.len();
        self.0.extend_from_slice(&[0, 0]);
        f(self);
        let n = self.0.len() - at - 2;
        self.0[at..at + 2].copy_from_slice(&(n as u16).to_be_bytes());
    }
    /// 写一个带 3 字节长度前缀的块（握手消息与证书用）。
    fn block_u24(&mut self, f: impl FnOnce(&mut Writer)) {
        let at = self.0.len();
        self.0.extend_from_slice(&[0, 0, 0]);
        f(self);
        let n = self.0.len() - at - 3;
        self.0[at] = (n >> 16) as u8;
        self.0[at + 1] = (n >> 8) as u8;
        self.0[at + 2] = n as u8;
    }
}

/// 顺序读取的字节游标。
struct Cursor<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cursor<'a> {
    fn new(d: &'a [u8]) -> Cursor<'a> {
        Cursor { d, p: 0 }
    }
    fn left(&self) -> usize {
        self.d.len() - self.p
    }
    fn is_empty(&self) -> bool {
        self.left() == 0
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.left() < n {
            return Err(format!(
                "TLS：消息截断（要 {n} 字节，只剩 {}）",
                self.left()
            ));
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u24(&mut self) -> Result<usize> {
        let b = self.take(3)?;
        Ok(((b[0] as usize) << 16) | ((b[1] as usize) << 8) | b[2] as usize)
    }
    fn block_u8(&mut self) -> Result<&'a [u8]> {
        let n = self.u8()? as usize;
        self.take(n)
    }
    fn block_u16(&mut self) -> Result<&'a [u8]> {
        let n = self.u16()? as usize;
        self.take(n)
    }
}

/// 构造 ClientHello 的**消息体**（不含握手头）。
fn client_hello(client_random: &[u8; 32], public_key: &[u8; 32], server_name: &str) -> Vec<u8> {
    let mut w = Writer::new();
    // legacy_version 恒 0x0303（真实版本在 supported_versions 里）
    w.u16(0x0303);
    w.bytes(client_random);
    // legacy_session_id：随便给 32 字节，兼容中间设备（RFC 8446 §4.1.2）
    w.block_u8(|w| w.bytes(&rand::bytes::<32>()));
    // cipher_suites
    w.block_u16(|w| {
        w.u16(TLS_AES_128_GCM_SHA256);
        w.u16(TLS_AES_256_GCM_SHA384);
    });
    // legacy_compression_methods：TLS 1.3 只允许 null
    w.block_u8(|w| w.u8(0));

    w.block_u16(|w| {
        // server_name (0)
        if !server_name.is_empty() && !is_ip_literal(server_name) {
            w.u16(0);
            w.block_u16(|w| {
                w.block_u16(|w| {
                    w.u8(0); // host_name
                    w.block_u16(|w| w.bytes(server_name.as_bytes()));
                });
            });
        }
        // supported_groups (10)
        w.u16(10);
        w.block_u16(|w| w.block_u16(|w| w.u16(GROUP_X25519)));
        // signature_algorithms (13)
        w.u16(13);
        w.block_u16(|w| {
            w.block_u16(|w| {
                for s in [
                    SIG_ECDSA_P256_SHA256,
                    SIG_ECDSA_P384_SHA384,
                    SIG_RSA_PSS_RSAE_SHA256,
                    SIG_RSA_PSS_RSAE_SHA384,
                    SIG_RSA_PSS_RSAE_SHA512,
                    SIG_RSA_PKCS1_SHA256,
                    SIG_RSA_PKCS1_SHA384,
                ] {
                    w.u16(s);
                }
            })
        });
        // supported_versions (43)：**只列 1.3**。不列 1.2 就不可能被降级。
        w.u16(43);
        w.block_u16(|w| w.block_u8(|w| w.u16(0x0304)));
        // key_share (51)
        w.u16(51);
        w.block_u16(|w| {
            w.block_u16(|w| {
                w.u16(GROUP_X25519);
                w.block_u16(|w| w.bytes(public_key));
            })
        });
    });
    w.0
}

/// 主机名是不是 IP 字面量（SNI 里不该放 IP）。
fn is_ip_literal(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

/// 把消息体包成握手消息（1 字节类型 + 3 字节长度）。
fn handshake_message(ty: u8, body: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(ty);
    w.block_u24(|w| w.bytes(body));
    w.0
}

/// 握手产出的密钥材料。
pub struct Secrets {
    pub client: Keys,
    pub server: Keys,
}

/// 已完成的握手。
pub struct Handshake {
    pub secrets: Secrets,
}

/// 底层字节通道（TCP 或代理隧道）。
pub trait Transcript {
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()>;
    fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()>;
}

impl<T: std::io::Read + std::io::Write> Transcript for T {
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(self, buf)
    }
    fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        std::io::Read::read_exact(self, buf)
    }
}

/// 读一条记录，返回（外层类型, 头, 体）。
fn read_record(io: &mut impl Transcript) -> Result<(u8, [u8; 5], Vec<u8>)> {
    let mut header = [0u8; 5];
    io.read_exact(&mut header)
        .map_err(|e| format!("TLS：读记录头失败：{e}"))?;
    let len = u16::from_be_bytes([header[3], header[4]]) as usize;
    if len > record::MAX_CIPHERTEXT {
        return Err(format!("TLS：记录过长（{len}）"));
    }
    let mut body = vec![0u8; len];
    io.read_exact(&mut body)
        .map_err(|e| format!("TLS：读记录体失败：{e}"))?;
    Ok((header[0], header, body))
}

/// 从记录流里取握手消息的读取器。
///
/// 握手消息可能**跨记录分片**，也可能一条记录里挤了好几条消息——
/// 两种情况真实服务器都会产生，所以必须有这一层重组。
struct HandshakeReader {
    buf: Vec<u8>,
}

impl HandshakeReader {
    fn new() -> Self {
        HandshakeReader { buf: Vec::new() }
    }

    fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// 取出一条完整的握手消息（含头），不足则返回 `None`。
    fn take(&mut self) -> Result<Option<Vec<u8>>> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len =
            ((self.buf[1] as usize) << 16) | ((self.buf[2] as usize) << 8) | self.buf[3] as usize;
        // 单条握手消息上限：证书链可以很大，但不该无上限。
        if len > 1 << 20 {
            return Err(format!("TLS：握手消息过长（{len}）"));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        Ok(Some(self.buf.drain(..4 + len).collect()))
    }
}

/// 执行完整的客户端握手。
///
/// `hostname` 用于 SNI 与证书主机名校验；`now` 是当前 Unix 秒（证书有效期）。
pub fn client_handshake(
    io: &mut impl Transcript,
    hostname: &str,
    now: i64,
    extra_roots: &[Vec<u8>],
) -> Result<Handshake> {
    // ---- 1. 发 ClientHello ----
    let secret = rand::bytes::<32>();
    let public = x25519::public_key(&secret);
    let client_random = rand::bytes::<32>();
    let ch_body = client_hello(&client_random, &public, hostname);
    let ch = handshake_message(HS_CLIENT_HELLO, &ch_body);
    io.write_all(&record::plaintext_record(ContentType::Handshake, &ch))
        .map_err(|e| format!("TLS：发送 ClientHello 失败：{e}"))?;

    // ---- 2. 收 ServerHello ----
    let mut hs_reader = HandshakeReader::new();
    let sh = loop {
        let (ty, _h, body) = read_record(io)?;
        match ContentType::from_u8(ty) {
            Some(ContentType::Handshake) => {
                hs_reader.feed(&body);
                if let Some(m) = hs_reader.take()? {
                    break m;
                }
            }
            // 中间设备兼容用的 CCS，直接忽略（RFC 8446 §5）。
            Some(ContentType::ChangeCipherSpec) => continue,
            Some(ContentType::Alert) => return Err(alert_text(&body)),
            _ => return Err("TLS：ServerHello 之前收到意外的记录".into()),
        }
    };
    if sh.first() != Some(&HS_SERVER_HELLO) {
        return Err("TLS：期待 ServerHello".into());
    }
    let (suite_id, server_public) = parse_server_hello(&sh[4..])?;
    let suite = suite_params(suite_id).ok_or("TLS：服务器选了我们不支持的套件")?;

    // ---- 3. 密钥调度 ----
    let mut transcript = suite.hash.hasher();
    transcript.update(&ch);
    transcript.update(&sh);

    let shared = x25519::shared_secret(&secret, &server_public)
        .ok_or("TLS：X25519 共享密钥为零（对端给了小子群点）")?;

    let h = suite.hash;
    let zeros = vec![0u8; h.len()];
    let early = kdf::extract(h, &[], &zeros);
    let derived = kdf::derive_secret(h, &early, "derived", &h.digest(&[]));
    let handshake_secret = kdf::extract(h, &derived, &shared);

    let th_sh = transcript.snapshot().finish();
    let client_hs = kdf::derive_secret(h, &handshake_secret, "c hs traffic", &th_sh);
    let server_hs = kdf::derive_secret(h, &handshake_secret, "s hs traffic", &th_sh);

    let mut client_keys = traffic_keys(h, &client_hs, suite.key_len);
    let mut server_keys = traffic_keys(h, &server_hs, suite.key_len);

    // ---- 4. 收服务器的加密握手消息 ----
    let mut hs_reader = HandshakeReader::new();
    let mut cert_chain: Vec<Vec<u8>> = Vec::new();
    let mut peer_key: Option<x509::PublicKey> = None;
    let mut got_finished = false;
    // 收到 CertificateVerify 时，转录哈希要停在它**之前**。
    let mut th_before_cv: Option<Vec<u8>> = None;

    while !got_finished {
        let (ty, header, mut body) = read_record(io)?;
        match ContentType::from_u8(ty) {
            Some(ContentType::ChangeCipherSpec) => continue,
            Some(ContentType::ApplicationData) => {}
            Some(ContentType::Alert) => return Err(alert_text(&body)),
            _ => return Err("TLS：握手期间收到意外的记录类型".into()),
        }
        let inner_ty = server_keys.open(&header, &mut body)?;
        match inner_ty {
            ContentType::Alert => return Err(alert_text(&body)),
            ContentType::Handshake => {}
            _ => return Err("TLS：握手期间收到意外的加密记录".into()),
        }
        hs_reader.feed(&body);

        while let Some(msg) = hs_reader.take()? {
            let ty = msg[0];
            let payload = &msg[4..];
            match ty {
                HS_ENCRYPTED_EXTENSIONS => {
                    transcript.update(&msg);
                }
                HS_CERTIFICATE => {
                    cert_chain = parse_certificate(payload)?;
                    transcript.update(&msg);
                }
                HS_CERTIFICATE_VERIFY => {
                    // 签名覆盖的是**这条消息之前**的转录，所以先取快照。
                    let th = transcript.snapshot().finish().to_vec();
                    th_before_cv = Some(th.clone());

                    let roots: Vec<&[u8]> = crate::roots::TRUSTED_ROOTS
                        .iter()
                        .copied()
                        .chain(extra_roots.iter().map(|v| v.as_slice()))
                        .collect();
                    let chain: Vec<&[u8]> = cert_chain.iter().map(|v| v.as_slice()).collect();
                    let verified = x509::verify_chain(&chain, &roots, hostname, now)?;

                    verify_certificate_verify(payload, &verified.public_key, &th)?;
                    peer_key = Some(verified.public_key);
                    transcript.update(&msg);
                }
                HS_FINISHED => {
                    if peer_key.is_none() {
                        return Err("TLS：服务器没有出示可验证的证书".into());
                    }
                    let th = transcript.snapshot().finish();
                    let key = kdf::expand_label(h, &server_hs, "finished", &[], h.len());
                    let expect = hmac(h, &key, &th);
                    if payload != &*expect {
                        return Err("TLS：服务器 Finished 校验失败".into());
                    }
                    transcript.update(&msg);
                    got_finished = true;
                    break;
                }
                HS_NEW_SESSION_TICKET => {
                    // 我们不做会话恢复，票据直接丢弃（也不进转录）。
                }
                other => return Err(format!("TLS：握手期间收到意外的消息类型 {other}")),
            }
        }
    }
    let _ = th_before_cv;

    // ---- 5. 发客户端 Finished ----
    let th_done = transcript.snapshot().finish();
    let ckey = kdf::expand_label(h, &client_hs, "finished", &[], h.len());
    let cfin = hmac(h, &ckey, &th_done);
    let fin_msg = handshake_message(HS_FINISHED, &cfin);
    // 兼容中间设备：先发一条明文 CCS（RFC 8446 附录 D.4）。
    io.write_all(&record::plaintext_record(
        ContentType::ChangeCipherSpec,
        &[1],
    ))
    .map_err(|e| format!("TLS：发送 CCS 失败：{e}"))?;
    let wire = client_keys.seal(ContentType::Handshake, &fin_msg);
    io.write_all(&wire)
        .map_err(|e| format!("TLS：发送 Finished 失败：{e}"))?;

    // ---- 6. 切到应用密钥 ----
    let derived = kdf::derive_secret(h, &handshake_secret, "derived", &h.digest(&[]));
    let master = kdf::extract(h, &derived, &zeros);
    let client_app = kdf::derive_secret(h, &master, "c ap traffic", &th_done);
    let server_app = kdf::derive_secret(h, &master, "s ap traffic", &th_done);

    Ok(Handshake {
        secrets: Secrets {
            client: traffic_keys(h, &client_app, suite.key_len),
            server: traffic_keys(h, &server_app, suite.key_len),
        },
    })
}

/// 由 traffic secret 派生记录层密钥与 IV。
fn traffic_keys(h: HashAlg, secret: &[u8], key_len: usize) -> Keys {
    let key = kdf::expand_label(h, secret, "key", &[], key_len);
    let iv = kdf::expand_label(h, secret, "iv", &[], 12);
    Keys::new(&key, &iv)
}

fn parse_server_hello(body: &[u8]) -> Result<(u16, [u8; 32])> {
    let mut c = Cursor::new(body);
    let _legacy_version = c.u16()?;
    let random = c.take(32)?;
    // HelloRetryRequest 的 random 是一个固定的特殊值。我们只提供 X25519，
    // 服务器要 HRR 说明它想要别的组——那我们给不了，明确报错。
    const HRR: [u8; 32] = [
        0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8,
        0x91, 0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8,
        0x33, 0x9c,
    ];
    if random == HRR {
        return Err("TLS：服务器要求 HelloRetryRequest（我们只提供 X25519）".into());
    }
    let _session_id = c.block_u8()?;
    let suite = c.u16()?;
    let _compression = c.u8()?;

    let exts = c.block_u16()?;
    let mut e = Cursor::new(exts);
    let mut version_ok = false;
    let mut server_public: Option<[u8; 32]> = None;
    while !e.is_empty() {
        let ext_type = e.u16()?;
        let data = e.block_u16()?;
        match ext_type {
            43 => {
                // supported_versions：**真实版本看这里**，不是 legacy_version。
                let mut d = Cursor::new(data);
                if d.u16()? != 0x0304 {
                    return Err("TLS：服务器不是 TLS 1.3".into());
                }
                version_ok = true;
            }
            51 => {
                let mut d = Cursor::new(data);
                let group = d.u16()?;
                if group != GROUP_X25519 {
                    return Err(format!("TLS：服务器选了非 X25519 的组（{group}）"));
                }
                let key = d.block_u16()?;
                if key.len() != 32 {
                    return Err("TLS：X25519 公钥长度不对".into());
                }
                let mut k = [0u8; 32];
                k.copy_from_slice(key);
                server_public = Some(k);
            }
            _ => {}
        }
    }
    if !version_ok {
        // 没有 supported_versions 扩展 = 对端在说 TLS 1.2 或更早。
        // **明确报错而不是降级**：不实现回退就不可能被降级攻击。
        return Err("TLS：服务器未协商 TLS 1.3（缺 supported_versions）".into());
    }
    let pk = server_public.ok_or("TLS：ServerHello 缺 key_share")?;
    Ok((suite, pk))
}

/// 解析 Certificate 消息，返回按序的 DER 证书链。
fn parse_certificate(payload: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut c = Cursor::new(payload);
    let ctx = c.block_u8()?;
    if !ctx.is_empty() {
        return Err("TLS：服务器 Certificate 的上下文应为空".into());
    }
    let list_len = c.u24()?;
    let list = c.take(list_len)?;
    let mut l = Cursor::new(list);
    let mut out = Vec::new();
    while !l.is_empty() {
        let n = l.u24()?;
        out.push(l.take(n)?.to_vec());
        // 每张证书后跟一段扩展，忽略内容但要跳过。
        let _exts = l.block_u16()?;
    }
    if out.is_empty() {
        return Err("TLS：服务器没有出示证书".into());
    }
    Ok(out)
}

/// 验证 CertificateVerify（RFC 8446 §4.4.3）。
fn verify_certificate_verify(
    payload: &[u8],
    key: &x509::PublicKey,
    transcript_hash: &[u8],
) -> Result<()> {
    let mut c = Cursor::new(payload);
    let scheme = c.u16()?;
    let sig = c.block_u16()?;
    if !c.is_empty() {
        return Err("TLS：CertificateVerify 后有多余字节".into());
    }

    // 签名对象有固定前缀：64 个 0x20 + 上下文串 + 0x00 + 转录哈希。
    // 少一段就验不过，而错误信息完全指不到这里。
    let mut signed = Vec::with_capacity(64 + 34 + transcript_hash.len());
    signed.extend_from_slice(&[0x20u8; 64]);
    signed.extend_from_slice(b"TLS 1.3, server CertificateVerify");
    signed.push(0x00);
    signed.extend_from_slice(transcript_hash);

    let alg = match scheme {
        SIG_ECDSA_P256_SHA256 => SigAlg::Ecdsa(HashAlg::Sha256),
        SIG_ECDSA_P384_SHA384 => SigAlg::Ecdsa(HashAlg::Sha384),
        SIG_RSA_PSS_RSAE_SHA256 => SigAlg::RsaPss(HashAlg::Sha256),
        SIG_RSA_PSS_RSAE_SHA384 => SigAlg::RsaPss(HashAlg::Sha384),
        SIG_RSA_PSS_RSAE_SHA512 => SigAlg::RsaPss(HashAlg::Sha512),
        SIG_RSA_PKCS1_SHA256 => SigAlg::RsaPkcs1(HashAlg::Sha256),
        SIG_RSA_PKCS1_SHA384 => SigAlg::RsaPkcs1(HashAlg::Sha384),
        other => return Err(format!("TLS：不支持的签名方案 0x{other:04x}")),
    };
    if !x509::verify_with_key(key, alg, &signed, sig) {
        return Err("TLS：CertificateVerify 签名无效".into());
    }
    Ok(())
}

/// 把 alert 记录翻译成人能看懂的错误。
fn alert_text(body: &[u8]) -> String {
    if body.len() < 2 {
        return "TLS：收到格式不明的 alert".into();
    }
    let desc = match body[1] {
        0 => "close_notify",
        40 => "handshake_failure",
        42 => "bad_certificate",
        43 => "unsupported_certificate",
        44 => "certificate_revoked",
        45 => "certificate_expired",
        47 => "illegal_parameter",
        48 => "unknown_ca",
        50 => "decode_error",
        51 => "decrypt_error",
        70 => "protocol_version",
        71 => "insufficient_security",
        80 => "internal_error",
        112 => "unrecognized_name",
        116 => "certificate_required",
        120 => "no_application_protocol",
        _ => "unknown",
    };
    format!("TLS：对端发来 alert（{}，代码 {}）", desc, body[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_hello_has_the_required_shape() {
        let ch = client_hello(&[0xaa; 32], &[0xbb; 32], "registry-1.docker.io");
        let mut c = Cursor::new(&ch);
        assert_eq!(c.u16().unwrap(), 0x0303, "legacy_version 恒 0x0303");
        assert_eq!(c.take(32).unwrap(), [0xaa; 32]);
        assert_eq!(c.block_u8().unwrap().len(), 32, "legacy_session_id");
        let suites = c.block_u16().unwrap();
        assert_eq!(suites.len(), 4, "两个套件");
        assert_eq!(c.block_u8().unwrap(), [0], "只允许 null 压缩");
        let exts = c.block_u16().unwrap();

        // supported_versions 里**只能有 1.3**——列了 1.2 就可能被降级。
        let mut e = Cursor::new(exts);
        let mut seen_versions = false;
        let mut seen_sni = false;
        let mut seen_keyshare = false;
        while !e.is_empty() {
            let t = e.u16().unwrap();
            let d = e.block_u16().unwrap();
            match t {
                0 => seen_sni = true,
                43 => {
                    assert_eq!(d, [2, 0x03, 0x04], "supported_versions 只列 TLS 1.3");
                    seen_versions = true;
                }
                51 => {
                    seen_keyshare = true;
                    let mut k = Cursor::new(d);
                    let _ = k.u16().unwrap();
                    assert_eq!(k.u16().unwrap(), GROUP_X25519);
                    assert_eq!(k.block_u16().unwrap(), [0xbb; 32]);
                }
                _ => {}
            }
        }
        assert!(seen_versions && seen_sni && seen_keyshare);
    }

    #[test]
    fn sni_is_omitted_for_ip_literals() {
        // SNI 里放 IP 是不合规的，某些服务器会因此拒绝握手。
        let ch = client_hello(&[0; 32], &[0; 32], "127.0.0.1");
        let mut c = Cursor::new(&ch);
        c.u16().unwrap();
        c.take(32).unwrap();
        c.block_u8().unwrap();
        c.block_u16().unwrap();
        c.block_u8().unwrap();
        let exts = c.block_u16().unwrap();
        let mut e = Cursor::new(exts);
        while !e.is_empty() {
            let t = e.u16().unwrap();
            e.block_u16().unwrap();
            assert_ne!(t, 0, "IP 字面量不该出现在 SNI 里");
        }
        assert!(is_ip_literal("127.0.0.1") && is_ip_literal("::1"));
        assert!(!is_ip_literal("registry-1.docker.io"));
    }

    #[test]
    fn rejects_server_hello_without_supported_versions() {
        // 缺 supported_versions = 对端在说 TLS 1.2。必须**明确报错**，
        // 而不是悄悄降级——不实现回退就不可能被降级攻击。
        let mut w = Writer::new();
        w.u16(0x0303);
        w.bytes(&[0u8; 32]);
        w.block_u8(|w| w.bytes(&[]));
        w.u16(TLS_AES_128_GCM_SHA256);
        w.u8(0);
        w.block_u16(|_| {}); // 没有扩展
        let e = parse_server_hello(&w.0).unwrap_err();
        assert!(e.contains("supported_versions"), "{e}");
    }

    #[test]
    fn rejects_hello_retry_request() {
        let mut w = Writer::new();
        w.u16(0x0303);
        w.bytes(&[
            0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65,
            0xb8, 0x91, 0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2,
            0xc8, 0xa8, 0x33, 0x9c,
        ]);
        w.block_u8(|w| w.bytes(&[]));
        w.u16(TLS_AES_128_GCM_SHA256);
        w.u8(0);
        w.block_u16(|_| {});
        let e = parse_server_hello(&w.0).unwrap_err();
        assert!(e.contains("HelloRetryRequest"), "{e}");
    }

    #[test]
    fn parses_a_well_formed_server_hello() {
        let mut w = Writer::new();
        w.u16(0x0303);
        w.bytes(&[7u8; 32]);
        w.block_u8(|w| w.bytes(&[9u8; 32]));
        w.u16(TLS_AES_256_GCM_SHA384);
        w.u8(0);
        w.block_u16(|w| {
            w.u16(43);
            w.block_u16(|w| w.u16(0x0304));
            w.u16(51);
            w.block_u16(|w| {
                w.u16(GROUP_X25519);
                w.block_u16(|w| w.bytes(&[0x5au8; 32]));
            });
        });
        let (suite, pk) = parse_server_hello(&w.0).unwrap();
        assert_eq!(suite, TLS_AES_256_GCM_SHA384);
        assert_eq!(pk, [0x5a; 32]);
    }

    #[test]
    fn handshake_reader_reassembles_across_records() {
        // 握手消息会跨记录分片，一条记录里也可能挤好几条消息。
        // 两种情况真实服务器都会产生。
        let m1 = handshake_message(HS_ENCRYPTED_EXTENSIONS, &[1, 2, 3]);
        let m2 = handshake_message(HS_FINISHED, &[4; 32]);
        let mut all = m1.clone();
        all.extend_from_slice(&m2);

        // 分片喂入
        let mut r = HandshakeReader::new();
        assert!(r.take().unwrap().is_none());
        for chunk in all.chunks(3) {
            r.feed(chunk);
        }
        assert_eq!(r.take().unwrap().unwrap(), m1);
        assert_eq!(r.take().unwrap().unwrap(), m2);
        assert!(r.take().unwrap().is_none());

        // 一次全喂
        let mut r = HandshakeReader::new();
        r.feed(&all);
        assert_eq!(r.take().unwrap().unwrap(), m1);
        assert_eq!(r.take().unwrap().unwrap(), m2);
    }

    #[test]
    fn handshake_reader_rejects_absurd_lengths() {
        let mut r = HandshakeReader::new();
        r.feed(&[HS_CERTIFICATE, 0xff, 0xff, 0xff]);
        assert!(r.take().is_err());
    }

    #[test]
    fn certificate_message_parsing() {
        let mut w = Writer::new();
        w.block_u8(|_| {}); // 空上下文
        w.block_u24(|w| {
            for cert in [&b"cert-a"[..], &b"cert-bb"[..]] {
                w.block_u24(|w| w.bytes(cert));
                w.block_u16(|_| {}); // 每张证书后的扩展
            }
        });
        let chain = parse_certificate(&w.0).unwrap();
        assert_eq!(chain, vec![b"cert-a".to_vec(), b"cert-bb".to_vec()]);

        // 非空上下文要拒绝
        let mut w = Writer::new();
        w.block_u8(|w| w.u8(1));
        w.block_u24(|_| {});
        assert!(parse_certificate(&w.0).is_err());
    }

    #[test]
    fn certificate_verify_signed_data_has_the_rfc_prefix() {
        // 前缀写错的话，握手会在这一步失败，而错误信息完全指不到这里。
        // 所以单独钉住这段字节的形状。
        let th = [0x11u8; 32];
        let mut want = Vec::new();
        want.extend_from_slice(&[0x20u8; 64]);
        want.extend_from_slice(b"TLS 1.3, server CertificateVerify");
        want.push(0);
        want.extend_from_slice(&th);
        assert_eq!(want.len(), 64 + 33 + 1 + 32);
        assert_eq!(want[63], 0x20);
        assert_eq!(want[64], b'T');
        assert_eq!(want[64 + 33], 0);
    }

    #[test]
    fn alert_messages_are_readable() {
        assert!(alert_text(&[2, 48]).contains("unknown_ca"));
        assert!(alert_text(&[2, 51]).contains("decrypt_error"));
        assert!(alert_text(&[1]).contains("格式不明"));
    }

    #[test]
    fn writer_length_prefixes_are_correct() {
        let mut w = Writer::new();
        w.block_u8(|w| w.bytes(&[1, 2, 3]));
        w.block_u16(|w| w.bytes(&[4; 300]));
        w.block_u24(|w| w.bytes(&[5; 70000]));
        assert_eq!(w.0[0], 3);
        assert_eq!(&w.0[4..6], &[0x01, 0x2c]); // 300
        let at = 6 + 300;
        assert_eq!(&w.0[at..at + 3], &[0x01, 0x11, 0x70]); // 70000
    }
}
