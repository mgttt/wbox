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

/// 取"一个或多个容器名"（`start` / `rm` / `wait` 这类）：拒绝选项，缺失时报错。
///
/// 这个形状原本在四五个子命令里各写了一遍，措辞各不相同——同一件事
/// （"你少给了容器名"）用户会看到好几种说法。收到一处后措辞统一，
/// 而且以后要加"名字里不许有路径分隔符"之类的校验只需改一个地方。
pub fn take_container_names<'a>(args: &'a [String], verb: &str) -> Result<Vec<&'a str>> {
    let mut names = Vec::new();
    for a in args {
        if a.starts_with('-') {
            return Err(WboxError::args(format!(
                "{}: 当前不支持参数 '{}'（用法：wbox {} <NAME>...）",
                verb, a, verb
            )));
        }
        names.push(a.as_str());
    }
    if names.is_empty() {
        return Err(WboxError::args(format!(
            "{}: 缺少容器名（用法：wbox {} <NAME>...）",
            verb, verb
        )));
    }
    Ok(names)
}

/// 成功时要不要把名字回显出来。
///
/// `rm`/`restart` 回显（脚本靠它确认动了哪些）；`compose down` 不回显，
/// 它在整轮结束时给一句总结。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Echo {
    Name,
    Nothing,
}

/// 对一批名字逐个执行，**一个失败不中断后面的**。
///
/// 这条取舍原本在 `rm`/`prune`/`restart`/`compose down` 里各实现了一遍。
/// 之所以四处都选了"不中断"：中断会让用户以为一个都没成功，而实际上前面几个
/// 已经做完了——那是最容易导致重复操作或漏操作的一种误导。
///
/// 全部成功返回 0；有失败则逐条打到 stderr，最后用一个汇总错误收口，
/// 让退出码如实反映"没全成"。
pub fn each_named<F>(names: &[String], verb: &str, echo: Echo, mut f: F) -> Result<u32>
where
    F: FnMut(&str) -> Result<()>,
{
    let mut failed = 0usize;
    // 只给了一个名字时，把**原始错误**原样返回，不套一层"1 个容器未成功"的汇总。
    // 汇总只有在"有些成功有些失败"时才有信息量；对单个容器它纯属噪音，
    // 而且会把真正的原因（为什么失败）挤到 stderr 上另起一行——用户看到的
    // 主错误反而什么都没说。
    let single = names.len() == 1;
    let mut only_err = None;
    for name in names {
        match f(name) {
            Ok(()) => {
                if echo == Echo::Name {
                    println!("{}", name);
                }
            }
            Err(e) => {
                failed += 1;
                if single {
                    only_err = Some(e);
                } else {
                    eprintln!("wbox: {} '{}' 失败：{}", verb, name, e);
                }
            }
        }
    }
    if let Some(e) = only_err {
        return Err(e);
    }
    if failed > 0 {
        return Err(WboxError::args(format!(
            "{}失败：{} 个容器未成功（共 {} 个，详见上面逐条说明）",
            verb,
            failed,
            names.len()
        )));
    }
    Ok(0)
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
    fn take_container_names_rejects_flags_and_empty() {
        assert_eq!(take_container_names(&v(&["a", "b"]), "rm").unwrap(), vec!["a", "b"]);
        let e = format!("{}", take_container_names(&v(&[]), "rm").unwrap_err());
        assert!(e.contains("wbox rm"), "用法提示要带上动词：{}", e);
        assert!(take_container_names(&v(&["-f", "a"]), "rm").is_err());
    }

    /// 一个失败不该中断后面的：中断会让用户以为一个都没成功，
    /// 而实际上前面几个已经做完了。
    #[test]
    fn each_named_continues_past_failures_and_reports_the_tally() {
        let names = v(&["ok1", "bad", "ok2"]);
        let mut seen = Vec::new();
        let r = each_named(&names, "删除", Echo::Nothing, |n| {
            seen.push(n.to_string());
            if n == "bad" {
                Err(WboxError::args("boom"))
            } else {
                Ok(())
            }
        });
        assert_eq!(seen, vec!["ok1", "bad", "ok2"], "失败之后仍要继续走完");
        let m = format!("{}", r.unwrap_err());
        assert!(m.contains('1') && m.contains('3'), "要汇总几个失败/共几个：{}", m);

        // 全成功 → 0
        assert_eq!(
            each_named(&v(&["a"]), "删除", Echo::Nothing, |_| Ok(())).unwrap(),
            0
        );
    }

    /// 只给一个名字时要把**原始错误**原样返回。套一层"1 个容器未成功"的汇总，
    /// 等于把真正的原因挤到 stderr 上，用户看到的主错误什么都没说。
    #[test]
    fn a_single_name_surfaces_the_real_reason_not_a_tally() {
        let r = each_named(&v(&["only"]), "删除", Echo::Nothing, |_| {
            Err(WboxError::args("因为它还在运行"))
        });
        let m = format!("{}", r.unwrap_err());
        assert!(m.contains("因为它还在运行"), "要给出真正的原因：{}", m);
        assert!(!m.contains("共 1 个"), "单个容器不该套汇总：{}", m);
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
