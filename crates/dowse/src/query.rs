//! 查询串里的内联操作符解析。
//!
//! 三端（CLI / GUI / MCP）共用同一个查询字符串入口，所以把 `path:`、`mtime:`、
//! `size:`、`OR`、`NOT`/`-` 这些操作符直接做进查询串里，任何一端都零接线自动获益。
//! tantivy 自带的 `QueryParser` 只懂"字段:词"这类文本查询，不认识我们的日期 /
//! 体积 / 路径语义，也没法表达"AND 优先级高于 OR"的分组；因此这里单独放一层
//! **纯字符串解析**：只把查询串拆成结构化的 [`Parsed`]，完全不碰 tantivy、不碰
//! 索引，好离线单测、也好独立演进。真正把结构翻译成 tantivy 查询在 `searcher.rs`。
//!
//! 向后兼容是硬约束：不含任何操作符的查询必须和从前逐字节一致地走老路径。为此
//! [`parse`] 一旦发现整串里没有任何操作符，就把 [`Parsed::has_operators`] 置 false、
//! 不做任何结构化拆分，让调用方把**原始查询串**原样交回给老的 `QueryParser`。
//! 只有确实出现操作符时才启用下面这套分组/过滤逻辑。

use anyhow::{Result, bail};

/// 比较符：日期和体积过滤共用同一组。
///
/// 只支持这四种；`mtime:` / `size:` 后面缺了比较符会直接报错，而不是猜一个默认
/// 语义——过滤条件写错却静默生效，比直接报错危险得多（用户会以为过滤起作用了）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cmp {
    /// `>`：严格大于。
    Gt,
    /// `>=`：大于等于。
    Ge,
    /// `<`：严格小于。
    Lt,
    /// `<=`：小于等于。
    Le,
}

/// 一条 `mtime:` 过滤，已把日期归约成"当天 00:00 的毫秒时间戳"和"次日 00:00 的
/// 毫秒时间戳"两个边界（都按 UTC，跟索引里存的 mtime 口径一致——见 indexer 的
/// `file_stat`，取的是 `SystemTime` 相对 `UNIX_EPOCH` 的毫秒数）。
///
/// 为什么按"整天"而不是"某一瞬间"来比：用户写 `mtime:>2026-01-01` 的直觉是
/// "在这一天之后修改的"，把 1 月 1 号一整天都算进去或排除掉，比纠结当天几点几分
/// 更符合预期。所以：
/// - `>D`  → mtime ≥ 次日 0 点（1 号当天不算，从 2 号起）；
/// - `>=D` → mtime ≥ 当天 0 点（含 1 号当天）；
/// - `<D`  → mtime < 当天 0 点（不含 1 号当天）；
/// - `<=D` → mtime < 次日 0 点（含 1 号一整天）。
///
/// `YYYY-MM` 只精确到月：把"当天"换成"当月 1 号 0 点"、"次日"换成"次月 1 号 0 点"，
/// 语义完全同构。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DateBound {
    /// 比较符。
    pub cmp: Cmp,
    /// 日期粒度起点（当天 / 当月 1 号）的 UTC 毫秒时间戳。
    pub start_ms: i64,
    /// 日期粒度终点（次日 / 次月 1 号）的 UTC 毫秒时间戳。
    pub next_ms: i64,
}

/// 一条 `size:` 过滤，已把 `10mb` / `500kb` 这类写法折算成字节数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SizeBound {
    /// 比较符。
    pub cmp: Cmp,
    /// 折算成字节的阈值。
    pub bytes: u64,
}

/// 一条 `path:` 过滤的操作数。
///
/// `phrase` 记录用户是否给操作数加了引号（`path:"my docs"`）：加了就按整体（相邻
/// 短语）匹配路径，没加就按分词后的词/子词各自匹配。真正建查询时用得到这个区分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathTerm {
    /// 剥掉引号后的路径关键词原文（大小写保留，交给分词器时再统一小写）。
    pub operand: String,
    /// 是否是带引号的整体匹配。
    pub phrase: bool,
}

/// 一个 AND 分组：`OR` 把整串切成若干组，**同一组内**的所有条件是 AND（合取）。
///
/// 这就落实了"AND 优先级高于 OR"——空格分隔的相邻条件先在组内 AND 到一起，
/// `OR` 才把组与组并起来。各类条件分桶存放，建查询时分别翻译成 tantivy 子查询
/// 再按 Must / MustNot 组合。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Group {
    /// 正内容词，**保留引号原样**：直接交给 tantivy 的 `QueryParser`，让它自己
    /// 决定是普通词还是 `"短语"`，不重复造一套短语解析。
    pub content: Vec<String>,
    /// 被排除的内容词（`-词` 或 `NOT 词`），同样保留引号原样交给 `QueryParser`，
    /// 建查询时挂到 `Occur::MustNot`。
    pub excluded: Vec<String>,
    /// `path:` 路径过滤。
    pub paths: Vec<PathTerm>,
    /// `mtime:` 时间过滤。
    pub mtimes: Vec<DateBound>,
    /// `size:` 体积过滤。
    pub sizes: Vec<SizeBound>,
}

impl Group {
    /// 组里一条有效条件都没有（可能是 `OR OR` 之间的空段）——这种空组要丢掉，
    /// 不能让它变成一个"匹配任意文档"的空 BooleanQuery 把结果放宽。
    fn is_empty(&self) -> bool {
        self.content.is_empty()
            && self.excluded.is_empty()
            && self.paths.is_empty()
            && self.mtimes.is_empty()
            && self.sizes.is_empty()
    }
}

/// 一次查询串解析的结果。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// 整串里到底有没有出现操作符。false 时 `groups` 一定为空，调用方应当忽略它、
    /// 走老的"整串原样交给 QueryParser"路径，保证无操作符查询逐字节向后兼容。
    pub has_operators: bool,
    /// OR 分隔出来的各个 AND 组（已剔掉空组）。仅在 `has_operators` 为 true 时有意义。
    pub groups: Vec<Group>,
}

/// 把查询串切成保留引号的顶层 token：空白分词，但一对双引号会把中间的空白保护起来
/// 成为同一个 token（引号本身一并保留，好让上层区分"带引号的整体"和普通词）。
///
/// 这样 `path:"my docs"` 会整体成一个 token（引号在 `path:` 之后才打开，中间空格
/// 被保护），`"unique marker"` 也是一个 token——分组和分类都在这个 token 粒度上做。
fn tokenize(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut has = false;
    for ch in query.chars() {
        if ch == '"' {
            in_quote = !in_quote;
            cur.push(ch);
            has = true;
        } else if ch.is_whitespace() && !in_quote {
            if has {
                out.push(std::mem::take(&mut cur));
                has = false;
            }
        } else {
            cur.push(ch);
            has = true;
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// 把 `字段:剩余` 的 token 拆成（小写字段名, 剩余）。只有冒号前是非空、纯 ASCII
/// 字母时才认作字段前缀——否则像 `12:30`、`http://x` 这种含冒号的普通词会被误判
/// 成字段查询。字段名统一小写，好让 `PATH:`、`Path:` 也能被识别。
fn split_field(token: &str) -> Option<(String, &str)> {
    let colon = token.find(':')?;
    let field = &token[..colon];
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some((field.to_ascii_lowercase(), &token[colon + 1..]))
}

/// 一个 token 是不是操作符（用来判断整串要不要走结构化解析）。
///
/// 注意大小写规则：`OR` / `NOT` **只有大写**才算操作符，小写 `or` / `not` 当普通词
/// （英文文本里 or/not 太常见，大写才当连接词最不容易误伤）。`-词` 的前导减号、
/// 以及 `path:` / `mtime:` / `size:` 三个已知字段前缀也都算操作符。
fn is_operator_token(token: &str) -> bool {
    if token == "OR" || token == "NOT" {
        return true;
    }
    // 前导 `-` 且后面还有内容 → 排除操作符（单独一个 `-` 不算）。
    if token.strip_prefix('-').is_some_and(|rest| !rest.is_empty()) {
        return true;
    }
    matches!(
        split_field(token),
        Some((ref f, _)) if f == "path" || f == "mtime" || f == "size"
    )
}

/// 剥掉操作数两端配对的双引号，返回（内层文本, 是否原本带引号）。
fn unquote(s: &str) -> (String, bool) {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        (s[1..s.len() - 1].to_string(), true)
    } else {
        (s.to_string(), false)
    }
}

/// 解析查询串里的内联操作符。无操作符时返回 `has_operators: false`，让调用方走老路径。
pub(crate) fn parse(query: &str) -> Result<Parsed> {
    let tokens = tokenize(query);
    if !tokens.iter().any(|t| is_operator_token(t)) {
        // 没有任何操作符：不做结构化拆分，交回调用方按老逻辑整串解析，保证向后兼容。
        return Ok(Parsed {
            has_operators: false,
            groups: Vec::new(),
        });
    }

    // 先按大写 OR 把 token 流切成一组组；组内再逐个 token 分类。
    let mut groups_tokens: Vec<Vec<&str>> = vec![Vec::new()];
    for tok in &tokens {
        if tok == "OR" {
            groups_tokens.push(Vec::new());
        } else {
            groups_tokens
                .last_mut()
                .expect("至少有一组")
                .push(tok.as_str());
        }
    }

    let mut groups = Vec::new();
    for gt in groups_tokens {
        let group = classify_group(&gt)?;
        // 空组（例如首尾多余的 OR、或 `OR OR` 之间）直接丢弃，不放宽结果。
        if !group.is_empty() {
            groups.push(group);
        }
    }
    if groups.is_empty() {
        bail!("查询里只有 OR / NOT 之类的连接词，没有实际的检索条件");
    }

    Ok(Parsed {
        has_operators: true,
        groups,
    })
}

/// 把一组（OR 之间的一段）token 分类进 [`Group`] 的各个桶。非法操作数（如
/// `mtime:>abc`）在这里就地报清晰中文错误，不会被静默当成普通词——静默会让用户
/// 误以为过滤生效了。
fn classify_group(tokens: &[&str]) -> Result<Group> {
    let mut g = Group::default();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        i += 1;

        // NOT 词：吃掉后面一个 token 作为排除项（保留引号交给 QueryParser 处理短语）。
        if tok == "NOT" {
            match tokens.get(i) {
                Some(next) => {
                    g.excluded.push((*next).to_string());
                    i += 1;
                }
                None => bail!("NOT 后面缺少要排除的词，例如 `限流 NOT 废弃`"),
            }
            continue;
        }

        // -词：前导减号排除。单独一个 `-` 不是操作数，忽略。
        if let Some(rest) = tok.strip_prefix('-') {
            if !rest.is_empty() {
                g.excluded.push(rest.to_string());
                continue;
            }
        }

        // 已知字段前缀。ext: 等未知字段一律落回内容词，原样交给 QueryParser
        // （tantivy 本来就支持 `字段:词`，`ext:md` 这类既有写法保持不变）。
        if let Some((field, rest)) = split_field(tok) {
            match field.as_str() {
                "path" => {
                    let (operand, phrase) = unquote(rest);
                    if operand.trim().is_empty() {
                        bail!("path: 后面要跟路径关键词，例如 `path:报告` 或 `path:\"我的 文档\"`");
                    }
                    g.paths.push(PathTerm { operand, phrase });
                    continue;
                }
                "mtime" => {
                    g.mtimes.push(parse_date_bound(rest)?);
                    continue;
                }
                "size" => {
                    g.sizes.push(parse_size_bound(rest)?);
                    continue;
                }
                _ => {}
            }
        }

        // 普通内容词（含 `"短语"`）：原样保留，交给 tantivy 的 QueryParser。
        g.content.push(tok.to_string());
    }
    Ok(g)
}

/// 从 `>=2026-01-01` 这类操作数里切出比较符和后面的实体，比较符缺失直接报错。
/// 注意 `>=` / `<=` 要在 `>` / `<` 之前判，否则会把两字符符号错切成单字符。
fn split_cmp(operand: &str) -> Result<(Cmp, &str)> {
    if let Some(rest) = operand.strip_prefix(">=") {
        Ok((Cmp::Ge, rest))
    } else if let Some(rest) = operand.strip_prefix("<=") {
        Ok((Cmp::Le, rest))
    } else if let Some(rest) = operand.strip_prefix('>') {
        Ok((Cmp::Gt, rest))
    } else if let Some(rest) = operand.strip_prefix('<') {
        Ok((Cmp::Lt, rest))
    } else {
        bail!(
            "过滤条件 \"{operand}\" 缺少比较符，要写成 >、>=、< 或 <=，\
             例如 mtime:>2026-01-01 或 size:<500kb"
        )
    }
}

/// 解析 `mtime:` 操作数，支持 `YYYY-MM-DD` 和 `YYYY-MM` 两种日期粒度。
fn parse_date_bound(operand: &str) -> Result<DateBound> {
    let (cmp, date_str) = split_cmp(operand)?;
    let parts: Vec<&str> = date_str.split('-').collect();
    let bad = || format!("日期 \"{date_str}\" 格式不对，要写成 YYYY-MM-DD 或 YYYY-MM");

    let parse_num =
        |s: &str| -> Result<i64> { s.parse::<i64>().map_err(|_| anyhow::anyhow!(bad())) };

    let (year, month, day, has_day) = match parts.as_slice() {
        [y, m, d] => (parse_num(y)?, parse_num(m)?, parse_num(d)?, true),
        [y, m] => (parse_num(y)?, parse_num(m)?, 1, false),
        _ => bail!(bad()),
    };
    if !(1..=12).contains(&month) {
        bail!("日期 \"{date_str}\" 的月份要在 1..=12 之间");
    }
    let dim = days_in_month(year, month as u32);
    if has_day && !(1..=dim as i64).contains(&day) {
        bail!("日期 \"{date_str}\" 的日要在 1..={dim} 之间");
    }

    let start_ms = date_to_ms(year, month as u32, day as u32);
    // 同一天/同一月的"下一格"起点：有日就 +1 天（UTC 下一天恒为 86_400_000 毫秒，
    // 没有夏令时/闰秒的坑），没日就跳到次月 1 号。
    let next_ms = if has_day {
        start_ms + MS_PER_DAY
    } else {
        let (ny, nm) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month as u32 + 1)
        };
        date_to_ms(ny, nm, 1)
    };
    Ok(DateBound {
        cmp,
        start_ms,
        next_ms,
    })
}

/// 解析 `size:` 操作数：数字 + 可选单位（kb/mb/gb，大小写不敏感；无单位或 b 按字节）。
fn parse_size_bound(operand: &str) -> Result<SizeBound> {
    let (cmp, rest) = split_cmp(operand)?;
    let rest = rest.trim();
    if rest.is_empty() {
        bail!("size: 后面要跟体积，例如 size:>10mb 或 size:<500kb");
    }

    // 数字部分：数字和小数点连续段；剩下的当单位。允许 `1.5mb` 这种小数写法。
    let split_at = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let (num_str, unit_str) = rest.split_at(split_at);
    let num: f64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("体积 \"{rest}\" 里的数字部分无法解析"))?;
    if num < 0.0 {
        bail!("体积不能是负数：\"{rest}\"");
    }

    let multiplier: f64 = match unit_str.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        other => bail!("不认识的体积单位 \"{other}\"，只支持 kb / mb / gb（或省略按字节）"),
    };

    let bytes = (num * multiplier).round();
    // f64 乘出来可能超出 u64（比如 `size:>99999999gb`）；夹到 u64::MAX 而不是溢出 panic。
    let bytes = if bytes >= u64::MAX as f64 {
        u64::MAX
    } else {
        bytes as u64
    };
    Ok(SizeBound { cmp, bytes })
}

/// 一天的毫秒数（UTC，无夏令时/闰秒）。
const MS_PER_DAY: i64 = 86_400_000;

/// 某年某月的天数（处理闰年二月）。
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// 公历闰年判定。
fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// 把公历 (年,月,日) 折算成 UTC 0 点相对 1970-01-01 的毫秒时间戳，跟索引里 mtime
/// 的口径一致。用 Howard Hinnant 的 days_from_civil 算法直接算"距纪元的天数"，
/// 不引入 chrono/time 依赖——只做日期到时间戳的定点换算，一个纯算术函数足够，
/// 没必要为此多背一个日期库。
fn date_to_ms(year: i64, month: u32, day: u32) -> i64 {
    let m = month as i64;
    let d = day as i64;
    // 把 1、2 月算作上一年的 13、14 月，好让 2 月的闰日落在"年末"，公式更规整。
    let y = if m <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146097 + doe - 719468; // 距 1970-01-01 的天数
    days * MS_PER_DAY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_operator_query_signals_passthrough() {
        // 纯词、短语、ext: 都不算我们的操作符 → has_operators=false，走老路径。
        for q in ["限流 中间件", "\"unique marker\"", "分布式", "ext:md 限流"] {
            let parsed = parse(q).unwrap();
            assert!(!parsed.has_operators, "查询 {q:?} 不该被判成含操作符");
            assert!(parsed.groups.is_empty());
        }
    }

    #[test]
    fn lowercase_or_and_not_are_plain_words() {
        // 小写 or/not 是普通英文词，不当连接/排除操作符。
        let parsed = parse("cats or dogs").unwrap();
        assert!(!parsed.has_operators);
        let parsed = parse("shall not pass").unwrap();
        assert!(!parsed.has_operators);
    }

    #[test]
    fn date_to_ms_epoch_and_known_dates() {
        assert_eq!(date_to_ms(1970, 1, 1), 0);
        assert_eq!(date_to_ms(1970, 1, 2), MS_PER_DAY);
        // 2000-01-01 距纪元 10957 天（含 8 个闰年：72,76,...,96 共 8 个 + 无 1900）。
        assert_eq!(date_to_ms(2000, 1, 1), 10957 * MS_PER_DAY);
    }

    #[test]
    fn parse_mtime_day_bounds() {
        let b = parse_date_bound(">2026-01-01").unwrap();
        assert_eq!(b.cmp, Cmp::Gt);
        assert_eq!(b.start_ms, date_to_ms(2026, 1, 1));
        assert_eq!(b.next_ms, date_to_ms(2026, 1, 2));

        let b = parse_date_bound(">=2026-07").unwrap();
        assert_eq!(b.cmp, Cmp::Ge);
        assert_eq!(b.start_ms, date_to_ms(2026, 7, 1));
        // YYYY-MM 的 next 是次月 1 号。
        assert_eq!(b.next_ms, date_to_ms(2026, 8, 1));

        // 12 月的次月要跨年。
        let b = parse_date_bound("<2026-12").unwrap();
        assert_eq!(b.next_ms, date_to_ms(2027, 1, 1));
    }

    #[test]
    fn parse_mtime_rejects_garbage() {
        assert!(parse_date_bound(">abc").is_err());
        assert!(parse_date_bound(">2026-13-01").is_err(), "月份越界应报错");
        assert!(parse_date_bound(">2026-02-30").is_err(), "2 月没有 30 号");
        assert!(parse_date_bound("2026-01-01").is_err(), "缺比较符应报错");
    }

    #[test]
    fn parse_size_units_case_insensitive() {
        assert_eq!(parse_size_bound(">10mb").unwrap().bytes, 10 * 1024 * 1024);
        assert_eq!(parse_size_bound("<500KB").unwrap().bytes, 500 * 1024);
        assert_eq!(parse_size_bound(">=1Gb").unwrap().bytes, 1024 * 1024 * 1024);
        // 无单位按字节。
        assert_eq!(parse_size_bound(">1024").unwrap().bytes, 1024);
        // 小数写法。
        assert_eq!(
            parse_size_bound(">1.5mb").unwrap().bytes,
            (1.5 * 1024.0 * 1024.0) as u64
        );
    }

    #[test]
    fn parse_size_rejects_garbage() {
        assert!(parse_size_bound(">abc").is_err());
        assert!(parse_size_bound(">10tb").is_err(), "不支持的单位应报错");
        assert!(parse_size_bound("10mb").is_err(), "缺比较符应报错");
    }

    #[test]
    fn or_splits_into_groups_and_space_is_and_within_group() {
        let parsed = parse("限流 中间件 OR 熔断").unwrap();
        assert!(parsed.has_operators);
        assert_eq!(parsed.groups.len(), 2, "OR 应切成两组");
        assert_eq!(parsed.groups[0].content, vec!["限流", "中间件"]);
        assert_eq!(parsed.groups[1].content, vec!["熔断"]);
    }

    #[test]
    fn exclusion_via_dash_and_not() {
        let parsed = parse("限流 -废弃").unwrap();
        assert_eq!(parsed.groups[0].content, vec!["限流"]);
        assert_eq!(parsed.groups[0].excluded, vec!["废弃"]);

        let parsed = parse("限流 NOT 废弃").unwrap();
        assert_eq!(parsed.groups[0].content, vec!["限流"]);
        assert_eq!(parsed.groups[0].excluded, vec!["废弃"]);
    }

    #[test]
    fn path_operand_quoted_vs_bare() {
        let parsed = parse("path:报告").unwrap();
        assert_eq!(
            parsed.groups[0].paths,
            vec![PathTerm {
                operand: "报告".to_string(),
                phrase: false
            }]
        );

        let parsed = parse("path:\"my docs\"").unwrap();
        assert_eq!(
            parsed.groups[0].paths,
            vec![PathTerm {
                operand: "my docs".to_string(),
                phrase: true
            }]
        );
    }

    #[test]
    fn combined_operators_in_one_group() {
        let parsed = parse("限流 path:src mtime:>2026-01-01 size:<1mb -草稿").unwrap();
        let g = &parsed.groups[0];
        assert_eq!(g.content, vec!["限流"]);
        assert_eq!(g.excluded, vec!["草稿"]);
        assert_eq!(g.paths.len(), 1);
        assert_eq!(g.mtimes.len(), 1);
        assert_eq!(g.sizes.len(), 1);
    }

    #[test]
    fn ext_prefix_stays_content_for_backward_compat() {
        // ext: 不是我们接管的字段，但和别的操作符同现时要原样留在内容词里，
        // 交给 tantivy QueryParser（它本就支持 ext:md）。
        let parsed = parse("ext:md -草稿").unwrap();
        assert!(parsed.has_operators);
        assert_eq!(parsed.groups[0].content, vec!["ext:md"]);
        assert_eq!(parsed.groups[0].excluded, vec!["草稿"]);
    }

    #[test]
    fn empty_operand_errors() {
        assert!(parse("path:").is_err(), "path: 空操作数应报错");
        assert!(parse("path:\"\"").is_err(), "path 引号里为空应报错");
    }

    #[test]
    fn only_connectives_errors() {
        // 只有 NOT 缺被排除词、或只有 OR，都不是有效查询。
        assert!(parse("NOT").is_err());
    }
}
