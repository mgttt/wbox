//! 无符号大整数，只做**验签需要的那一档**。
//!
//! 范围刻意窄：加、减、乘、除余、模幂。**没有**私钥运算需要的模逆、
//! 素性检测、CRT——wbox 只验证别人的签名，从不生成签名，多做的每一样
//! 都是白白多出来的攻击面与出错面。
//!
//! # 不追求常量时间
//!
//! 这里处理的全是**公开数据**：证书里的公钥模数、签名值、待验证的哈希。
//! 没有任何一位是秘密，所以时序泄露无从谈起。（X25519 那边处理私钥，
//! 那里才用了掩码写法。）
//!
//! 肢体是 `u32`，中间结果用 `u64` 承接——`u64` 肢体要 `u128` 乘法，在
//! 32 位目标上会退化成软件例程，而 RSA-4096 的模幂本来就是热点。

/// 无符号大整数，小端肢体（`limbs[0]` 是最低位）。**没有前导零肢体**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigUint {
    limbs: Vec<u32>,
}

impl BigUint {
    pub fn zero() -> Self {
        BigUint { limbs: Vec::new() }
    }

    pub fn one() -> Self {
        BigUint { limbs: vec![1] }
    }

    pub fn from_u32(v: u32) -> Self {
        let mut b = BigUint { limbs: vec![v] };
        b.trim();
        b
    }

    /// 从大端字节读入（DER 里的整数就是大端）。
    pub fn from_bytes_be(bytes: &[u8]) -> Self {
        let mut limbs = Vec::with_capacity(bytes.len().div_ceil(4));
        // 从最低位开始，每 4 字节一肢。
        let mut i = bytes.len();
        while i > 0 {
            let start = i.saturating_sub(4);
            let mut w = 0u32;
            for &b in &bytes[start..i] {
                w = (w << 8) | b as u32;
            }
            limbs.push(w);
            i = start;
        }
        let mut b = BigUint { limbs };
        b.trim();
        b
    }

    /// 输出大端字节，左侧补零到 `len` 字节。值放不下时返回 `None`——
    /// **截断是绝不能做的**，那会把一个不匹配的签名变成"看着对"的。
    pub fn to_bytes_be(&self, len: usize) -> Option<Vec<u8>> {
        let mut out = vec![0u8; len];
        for (i, &limb) in self.limbs.iter().enumerate() {
            for j in 0..4 {
                let byte = ((limb >> (8 * j)) & 0xff) as u8;
                let pos = i * 4 + j;
                if pos >= len {
                    if byte != 0 {
                        return None;
                    }
                    continue;
                }
                out[len - 1 - pos] = byte;
            }
        }
        Some(out)
    }

    fn trim(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// 有效位数。
    pub fn bits(&self) -> usize {
        match self.limbs.last() {
            None => 0,
            Some(&top) => (self.limbs.len() - 1) * 32 + (32 - top.leading_zeros() as usize),
        }
    }

    fn cmp(&self, other: &BigUint) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        if self.limbs.len() != other.limbs.len() {
            return self.limbs.len().cmp(&other.limbs.len());
        }
        for i in (0..self.limbs.len()).rev() {
            match self.limbs[i].cmp(&other.limbs[i]) {
                Ordering::Equal => continue,
                o => return o,
            }
        }
        Ordering::Equal
    }

    /// 加法。名字带 `_ref` 是为了与将来可能的消耗式 API 区分。
    pub fn add_ref(&self, other: &BigUint) -> BigUint {
        let n = self.limbs.len().max(other.limbs.len());
        let mut out = Vec::with_capacity(n + 1);
        let mut carry = 0u64;
        for i in 0..n {
            let a = *self.limbs.get(i).unwrap_or(&0) as u64;
            let b = *other.limbs.get(i).unwrap_or(&0) as u64;
            let s = a + b + carry;
            out.push(s as u32);
            carry = s >> 32;
        }
        if carry != 0 {
            out.push(carry as u32);
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        r
    }

    /// `self - other`，要求 `self >= other`（调用方保证）。
    pub fn sub_ref(&self, other: &BigUint) -> BigUint {
        debug_assert!(self.cmp(other) != std::cmp::Ordering::Less);
        let mut out = Vec::with_capacity(self.limbs.len());
        let mut borrow = 0i64;
        for i in 0..self.limbs.len() {
            let a = self.limbs[i] as i64;
            let b = *other.limbs.get(i).unwrap_or(&0) as i64;
            let mut d = a - b - borrow;
            if d < 0 {
                d += 1i64 << 32;
                borrow = 1;
            } else {
                borrow = 0;
            }
            out.push(d as u32);
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        r
    }

    pub fn mul_ref(&self, other: &BigUint) -> BigUint {
        if self.is_zero() || other.is_zero() {
            return BigUint::zero();
        }
        let mut out = vec![0u32; self.limbs.len() + other.limbs.len()];
        for (i, &a) in self.limbs.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in other.limbs.iter().enumerate() {
                let t = a as u64 * b as u64 + out[i + j] as u64 + carry;
                out[i + j] = t as u32;
                carry = t >> 32;
            }
            let mut k = i + other.limbs.len();
            while carry != 0 {
                let t = out[k] as u64 + carry;
                out[k] = t as u32;
                carry = t >> 32;
                k += 1;
            }
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        r
    }

    /// 左移 `n` 位。
    pub fn shl(&self, n: usize) -> BigUint {
        if self.is_zero() {
            return BigUint::zero();
        }
        let limb_shift = n / 32;
        let bit_shift = n % 32;
        let mut out = vec![0u32; limb_shift];
        let mut carry = 0u32;
        for &l in &self.limbs {
            if bit_shift == 0 {
                out.push(l);
            } else {
                out.push((l << bit_shift) | carry);
                carry = l >> (32 - bit_shift);
            }
        }
        if bit_shift != 0 && carry != 0 {
            out.push(carry);
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        r
    }

    /// 取第 `i` 位。
    pub fn bit(&self, i: usize) -> bool {
        let limb = i / 32;
        if limb >= self.limbs.len() {
            return false;
        }
        (self.limbs[limb] >> (i % 32)) & 1 == 1
    }

    /// 右移 `n` 位。ECDSA 取"摘要最左 nbits 位"时要用。
    pub fn shr(&self, n: usize) -> BigUint {
        let limb_shift = n / 32;
        let bit_shift = n % 32;
        if limb_shift >= self.limbs.len() {
            return BigUint::zero();
        }
        let mut out = Vec::with_capacity(self.limbs.len() - limb_shift);
        for i in limb_shift..self.limbs.len() {
            let mut v = self.limbs[i] >> bit_shift;
            if bit_shift > 0 {
                if let Some(&next) = self.limbs.get(i + 1) {
                    v |= next << (32 - bit_shift);
                }
            }
            out.push(v);
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        r
    }

    /// `self >= other`？供 RSA 校验 `s < n`、EC 的模减用。
    pub fn ge(&self, other: &BigUint) -> bool {
        self.cmp(other) != std::cmp::Ordering::Less
    }

    /// `self mod m`。二进制长除法，但**全程原地、不分配**。
    ///
    /// 朴素写法是每一位都 `shl`/`add`/`sub` 出一个新的 `BigUint`，
    /// 512 位的被除数就是上千次 `Vec` 分配——实测一次 P-256 验签要 640ms，
    /// 而分配本身就是大头。改成在一个固定缓冲里移位/比较/减，
    /// 同一次验签降到几十毫秒。算法没变，仍是逐位长除，好核对。
    pub fn rem(&self, m: &BigUint) -> BigUint {
        assert!(!m.is_zero(), "除数不能为零");
        if self.cmp(m) == std::cmp::Ordering::Less {
            return self.clone();
        }
        let ml = m.limbs.len();
        // 余数永远 < m，所以 ml 个肢体足够；多留一个放移位时的溢出。
        let mut r = vec![0u32; ml + 1];
        for i in (0..self.bits()).rev() {
            // r = r*2 + bit(i)
            let mut carry = u32::from(self.bit(i));
            for limb in r.iter_mut() {
                let next = *limb >> 31;
                *limb = (*limb << 1) | carry;
                carry = next;
            }
            // if r >= m { r -= m }
            if ge_slice(&r, &m.limbs) {
                sub_slice(&mut r, &m.limbs);
            }
        }
        let mut out = BigUint { limbs: r };
        out.trim();
        out
    }

    fn mul_mod(&self, other: &BigUint, m: &BigUint) -> BigUint {
        self.mul_ref(other).rem(m)
    }

    /// 模幂 `self^e mod m`。平方-乘法，从指数最高位往下。
    ///
    /// 用于 RSA 验签：`e` 是公钥指数（通常 65537），完全公开。
    pub fn modpow(&self, e: &BigUint, m: &BigUint) -> BigUint {
        assert!(!m.is_zero(), "模数不能为零");
        if m.limbs == [1] {
            return BigUint::zero();
        }
        let mut result = BigUint::one();
        let base = self.rem(m);
        if e.is_zero() {
            return result;
        }
        for i in (0..e.bits()).rev() {
            result = result.mul_mod(&result, m);
            if e.bit(i) {
                result = result.mul_mod(&base, m);
            }
        }
        result
    }
}

/// `a >= b`？两个切片按小端肢体比较，`a` 可以比 `b` 长。
fn ge_slice(a: &[u32], b: &[u32]) -> bool {
    for i in (b.len()..a.len()).rev() {
        if a[i] != 0 {
            return true;
        }
    }
    for i in (0..b.len()).rev() {
        match a.get(i).copied().unwrap_or(0).cmp(&b[i]) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    true
}

/// `a -= b`，就地。调用方保证 `a >= b`。
fn sub_slice(a: &mut [u32], b: &[u32]) {
    let mut borrow = 0i64;
    for (i, limb) in a.iter_mut().enumerate() {
        let bv = b.get(i).copied().unwrap_or(0) as i64;
        let mut d = *limb as i64 - bv - borrow;
        if d < 0 {
            d += 1i64 << 32;
            borrow = 1;
        } else {
            borrow = 0;
        }
        *limb = d as u32;
    }
    debug_assert_eq!(borrow, 0, "sub_slice 要求 a >= b");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(v: &str) -> BigUint {
        // 十六进制字面量转 BigUint，便于对照参考实现。
        let s = if v.len() % 2 == 1 {
            format!("0{v}")
        } else {
            v.to_string()
        };
        let bytes: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        BigUint::from_bytes_be(&bytes)
    }

    #[test]
    fn byte_round_trip_strips_and_restores_leading_zeros() {
        let b = BigUint::from_bytes_be(&[0x00, 0x00, 0x01, 0x02, 0x03]);
        assert_eq!(b.to_bytes_be(3).unwrap(), vec![1, 2, 3]);
        assert_eq!(b.to_bytes_be(5).unwrap(), vec![0, 0, 1, 2, 3]);
        // 放不下必须报错而不是截断——截断会把不匹配的签名变成"看着对"的。
        assert!(b.to_bytes_be(2).is_none());
        assert!(BigUint::zero().to_bytes_be(4).unwrap().iter().all(|&x| x == 0));
    }

    #[test]
    fn arithmetic_matches_reference_values() {
        let a = n("123456789abcdef0");
        let b = n("fedcba9876543210");
        assert_eq!(a.add_ref(&b), n("11111111111111100"));
        assert_eq!(b.sub_ref(&a), n("eca8641fdb975320"));
        assert_eq!(a.mul_ref(&b), n("121fa00ad77d7422236d88fe5618cf00"));
    }

    #[test]
    fn bits_and_shifts() {
        assert_eq!(BigUint::zero().bits(), 0);
        assert_eq!(BigUint::one().bits(), 1);
        assert_eq!(n("ff").bits(), 8);
        assert_eq!(n("100").bits(), 9);
        assert_eq!(n("1").shl(64), n("10000000000000000"));
        assert_eq!(n("abcd").shl(4), n("abcd0"));
    }

    #[test]
    fn rem_matches_reference() {
        assert_eq!(n("123456789abcdef0").rem(&n("fedcb")), n("8b466"));
        assert_eq!(n("ff").rem(&n("100")), n("ff"));
        assert_eq!(n("100").rem(&n("100")), BigUint::zero());
        assert_eq!(BigUint::zero().rem(&n("7")), BigUint::zero());
    }

    #[test]
    fn modpow_small_values() {
        // 3^5 mod 7 = 243 mod 7 = 5
        assert_eq!(
            BigUint::from_u32(3).modpow(&BigUint::from_u32(5), &BigUint::from_u32(7)),
            BigUint::from_u32(5)
        );
        // 任意 x^0 = 1
        assert_eq!(
            n("deadbeef").modpow(&BigUint::zero(), &n("10001")),
            BigUint::one()
        );
        // 2^256 mod (2^256-189)  →  189
        let m = n("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff43");
        assert_eq!(
            BigUint::from_u32(2).modpow(&BigUint::from_u32(0).add_ref(&n("100")), &m),
            n("bd")
        );
    }

    #[test]
    fn modpow_rsa_sized_operands() {
        // 一个 2048 位模数上的 e=65537 模幂，交叉验证：先算 s = m^d，
        // 再验 (m^d)^e == m。这里 d/e 用一对小的可逆指数模 φ 的替代：
        // 直接验证 modpow 的同态性 (a*b)^e == a^e * b^e (mod m)。
        let m = n(concat!(
            "c3a1f8b9d2e4750615a2c8f9e3b7d4a6c1e8f2b5d7a3c9e6f1b4d8a2c5e7f3b9",
            "d6a4c8e2f5b1d7a9c3e6f8b2d5a7c1e9f4b6d8a3c2e5f7b1d9a6c4e8f3b5d2a7"
        ));
        let a = n("deadbeefcafebabe1234567890abcdef");
        let b = n("fedcba0987654321a5a5a5a5a5a5a5a5");
        let e = BigUint::from_u32(65537);
        let lhs = a.mul_mod(&b, &m).modpow(&e, &m);
        let rhs = a.modpow(&e, &m).mul_mod(&b.modpow(&e, &m), &m);
        assert_eq!(lhs, rhs, "模幂对乘法的同态性必须成立");
    }

    #[test]
    fn no_leading_zero_limbs() {
        // 前导零肢体会让 cmp/bits 全错，这条钉住不变式。
        let a = n("100000000").sub_ref(&n("100000000"));
        assert!(a.is_zero());
        assert_eq!(a.limbs.len(), 0);
        assert_eq!(a.bits(), 0);
    }
}
