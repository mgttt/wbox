//! JSON —— 取代 `serde_json`。
//!
//! wbox 只用 `serde_json` 的**动态 `Value`** 一档（从来没有 derive），
//! 所以这里也只做那一档：解析成 [`Value`]、按路径取值、再序列化回去。
//!
//! 三条与 `serde_json` 保持一致的取舍，换掉时不能变，否则镜像的 digest 会变：
//!
//! 1. **对象键按字典序**。`serde_json` 默认的 `Map` 是 `BTreeMap`，序列化即
//!    有序。config/manifest 的字节被 sha256 之后就是镜像 digest，键序一变
//!    digest 就变，本地缓存与已推上去的镜像会对不上。
//! 2. **紧凑输出不含任何空白**，`to_string_pretty` 用两个空格缩进。
//! 3. **非 ASCII 不转义**，直接出 UTF-8 字节。
//!
//! 解析器对**敌意输入**要安全：manifest 来自 registry，是网络输入。因此有
//! 嵌套深度上限（[`MAX_DEPTH`]），避免深层嵌套把解析递归打爆栈。

use std::collections::BTreeMap;
use std::fmt;

/// 嵌套深度上限。与 `serde_json` 的默认值同量级；registry 的 manifest 实际
/// 只有三四层，128 已经宽松到不可能误伤。
pub const MAX_DEPTH: usize = 128;

/// JSON 对象。键有序（见模块注释第 1 条）。
pub type Map = BTreeMap<String, Value>;

/// JSON 数值。
///
/// 分三种而不是统一 `f64`，是因为 layer 大小、端口号这类整数经过 `f64`
/// 会在超过 2^53 时悄悄变样；镜像 config 里的 `size` 是真的可能很大。
#[derive(Debug, Clone, PartialEq)]
pub enum Number {
    PosInt(u64),
    NegInt(i64),
    Float(f64),
}

/// 一个 JSON 值。
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(Map),
}

/// 解析错误。带字节偏移，出错时能指到具体位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    msg: String,
    offset: usize,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON 解析失败（偏移 {}）：{}", self.offset, self.msg)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------- 取值 API

impl Value {
    /// 对象取字段 / 数组取下标。键不存在返回 `None`（不是 `Value::Null`），
    /// 这样调用方能区分"没有这个字段"和"字段是 null"。
    pub fn get<I: Index>(&self, index: I) -> Option<&Value> {
        index.index_into(self)
    }

    /// 按 JSON Pointer（RFC 6901）取值，如 `/config/digest`。
    pub fn pointer(&self, pointer: &str) -> Option<&Value> {
        if pointer.is_empty() {
            return Some(self);
        }
        if !pointer.starts_with('/') {
            return None;
        }
        let mut cur = self;
        for token in pointer[1..].split('/') {
            // RFC 6901 的转义：~1 是 /，~0 是 ~。顺序不能反。
            let token = token.replace("~1", "/").replace("~0", "~");
            cur = match cur {
                Value::Object(m) => m.get(&token)?,
                Value::Array(a) => a.get(token.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(Number::PosInt(n)) => Some(*n),
            Value::Number(Number::NegInt(n)) if *n >= 0 => Some(*n as u64),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(Number::NegInt(n)) => Some(*n),
            Value::Number(Number::PosInt(n)) if *n <= i64::MAX as u64 => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(Number::Float(f)) => Some(*f),
            Value::Number(Number::PosInt(n)) => Some(*n as f64),
            Value::Number(Number::NegInt(n)) => Some(*n as f64),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// 两空格缩进的可读序列化。
    ///
    /// 紧凑序列化走 `Display`（即 `.to_string()`）——不另开一个同名的固有
    /// 方法，免得两条路以后走岔。
    pub fn to_string_pretty(&self) -> String {
        let mut s = String::new();
        write_pretty(&mut s, self, 0);
        s
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::new();
        write_compact(&mut s, self);
        f.write_str(&s)
    }
}

/// `Value::get` 的下标：字符串取对象字段，整数取数组元素。
pub trait Index {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value>;
}

impl Index for str {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        v.as_object()?.get(self)
    }
}

impl Index for String {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        self.as_str().index_into(v)
    }
}

impl<T: Index + ?Sized> Index for &T {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        (**self).index_into(v)
    }
}

impl Index for usize {
    fn index_into<'v>(&self, v: &'v Value) -> Option<&'v Value> {
        v.as_array()?.get(*self)
    }
}

/// `v["key"]` 语法：缺字段返回 `Value::Null`（与 `serde_json` 一致）。
impl<I: Index> std::ops::Index<I> for Value {
    type Output = Value;
    fn index(&self, index: I) -> &Value {
        const NULL: Value = Value::Null;
        index.index_into(self).unwrap_or(&NULL)
    }
}

// ------------------------------------------------------------ From 转换

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

macro_rules! from_unsigned {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(n: $t) -> Self { Value::Number(Number::PosInt(n as u64)) }
        }
    )*};
}
from_unsigned!(u8, u16, u32, u64, usize);

macro_rules! from_signed {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(n: $t) -> Self {
                if n >= 0 { Value::Number(Number::PosInt(n as u64)) }
                else { Value::Number(Number::NegInt(n as i64)) }
            }
        }
    )*};
}
from_signed!(i8, i16, i32, i64, isize);

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        // NaN / ±Inf 在 JSON 里没有表示，`serde_json` 也是转成 null。
        if f.is_finite() {
            Value::Number(Number::Float(f))
        } else {
            Value::Null
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<&String> for Value {
    fn from(s: &String) -> Self {
        Value::String(s.clone())
    }
}

impl From<std::borrow::Cow<'_, str>> for Value {
    fn from(s: std::borrow::Cow<'_, str>) -> Self {
        Value::String(s.into_owned())
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Self {
        Value::Array(v.into_iter().map(Into::into).collect())
    }
}

impl<T: Into<Value> + Clone> From<&Vec<T>> for Value {
    fn from(v: &Vec<T>) -> Self {
        Value::Array(v.iter().cloned().map(Into::into).collect())
    }
}

impl<T: Into<Value> + Clone> From<&[T]> for Value {
    fn from(v: &[T]) -> Self {
        Value::Array(v.iter().cloned().map(Into::into).collect())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Value::Null)
    }
}

impl From<Map> for Value {
    fn from(m: Map) -> Self {
        Value::Object(m)
    }
}

impl<T: Into<Value>> FromIterator<T> for Value {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Value::Array(iter.into_iter().map(Into::into).collect())
    }
}

// ------------------------------------------------------------ 与标量比较

// `assert_eq!(v["k"], "x")` 这种写法在测试里到处都是（`serde_json` 给
// `Value` 实现了一整套 `PartialEq<标量>`）。这里补齐同样的一套，否则每个
// 断言都要手写 `.as_str() == Some(...)`，可读性掉一大截。

impl PartialEq<str> for Value {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}

impl PartialEq<&str> for Value {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}

impl PartialEq<String> for Value {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == Some(other.as_str())
    }
}

impl PartialEq<Value> for &str {
    fn eq(&self, other: &Value) -> bool {
        other.as_str() == Some(*self)
    }
}

impl PartialEq<bool> for Value {
    fn eq(&self, other: &bool) -> bool {
        self.as_bool() == Some(*other)
    }
}

macro_rules! eq_number {
    ($($t:ty),*) => {$(
        impl PartialEq<$t> for Value {
            fn eq(&self, other: &$t) -> bool {
                self.as_f64() == Some(*other as f64)
            }
        }
    )*};
}
eq_number!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f64);

// ------------------------------------------------------------ 借用式转换

/// 由**引用**构造 [`Value`]。
///
/// `json!` 宏里的值走这条路而不是 `From`，因为 `serde_json::json!` 是靠
/// `Serialize` 借用取值的：写 `json!({"a": x, "b": format!("{x}")})` 时 `x`
/// 不该被移走。用 `From` 的话每个字段都要在调用点补 `.clone()`——那是几十处
/// 无谓的改动，而且以后每加一个字段都要再想一次。
pub trait ToValue {
    fn to_value(&self) -> Value;
}

/// `json!` 宏的取值入口。
pub fn to_value<T: ToValue + ?Sized>(v: &T) -> Value {
    v.to_value()
}

impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

impl ToValue for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }
}

impl ToValue for str {
    fn to_value(&self) -> Value {
        Value::String(self.to_string())
    }
}

impl ToValue for String {
    fn to_value(&self) -> Value {
        Value::String(self.clone())
    }
}

impl ToValue for std::borrow::Cow<'_, str> {
    fn to_value(&self) -> Value {
        Value::String(self.clone().into_owned())
    }
}

impl ToValue for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
}

macro_rules! to_value_via_from {
    ($($t:ty),*) => {$(
        impl ToValue for $t {
            fn to_value(&self) -> Value { Value::from(*self) }
        }
    )*};
}
to_value_via_from!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f64);

impl<T: ToValue> ToValue for Vec<T> {
    fn to_value(&self) -> Value {
        Value::Array(self.iter().map(ToValue::to_value).collect())
    }
}

impl<T: ToValue> ToValue for [T] {
    fn to_value(&self) -> Value {
        Value::Array(self.iter().map(ToValue::to_value).collect())
    }
}

impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value {
        self.as_ref().map(ToValue::to_value).unwrap_or(Value::Null)
    }
}

impl ToValue for Map {
    fn to_value(&self) -> Value {
        Value::Object(self.clone())
    }
}

// ---------------------------------------------------------------- 序列化

fn write_compact(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_string(out, s),
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(out, e);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, val)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_compact(out, val);
            }
            out.push('}');
        }
    }
}

fn write_pretty(out: &mut String, v: &Value, indent: usize) {
    let pad = |out: &mut String, n: usize| {
        for _ in 0..n {
            out.push_str("  ");
        }
    };
    match v {
        Value::Array(a) if !a.is_empty() => {
            out.push_str("[\n");
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                pad(out, indent + 1);
                write_pretty(out, e, indent + 1);
            }
            out.push('\n');
            pad(out, indent);
            out.push(']');
        }
        Value::Object(m) if !m.is_empty() => {
            out.push_str("{\n");
            for (i, (k, val)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                pad(out, indent + 1);
                write_string(out, k);
                out.push_str(": ");
                write_pretty(out, val, indent + 1);
            }
            out.push('\n');
            pad(out, indent);
            out.push('}');
        }
        // 空容器与标量都是单行，与 serde_json 的 pretty 一致。
        other => write_compact(out, other),
    }
}

fn write_number(out: &mut String, n: &Number) {
    match n {
        Number::PosInt(v) => out.push_str(&v.to_string()),
        Number::NegInt(v) => out.push_str(&v.to_string()),
        Number::Float(f) => {
            // `{}` 会把 1.0 打成 "1"，那读回来就变成整数了。补上 ".0"
            // 保住类型往返。
            let s = f.to_string();
            out.push_str(&s);
            if !s.contains(['.', 'e', 'E']) {
                out.push_str(".0");
            }
        }
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// 序列化为紧凑字节。
pub fn to_vec(v: &Value) -> Vec<u8> {
    v.to_string().into_bytes()
}

/// 序列化为紧凑字符串。
pub fn to_string(v: &Value) -> String {
    v.to_string()
}

/// 序列化为两空格缩进的字符串。
pub fn to_string_pretty(v: &Value) -> String {
    v.to_string_pretty()
}

// ---------------------------------------------------------------- 解析

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

/// 解析 UTF-8 字符串。
pub fn from_str(s: &str) -> Result<Value> {
    from_slice(s.as_bytes())
}

/// 解析字节。要求整体是合法 UTF-8（JSON 规范如此），尾部只允许空白。
pub fn from_slice(b: &[u8]) -> Result<Value> {
    let mut p = Parser { b, i: 0, depth: 0 };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.i != p.b.len() {
        return Err(p.err("文档结尾之后还有内容"));
    }
    Ok(v)
}

impl<'a> Parser<'a> {
    fn err(&self, msg: impl Into<String>) -> Error {
        Error {
            msg: msg.into(),
            offset: self.i,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(format!("期待 '{}'", c as char)))
        }
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err(format!("期待 {word}")))
        }
    }

    fn value(&mut self) -> Result<Value> {
        match self.peek() {
            None => Err(self.err("输入意外结束")),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(self.err(format!("意外的字符 {:?}", c as char))),
        }
    }

    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            // 网络来的 manifest 可能是敌意构造的，深层嵌套会打爆递归栈。
            return Err(self.err(format!("嵌套深度超过 {MAX_DEPTH}")));
        }
        Ok(())
    }

    fn array(&mut self) -> Result<Value> {
        self.enter()?;
        self.expect(b'[')?;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            self.depth -= 1;
            return Ok(Value::Array(out));
        }
        loop {
            self.skip_ws();
            out.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    break;
                }
                _ => return Err(self.err("数组里期待 ',' 或 ']'")),
            }
        }
        self.depth -= 1;
        Ok(Value::Array(out))
    }

    fn object(&mut self) -> Result<Value> {
        self.enter()?;
        self.expect(b'{')?;
        let mut map = Map::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            self.depth -= 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("对象的键必须是字符串"));
            }
            let k = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let v = self.value()?;
            // 重复键取后者，与 serde_json 一致。
            map.insert(k, v);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                _ => return Err(self.err("对象里期待 ',' 或 '}'")),
            }
        }
        self.depth -= 1;
        Ok(Value::Object(map))
    }

    fn string(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or_else(|| self.err("字符串未闭合"))?;
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let e = self.peek().ok_or_else(|| self.err("转义未完成"))?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        other => {
                            return Err(self.err(format!("未知转义 \\{}", other as char)));
                        }
                    }
                }
                // 未转义的控制字符是非法的（RFC 8259）。
                0x00..=0x1f => return Err(self.err("字符串里有未转义的控制字符")),
                _ => {
                    // 多字节 UTF-8 原样取走；整体合法性由下面的 from_utf8 校验。
                    let start = self.i;
                    while let Some(c) = self.peek() {
                        if c == b'"' || c == b'\\' || c < 0x20 {
                            break;
                        }
                        self.i += 1;
                    }
                    let s = std::str::from_utf8(&self.b[start..self.i])
                        .map_err(|_| self.err("字符串不是合法 UTF-8"))?;
                    out.push_str(s);
                }
            }
        }
    }

    /// `\uXXXX`，含 UTF-16 代理对。
    fn unicode_escape(&mut self) -> Result<char> {
        let hi = self.hex4()?;
        // 高代理必须紧跟一个低代理，否则这个字符串无法表示成 Rust 的 char。
        if (0xd800..0xdc00).contains(&hi) {
            if self.peek() != Some(b'\\') {
                return Err(self.err("孤立的 UTF-16 高代理"));
            }
            self.i += 1;
            if self.peek() != Some(b'u') {
                return Err(self.err("孤立的 UTF-16 高代理"));
            }
            self.i += 1;
            let lo = self.hex4()?;
            if !(0xdc00..0xe000).contains(&lo) {
                return Err(self.err("UTF-16 代理对的低位非法"));
            }
            let c = 0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00);
            return char::from_u32(c).ok_or_else(|| self.err("代理对不是合法码点"));
        }
        if (0xdc00..0xe000).contains(&hi) {
            return Err(self.err("孤立的 UTF-16 低代理"));
        }
        char::from_u32(hi).ok_or_else(|| self.err("\\u 不是合法码点"))
    }

    fn hex4(&mut self) -> Result<u32> {
        if self.i + 4 > self.b.len() {
            return Err(self.err("\\u 后不足四位"));
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4])
            .map_err(|_| self.err("\\u 后不是十六进制"))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| self.err("\\u 后不是十六进制"))?;
        self.i += 4;
        Ok(v)
    }

    fn number(&mut self) -> Result<Value> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        // 整数部分：0 单独一位，或 1-9 开头（JSON 不允许前导零）。
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            _ => return Err(self.err("数字缺少整数部分")),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.i += 1;
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err(self.err("小数点后缺少数字"));
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return Err(self.err("指数缺少数字"));
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.err("数字非法"))?;
        if !is_float {
            if let Ok(v) = text.parse::<u64>() {
                return Ok(Value::Number(Number::PosInt(v)));
            }
            if let Ok(v) = text.parse::<i64>() {
                return Ok(Value::Number(Number::NegInt(v)));
            }
            // 超出 64 位整数范围时退回浮点，而不是报错——registry 不会给出
            // 这种值，但拒绝一个合法 JSON 文档的代价更大。
        }
        let f = text.parse::<f64>().map_err(|_| self.err("数字非法"))?;
        // `1e9999` 会 parse 成 inf。放行的话序列化时会吐出 `inf`——**那不是
        // 合法 JSON**，等于我们自己产出了读不回来的文档。JSON 规范也没有
        // 无穷与 NaN 的表示，所以这里拒绝是唯一自洽的选择。
        if !f.is_finite() {
            return Err(self.err("数字超出可表示范围（JSON 没有 inf/NaN）"));
        }
        Ok(Value::Number(Number::Float(f)))
    }
}

// ---------------------------------------------------------------- json! 宏

/// 构造 [`Value`] 的字面量宏，语法与 `serde_json::json!` 一致。
///
/// ```
/// # use wbox_codec::json;
/// let v = json!({ "a": [1, 2], "b": null });
/// assert_eq!(v.to_string(), r#"{"a":[1,2],"b":null}"#);
/// ```
#[macro_export]
macro_rules! json {
    ($($tt:tt)+) => {
        $crate::json_internal!($($tt)+)
    };
}

/// `json!` 的实现细节（tt muncher）。不要直接调用。
#[doc(hidden)]
#[macro_export]
macro_rules! json_internal {
    // ---- 数组内部：把元素逐个啃出来，攒成 vec![...] ----
    (@array [$($elems:expr,)*]) => {
        ::std::vec![$($elems,)*]
    };
    (@array [$($elems:expr,)*] null $($rest:tt)*) => {
        $crate::json_internal!(@array [$($elems,)* $crate::json_internal!(null),] $($rest)*)
    };
    (@array [$($elems:expr,)*] [$($array:tt)*] $($rest:tt)*) => {
        $crate::json_internal!(@array [$($elems,)* $crate::json_internal!([$($array)*]),] $($rest)*)
    };
    (@array [$($elems:expr,)*] {$($map:tt)*} $($rest:tt)*) => {
        $crate::json_internal!(@array [$($elems,)* $crate::json_internal!({$($map)*}),] $($rest)*)
    };
    (@array [$($elems:expr,)*] $next:expr, $($rest:tt)*) => {
        $crate::json_internal!(@array [$($elems,)* $crate::json_internal!($next),] $($rest)*)
    };
    (@array [$($elems:expr,)*] $last:expr) => {
        $crate::json_internal!(@array [$($elems,)* $crate::json_internal!($last),])
    };
    (@array [$($elems:expr,)*] , $($rest:tt)*) => {
        $crate::json_internal!(@array [$($elems,)*] $($rest)*)
    };

    // ---- 对象内部：键可以是任意 tt 序列（表达式也行），值分四种形态 ----
    (@object $object:ident () () ()) => {};
    (@object $object:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        let _ = $object.insert(($($key)+).into(), $value);
        $crate::json_internal!(@object $object () ($($rest)*) ($($rest)*));
    };
    (@object $object:ident [$($key:tt)+] ($value:expr)) => {
        let _ = $object.insert(($($key)+).into(), $value);
    };
    (@object $object:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => {
        $crate::json_internal!(@object $object [$($key)+] ($crate::json_internal!(null)) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: [$($array:tt)*] $($rest:tt)*) $copy:tt) => {
        $crate::json_internal!(@object $object [$($key)+] ($crate::json_internal!([$($array)*])) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: {$($map:tt)*} $($rest:tt)*) $copy:tt) => {
        $crate::json_internal!(@object $object [$($key)+] ($crate::json_internal!({$($map)*})) $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: $value:expr , $($rest:tt)*) $copy:tt) => {
        $crate::json_internal!(@object $object [$($key)+] ($crate::json_internal!($value)) , $($rest)*);
    };
    (@object $object:ident ($($key:tt)+) (: $value:expr) $copy:tt) => {
        $crate::json_internal!(@object $object [$($key)+] ($crate::json_internal!($value)));
    };
    (@object $object:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => {
        $crate::json_internal!(@object $object ($($key)* $tt) ($($rest)*) ($($rest)*));
    };

    // ---- 入口形态 ----
    (null) => { $crate::json::Value::Null };
    ([]) => { $crate::json::Value::Array(::std::vec::Vec::new()) };
    ([ $($tt:tt)+ ]) => {
        $crate::json::Value::Array($crate::json_internal!(@array [] $($tt)+))
    };
    ({}) => { $crate::json::Value::Object($crate::json::Map::new()) };
    ({ $($tt:tt)+ }) => {
        $crate::json::Value::Object({
            let mut object = $crate::json::Map::new();
            $crate::json_internal!(@object object () ($($tt)+) ($($tt)+));
            object
        })
    };
    ($other:expr) => { $crate::json::to_value(&$other) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_reserialize_keeps_key_order_sorted() {
        // 键序是 digest 的一部分（模块注释第 1 条），必须与 serde_json 一致：
        // 字典序，与输入顺序无关。
        let v = from_str(r#"{"z":1,"a":{"y":2,"b":3}}"#).unwrap();
        assert_eq!(v.to_string(), r#"{"a":{"b":3,"y":2},"z":1}"#);
    }

    #[test]
    fn parses_all_scalar_forms() {
        let v = from_str(r#"{"n":null,"t":true,"f":false,"i":-12,"u":18446744073709551615,"x":1.5e2,"s":"a\"b\\c\nA😀"}"#).unwrap();
        assert!(v.get("n").unwrap().is_null());
        assert_eq!(v.get("t").unwrap().as_bool(), Some(true));
        assert_eq!(v.get("f").unwrap().as_bool(), Some(false));
        assert_eq!(v.get("i").unwrap().as_i64(), Some(-12));
        assert_eq!(v.get("u").unwrap().as_u64(), Some(u64::MAX));
        assert_eq!(v.get("x").unwrap().as_f64(), Some(150.0));
        assert_eq!(v.get("s").unwrap().as_str(), Some("a\"b\\c\nA😀"));
    }

    #[test]
    fn string_escapes_round_trip() {
        let s = "引号\" 反斜杠\\ 换行\n 制表\t 控制\u{01} 中文 😀";
        let v = Value::String(s.to_string());
        let text = v.to_string();
        assert!(text.contains("\\u0001"), "控制字符要转义：{text}");
        assert!(text.contains("中文"), "非 ASCII 不转义：{text}");
        assert_eq!(from_str(&text).unwrap(), v);
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\":}",
            "{a:1}",
            "01",
            "1.",
            "\"unterminated",
            "nul",
            "{} trailing",
            "\"\u{01}\"",
            r#""\ud800""#, // 孤立高代理
        ] {
            assert!(from_str(bad).is_err(), "应当拒绝 {bad:?}");
        }
    }

    #[test]
    fn depth_limit_stops_runaway_nesting() {
        // 网络输入可能是敌意构造的深层嵌套：必须报错而不是打爆栈。
        let deep = format!("{}{}", "[".repeat(1000), "]".repeat(1000));
        let e = from_str(&deep).unwrap_err();
        assert!(e.to_string().contains("嵌套深度"), "{e}");
    }

    #[test]
    fn pointer_walks_objects_and_arrays() {
        let v = from_str(r#"{"config":{"digest":"sha256:ab"},"l":[{"k":1}]}"#).unwrap();
        assert_eq!(
            v.pointer("/config/digest").and_then(|d| d.as_str()),
            Some("sha256:ab")
        );
        assert_eq!(v.pointer("/l/0/k").and_then(|d| d.as_u64()), Some(1));
        assert!(v.pointer("/nope").is_none());
        assert!(v.pointer("relative").is_none());
    }

    #[test]
    fn macro_builds_nested_values() {
        let arch = "amd64";
        let ids = vec!["sha256:a".to_string()];
        let v = json!({
            "architecture": arch,
            "rootfs": { "type": "layers", "diff_ids": ids },
            "history": [],
            "empty": {},
            "nested": [{"a": 1}, null, true],
        });
        assert_eq!(
            v.to_string(),
            r#"{"architecture":"amd64","empty":{},"history":[],"nested":[{"a":1},null,true],"rootfs":{"diff_ids":["sha256:a"],"type":"layers"}}"#
        );
    }

    #[test]
    fn macro_accepts_multi_token_values() {
        // 值是方法调用（多个 token）时也要能用——这是 tt muncher 存在的理由。
        struct S {
            name: String,
        }
        let s = S { name: "x".into() };
        let v = json!({ "ref": s.name.to_uppercase(), "n": 1 + 1 });
        assert_eq!(v.to_string(), r#"{"n":2,"ref":"X"}"#);
    }

    #[test]
    fn pretty_matches_two_space_indent() {
        let v = json!({"a": [1, {"b": 2}], "c": {}, "d": []});
        assert_eq!(
            v.to_string_pretty(),
            "{\n  \"a\": [\n    1,\n    {\n      \"b\": 2\n    }\n  ],\n  \"c\": {},\n  \"d\": []\n}"
        );
    }

    #[test]
    fn floats_round_trip_as_floats() {
        let v = json!(1.0f64);
        assert_eq!(v.to_string(), "1.0");
        assert_eq!(from_str("1.0").unwrap(), v);
        // 非有限值 JSON 表示不了，与 serde_json 一样落成 null。
        assert!(Value::from(f64::NAN).is_null());
    }

    #[test]
    fn rejects_numbers_that_overflow_to_infinity() {
        // `1e9999` parse 成 inf。放行的话序列化会吐出 `inf`——那不是合法
        // JSON，等于我们自己产出了读不回来的文档。
        for bad in ["1e9999", "-1e9999", "1e400", "1E999999999"] {
            let e = from_str(bad).unwrap_err();
            assert!(e.to_string().contains("超出可表示范围"), "{bad}: {e}");
        }
        // 边界之内照常解析。
        assert!(from_str("1e308").is_ok());
        assert!(from_str("1e-308").is_ok());
    }

    #[test]
    fn every_parsed_value_reserializes_to_parsable_json() {
        // 更强的不变式：解析出来的东西再序列化，必须还能解析回来。
        // 这条能兜住"某个分支产出了非法字面量"的一整类问题。
        for src in [
            r#"{"a":1e308,"b":-0.0,"c":1.5,"d":[1,2,3]}"#,
            r#"{"big":18446744073709551615,"neg":-9223372036854775808}"#,
            r#"{"s":"😀 emoji","esc":"a\\b\"c","ctl":"\n\t"}"#,
        ] {
            let v = from_str(src).unwrap();
            let text = v.to_string();
            let back =
                from_str(&text).unwrap_or_else(|e| panic!("重新序列化后解析不回来：{text} → {e}"));
            assert_eq!(back, v, "往返不一致：{src}");
        }
    }

    #[test]
    fn index_returns_null_for_missing() {
        let v = json!({"a": 1});
        assert_eq!(v["a"].as_u64(), Some(1));
        assert!(v["missing"].is_null());
        assert!(v[3].is_null());
    }
}
