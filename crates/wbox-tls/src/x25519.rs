//! X25519 密钥交换（RFC 7748）。
//!
//! TLS 1.3 的密钥协商。之所以只做 X25519、不做 P-256 的 ECDHE：
//! X25519 是 RFC 8446 的推荐组，所有现代 registry 都支持，而且它的实现
//! **天然抗侧信道**——Montgomery ladder 每一步的操作序列与私钥无关，
//! 不像 ECDSA 那样需要额外小心。少一条曲线就少一份出错的面。
//!
//! # 域运算表示
//!
//! 元素模 p = 2^255 - 19，用 5 个 51 位肢体（limb）表示，存在 `u64` 里。
//! 51 位是因为：乘法时两个 51 位相乘得 102 位，5 个这样的乘积累加约 105 位，
//! 仍在 `u128` 内，不会溢出。这是 curve25519 实现的标准做法。

/// 域元素：5 个 51 位肢体，小端（`l[0]` 是最低位）。
///
/// 不保证完全约化——只保证每次运算后各肢体不超过 52 位左右，最终由
/// [`Fe::to_bytes`] 做一次完整约化。
#[derive(Clone, Copy, Debug)]
struct Fe([u64; 5]);

const MASK51: u64 = (1u64 << 51) - 1;

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    /// 从 32 字节小端读入。最高位按 RFC 7748 忽略。
    fn from_bytes(b: &[u8; 32]) -> Fe {
        let ld = |i: usize| -> u64 {
            let mut v = [0u8; 8];
            v.copy_from_slice(&b[i..i + 8]);
            u64::from_le_bytes(v)
        };
        // 逐个抠出 51 位窗口。
        let mut l = [0u64; 5];
        l[0] = ld(0) & MASK51;
        l[1] = (ld(6) >> 3) & MASK51;
        l[2] = (ld(12) >> 6) & MASK51;
        l[3] = (ld(19) >> 1) & MASK51;
        // 最后一肢只取 51 位，第 255 位按规范丢弃。
        l[4] = (ld(24) >> 12) & MASK51;
        Fe(l)
    }

    /// 完全约化后输出 32 字节小端。
    fn to_bytes(self) -> [u8; 32] {
        let mut l = self.reduce().0;
        // reduce 之后 l < 2p，可能仍落在 [p, 2p)。判断"是否 >= p"的办法是
        // 试加 19 看有没有进到第 255 位——p = 2^255 - 19，所以 l >= p
        // 等价于 l + 19 >= 2^255。
        let mut q = (l[0] + 19) >> 51;
        q = (l[1] + q) >> 51;
        q = (l[2] + q) >> 51;
        q = (l[3] + q) >> 51;
        q = (l[4] + q) >> 51;
        // q ∈ {0,1}。为 1 时减去 p，等价于加 19 之后丢掉第 255 位。
        l[0] += 19 * q;
        let mut carry = 0u64;
        for x in l.iter_mut() {
            carry += *x;
            *x = carry & MASK51;
            carry >>= 51;
        }
        // 丢掉溢出的第 255 位（此时它就是被减掉的那个 2^255）。
        l[4] &= MASK51;

        let mut out = [0u8; 32];
        let mut acc = 0u128;
        let mut bits = 0u32;
        let mut oi = 0usize;
        for &x in l.iter() {
            acc |= (x as u128) << bits;
            bits += 51;
            while bits >= 8 && oi < 32 {
                out[oi] = (acc & 0xff) as u8;
                acc >>= 8;
                bits -= 8;
                oi += 1;
            }
        }
        while oi < 32 {
            out[oi] = (acc & 0xff) as u8;
            acc >>= 8;
            oi += 1;
        }
        out[31] &= 0x7f;
        out
    }

    /// 把各肢体归到 51 位以内（结果 < 2p）。
    fn reduce(self) -> Fe {
        let mut l = self.0;
        let mut carry = 0u64;
        for x in l.iter_mut() {
            carry += *x;
            *x = carry & MASK51;
            carry >>= 51;
        }
        // 溢出的部分乘 19 折回最低肢（因为 2^255 ≡ 19 mod p）。
        l[0] += carry * 19;
        // 再传播一次即可（l[0] 最多多出 19*小量）。
        carry = 0;
        for x in l.iter_mut() {
            carry += *x;
            *x = carry & MASK51;
            carry >>= 51;
        }
        l[0] += carry * 19;
        Fe(l)
    }

    fn add(self, o: Fe) -> Fe {
        let mut l = [0u64; 5];
        for (i, x) in l.iter_mut().enumerate() {
            *x = self.0[i] + o.0[i];
        }
        Fe(l).reduce()
    }

    fn sub(self, o: Fe) -> Fe {
        // 先加 2p 保证不下溢。2p 的肢体是 (2^52-38, 2^52-2, 2^52-2, 2^52-2, 2^52-2)
        // ——注意是 52 位不是 51 位，写成 51 位的常数会让减法悄悄下溢。
        const TWO_P0: u64 = 0xF_FFFF_FFFF_FFDA; // 2*(2^51-19)
        const TWO_PN: u64 = 0xF_FFFF_FFFF_FFFE; // 2*(2^51-1)
        let mut l = [0u64; 5];
        l[0] = self.0[0] + TWO_P0 - o.0[0];
        for (i, x) in l.iter_mut().enumerate().skip(1) {
            *x = self.0[i] + TWO_PN - o.0[i];
        }
        Fe(l).reduce()
    }

    fn mul(self, o: Fe) -> Fe {
        let a = self.0;
        let b = o.0;
        // 2^255 ≡ 19 (mod p)，所以 a[i]*b[j] 里 i+j >= 5 的项折回到下标
        // i+j-5，并带上系数 19。
        let mut t = [0u128; 5];
        for (i, &ai) in a.iter().enumerate() {
            for (j, &bj) in b.iter().enumerate() {
                let prod = ai as u128 * bj as u128;
                let k = i + j;
                if k < 5 {
                    t[k] += prod;
                } else {
                    t[k - 5] += prod * 19;
                }
            }
        }
        // 传播进位。
        let mut l = [0u64; 5];
        let mut carry: u128 = 0;
        for (i, x) in l.iter_mut().enumerate() {
            carry += t[i];
            *x = (carry as u64) & MASK51;
            carry >>= 51;
        }
        l[0] += (carry as u64) * 19;
        Fe(l).reduce()
    }

    fn sq(self) -> Fe {
        self.mul(self)
    }

    /// 乘一个小常数（Montgomery ladder 里的 a24）。
    fn mul_small(self, k: u64) -> Fe {
        let mut carry: u128 = 0;
        let mut l = [0u64; 5];
        for (i, x) in l.iter_mut().enumerate() {
            carry += self.0[i] as u128 * k as u128;
            *x = (carry as u64) & MASK51;
            carry >>= 51;
        }
        l[0] += (carry as u64) * 19;
        Fe(l).reduce()
    }

    /// 求逆：a^(p-2)，用 RFC 7748 给的加法链。
    fn invert(self) -> Fe {
        let z1 = self;
        let z2 = z1.sq();
        let z8 = z2.sq().sq();
        let z9 = z1.mul(z8);
        let z11 = z2.mul(z9);
        let z22 = z11.sq();
        let z_5_0 = z9.mul(z22);

        let mut t = z_5_0;
        for _ in 0..5 {
            t = t.sq();
        }
        let z_10_0 = t.mul(z_5_0);

        t = z_10_0;
        for _ in 0..10 {
            t = t.sq();
        }
        let z_20_0 = t.mul(z_10_0);

        t = z_20_0;
        for _ in 0..20 {
            t = t.sq();
        }
        let z_40_0 = t.mul(z_20_0);

        t = z_40_0;
        for _ in 0..10 {
            t = t.sq();
        }
        let z_50_0 = t.mul(z_10_0);

        t = z_50_0;
        for _ in 0..50 {
            t = t.sq();
        }
        let z_100_0 = t.mul(z_50_0);

        t = z_100_0;
        for _ in 0..100 {
            t = t.sq();
        }
        let z_200_0 = t.mul(z_100_0);

        t = z_200_0;
        for _ in 0..50 {
            t = t.sq();
        }
        let z_250_0 = t.mul(z_50_0);

        t = z_250_0;
        for _ in 0..5 {
            t = t.sq();
        }
        t.mul(z11)
    }

    /// 条件交换。**用掩码而不是分支**：分支会让执行路径依赖私钥位。
    fn cswap(swap: u64, a: &mut Fe, b: &mut Fe) {
        let mask = 0u64.wrapping_sub(swap);
        for i in 0..5 {
            let t = mask & (a.0[i] ^ b.0[i]);
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }
}

/// X25519 标量乘：`scalar * point`。
///
/// `point` 是 u 坐标（32 字节小端）。返回共享密钥的 u 坐标。
pub fn x25519(scalar: &[u8; 32], point: &[u8; 32]) -> [u8; 32] {
    // RFC 7748 §5：私钥要做 clamp——清低 3 位（避免小子群）、
    // 清最高位、置次高位（固定标量长度，防时序泄露）。
    let mut k = *scalar;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;

    let x1 = Fe::from_bytes(point);
    let mut x2 = Fe::ONE;
    let mut z2 = Fe::ZERO;
    let mut x3 = x1;
    let mut z3 = Fe::ONE;
    let mut swap = 0u64;

    // Montgomery ladder：从最高位往下，每位的操作序列完全一致。
    for i in (0..255).rev() {
        let bit = ((k[i / 8] >> (i % 8)) & 1) as u64;
        swap ^= bit;
        Fe::cswap(swap, &mut x2, &mut x3);
        Fe::cswap(swap, &mut z2, &mut z3);
        swap = bit;

        let a = x2.add(z2);
        let aa = a.sq();
        let b = x2.sub(z2);
        let bb = b.sq();
        let e = aa.sub(bb);
        let c = x3.add(z3);
        let d = x3.sub(z3);
        let da = d.mul(a);
        let cb = c.mul(b);
        x3 = da.add(cb).sq();
        z3 = x1.mul(da.sub(cb).sq());
        x2 = aa.mul(bb);
        // a24 = (486662 - 2) / 4 = 121665
        z2 = e.mul(aa.add(e.mul_small(121665)));
    }
    Fe::cswap(swap, &mut x2, &mut x3);
    Fe::cswap(swap, &mut z2, &mut z3);

    x2.mul(z2.invert()).to_bytes()
}

/// X25519 基点（u = 9）。
pub const BASE: [u8; 32] = [
    9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// 由私钥算公钥。
pub fn public_key(secret: &[u8; 32]) -> [u8; 32] {
    x25519(secret, &BASE)
}

/// 共享密钥。全零结果表示对端给了小子群点，**必须当成握手失败**
/// （RFC 8446 §7.4.2 明确要求检查）。
pub fn shared_secret(secret: &[u8; 32], peer: &[u8; 32]) -> Option<[u8; 32]> {
    let s = x25519(secret, peer);
    if s.iter().all(|&b| b == 0) {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wbox_codec::sha256::hex;

    fn unhex32(s: &str) -> [u8; 32] {
        let v: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        let mut o = [0u8; 32];
        o.copy_from_slice(&v);
        o
    }

    #[test]
    fn rfc7748_scalar_mult_vectors() {
        // RFC 7748 §5.2 的两个标准向量。
        let k = unhex32("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
        let u = unhex32("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
        assert_eq!(
            hex(&x25519(&k, &u)),
            "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552"
        );

        let k = unhex32("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d");
        let u = unhex32("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493");
        assert_eq!(
            hex(&x25519(&k, &u)),
            "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957"
        );
    }

    #[test]
    fn rfc7748_diffie_hellman() {
        // RFC 7748 §6.1 的完整 DH 交换。
        let alice_sk = unhex32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let bob_sk = unhex32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");

        assert_eq!(
            hex(&public_key(&alice_sk)),
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
        );
        assert_eq!(
            hex(&public_key(&bob_sk)),
            "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"
        );

        let alice_pk = public_key(&alice_sk);
        let bob_pk = public_key(&bob_sk);
        let s1 = shared_secret(&alice_sk, &bob_pk).unwrap();
        let s2 = shared_secret(&bob_sk, &alice_pk).unwrap();
        assert_eq!(s1, s2, "双方必须算出同一个共享密钥");
        assert_eq!(
            hex(&s1),
            "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742"
        );
    }

    #[test]
    fn rejects_small_order_points() {
        // 小子群点会让共享密钥变成全零——RFC 8446 §7.4.2 要求当失败处理。
        // 全零 u 是最典型的一个。
        let sk = [1u8; 32];
        assert!(shared_secret(&sk, &[0u8; 32]).is_none());
    }

    #[test]
    fn iterated_vector_matches_rfc7748() {
        // RFC 7748 §5.2 的迭代向量（1 次与 1000 次）。这条能抓出普通向量
        // 抓不到的进位/约化错误——错一位就会在几百轮内被放大。
        let mut k = unhex32("0900000000000000000000000000000000000000000000000000000000000000");
        let mut u = k;
        for i in 1..=1000 {
            let r = x25519(&k, &u);
            u = k;
            k = r;
            if i == 1 {
                assert_eq!(
                    hex(&k),
                    "422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079"
                );
            }
        }
        assert_eq!(
            hex(&k),
            "684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51"
        );
    }

    #[test]
    fn field_arithmetic_round_trips() {
        // 往返只对**规范表示**（< p）成立。0x7fff..ff = 2^255-1 已经大于
        // p = 2^255-19，它会被约化成 18——那是对的，不是缺陷，所以不能
        // 拿它来断言往返。
        for seed in [0u8, 1, 0x7e] {
            let mut b = [seed; 32];
            b[31] &= 0x7f;
            let f = Fe::from_bytes(&b);
            assert_eq!(f.to_bytes(), b, "seed={seed}");
        }
        // 反过来钉住上面那条：超出 p 的输入必须被折回。
        let mut over = [0xffu8; 32];
        over[31] = 0x7f; // 2^255 - 1
        assert_eq!(Fe::from_bytes(&over).to_bytes()[0], 18);
        // a * a^-1 == 1
        let a = Fe::from_bytes(&[3u8; 32]);
        assert_eq!(a.mul(a.invert()).to_bytes(), Fe::ONE.to_bytes());
        // a + b - b == a
        let b = Fe::from_bytes(&[5u8; 32]);
        assert_eq!(a.add(b).sub(b).to_bytes(), a.to_bytes());
    }
}
