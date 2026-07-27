//! compose 文件用的**有界 YAML 子集**解析器（`PRD.md` F9.14）。
//!
//! # 为什么手写而不是引依赖
//!
//! `serde_yaml` 已归档停维护；引一个不再收安全修复的解析器去读用户提供的文件，
//! 不划算。而 compose 实际用到的 YAML 是很窄的一块：缩进嵌套映射、`- ` 序列、
//! 标量、行内流式序列。本项目已有手写 Dockerfile 子集解析器的先例。
//!
//! # 关键取舍：不支持的构造**显式报错**，绝不猜
//!
//! 子集解析器最危险的失败形态是"看着解析成功了，其实理解错了"——例如把
//! `|` 多行标量的后续行当成新的 key。所以凡是本子集不支持的 YAML 特性
//! （锚点/别名、多行标量、流式映射、tab 缩进）一律报错并指名道姓，
//! 让用户知道该改哪一行，而不是拿到一个语义被悄悄改掉的配置。

use crate::error::{Result, WboxError};

/// 子集 YAML 的节点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Scalar(String),
    Seq(Vec<Node>),
    Map(Vec<(String, Node)>),
}

impl Node {
    /// 按 key 取子节点（映射）。
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// 标量取值。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::Scalar(s) => Some(s),
            _ => None,
        }
    }

    /// 映射的键值对。
    pub fn as_map(&self) -> Option<&[(String, Node)]> {
        match self {
            Node::Map(m) => Some(m),
            _ => None,
        }
    }

    /// **标量或序列**统一取成字符串列表。
    ///
    /// compose 里同一个字段常有两种写法（`command: sh -c x` 与
    /// `command: ["sh","-c","x"]`），调用方不该各写一遍分支。
    /// 标量按 shell 词法**不做**拆分——拆分规则是 compose 的语义问题，
    /// 交给上层决定，这里只负责形状归一。
    pub fn as_list(&self) -> Option<Vec<String>> {
        match self {
            Node::Scalar(s) => Some(vec![s.clone()]),
            Node::Seq(items) => items
                .iter()
                .map(|n| n.as_str().map(str::to_string))
                .collect(),
            _ => None,
        }
    }
}

/// 取出一行里要做"不支持构造"检查的部分：有 `key:` 就取冒号之后的值，
/// 否则取整行（去掉序列项的 `- ` 前缀）。
fn unsupported_scan_target(trimmed: &str) -> &str {
    let body = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim();
    match find_key_colon(body) {
        Some(i) => body[i + 1..].trim(),
        None => body,
    }
}

/// 一行的结构化形式。
struct Line {
    no: usize,
    indent: usize,
    text: String,
}

/// 解析 YAML 子集。
pub fn parse(input: &str) -> Result<Node> {
    let lines = scan(input)?;
    if lines.is_empty() {
        return Ok(Node::Map(Vec::new()));
    }
    let mut pos = 0usize;
    let node = parse_block(&lines, &mut pos, lines[0].indent)?;
    if pos < lines.len() {
        return Err(err(lines[pos].no, "缩进回退到未出现过的层级"));
    }
    Ok(node)
}

fn err(line: usize, msg: &str) -> WboxError {
    WboxError::args(format!("compose 文件第 {} 行：{}", line, msg))
}

/// 预扫描：去注释与空行，算缩进，并**在这里就拦掉不支持的构造**。
fn scan(input: &str) -> Result<Vec<Line>> {
    let mut out = Vec::new();
    for (i, raw) in input.lines().enumerate() {
        let no = i + 1;
        if raw.contains('\t') {
            return Err(err(no, "含 tab；YAML 缩进只能用空格"));
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // 文档分隔符：单文档场景直接跳过 `---`，多文档（第二个 ---）不支持
        if trimmed == "---" {
            if !out.is_empty() {
                return Err(err(no, "不支持多文档 YAML（第二个 `---`）"));
            }
            continue;
        }
        // 这些检查要看**值**的位置，不能只看行首：`a: &anchor` 与 `a: {b: 1}`
        // 的行首都是普通字符，只查行首会让它们静默通过——而"看着解析成功了、
        // 其实理解错了"正是子集解析器最危险的失败形态。
        let value = unsupported_scan_target(trimmed);
        if value.starts_with('&') || value.starts_with('*') {
            return Err(err(no, "不支持 YAML 锚点/别名（& 与 *）"));
        }
        if value == "|" || value == ">" || value.starts_with("|-") || value.starts_with(">-") {
            return Err(err(no, "不支持多行标量（| 与 >）"));
        }
        if value.starts_with('{') {
            return Err(err(no, "不支持流式映射（{...}）；请用缩进写法"));
        }
        let indent = raw.len() - raw.trim_start().len();
        out.push(Line {
            no,
            indent,
            text: trimmed.to_string(),
        });
    }
    Ok(out)
}

/// 解析一个缩进层级的块（映射或序列）。
fn parse_block(lines: &[Line], pos: &mut usize, indent: usize) -> Result<Node> {
    if lines[*pos].text.starts_with("- ") || lines[*pos].text == "-" {
        parse_seq(lines, pos, indent)
    } else {
        parse_map(lines, pos, indent)
    }
}

fn parse_seq(lines: &[Line], pos: &mut usize, indent: usize) -> Result<Node> {
    let mut items = Vec::new();
    while *pos < lines.len() && lines[*pos].indent == indent {
        let line = &lines[*pos];
        let Some(rest) = line.text.strip_prefix('-') else {
            break;
        };
        let rest = rest.trim();
        if rest.is_empty() {
            return Err(err(line.no, "序列项不能为空"));
        }
        // `- key: value` 形式（序列里的映射）本子集不支持：compose 的
        // services/ports/volumes/environment 都用不到，支持它会让缩进
        // 规则复杂一大截，而复杂正是误解的温床。
        if is_key_line(rest) {
            return Err(err(
                line.no,
                "不支持序列项里的映射（`- key: value`）；本子集只收标量序列项",
            ));
        }
        items.push(Node::Scalar(unquote(rest)));
        *pos += 1;
    }
    Ok(Node::Seq(items))
}

fn parse_map(lines: &[Line], pos: &mut usize, indent: usize) -> Result<Node> {
    let mut map: Vec<(String, Node)> = Vec::new();
    while *pos < lines.len() && lines[*pos].indent == indent {
        let line = &lines[*pos];
        if line.text.starts_with('-') {
            return Err(err(line.no, "映射层级里出现了序列项"));
        }
        let (key, rest) = split_key(line.no, &line.text)?;
        *pos += 1;
        let value = if !rest.is_empty() {
            // 行内值：流式序列 或 标量
            if rest.starts_with('[') {
                parse_flow_seq(line.no, rest)?
            } else {
                Node::Scalar(unquote(rest))
            }
        } else {
            // 块值：看下一行的缩进决定
            if *pos < lines.len() && lines[*pos].indent > indent {
                let child = lines[*pos].indent;
                parse_block(lines, pos, child)?
            } else {
                // `key:` 后面什么都没有 —— 空值。compose 里这通常是写漏了，
                // 但 YAML 语义上合法（null），故当成空映射而不是报错。
                Node::Map(Vec::new())
            }
        };
        if map.iter().any(|(k, _)| *k == key) {
            return Err(err(line.no, &format!("重复的键 '{}'", key)));
        }
        map.push((key, value));
    }
    Ok(Node::Map(map))
}

/// 这一行看着像 `key: value` 吗（用于拒绝序列里的映射）。
fn is_key_line(s: &str) -> bool {
    find_key_colon(s).is_some()
}

/// 找到分隔 key 与 value 的冒号位置：**引号内的冒号不算**，
/// 且冒号后必须是空格或行尾（否则 `image: nginx:latest` 的第二个冒号会误伤）。
fn find_key_colon(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &c) in b.iter().enumerate() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == b'"' || c == b'\'' {
                    quote = Some(c);
                } else if c == b':' && (i + 1 == b.len() || b[i + 1] == b' ') {
                    return Some(i);
                }
            }
        }
    }
    None
}

fn split_key(no: usize, s: &str) -> Result<(String, &str)> {
    let i = find_key_colon(s)
        .ok_or_else(|| err(no, "不是合法的 `key: value`（缺少冒号，或冒号后没有空格）"))?;
    let key = unquote(s[..i].trim());
    if key.is_empty() {
        return Err(err(no, "键名为空"));
    }
    Ok((key, s[i + 1..].trim()))
}

/// 解析行内流式序列：`["a", "b"]`。只收标量项。
fn parse_flow_seq(no: usize, s: &str) -> Result<Node> {
    let inner = s
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .ok_or_else(|| err(no, "流式序列缺少配对的 ']'"))?;
    if inner.trim().is_empty() {
        return Ok(Node::Seq(Vec::new()));
    }
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                ',' => {
                    items.push(Node::Scalar(unquote(cur.trim())));
                    cur.clear();
                }
                _ => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return Err(err(no, "流式序列里的引号没有闭合"));
    }
    if !cur.trim().is_empty() {
        items.push(Node::Scalar(unquote(cur.trim())));
    }
    Ok(Node::Seq(items))
}

/// 去掉成对的引号。**不做转义处理**：compose 里用到转义的场合极少，
/// 而半吊子的转义实现比不实现更容易出错。
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        return s[1..s.len() - 1].to_string();
    }
    // 行尾注释：只在**未加引号**的标量上剥离，且要求 `#` 前有空格
    // （否则 `image: a#b` 这种合法值会被截断）
    if let Some(i) = s.find(" #") {
        return s[..i].trim_end().to_string();
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Node {
        parse(s).unwrap()
    }

    #[test]
    fn parses_nested_maps_and_seqs() {
        let n = p("services:\n  web:\n    image: nginx:latest\n    ports:\n      - \"8080:80\"\n      - '9090:90'\n");
        let web = n.get("services").unwrap().get("web").unwrap();
        // `image: nginx:latest` —— 值里的冒号不能被当成 key 分隔符
        assert_eq!(web.get("image").unwrap().as_str(), Some("nginx:latest"));
        assert_eq!(
            web.get("ports").unwrap().as_list().unwrap(),
            vec!["8080:80", "9090:90"]
        );
    }

    #[test]
    fn flow_sequence_and_scalar_both_become_list() {
        let n = p("a:\n  command: [\"/bin/sh\", \"-c\", \"echo hi, there\"]\nb:\n  command: /bin/true\n");
        assert_eq!(
            n.get("a").unwrap().get("command").unwrap().as_list().unwrap(),
            vec!["/bin/sh", "-c", "echo hi, there"]
        );
        assert_eq!(
            n.get("b").unwrap().get("command").unwrap().as_list().unwrap(),
            vec!["/bin/true"]
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let n = p("# top\n\nservices:\n  # inner\n  a:\n    image: x  # trailing\n");
        assert_eq!(
            n.get("services").unwrap().get("a").unwrap().get("image").unwrap().as_str(),
            Some("x")
        );
    }

    /// 井号只有在**前面有空格且未加引号**时才是注释。
    /// `a#b` 是合法值，截断它就是静默改掉用户的配置。
    #[test]
    fn hash_without_space_is_part_of_the_value() {
        let n = p("k: a#b\nq: \"has # inside\"\n");
        assert_eq!(n.get("k").unwrap().as_str(), Some("a#b"));
        assert_eq!(n.get("q").unwrap().as_str(), Some("has # inside"));
    }

    /// 子集解析器最危险的失败是"看着成功了其实理解错了"。
    /// 不支持的构造必须报错，且指出行号。
    #[test]
    fn unsupported_constructs_are_rejected_with_line_numbers() {
        for (src, want) in [
            ("a:\n\tb: 1\n", "tab"),
            ("a: &anchor\n", "锚点"),
            ("a: |\n  text\n", "多行标量"),
            ("a: {b: 1}\n", "流式映射"),
            ("services:\n  - name: x\n    image: y\n", "序列项里的映射"),
            ("a: [\"unclosed\n", "配对"),
            ("just text\n", "冒号"),
            ("a: 1\na: 2\n", "重复的键"),
        ] {
            let e = parse(src).unwrap_err();
            let m = format!("{}", e);
            assert!(m.contains(want), "源 {:?} 应报 {:?}，实际 {}", src, want, m);
            assert!(m.contains("第"), "要带行号：{}", m);
        }
    }

    #[test]
    fn empty_and_valueless_keys() {
        assert_eq!(parse("").unwrap(), Node::Map(Vec::new()));
        let n = p("a:\nb: 1\n");
        assert_eq!(n.get("a").unwrap(), &Node::Map(Vec::new()));
        assert_eq!(n.get("b").unwrap().as_str(), Some("1"));
    }

    #[test]
    fn document_separator_is_tolerated_once() {
        assert!(parse("---\na: 1\n").is_ok());
        assert!(parse("a: 1\n---\nb: 2\n").is_err());
    }
}
