//! 内容分词器：把文本按"汉字 / 非汉字"切成一段段，汉字段交给 jieba 继续
//! 按中文习惯切词，非汉字段（拉丁字母、数字、标点、空白等）按字母数字连续
//! 段切成词；两侧产出的 token 一律小写归一。
//!
//! 为什么要在 jieba 外面套这层：`tantivy_jieba` 直接把整串喂给 jieba 的
//! 中文 HMM 统计分词，遇到英文/连字符串（如 `glimmerfrost-9931-unique-marker`）
//! 会按中文习惯乱切，而且"整篇一起切"和"单独查询词切"两次结果不一致，
//! 导致子词搜不到。这里先把汉字和非汉字分开：只有真正的汉字才进 jieba，
//! 其余走确定性的字母数字切分，两侧口径一致，`api` 能搜到 `API`、
//! `covid` 能搜到 `covid-19`。
//!
//! offset 语义是硬约束：每个 token 的 `offset_from`/`offset_to` 必须是**原始**
//! 输入串里的精确字节下标——搜索侧的摘要/高亮会拿这些区间去切原文，错一个
//! 字节就是切串 panic，不只是搜错。所以汉字段喂给 jieba 后要把它给出的
//! 段内 offset 加回该段在原串里的字节基址。

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// 判断一个字符是否属于要交给 jieba 的汉字区间：
/// CJK 统一表意文字（U+4E00–9FFF）和扩展 A（U+3400–4DBF）。
/// CJK 标点（U+3000–303F）、全角形（U+FF00–FFEF）等**不**算汉字，
/// 落进非汉字段，跟拉丁/数字/标点一样处理。
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

/// 混合分词器：汉字段走 jieba，非汉字段按字母数字切，全部小写归一。
#[derive(Clone)]
pub(crate) struct MixedTokenizer {
    jieba: tantivy_jieba::JiebaTokenizer,
}

impl MixedTokenizer {
    pub(crate) fn new() -> Self {
        Self {
            jieba: tantivy_jieba::JiebaTokenizer::new(),
        }
    }

    /// 汉字段：喂给 jieba，把段内 offset 加上基址后原样收下（保持 jieba 的
    /// 中文切词质量不变），position 用外部的顺序计数器覆盖。
    fn emit_cjk(&mut self, run: &str, base: usize, out: &mut Vec<Token>, position: &mut usize) {
        let mut stream = self.jieba.token_stream(run);
        while stream.advance() {
            let tok = stream.token();
            out.push(Token {
                offset_from: base + tok.offset_from,
                offset_to: base + tok.offset_to,
                position: *position,
                text: tok.text.to_lowercase(),
                position_length: 1,
            });
            *position += 1;
        }
    }

    /// 非汉字段：每一段极大连续的 `char::is_alphanumeric()` 字符成一个词
    /// （对齐 tantivy `SimpleTokenizer` 的口径），连字符/点/空白/ASCII 标点
    /// 都是分隔符、不产 token。offset 是 `base + 段内字节下标`。
    fn emit_non_cjk(run: &str, base: usize, out: &mut Vec<Token>, position: &mut usize) {
        let mut span_start: Option<usize> = None;
        for (i, ch) in run.char_indices() {
            if ch.is_alphanumeric() {
                if span_start.is_none() {
                    span_start = Some(i);
                }
            } else if let Some(start) = span_start.take() {
                out.push(Token {
                    offset_from: base + start,
                    offset_to: base + i,
                    position: *position,
                    text: run[start..i].to_lowercase(),
                    position_length: 1,
                });
                *position += 1;
            }
        }
        if let Some(start) = span_start {
            out.push(Token {
                offset_from: base + start,
                offset_to: base + run.len(),
                position: *position,
                text: run[start..].to_lowercase(),
                position_length: 1,
            });
            *position += 1;
        }
    }
}

impl Tokenizer for MixedTokenizer {
    type TokenStream<'a> = VecTokenStream;

    fn token_stream(&mut self, text: &str) -> VecTokenStream {
        let mut tokens: Vec<Token> = Vec::new();
        let mut position: usize = 0;

        // 单趟扫描：把原串切成一段段极大的"同类"run（同为汉字或同为非汉字），
        // 记住每段在原串里的字节基址，逐段产 token。position 是贯穿全程的
        // 顺序计数器，每产一个 token +1，跟 run 类型/跨了几个字无关——tantivy
        // 的短语查询要求相邻词位置差 1，不能用 jieba 的段内字符偏移。
        let mut run_start = 0usize;
        let mut run_is_cjk: Option<bool> = None;
        for (idx, ch) in text.char_indices() {
            let cjk = is_cjk(ch);
            match run_is_cjk {
                None => run_is_cjk = Some(cjk),
                Some(prev) if prev != cjk => {
                    let run = &text[run_start..idx];
                    if prev {
                        self.emit_cjk(run, run_start, &mut tokens, &mut position);
                    } else {
                        Self::emit_non_cjk(run, run_start, &mut tokens, &mut position);
                    }
                    run_start = idx;
                    run_is_cjk = Some(cjk);
                }
                _ => {}
            }
        }
        if let Some(prev) = run_is_cjk {
            let run = &text[run_start..];
            if prev {
                self.emit_cjk(run, run_start, &mut tokens, &mut position);
            } else {
                Self::emit_non_cjk(run, run_start, &mut tokens, &mut position);
            }
        }

        VecTokenStream { tokens, index: 0 }
    }
}

/// 把整段分词结果先算成一个拥有所有权的 `Vec<Token>`，再按下标顺序吐出来。
/// 这样就不用把 jieba 借用原串的零拷贝 token 穿过自己的生命周期，简单稳妥。
pub(crate) struct VecTokenStream {
    tokens: Vec<Token>,
    index: usize,
}

impl TokenStream for VecTokenStream {
    fn advance(&mut self) -> bool {
        if self.index < self.tokens.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.index - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把分词结果收成 Vec 方便断言。
    fn collect(text: &str) -> Vec<Token> {
        let mut tokenizer = MixedTokenizer::new();
        let mut stream = tokenizer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().clone());
        }
        out
    }

    #[test]
    fn cjk_then_latin_offsets_are_byte_based_not_char_based() {
        // "报告" 两个汉字在 UTF-8 里各占 3 字节，后面的拉丁段基址必须落在
        // 字节 6 之后，而不是字符数 2 之后——这是最容易把字节/字符搞混的地方。
        let text = "报告api";
        let tokens = collect(text);

        // 非汉字段 "api" 的 offset 必须能切回原串的那三个字节。
        let api = tokens
            .iter()
            .find(|t| t.text == "api")
            .expect("应该切出 api");
        assert_eq!(
            api.offset_from,
            "报告".len(),
            "拉丁段基址应在汉字段字节数之后"
        );
        assert_eq!(api.offset_to, text.len());
        assert_eq!(&text[api.offset_from..api.offset_to], "api");

        // position 是顺序递增的，不是 jieba 的段内字符偏移。
        for (i, t) in tokens.iter().enumerate() {
            assert_eq!(t.position, i, "position 应顺序递增: {tokens:?}");
            assert_eq!(t.position_length, 1);
        }
    }

    #[test]
    fn hyphenated_string_splits_into_subwords_with_exact_offsets() {
        let text = "glimmerfrost-9931-unique-marker";
        let tokens = collect(text);
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["glimmerfrost", "9931", "unique", "marker"]);

        // 每个子词的 offset 都要能精确切回原串对应的子串。
        for t in &tokens {
            assert_eq!(&text[t.offset_from..t.offset_to], t.text);
        }
    }

    #[test]
    fn dotted_version_and_case_folding() {
        assert_eq!(
            collect("v1.2.3")
                .iter()
                .map(|t| t.text.clone())
                .collect::<Vec<_>>(),
            vec!["v1", "2", "3"]
        );
        assert_eq!(
            collect("GPT-4")
                .iter()
                .map(|t| t.text.clone())
                .collect::<Vec<_>>(),
            vec!["gpt", "4"]
        );
    }
}
