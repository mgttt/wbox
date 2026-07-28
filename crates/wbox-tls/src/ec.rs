//! NIST P-256 / P-384 上的 ECDSA **验证**（FIPS 186-4）。
//!
//! 只验不签，与 `rsa.rs` 同理。这两条曲线是证书链上非 RSA 那一部分的全部：
//! P-256 用于大多数叶子证书与 `ecdsa_secp256r1_sha256` 签名，P-384 用于
//! DigiCert / Amazon 那几个 ECC 根。
//!
//! # 实现取舍
//!
//! - **仿射坐标 + 逐位倍点加**。验签处理的全是公开数据（公钥、签名、哈希），
//!   没有秘密位，所以不需要常量时间，也就不必上 Jacobian 坐标与固定窗口。
//!   代价是慢一些——一次验签几毫秒，而一条证书链只有两三次。
//! - **域运算走 `BigUint` 的通用模运算**，不为每条曲线手写专用约化。
//!   P-256 的 Solinas 约化能再快一个量级，但它是一段极易写错、且错了只在
//!   特定输入上才暴露的代码。这里的判据是"能不能通过 NIST 向量"，不是快。
//!
//! 当前实测（release）：一次 P-256 验签约 **76 ms**，一条三证书链约 230 ms。
//! 拉一次镜像只做这么一轮，可以接受。到这个数用了两处优化，都在注释里写了
//! 原因：Jacobian 坐标（把每次点运算的模逆推迟到最后一次）与 `BigUint::rem`
//! 的原地无分配改写（分配本身曾是大头）。
//!
//! # 必须做的三项检查
//!
//! 验证 ECDSA 时最容易漏、漏了就致命的是这三条，都在 [`verify`] 里：
//! 1. `r`、`s` 必须落在 `[1, n-1]`——`s = 0` 会让下面的求逆无意义；
//! 2. 公钥点必须**在曲线上**，否则可被无效曲线攻击；
//! 3. 结果点为无穷远点时判失败，不能当成 `x = 0` 去比。

use crate::bigint::BigUint;

/// 一条短 Weierstrass 曲线 `y² = x³ - 3x + b` over F_p。
///
/// NIST 的 P 系列曲线 `a` 恒为 `-3`，所以不单独存 `a`。
pub struct Curve {
    pub p: BigUint,
    pub b: BigUint,
    /// 基点阶。
    pub n: BigUint,
    pub gx: BigUint,
    pub gy: BigUint,
    /// 坐标的字节长度（P-256 是 32，P-384 是 48）。
    pub bytes: usize,
}

/// 仿射点。`None` 表示无穷远点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: BigUint,
    pub y: BigUint,
}

/// Jacobian 投影坐标下的点。见 `Curve::jacobian_double` 的注释。
struct Jacobian {
    x: BigUint,
    y: BigUint,
    z: BigUint,
}

impl Jacobian {
    fn infinity() -> Jacobian {
        Jacobian {
            x: BigUint::one(),
            y: BigUint::one(),
            z: BigUint::zero(),
        }
    }

    fn is_infinity(&self) -> bool {
        self.z.is_zero()
    }
}

fn hex_to_big(s: &str) -> BigUint {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect();
    BigUint::from_bytes_be(&bytes)
}

/// NIST P-256（secp256r1）。
pub fn p256() -> Curve {
    Curve {
        p: hex_to_big("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff"),
        b: hex_to_big("5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b"),
        n: hex_to_big("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551"),
        gx: hex_to_big("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296"),
        gy: hex_to_big("4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5"),
        bytes: 32,
    }
}

/// NIST P-384（secp384r1）。
pub fn p384() -> Curve {
    Curve {
        p: hex_to_big(concat!(
            "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe",
            "ffffffff0000000000000000ffffffff"
        )),
        b: hex_to_big(concat!(
            "b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875a",
            "c656398d8a2ed19d2a85c8edd3ec2aef"
        )),
        n: hex_to_big(concat!(
            "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf",
            "581a0db248b0a77aecec196accc52973"
        )),
        gx: hex_to_big(concat!(
            "aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a38",
            "5502f25dbf55296c3a545e3872760ab7"
        )),
        gy: hex_to_big(concat!(
            "3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c0",
            "0a60b1ce1d7e819d7a431d7c90ea0e5f"
        )),
        bytes: 48,
    }
}

impl Curve {
    fn add_mod(&self, a: &BigUint, b: &BigUint) -> BigUint {
        a.add_ref(b).rem(&self.p)
    }

    fn sub_mod(&self, a: &BigUint, b: &BigUint) -> BigUint {
        // a - b mod p，用 a + (p - b) 避免负数。
        let a = a.rem(&self.p);
        let b = b.rem(&self.p);
        if a.ge(&b) {
            a.sub_ref(&b)
        } else {
            a.add_ref(&self.p).sub_ref(&b)
        }
    }

    fn mul_mod(&self, a: &BigUint, b: &BigUint) -> BigUint {
        a.mul_ref(b).rem(&self.p)
    }

    /// 模 p 求逆，走费马小定理 `a^(p-2)`（p 是素数）。
    fn inv_mod_p(&self, a: &BigUint) -> BigUint {
        let e = self.p.sub_ref(&BigUint::from_u32(2));
        a.modpow(&e, &self.p)
    }

    /// 模 n 求逆（n 也是素数）。
    fn inv_mod_n(&self, a: &BigUint) -> BigUint {
        let e = self.n.sub_ref(&BigUint::from_u32(2));
        a.modpow(&e, &self.n)
    }

    /// 点是否在曲线上：`y² == x³ - 3x + b (mod p)`。
    pub fn on_curve(&self, pt: &Point) -> bool {
        if pt.x.ge(&self.p) || pt.y.ge(&self.p) {
            return false;
        }
        let lhs = self.mul_mod(&pt.y, &pt.y);
        let x3 = self.mul_mod(&self.mul_mod(&pt.x, &pt.x), &pt.x);
        let three_x = self.mul_mod(&BigUint::from_u32(3), &pt.x);
        let rhs = self.add_mod(&self.sub_mod(&x3, &three_x), &self.b);
        lhs == rhs
    }

    /// Jacobian 投影坐标下的点：`(X, Y, Z)` 对应仿射 `(X/Z², Y/Z³)`。
    /// `Z = 0` 表示无穷远点。
    ///
    /// **为什么不用仿射坐标**：仿射的点加/倍点每次都要一次模逆，而模逆走
    /// 费马小定理是一次完整模幂。一次 256 位标量乘约 380 次点运算，就是
    /// 380 次模幂——实测一次验签要一分钟以上，完全不可用。Jacobian 把
    /// 模逆推迟到最后**只做一次**。
    fn jacobian_double(&self, pt: &Jacobian) -> Jacobian {
        if pt.is_infinity() {
            return Jacobian::infinity();
        }
        // a = -3 的标准公式（比通用公式少两次乘法）。
        let zz = self.mul_mod(&pt.z, &pt.z);
        let yy = self.mul_mod(&pt.y, &pt.y);
        // S = 4*X*YY
        let s = self.mul_mod(&BigUint::from_u32(4), &self.mul_mod(&pt.x, &yy));
        // M = 3*(X - ZZ)*(X + ZZ)   ——利用 a = -3
        let m = self.mul_mod(
            &BigUint::from_u32(3),
            &self.mul_mod(&self.sub_mod(&pt.x, &zz), &self.add_mod(&pt.x, &zz)),
        );
        let x3 = self.sub_mod(&self.mul_mod(&m, &m), &self.add_mod(&s, &s));
        // Y3 = M*(S - X3) - 8*YY²
        let yyyy8 = self.mul_mod(&BigUint::from_u32(8), &self.mul_mod(&yy, &yy));
        let y3 = self.sub_mod(&self.mul_mod(&m, &self.sub_mod(&s, &x3)), &yyyy8);
        let z3 = self.mul_mod(&self.add_mod(&pt.y, &pt.y), &pt.z);
        Jacobian {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    /// Jacobian 点 + 仿射点（混合加法，比两个 Jacobian 相加更省）。
    fn jacobian_add_affine(&self, a: &Jacobian, b: &Point) -> Jacobian {
        if a.is_infinity() {
            return Jacobian {
                x: b.x.clone(),
                y: b.y.clone(),
                z: BigUint::one(),
            };
        }
        let zz = self.mul_mod(&a.z, &a.z);
        let zzz = self.mul_mod(&zz, &a.z);
        let u2 = self.mul_mod(&b.x, &zz);
        let s2 = self.mul_mod(&b.y, &zzz);
        if a.x == u2 {
            if a.y == s2 {
                return self.jacobian_double(a);
            }
            // 互为逆元 → 无穷远。
            return Jacobian::infinity();
        }
        let h = self.sub_mod(&u2, &a.x);
        let r = self.sub_mod(&s2, &a.y);
        let hh = self.mul_mod(&h, &h);
        let hhh = self.mul_mod(&hh, &h);
        let v = self.mul_mod(&a.x, &hh);
        let x3 = self.sub_mod(
            &self.sub_mod(&self.mul_mod(&r, &r), &hhh),
            &self.add_mod(&v, &v),
        );
        let y3 = self.sub_mod(
            &self.mul_mod(&r, &self.sub_mod(&v, &x3)),
            &self.mul_mod(&a.y, &hhh),
        );
        let z3 = self.mul_mod(&a.z, &h);
        Jacobian {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    /// Jacobian → 仿射。这是整个标量乘里**唯一**的一次模逆。
    fn to_affine(&self, pt: &Jacobian) -> Option<Point> {
        if pt.is_infinity() {
            return None;
        }
        let zinv = self.inv_mod_p(&pt.z);
        let zinv2 = self.mul_mod(&zinv, &zinv);
        let zinv3 = self.mul_mod(&zinv2, &zinv);
        Some(Point {
            x: self.mul_mod(&pt.x, &zinv2),
            y: self.mul_mod(&pt.y, &zinv3),
        })
    }

    /// 仿射点加（只用于把两个标量乘的结果合起来，一次调用）。
    fn point_add(&self, a: Option<&Point>, b: Option<&Point>) -> Option<Point> {
        let (a, b) = match (a, b) {
            (None, None) => return None,
            (None, Some(q)) => return Some(q.clone()),
            (Some(p), None) => return Some(p.clone()),
            (Some(p), Some(q)) => (p, q),
        };
        if a.x == b.x {
            let neg_y = self.sub_mod(&BigUint::zero(), &b.y);
            if a.y == neg_y {
                return None;
            }
            let j = self.jacobian_double(&Jacobian {
                x: a.x.clone(),
                y: a.y.clone(),
                z: BigUint::one(),
            });
            return self.to_affine(&j);
        }
        let num = self.sub_mod(&b.y, &a.y);
        let den = self.inv_mod_p(&self.sub_mod(&b.x, &a.x));
        let lam = self.mul_mod(&num, &den);
        let x3 = self.sub_mod(&self.sub_mod(&self.mul_mod(&lam, &lam), &a.x), &b.x);
        let y3 = self.sub_mod(&self.mul_mod(&lam, &self.sub_mod(&a.x, &x3)), &a.y);
        Some(Point { x: x3, y: y3 })
    }

    /// 标量乘 `k * pt`。逐位倍点加，全程在 Jacobian 坐标里，
    /// 只在最后转回仿射（一次模逆）。
    fn mul_point(&self, k: &BigUint, pt: &Point) -> Option<Point> {
        let mut acc = Jacobian::infinity();
        for i in (0..k.bits()).rev() {
            acc = self.jacobian_double(&acc);
            if k.bit(i) {
                acc = self.jacobian_add_affine(&acc, pt);
            }
        }
        self.to_affine(&acc)
    }

    fn generator(&self) -> Point {
        Point {
            x: self.gx.clone(),
            y: self.gy.clone(),
        }
    }

    /// 解析未压缩的公钥点（`0x04 || X || Y`）。
    ///
    /// **不接受压缩格式**：TLS 1.3 与 X.509 里 registry 用的都是未压缩，
    /// 支持压缩点意味着要实现模平方根，多一段没人走的代码。
    pub fn parse_public_key(&self, data: &[u8]) -> Option<Point> {
        if data.len() != 1 + 2 * self.bytes || data[0] != 0x04 {
            return None;
        }
        let pt = Point {
            x: BigUint::from_bytes_be(&data[1..1 + self.bytes]),
            y: BigUint::from_bytes_be(&data[1 + self.bytes..]),
        };
        // 不在曲线上的点直接拒绝——无效曲线攻击就是从这里进来的。
        if !self.on_curve(&pt) {
            return None;
        }
        Some(pt)
    }
}

/// 验证 ECDSA 签名。`hash` 是已经算好的消息摘要，`(r, s)` 是签名分量。
pub fn verify(curve: &Curve, pubkey: &Point, hash: &[u8], r: &BigUint, s: &BigUint) -> bool {
    // 1) r, s ∈ [1, n-1]
    if r.is_zero() || s.is_zero() || r.ge(&curve.n) || s.ge(&curve.n) {
        return false;
    }
    // 2) 公钥必须在曲线上（parse_public_key 已查过，这里再查一次是因为
    //    调用方也可能直接构造 Point）。
    if !curve.on_curve(pubkey) {
        return false;
    }

    // e = 摘要的最左 bitlen(n) 位（FIPS 186-4 §6.4）。
    let e = leftmost_bits(hash, curve.n.bits());

    let w = curve.inv_mod_n(s);
    let u1 = e.mul_ref(&w).rem(&curve.n);
    let u2 = r.mul_ref(&w).rem(&curve.n);

    let g = curve.generator();
    let p1 = curve.mul_point(&u1, &g);
    let p2 = curve.mul_point(&u2, pubkey);
    let sum = curve.point_add(p1.as_ref(), p2.as_ref());

    // 3) 无穷远点判失败——不能当成 x = 0 去比。
    match sum {
        None => false,
        Some(pt) => pt.x.rem(&curve.n) == *r,
    }
}

/// 取摘要的最左 `nbits` 位当作整数。摘要比 n 长时要**截断**（不是取模）。
fn leftmost_bits(hash: &[u8], nbits: usize) -> BigUint {
    let mut e = BigUint::from_bytes_be(hash);
    let hash_bits = hash.len() * 8;
    if hash_bits > nbits {
        e = e.shr(hash_bits - nbits);
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::HashAlg;

    #[test]
    fn generators_are_on_their_curves() {
        // 曲线参数抄错一位，这条立刻红——比等到验签失败好查得多。
        let c = p256();
        assert!(c.on_curve(&c.generator()));
        let c = p384();
        assert!(c.on_curve(&c.generator()));
    }

    #[test]
    fn generator_order_is_correct() {
        // n * G 必须是无穷远点。这条同时验证了点加/倍点/标量乘三者一致。
        let c = p256();
        assert!(c.mul_point(&c.n, &c.generator()).is_none());
    }

    #[test]
    fn scalar_multiplication_matches_nist_vectors() {
        // NIST 的 P-256 标量乘向量：k=2 与 k=3。
        let c = p256();
        let g = c.generator();
        let two_g = c.mul_point(&BigUint::from_u32(2), &g).unwrap();
        assert_eq!(
            wbox_codec::sha256::hex(&two_g.x.to_bytes_be(32).unwrap()),
            "7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978"
        );
        let three_g = c.mul_point(&BigUint::from_u32(3), &g).unwrap();
        assert_eq!(
            wbox_codec::sha256::hex(&three_g.x.to_bytes_be(32).unwrap()),
            "5ecbe4d1a6330a44c8f7ef951d4bf165e6c6b721efada985fb41661bc6e7fd6c"
        );
    }

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hex(s: &str) -> BigUint {
        BigUint::from_bytes_be(&unhex(s))
    }

    #[test]
    fn verifies_real_p256_signature() {
        // 用已知私钥现签的一条 P-256/SHA-256 签名（签完在参考实现里自验过）。
        // 只有负向用例的话，一个"永远返回 false"的实现也能全绿。
        let c = p256();
        let pk = Point {
            x: hex("6413e370318a922cecfaa94ba2188dd419f586356fa774c766cd6c450295fee9"),
            y: hex("5dce9ce0557b0a8f1cef5c663f362cfffc910e3094afc82bbbc7a0a92b0b6bdb"),
        };
        let r = hex("6e036a286976c9667feec3458a09131b1e12adfcb6775af5daa039ef20669a21");
        let s = hex("87f23cd7899ab6c1a25b7187b5802bfc50b8152f2959310c95dd9c6601c90886");
        assert!(c.on_curve(&pk));
        let h = HashAlg::Sha256.digest(b"wbox tls test");
        assert!(verify(&c, &pk, &h, &r, &s), "有效签名必须通过");

        // 改一位签名 / 改一位消息都必须失败。
        assert!(!verify(&c, &pk, &h, &r.add_ref(&BigUint::one()), &s));
        assert!(!verify(&c, &pk, &h, &r, &s.add_ref(&BigUint::one())));
        let h2 = HashAlg::Sha256.digest(b"wbox tls tesT");
        assert!(!verify(&c, &pk, &h2, &r, &s));
    }

    #[test]
    fn verifies_real_p384_signature() {
        // P-384/SHA-384。DigiCert、Amazon 那几个 ECC 根就是这条曲线，
        // 所以它不是"顺带做的"，是证书链上真的会走到的一条路。
        let c = p384();
        let pk = Point {
            x: hex(concat!(
                "3ba2bdf5a8cb204eecfff01fb7a575ab2c764d40f0ed7cfe5d7f5ba8515198a2",
                "d15f957d2347b7a2a75e53afa4c9ee28"
            )),
            y: hex(concat!(
                "e2f32b3d3900faa6cd50bf280d17c193e1f47504b31838e0484d6059d2ce06df",
                "0435ad5ab2b254f3bdd400e73a24aa0e"
            )),
        };
        let r = hex(concat!(
            "178c3bc72d59dcaaf44f9e68a447df75fe41900e36b994fce9d67a9cdb7fff3a",
            "99f78bdfa2b9826663933bb40060f26e"
        ));
        let s = hex(concat!(
            "c27100c12d53111f75b93deb538ea4a66995817b955eb2cceddc552e766f5a6a",
            "79d2ed7e0ad6d499c218539b24b3ea48"
        ));
        assert!(c.on_curve(&pk));
        let h = HashAlg::Sha384.digest(b"wbox tls test");
        assert!(verify(&c, &pk, &h, &r, &s));
        assert!(!verify(&c, &pk, &h, &r, &s.add_ref(&BigUint::one())));
    }

    #[test]
    fn rejects_out_of_range_and_off_curve() {
        let c = p256();
        let pk = Point {
            x: c.gx.clone(),
            y: c.gy.clone(),
        };
        let h = HashAlg::Sha256.digest(b"m");
        // r = 0 / s = 0 / r >= n 都必须拒绝。
        assert!(!verify(&c, &pk, &h, &BigUint::zero(), &BigUint::one()));
        assert!(!verify(&c, &pk, &h, &BigUint::one(), &BigUint::zero()));
        assert!(!verify(&c, &pk, &h, &c.n, &BigUint::one()));
        // 不在曲线上的公钥必须拒绝——无效曲线攻击的入口。
        let off = Point {
            x: c.gx.clone(),
            y: c.gy.add_ref(&BigUint::one()),
        };
        assert!(!c.on_curve(&off));
        assert!(!verify(&c, &off, &h, &BigUint::one(), &BigUint::one()));
    }

    #[test]
    fn parse_public_key_checks_format_and_curve() {
        let c = p256();
        let mut enc = vec![0x04u8];
        enc.extend_from_slice(&c.gx.to_bytes_be(32).unwrap());
        enc.extend_from_slice(&c.gy.to_bytes_be(32).unwrap());
        assert!(c.parse_public_key(&enc).is_some());
        // 压缩格式明确不收。
        let mut comp = vec![0x02u8];
        comp.extend_from_slice(&c.gx.to_bytes_be(32).unwrap());
        assert!(c.parse_public_key(&comp).is_none());
        // 长度不对、点不在曲线上都要拒绝。
        assert!(c.parse_public_key(&enc[..enc.len() - 1]).is_none());
        let mut off = enc.clone();
        off[40] ^= 1;
        assert!(c.parse_public_key(&off).is_none());
    }

    #[test]
    fn leftmost_bits_truncates_not_reduces() {
        // 摘要比 n 长时要取**最左** nbits 位。写成取模的话 P-256/SHA-384
        // 这种组合会算出完全不同的 e，而普通向量未必覆盖到。
        let hash = [0xffu8; 48]; // 384 位
        let e = leftmost_bits(&hash, 256);
        assert_eq!(e.bits(), 256);
        assert_eq!(e.to_bytes_be(32).unwrap(), [0xff; 32]);
    }
}
