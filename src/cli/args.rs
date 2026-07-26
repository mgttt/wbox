//! 各子命令共享的解析原语（取选项值 / 数值解析 / 单位置参数）。

use crate::error::{Result, WboxError};

/// 取 `--opt <value>` 形式的值；i 指向选项本身，成功后移动到值。
pub fn take_value(args: &[String], i: &mut usize, opt: &str) -> Result<String> {
    if *i + 1 >= args.len() {
        return Err(WboxError::args(format!("选项 '{}' 缺少参数值", opt)));
    }
    *i += 1;
    Ok(args[*i].clone())
}

/// 解析非负整数选项值（u64），错误信息统一为 `'<opt>' 需为非负整数（<单位>），得到 '<值>'`。
pub fn parse_u64(opt: &str, v: &str, unit: &str) -> Result<u64> {
    v.parse::<u64>()
        .map_err(|_| WboxError::args(format!("{} 需为非负整数（{}），得到 '{}'", opt, unit, v)))
}

/// 解析非负整数选项值（u32）。
pub fn parse_u32(opt: &str, v: &str) -> Result<u32> {
    v.parse::<u32>()
        .map_err(|_| WboxError::args(format!("{} 需为非负整数，得到 '{}'", opt, v)))
}

/// 取"恰好一个位置参数"（如 `image show <REF>` / `image rm <REF>`）：
/// 拒绝选项、拒绝多余参数，缺失时报 `missing_hint`。
pub fn take_single_positional(args: &[String], missing_hint: &str) -> Result<String> {
    let mut positional: Option<String> = None;
    for a in args {
        if a.starts_with('-') {
            return Err(WboxError::args(format!("未知选项 '{}'", a)));
        }
        if positional.is_some() {
            return Err(WboxError::args(format!("多余的参数 '{}'", a)));
        }
        positional = Some(a.clone());
    }
    positional.ok_or_else(|| WboxError::args(missing_hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn take_value_moves_index_and_errors_on_missing() {
        let a = v(&["--os", "linux"]);
        let mut i = 0;
        assert_eq!(take_value(&a, &mut i, "--os").unwrap(), "linux");
        assert_eq!(i, 1);
        let a = v(&["--os"]);
        let mut i = 0;
        assert!(take_value(&a, &mut i, "--os").is_err());
    }

    #[test]
    fn numeric_parsers_report_opt_and_value() {
        assert_eq!(parse_u64("--memory", "256", "MB").unwrap(), 256);
        let e = parse_u64("--memory", "-1", "MB").unwrap_err();
        let s = format!("{}", e);
        assert!(s.contains("--memory") && s.contains("-1"), "{}", s);
        assert!(parse_u32("--max-procs", "abc").is_err());
        assert_eq!(parse_u32("--max-procs", "0").unwrap(), 0);
    }

    #[test]
    fn take_single_positional_rules() {
        assert_eq!(
            take_single_positional(&v(&["ubuntu:24.04"]), "缺引用").unwrap(),
            "ubuntu:24.04"
        );
        assert!(take_single_positional(&v(&[]), "缺引用").is_err());
        assert!(take_single_positional(&v(&["a", "b"]), "缺引用").is_err());
        assert!(take_single_positional(&v(&["-V"]), "缺引用").is_err());
    }
}
