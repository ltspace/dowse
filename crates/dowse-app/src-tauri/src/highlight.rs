use std::ops::Range;

use serde::Serialize;

/// 前端渲染高亮的最小单元：一段文本 + 是否命中。
///
/// 设计上刻意不把"字节区间 + 原文"这套契约透传给前端——tantivy 的区间是
/// UTF-8 字节偏移，JS 字符串是 UTF-16，两边偏移换算是个坑（尤其中文超出
/// BMP 或者带 emoji 时更容易错）。这里在 Rust 侧就把文本切好段落，前端
/// 只管按顺序渲染，不用碰任何偏移量。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TextSegment {
    pub text: String,
    pub highlighted: bool,
}

/// 把已排序且互不重叠的字节区间切成 TextSegment 序列。
///
/// 防御性设计：区间越界或没落在 UTF-8 字符边界上就跳过该区间（不 panic），
/// 因为 name 高亮的区间是本模块自己算的（不像 dowse 的 search()
/// 有严格契约保证），大小写转换在极少数 Unicode 场景下可能导致字节长度
/// 变化，宁可漏高亮也不能让浮窗直接崩溃。
pub fn segments_from_ranges(text: &str, ranges: &[Range<usize>]) -> Vec<TextSegment> {
    let mut segments = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut cursor = 0usize;

    for r in ranges {
        if r.start > r.end || r.end > text.len() || r.start < cursor {
            continue;
        }
        if !text.is_char_boundary(r.start) || !text.is_char_boundary(r.end) {
            continue;
        }
        if r.start > cursor {
            segments.push(TextSegment {
                text: text[cursor..r.start].to_string(),
                highlighted: false,
            });
        }
        if r.end > r.start {
            segments.push(TextSegment {
                text: text[r.start..r.end].to_string(),
                highlighted: true,
            });
        }
        cursor = r.end;
    }
    if cursor < text.len() {
        segments.push(TextSegment {
            text: text[cursor..].to_string(),
            highlighted: false,
        });
    }
    if segments.is_empty() {
        segments.push(TextSegment {
            text: text.to_string(),
            highlighted: false,
        });
    }
    segments
}

/// 文件名高亮：对查询词做大小写不敏感的子串匹配。
///
/// 这是展示层的轻量匹配，不是 dowse 的搜索相关性逻辑——文件名字段
/// 走的是 jieba 分词索引，但索引不回传"这个词命中了文件名的哪里"，
/// 而浮窗结果行必须让文件名里的命中词跟内容摘要一样高亮（验收清单第 2 条），
/// 所以在 UI 层用查询词直接对文件名做子串定位，够用且实现成本低。
pub fn highlight_name(name: &str, query_str: &str) -> Vec<TextSegment> {
    let terms: Vec<String> = query_str
        .split_whitespace()
        .map(|t| t.trim_matches('"').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();

    if terms.is_empty() {
        return vec![TextSegment {
            text: name.to_string(),
            highlighted: false,
        }];
    }

    let lower = name.to_lowercase();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for term in &terms {
        if term.is_empty() {
            continue;
        }
        let mut cursor = 0usize;
        while cursor < lower.len() {
            let Some(pos) = lower[cursor..].find(term.as_str()) else {
                break;
            };
            let start = cursor + pos;
            let end = start + term.len();
            ranges.push(start..end);
            cursor = end.max(start + 1);
        }
    }

    if ranges.is_empty() {
        return vec![TextSegment {
            text: name.to_string(),
            highlighted: false,
        }];
    }

    segments_from_ranges(name, &dowse::normalize_ranges(ranges))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_from_ranges_no_highlight() {
        let segs = segments_from_ranges("hello", &[]);
        assert_eq!(
            segs,
            vec![TextSegment {
                text: "hello".into(),
                highlighted: false
            }]
        );
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn segments_from_ranges_middle_highlight() {
        // 每个汉字 3 字节，"对比" 是第 12..18 字节
        let segs = segments_from_ranges("限流方案对比", &[12..18]);
        assert_eq!(
            segs,
            vec![
                TextSegment {
                    text: "限流方案".into(),
                    highlighted: false
                },
                TextSegment {
                    text: "对比".into(),
                    highlighted: true
                },
            ]
        );
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn segments_from_ranges_skips_invalid_range_instead_of_panicking() {
        // 越界区间应该被跳过，而不是让 &text[start..end] panic
        let segs = segments_from_ranges("abc", &[10..20]);
        assert_eq!(
            segs,
            vec![TextSegment {
                text: "abc".into(),
                highlighted: false
            }]
        );
    }

    #[test]
    fn highlight_name_case_insensitive_ascii() {
        let segs = highlight_name("Rate-Limiter.rs", "limiter");
        assert!(
            segs.iter()
                .any(|s| s.highlighted && s.text.eq_ignore_ascii_case("Limiter"))
        );
    }

    #[test]
    fn highlight_name_multi_term_and_chinese() {
        let segs = highlight_name("限流方案对比.md", "限流 对比");
        let highlighted: Vec<&str> = segs
            .iter()
            .filter(|s| s.highlighted)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(highlighted, vec!["限流", "对比"]);
    }

    #[test]
    fn highlight_name_no_match_returns_single_plain_segment() {
        let segs = highlight_name("readme.txt", "限流");
        assert_eq!(
            segs,
            vec![TextSegment {
                text: "readme.txt".into(),
                highlighted: false
            }]
        );
    }
}
