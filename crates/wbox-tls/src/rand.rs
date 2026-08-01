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
    if let Err(e) = try_fill(buf) {
        panic!("无法从宿主获取随机数，拒绝用弱随机继续：{e}");
    }
}

/// 生成 N 字节随机数组。
pub fn bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    fill(&mut b);
    b
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn try_fill(buf: &mut [u8]) -> Result<(), String> {
    // getrandom(2)。分段读是因为内核对单次请求有上限（Linux 是 32 MiB，
    // 但被信号打断时也可能返回短读）。
    let mut off = 0usize;
    while off < buf.len() {
        let n = unsafe {
            libc::getrandom(
                buf[off..].as_mut_ptr() as *mut libc::c_void,
                buf.len() - off,
                0,
            )
        };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err.to_string());
        }
        off += n as usize;
    }
    Ok(())
}

#[cfg(target_vendor = "apple")]
fn try_fill(buf: &mut [u8]) -> Result<(), String> {
    // arc4random_buf is the Apple/BSD system CSPRNG and has no failure return.
    // It is safe for arbitrary lengths, including an empty slice.
    unsafe {
        libc::arc4random_buf(buf.as_mut_ptr().cast(), buf.len());
    }
    Ok(())
}

#[cfg(windows)]
fn try_fill(buf: &mut [u8]) -> Result<(), String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(format!("BCryptGenRandom 失败：NTSTATUS 0x{status:08x}"));
    }
    Ok(())
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
