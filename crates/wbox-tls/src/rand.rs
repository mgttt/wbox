//! 宿主 CSPRNG。
//!
//! TLS 要三处随机：ClientHello 的 32 字节 random、X25519 的临时私钥、
//! 以及（本实现用不到的）会话票据。**这三处的质量直接决定连接安全**，
//! 所以只用操作系统的 CSPRNG，不自己搓伪随机数。
//!
//! 这是 §2.2.1 明确允许的那一类：调用宿主 OS 提供的接口是平台 ABI，
//! 不是引入第三方实现。
//!
//! **拿不到随机数就 panic，绝不降级。** 用一个可预测的"随机"私钥握手，
//! 比握手失败糟糕得多——前者看起来一切正常。

/// 填满 `buf`。失败即 panic（见模块注释）。
pub fn fill(buf: &mut [u8]) {
    if let Err(e) = agenterm_platform::entropy::fill_secure_random(buf) {
        panic!("无法从宿主获取随机数，拒绝用弱随机继续：{e}");
    }
}

/// 生成 N 字节随机数组。
pub fn bytes<const N: usize>() -> [u8; N] {
    agenterm_platform::entropy::secure_random_array()
        .unwrap_or_else(|error| panic!("无法从宿主获取随机数，拒绝用弱随机继续：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_different_values() {
        // 不是随机性检验（那需要统计工具），只是钉住"确实调到了系统 RNG"
        // ——返回常量或全零是最典型的接错方式。
        let a: [u8; 32] = bytes();
        let b: [u8; 32] = bytes();
        assert_ne!(a, b, "两次取值不该相同");
        assert!(a.iter().any(|&x| x != 0), "不该是全零");
    }

    #[test]
    fn fills_odd_lengths() {
        for n in [1usize, 7, 63, 4096] {
            let mut v = vec![0u8; n];
            fill(&mut v);
            assert!(v.iter().any(|&x| x != 0), "n={n} 全零很可疑");
        }
        // 空缓冲不该 panic
        fill(&mut []);
    }
}
