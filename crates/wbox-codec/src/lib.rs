//! `wbox-codec` —— wbox 自己实现的格式与摘要原语。
//!
//! # 这个 crate 为什么存在
//!
//! `PRD.md` §2.2.1 的 Rust-only 硬约束原本只盯"不许有 C/C++"，
//! 本轮收紧为**承载产品能力的实现必须是第一方 Rust**：
//! 换掉 `vendor/blink` 只是把最大的一块搬走，`serde_json` / `sha2` /
//! `base64` / `flate2` / `tar` 这五个第三方 crate 仍然承载着镜像管理的核心
//! 语义（digest 校验、层解包、manifest 解析）。本 crate 把它们全部替换掉。
//!
//! # 模块
//!
//! | 模块 | 取代 | 用在哪 |
//! |---|---|---|
//! | [`json`] | `serde_json` | manifest / config / 运行状态记录 |
//! | [`sha256`] | `sha2` | blob digest、构建缓存键、TLS 的 HKDF |
//! | [`base64`] | `base64` | registry 的 Basic 认证、PEM 解码 |
//! | [`deflate`] | `flate2` + `miniz_oxide` | 层的 gzip 压缩/解压 |
//! | [`tar`] | `tar` | 层与归档的打包/解包 |
//!
//! # 共同的取舍
//!
//! **解码方向面对的是外部输入**（registry 给的 manifest 与层），所以要完整、
//! 要对畸形输入报错、要有资源上界；**编码方向面对的是自己的输出**，只要
//! 合法且对端能读，简单可靠优先。每个模块的注释里都写清了它落在哪一边。
//!
//! 零第三方依赖是这个 crate 的硬性约束，`Cargo.toml` 的 `[dependencies]`
//! 必须保持为空。

pub mod base64;
pub mod deflate;
pub mod json;
pub mod path;
pub mod sha256;
pub mod tar;

pub use json::Value;
pub use sha256::{sha256, sha256_hex, Sha256};

#[cfg(test)]
mod ratchet {
    /// 依赖棘轮：本 crate 一旦有了依赖，"第一方实现"这句话就不成立了。
    ///
    /// 直接读 `Cargo.toml`，因为这是唯一能在编译期之外看见依赖表的地方。
    #[test]
    fn crate_has_no_dependencies() {
        let toml = include_str!("../Cargo.toml");
        let mut in_deps = false;
        for line in toml.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_deps = t.starts_with("[dependencies") || t.contains("dependencies]");
                continue;
            }
            if in_deps && !t.is_empty() && !t.starts_with('#') {
                panic!("wbox-codec 必须零依赖，但看到：{t}");
            }
        }
    }
}
