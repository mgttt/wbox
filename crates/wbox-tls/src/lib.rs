//! `wbox-tls` —— wbox 自实现的 TLS 1.3 客户端。
//!
//! # 为什么自己写
//!
//! `PRD.md` §2.2.1 第二档：承载产品能力的实现必须是第一方 Rust。TLS 是
//! `wbox pull/push` 那条链路上最后一处第三方实现（原先是 `rustls` +
//! `rustls-rustcrypto`）。
//!
//! # 范围：只做一条路
//!
//! **只支持 TLS 1.3、X25519、AES-GCM。** 不做 TLS 1.2 回退、不做会话恢复
//! （PSK / 0-RTT）、不做客户端证书、不做吊销检查。
//!
//! 这不是偷懒，是**减少攻击面**：TLS 的历史漏洞里有很大一部分出在版本回退
//! 与旧套件上（FREAK、Logjam、POODLE 都是）。registry 全都支持 1.3，
//! 没有回退的必要；不实现就不可能被降级到它。对端不支持 1.3 时明确报错。
//!
//! # 安全上的坦白
//!
//! **这是未经第三方安全审计的密码学实现，且不是常量时间的。**
//! 每个模块的注释里都写清了各自的取舍。wbox 的威胁模型是"从 registry
//! 拉镜像"：攻击者要利用侧信道，得在同一台机器上与 wbox 争抢缓存，
//! 而那种情形下他已经能直接读 wbox 的内存了。
//!
//! 影响面仅限 `wbox pull/push` 的 registry HTTPS，**不涉及容器隔离本身**。
//! 完整论证见 `docs/rust-rewrite.md` §5.1。
//!
//! # 模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`sha512`] | SHA-384/512（SHA-256 复用 `wbox-codec`）|
//! | [`hash`] | 哈希算法的运行期分派 + HMAC |
//! | [`aes`] | AES-128/256 与 GCM 认证加密 |
//! | [`x25519`] | 密钥协商 |
//! | [`bigint`] / [`rsa`] | 大整数与 RSA 验签（PKCS#1 v1.5 + PSS）|
//! | [`ec`] | ECDSA P-256 / P-384 验签 |
//! | [`der`] / [`x509`] / [`roots`] | 证书解析、链校验、内置根 |
//! | [`kdf`] | HKDF 与 TLS 1.3 密钥调度 |
//! | [`record`] / [`handshake`] / [`stream`] | 记录层、握手、对外的字节管道 |
//! | [`rand`] | 宿主 CSPRNG |

pub mod aes;
pub mod bigint;
pub mod der;
pub mod ec;
pub mod handshake;
pub mod hash;
pub mod kdf;
pub mod rand;
pub mod record;
pub mod roots;
pub mod rsa;
pub mod sha512;
pub mod stream;
pub mod x25519;
pub mod x509;

pub use stream::TlsStream;

#[cfg(test)]
mod ratchet {
    /// 依赖棘轮：本 crate 只允许依赖同仓的 `wbox-codec` 与平台 ABI 声明
    /// （`libc` / `windows-sys`）。多出任何第三方 crate，"第一方实现"
    /// 这句话就不成立了。
    #[test]
    fn only_first_party_and_platform_abi_dependencies() {
        const ALLOWED: &[&str] = &[
            "wbox-codec",
            "libc",
            "windows-sys",
            "workspace",
            "version",
            "features",
        ];
        let toml = include_str!("../Cargo.toml");
        let mut in_deps = false;
        for line in toml.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_deps = t.contains("dependencies");
                continue;
            }
            if !in_deps
                || t.is_empty()
                || t.starts_with('#')
                || t.starts_with(']')
                || t.starts_with('"')
            {
                continue;
            }
            let name = t.split(['=', ' ']).next().unwrap_or("");
            assert!(
                ALLOWED.contains(&name),
                "wbox-tls 不得引入第三方依赖，但看到：{name}"
            );
        }
    }
}
