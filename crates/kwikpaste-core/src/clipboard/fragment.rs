//! 文本片段：列表卡片上的快捷信息提取，以及拆词面板的分词与选区拼接（纯逻辑，便于单测）。
//!
//! 卡片快捷信息只从列表摘要（正文开头 [`SUMMARY_MAX_CHARS`] 个字符）里提取，列表查询不必读取
//! 完整正文；预览快捷信息、拆词与最终写回都按完整纯文本计算，前端只传「选了哪一段」。

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use super::ingest::SUMMARY_MAX_CHARS;
use crate::db::models::{ClipboardItem, ClipboardKind, ClipboardSubKind};

/// 列表卡片最多给出的快捷信息数量。
const MAX_QUICK_SNIPPETS: usize = 8;

/// 完整文本预览的快捷信息上限，不受卡片一行的展示数量限制。
const MAX_PREVIEW_SNIPPETS: usize = 32;

/// 拆词面板只处理正文开头这么多字符；再长的文本逐词点选没有意义，还会拖慢渲染。
pub const MAX_SPLIT_CHARS: usize = 2000;

/// 链接里不会出现、却常紧贴在链接前后的字符：空白、汉字与全角标点。
const URL_BODY: &str = r#"[^\s\p{Han}<>"'`，。；：！？、（）【】《》「」“”‘’]"#;

/// 带协议头或 `www.` 开头的链接；尾部标点在 [`trim_url_tail`] 里再修剪。
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)(?:https?|ftp)://{URL_BODY}+|www\.{URL_BODY}+\.{URL_BODY}+"
    ))
    .expect("invalid snippet url regex")
});

static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)+")
        .expect("invalid snippet email regex")
});

/// 需要跨越分隔符整体识别的数字串：中国大陆手机号（可带空格 / 连字符）、日期、时间，
/// 以及用连字符连接的编号（座机、订单号、序列号）。按词切分会把它们拆成几段。
/// 斜杠或连字符连接的月 / 日（`05/28`、`05-28`、`2026-05-28`）先按整段取出，再由
/// [`is_numeric_date`] 决定算日期还是编号。
static JOINED_NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?P<phone>(?:\+?86[\s-]?)?1[3-9][0-9](?:[\s-]?[0-9]{4}){2})",
        r"|(?P<time>[0-9]{2,4}年[0-9]{1,2}月(?:[0-9]{1,2}[日号])?",
        r"|[0-9]{1,2}月[0-9]{1,2}[日号]",
        r"|[0-9]{4}[/.][0-9]{1,2}[/.][0-9]{1,2}",
        r"|[0-9]{1,2}:[0-9]{2}(?::[0-9]{2})?)",
        r"|(?P<month_day>[0-9]{1,2}/[0-9]{1,2})",
        r"|[A-Za-z0-9]+(?:\.[A-Za-z0-9]+)*(?:-[A-Za-z0-9]+(?:\.[A-Za-z0-9]+)*)+",
    ))
    .expect("invalid snippet number regex")
});

/// 用空格分组书写的长号码：会议号 `881 234 5678`、银行卡号 `6222 0212 3456 7890`。
static SPACED_DIGITS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9]{3,4}(?: [0-9]{3,4}){2,}").expect("invalid snippet digits regex")
});

/// 空格分组的数字至少要这么多位才当成一个号码，否则 `100 200 300` 这类并列数字会被连成一个。
const MIN_SPACED_DIGITS: usize = 10;

/// UAX #29 算作词内连接符、在中文文本里却几乎总是分隔符的字符（`SKU:AB123`、`1，2，3`）。
const WORD_BREAKS: &[char] = &[':', '：', '，', '；'];

/// 拆词面板还从下划线处断开（`456_654`）：拆细了照样能拖选连续几个词，按原文取回整段。
const SPLIT_BREAKS: &[char] = &[':', '：', '，', '；', '_'];

/// 快捷信息所属的识别规则，不按片段内容二次猜测类型。
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum SnippetKind {
    Link,
    Email,
    Phone,
    Time,
    Number,
    Custom,
}

/// 列表卡片可单独使用的快捷信息，包含片段原文及其识别类型。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QuickSnippet {
    pub text: String,
    pub kind: SnippetKind,
}

/// 完整文本预览的快捷信息，区间是原始纯文本中的 UTF-8 字节偏移。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreviewSnippet {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub text: String,
    pub kind: SnippetKind,
    pub range: Range<usize>,
}

/// 前端点选后要写回剪贴板的一段文本；只描述取原文的哪一段，文本本身由 Rust 从原文取出。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClipboardFragment {
    /// 列表卡片上的快捷信息，必须原样出现在这条记录里。
    Snippet { text: String },
    /// 拆词面板里选中的词序号，对应 [`split_words`] 的结果。
    Words { indices: Vec<usize> },
    /// 图片文字预览里选中的词序号，对应识别文本的 [`split_words`] 结果。
    ImageWords { indices: Vec<usize> },
    /// 预览译文里选中的词序号，对应当前译文的 [`split_words`] 结果。
    TranslationWords { indices: Vec<usize> },
    /// 图片识别原文的字符区间，按 Unicode 标量计数，包含起点但不包含终点。
    ImageTextRange { start: usize, end: usize },
    /// 图片里二维码的内容（有几个就按行连起来）。
    ImageQrCode,
}

/// 拆词面板里的一个词。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WordToken {
    pub text: String,
    /// 与上一个词之间隔着换行，前端据此另起一行，保留原文的段落结构。
    pub line_break: bool,
    #[serde(skip)]
    range: Range<usize>,
}

/// 一条记录的拆词结果。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WordSplit {
    pub tokens: Vec<WordToken>,
    /// 原文超过 [`MAX_SPLIT_CHARS`]，只拆了开头部分。
    pub truncated: bool,
}

/// 预览面板里可点选的一个词：`(start, end)` 是 UTF-16 码元偏移，即前端字符串下标。
/// 序号与 [`split_words`] 一一对应，选中后按 [`ClipboardFragment::Words`] 粘贴。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct WordSpan(pub u32, pub u32);

/// 文本记录的纯文本：HTML / RTF 取 OS 同时提供的纯文本，其余就是 `content`。
pub fn fragment_source(item: &ClipboardItem) -> &str {
    match item.sub_kind {
        Some(ClipboardSubKind::Html | ClipboardSubKind::Rtf) => {
            item.search_text.as_deref().unwrap_or(&item.content)
        }
        _ => &item.content,
    }
}

/// 列表卡片展示的快捷信息。整条记录本身就是链接 / 邮箱 / 颜色 / 路径时不再拆出片段。
pub fn quick_snippets(
    item: &ClipboardItem,
    rules: &super::rules::CompiledRules,
) -> Vec<QuickSnippet> {
    if item.kind != ClipboardKind::Text {
        return Vec::new();
    }

    let Some(summary) = item.summary.as_deref() else {
        return Vec::new();
    };
    let may_be_cut = summary.chars().count() >= SUMMARY_MAX_CHARS;

    extract_snippets(summary, may_be_cut, MAX_QUICK_SNIPPETS, rules)
        .into_iter()
        .filter(|snippet| can_extract_snippets(item) || snippet.kind == SnippetKind::Custom)
        .map(|snippet| QuickSnippet {
            text: snippet.text,
            kind: snippet.kind,
        })
        .collect()
}

/// 从完整纯文本的开头提取预览快捷信息，区间与 [`fragment_source`] 的字节偏移一致。
pub fn preview_quick_snippets(
    item: &ClipboardItem,
    rules: &super::rules::CompiledRules,
) -> Vec<PreviewSnippet> {
    if item.kind != ClipboardKind::Text {
        return Vec::new();
    }
    let (text, truncated) = take_chars(fragment_source(item), MAX_SPLIT_CHARS);
    extract_snippets(text, truncated, MAX_PREVIEW_SNIPPETS, rules)
        .into_iter()
        .filter(|snippet| can_extract_snippets(item) || snippet.kind == SnippetKind::Custom)
        .collect()
}

/// 独立链接、邮箱、颜色和路径卡片不再重复拆出快捷信息。
fn can_extract_snippets(item: &ClipboardItem) -> bool {
    if item.kind != ClipboardKind::Text {
        return false;
    }

    if matches!(
        item.sub_kind,
        Some(
            ClipboardSubKind::Url
                | ClipboardSubKind::Email
                | ClipboardSubKind::Color
                | ClipboardSubKind::Path
        )
    ) {
        return false;
    }

    true
}

/// 从文本里按出现顺序提取可单独粘贴的信息：链接、邮箱、手机号、日期时间、编号与带数字的词。
///
/// `may_be_cut` 表示文本可能在末尾被截断，贴着末尾的片段可能只是半截，这种片段不给出。
/// 与整段文本相同的片段也不给出，那样直接粘贴整条记录就行。
fn extract_snippets(
    text: &str,
    may_be_cut: bool,
    limit: usize,
    rules: &super::rules::CompiledRules,
) -> Vec<PreviewSnippet> {
    let mut spans: Vec<(Range<usize>, SnippetKind)> = Vec::new();

    let custom: Vec<_> = rules
        .spans(super::rules::RuleAction::Snippet, text)
        .collect();
    for (range, _) in &custom {
        push_span(&mut spans, range.clone(), SnippetKind::Custom);
    }
    for found in URL_RE.find_iter(text) {
        let end = found.start() + trim_url_tail(found.as_str()).len();
        push_span(&mut spans, found.start()..end, SnippetKind::Link);
    }
    for found in EMAIL_RE.find_iter(text) {
        let local = found.as_str();
        let start = found.start() + (local.len() - local.trim_start_matches('.').len());
        push_span(&mut spans, start..found.end(), SnippetKind::Email);
    }
    for captures in JOINED_NUMBER_RE.captures_iter(text) {
        let Some(found) = captures.get(0) else {
            continue;
        };
        if is_embedded_in_word(text, found.range()) || !has_ascii_digit(found.as_str()) {
            continue;
        }
        let kind = if captures.name("phone").is_some() {
            SnippetKind::Phone
        } else if captures.name("time").is_some() || is_numeric_date(found.as_str()) {
            SnippetKind::Time
        } else if captures.name("month_day").is_some() {
            // 不像日期的 `a/b`（分数、页码）照旧按词切分。
            continue;
        } else {
            SnippetKind::Number
        };
        push_span(&mut spans, found.range(), kind);
    }
    for found in SPACED_DIGITS_RE.find_iter(text) {
        let digits = found.as_str().bytes().filter(u8::is_ascii_digit).count();
        if digits < MIN_SPACED_DIGITS || is_embedded_in_word(text, found.range()) {
            continue;
        }
        push_span(&mut spans, found.range(), SnippetKind::Number);
    }
    for (start, word) in word_segments(text, WORD_BREAKS) {
        if is_info_word(word) {
            push_span(&mut spans, start..start + word.len(), SnippetKind::Number);
        }
    }

    spans.sort_by_key(|(span, _)| span.start);

    let whole = text.trim();
    let mut snippets: Vec<PreviewSnippet> = Vec::new();
    for (span, kind) in spans {
        if may_be_cut && span.end == text.len() {
            continue;
        }

        let snippet = &text[span.clone()];
        if snippet == whole || snippets.iter().any(|existing| existing.text == snippet) {
            continue;
        }

        let label = (kind == SnippetKind::Custom)
            .then(|| {
                custom
                    .iter()
                    .find(|(range, _)| *range == span)
                    .map(|(_, name)| (*name).to_owned())
            })
            .flatten();
        snippets.push(PreviewSnippet {
            label,
            text: snippet.to_owned(),
            kind,
            range: span,
        });
        if snippets.len() == limit {
            break;
        }
    }

    snippets
}

/// 记录一段候选区间；与已记录区间重叠时丢弃（先识别的规则优先）。
fn push_span(spans: &mut Vec<(Range<usize>, SnippetKind)>, span: Range<usize>, kind: SnippetKind) {
    if span.is_empty() {
        return;
    }

    let overlaps = spans
        .iter()
        .any(|(existing, _)| span.start < existing.end && existing.start < span.end);
    if !overlaps {
        spans.push((span, kind));
    }
}

/// 链接末尾紧跟的句读和未配对的右括号不属于链接。
fn trim_url_tail(url: &str) -> &str {
    let mut trimmed = url;

    loop {
        let Some(last) = trimmed.chars().last() else {
            return trimmed;
        };
        let unmatched_close = match last {
            ')' => trimmed.matches('(').count() < trimmed.matches(')').count(),
            ']' => trimmed.matches('[').count() < trimmed.matches(']').count(),
            '}' => trimmed.matches('{').count() < trimmed.matches('}').count(),
            _ => false,
        };
        if !(matches!(last, '.' | ',' | ';' | ':' | '!' | '?') || unmatched_close) {
            return trimmed;
        }

        trimmed = &trimmed[..trimmed.len() - last.len_utf8()];
    }
}

/// 匹配结果是否只是更长字母数字串的一部分（如长数字中间恰好像手机号的一段）。
fn is_embedded_in_word(text: &str, span: Range<usize>) -> bool {
    let before = text[..span.start].chars().next_back();
    let after = text[span.end..].chars().next();

    before.is_some_and(|c| c.is_ascii_alphanumeric())
        || after.is_some_and(|c| c.is_ascii_alphanumeric())
}

fn has_ascii_digit(text: &str) -> bool {
    text.bytes().any(|byte| byte.is_ascii_digit())
}

/// 只由数字和 `/` `-` 组成的日期：`5/28`、`05-28`、`2026-05-28`；月、日超出范围的（`29-28`）不算。
/// 月 / 日两段都只有一位时更常是分数或范围（`1/2`、`3-5`），也不算。
fn is_numeric_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split(['/', '-']).collect();
    let (month, day) = match parts[..] {
        [year, month, day] if year.len() == 4 && year.bytes().all(|b| b.is_ascii_digit()) => {
            (month, day)
        }
        [month, day] if month.len() + day.len() >= 3 => (month, day),
        _ => return false,
    };
    let in_range = |part: &str, max: u8| {
        part.len() <= 2
            && part.bytes().all(|b| b.is_ascii_digit())
            && part.parse::<u8>().is_ok_and(|n| (1..=max).contains(&n))
    };

    in_range(month, 12) && in_range(day, 31)
}

/// 含数字的词才像编号、型号、尺寸、金额这类信息；单个字符或只有一位数字的短词（如 `4K`）太常见，跳过。
fn is_info_word(word: &str) -> bool {
    if !word.chars().any(char::is_alphanumeric) {
        return false;
    }

    let digits = word.bytes().filter(u8::is_ascii_digit).count();
    let chars = word.chars().count();

    digits > 0 && chars >= 2 && (digits >= 2 || chars >= 3)
}

/// 整段由同一个 ASCII 标点重复而成时返回该标点。
fn punctuation_run(segment: &str) -> Option<char> {
    let first = segment.chars().next().filter(char::is_ascii_punctuation)?;

    segment.chars().all(|c| c == first).then_some(first)
}

/// 按 UAX #29 切词，再从 `breaks` 里的字符处断开，断开的字符单独成词；
/// 连续相同的 ASCII 标点（`--`、`::`、`...`）并成一个词块。
fn word_segments<'a>(text: &'a str, breaks: &[char]) -> Vec<(usize, &'a str)> {
    let mut segments = Vec::new();

    for (start, segment) in text.split_word_bound_indices() {
        let mut piece_start = 0;
        for (offset, c) in segment.char_indices() {
            if !breaks.contains(&c) {
                continue;
            }

            if offset > piece_start {
                segments.push((start + piece_start, &segment[piece_start..offset]));
            }
            piece_start = offset + c.len_utf8();
            segments.push((start + offset, &segment[offset..piece_start]));
        }

        if piece_start < segment.len() {
            segments.push((start + piece_start, &segment[piece_start..]));
        }
    }

    let mut merged: Vec<(usize, &'a str)> = Vec::with_capacity(segments.len());
    for (start, segment) in segments {
        if let Some((last_start, last)) = merged.last_mut() {
            let repeats = punctuation_run(last).is_some()
                && punctuation_run(last) == punctuation_run(segment)
                && *last_start + last.len() == start;
            if repeats {
                *last = &text[*last_start..start + segment.len()];
                continue;
            }
        }
        merged.push((start, segment));
    }

    merged
}

/// 把文本按词切开：西文单词、数字、编号各自成词，汉字逐字成词，标点单独成词，空白丢弃。
pub fn split_words(text: &str) -> WordSplit {
    let (head, truncated) = take_chars(text, MAX_SPLIT_CHARS);
    let mut tokens: Vec<WordToken> = Vec::new();
    let mut pending_line_break = false;

    for (start, segment) in word_segments(head, SPLIT_BREAKS) {
        if segment.chars().all(char::is_whitespace) {
            pending_line_break |= segment.contains('\n');
            continue;
        }

        tokens.push(WordToken {
            text: segment.to_owned(),
            line_break: pending_line_break && !tokens.is_empty(),
            range: start..start + segment.len(),
        });
        pending_line_break = false;
    }

    WordSplit { tokens, truncated }
}

/// 按 [`split_words`] 切出的词在原文里的 UTF-16 区间，供预览面板在原文上直接点选；
/// 第二项表示原文过长、只切了开头。
pub fn word_spans(text: &str) -> (Vec<WordSpan>, bool) {
    let split = split_words(text);
    let mut spans = Vec::with_capacity(split.tokens.len());
    let mut byte = 0;
    let mut units = 0;

    for token in &split.tokens {
        units += utf16_len(&text[byte..token.range.start]);
        let start = units;
        units += utf16_len(&token.text);
        byte = token.range.end;

        spans.push(WordSpan(start, units));
    }

    (spans, split.truncated)
}

fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

/// 从一条记录里取出前端选中的片段；片段已不在原文里（或序号越界）时返回 `None`。
pub fn resolve_fragment(item: &ClipboardItem, fragment: &ClipboardFragment) -> Option<String> {
    let source = fragment_source(item);

    match fragment {
        ClipboardFragment::Snippet { text } => {
            (!text.is_empty() && source.contains(text.as_str())).then(|| text.clone())
        }
        ClipboardFragment::Words { indices } => select_words(source, indices),
        // 图片识别文本和译文来自派生表、二维码要解码原图，都由 core 的片段加载入口处理。
        ClipboardFragment::ImageWords { .. }
        | ClipboardFragment::TranslationWords { .. }
        | ClipboardFragment::ImageTextRange { .. }
        | ClipboardFragment::ImageQrCode => None,
    }
}

/// 按拆词序号拼出选区：相邻的词按原文连续截取，保留中间的空格与换行；
/// 不相邻的几段之间，只有两侧都是西文字母或数字时才补一个空格，中文等直接相连。
pub(crate) fn select_words(text: &str, indices: &[usize]) -> Option<String> {
    let split = split_words(text);
    let mut sorted = indices.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let last = *sorted.last()?;
    if last >= split.tokens.len() {
        return None;
    }

    let mut selected = String::new();
    let mut run_start = sorted[0];
    let mut run_end = sorted[0];
    for &index in &sorted[1..] {
        if index == run_end + 1 {
            run_end = index;
            continue;
        }

        push_run(&mut selected, text, &split.tokens, run_start, run_end);
        run_start = index;
        run_end = index;
    }
    push_run(&mut selected, text, &split.tokens, run_start, run_end);

    Some(selected)
}

fn push_run(selected: &mut String, text: &str, tokens: &[WordToken], first: usize, last: usize) {
    let run = &text[tokens[first].range.start..tokens[last].range.end];
    let needs_space = selected
        .chars()
        .next_back()
        .is_some_and(is_spaced_word_char)
        && run.chars().next().is_some_and(is_spaced_word_char);

    if needs_space {
        selected.push(' ');
    }
    selected.push_str(run);
}

/// 靠空格分词的文字里的字母或数字；中日文、泰文不以空格分词，不算在内。
fn is_spaced_word_char(c: char) -> bool {
    c.is_alphanumeric() && !is_unspaced_script(c)
}

fn is_unspaced_script(c: char) -> bool {
    matches!(
        u32::from(c),
        0x0E00..=0x0E7F // 泰文
            | 0x3040..=0x30FF // 平假名、片假名
            | 0x3400..=0x4DBF // CJK 扩展 A
            | 0x4E00..=0x9FFF // CJK 统一表意文字
            | 0xF900..=0xFAFF // CJK 兼容表意文字
            | 0x20000..=0x3134F // CJK 扩展 B–G
    )
}

/// 取文本开头最多 `max` 个字符，并返回是否截断。
fn take_chars(text: &str, max: usize) -> (&str, bool) {
    match text.char_indices().nth(max) {
        Some((end, _)) => (&text[..end], true),
        None => (text, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presenter::tests::text_item;

    #[test]
    fn custom_snippets_win_over_builtin_overlaps_and_keep_limits() {
        use super::super::rules::{tests::rule, CompiledRules, RuleAction};
        let rules = CompiledRules::new(&[rule("ABC-[0-9]+|[0-9]{11}", RuleAction::Snippet)]);
        let text = "工单ABC-12345，电话13800138000，时间14:00。";
        let mut item = text_item(None, false);
        item.content = text.into();
        item.summary = Some(text.into());
        let snippets = preview_quick_snippets(&item, &rules);
        assert_eq!(
            snippets
                .iter()
                .filter(|s| s.kind == SnippetKind::Custom)
                .count(),
            2
        );
        assert_eq!(snippets[0].label.as_deref(), Some("Example"));
        assert_eq!(&text[snippets[0].range.clone()], "ABC-12345");
        assert!(!snippets.iter().any(|s| s.kind == SnippetKind::Phone));
        assert_eq!(quick_snippets(&item, &rules).len(), snippets.len());
        assert!(extract_snippets("前ABC-12345", true, 8, &rules).is_empty());
        assert!(extract_snippets("ABC-12345", false, 8, &rules).is_empty());
        let many = (0..40).map(|i| format!("ABC-{i:05} ")).collect::<String>();
        assert_eq!(extract_snippets(&many, false, 8, &rules).len(), 8);
        assert_eq!(extract_snippets(&many, false, 32, &rules).len(), 32);
    }

    #[test]
    fn issue_snippets_keep_their_kinds_and_source_byte_ranges() {
        let text = "王工，快递单号 SF1234567890123 明天到，收货电话 138 0013 8000；发票抬头发到你邮箱 wang@example.com 了。周四 14:00 前确认一下，新版在 https://paste.fastthree.com 。";
        let mut item = text_item(None, false);
        item.content = text.to_owned();
        item.summary = Some(text.to_owned());
        let snippets =
            preview_quick_snippets(&item, &super::super::rules::CompiledRules::default());
        let expected = [
            ("SF1234567890123", SnippetKind::Number),
            ("138 0013 8000", SnippetKind::Phone),
            ("wang@example.com", SnippetKind::Email),
            ("14:00", SnippetKind::Time),
            ("https://paste.fastthree.com", SnippetKind::Link),
        ];

        assert_eq!(snippets.len(), expected.len());
        for (snippet, (value, kind)) in snippets.iter().zip(expected) {
            let start = text.find(value).unwrap();
            assert_eq!(snippet.text, value);
            assert_eq!(snippet.kind, kind);
            assert_eq!(snippet.range, start..start + value.len());
            assert_eq!(text.get(snippet.range.clone()), Some(value));
        }
        let card = quick_snippets(&item, &super::super::rules::CompiledRules::default());
        assert_eq!(card.len(), snippets.len());
        for (card, preview) in card.iter().zip(&snippets) {
            assert_eq!((&card.text, card.kind), (&preview.text, preview.kind));
        }
    }

    #[test]
    fn kinds_follow_the_matching_number_alternative() {
        let text = "电话 +86-138-1234-5678 日期 2026年9月24日 9月24号 2026/9/24 2026.9.24 时间 14:30:05 编号 010-12345678 SF-2026-0924；会议号 881 234 5678；型号 9140BT";
        let snippets = extract_snippets(
            text,
            false,
            MAX_PREVIEW_SNIPPETS,
            &super::super::rules::CompiledRules::default(),
        );
        assert_eq!(
            snippets
                .iter()
                .map(|snippet| snippet.kind)
                .collect::<Vec<_>>(),
            [
                SnippetKind::Phone,
                SnippetKind::Time,
                SnippetKind::Time,
                SnippetKind::Time,
                SnippetKind::Time,
                SnippetKind::Time,
                SnippetKind::Number,
                SnippetKind::Number,
                SnippetKind::Number,
                SnippetKind::Number
            ]
        );
        for snippet in snippets {
            assert_eq!(text.get(snippet.range), Some(snippet.text.as_str()));
        }
    }

    #[test]
    fn month_day_dates_stay_whole() {
        let text = "2026年10月18日 05/28 29-28 29、21 2026/01/02 21+23 2026-05-28 12-31 3-5 1/2";
        let snippets = extract_snippets(
            text,
            false,
            MAX_PREVIEW_SNIPPETS,
            &super::super::rules::CompiledRules::default(),
        );
        assert_eq!(
            snippets
                .iter()
                .map(|snippet| (snippet.text.as_str(), snippet.kind))
                .collect::<Vec<_>>(),
            [
                ("2026年10月18日", SnippetKind::Time),
                ("05/28", SnippetKind::Time),
                ("29-28", SnippetKind::Number),
                ("29", SnippetKind::Number),
                ("21", SnippetKind::Number),
                ("2026/01/02", SnippetKind::Time),
                ("23", SnippetKind::Number),
                ("2026-05-28", SnippetKind::Time),
                ("12-31", SnippetKind::Time),
                ("3-5", SnippetKind::Number),
            ]
        );
    }

    #[test]
    fn preview_uses_full_plain_text_with_a_separate_snippet_cap() {
        let mut item = text_item(Some(ClipboardSubKind::Html), false);
        item.content = "<b>not the rendered text</b>".to_owned();
        item.summary = Some("摘要里没有编号".to_owned());
        let plain = (10..50)
            .map(|n| format!("编号 {n}"))
            .collect::<Vec<_>>()
            .join(" ");
        item.search_text = Some(plain.clone());

        assert!(quick_snippets(&item, &super::super::rules::CompiledRules::default()).is_empty());
        let snippets =
            preview_quick_snippets(&item, &super::super::rules::CompiledRules::default());
        assert_eq!(snippets.len(), MAX_PREVIEW_SNIPPETS);
        assert!(snippets.len() > MAX_QUICK_SNIPPETS);
        for snippet in snippets {
            assert_eq!(plain.get(snippet.range), Some(snippet.text.as_str()));
        }
    }

    #[test]
    fn preview_caps_chars_and_drops_a_cut_snippet() {
        let mut item = text_item(None, false);
        let prefix = format!("编号 12345 {}", "字".repeat(MAX_SPLIT_CHARS - 12));
        item.content = format!("{prefix} 6789012345 末尾 99999");
        let snippets =
            preview_quick_snippets(&item, &super::super::rules::CompiledRules::default());

        assert_eq!(snippets.len(), 1);
        assert_eq!(snippets[0].text, "12345");
        assert_eq!(snippets[0].range, 7..12);
        for sub_kind in [
            ClipboardSubKind::Url,
            ClipboardSubKind::Email,
            ClipboardSubKind::Color,
            ClipboardSubKind::Path,
        ] {
            item.sub_kind = Some(sub_kind);
            assert!(
                preview_quick_snippets(&item, &super::super::rules::CompiledRules::default())
                    .is_empty()
            );
        }
        item.kind = ClipboardKind::Image;
        item.sub_kind = None;
        assert!(
            preview_quick_snippets(&item, &super::super::rules::CompiledRules::default())
                .is_empty()
        );
    }

    fn snippet_texts(text: &str, may_be_cut: bool) -> Vec<String> {
        extract_snippets(
            text,
            may_be_cut,
            MAX_QUICK_SNIPPETS,
            &super::super::rules::CompiledRules::default(),
        )
        .into_iter()
        .map(|snippet| snippet.text)
        .collect()
    }

    fn token_texts(text: &str) -> Vec<String> {
        split_words(text)
            .tokens
            .into_iter()
            .map(|token| token.text)
            .collect()
    }

    #[test]
    fn extracts_codes_sizes_and_prices_from_mixed_text() {
        let text = "桌子上的玻璃是：9140BT  诺莫斯餐桌   19mm钢化玻璃桌面尺寸W2000*D800";

        assert_eq!(
            snippet_texts(text, false),
            ["9140BT", "19mm", "W2000", "D800"]
        );
        assert_eq!(
            snippet_texts("是个设计师大品牌的，原价一万五左右，我9000收的", false),
            ["9000"]
        );
    }

    #[test]
    fn keeps_urls_and_emails_whole() {
        let text = "文档见https://example.com/a/b?id=42。有问题发 first.last@example.com，谢谢";

        assert_eq!(
            snippet_texts(text, false),
            ["https://example.com/a/b?id=42", "first.last@example.com"]
        );
        assert_eq!(
            snippet_texts(
                "see (https://en.wikipedia.org/wiki/Rust_(language)).",
                false
            ),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
    }

    #[test]
    fn joins_numbers_split_by_separators() {
        let text =
            "电话 138 1234 5678，座机 010-12345678，2026年9月24日 14:30 取件，单号 SF-2026-0924";

        assert_eq!(
            snippet_texts(text, false),
            [
                "138 1234 5678",
                "010-12345678",
                "2026年9月24日",
                "14:30",
                "SF-2026-0924"
            ]
        );
    }

    #[test]
    fn joins_long_space_grouped_numbers_only() {
        assert_eq!(
            snippet_texts("会议号 881 234 5678，卡号 6222 0212 3456 7890", false),
            ["881 234 5678", "6222 0212 3456 7890"]
        );
        assert_eq!(
            snippet_texts("单价 100 200 300 元", false),
            ["100", "200", "300"]
        );
    }

    #[test]
    fn keeps_dotted_parts_of_hyphenated_codes() {
        assert_eq!(
            snippet_texts(
                "基于 EcoPaste（Apache-2.0）二次开发，测试版 v1.3.0-beta.1。",
                false
            ),
            ["Apache-2.0", "v1.3.0-beta.1"]
        );
    }

    #[test]
    fn keeps_decimal_numbers_and_versions_whole() {
        assert_eq!(
            snippet_texts("总价 ¥1,299.00，版本 v1.3.0，服务器 192.168.1.10", false),
            ["1,299.00", "v1.3.0", "192.168.1.10"]
        );
    }

    #[test]
    fn skips_noise_duplicates_and_the_whole_text() {
        assert!(snippet_texts("我有3个苹果和4K显示器", false).is_empty());
        assert!(snippet_texts("9000", false).is_empty());
        assert_eq!(
            snippet_texts("验证码 384756，384756 五分钟内有效", false),
            ["384756"]
        );
    }

    #[test]
    fn drops_the_snippet_at_a_possible_cut() {
        assert_eq!(snippet_texts("订单 12345 运单 67890", true), ["12345"]);
        assert_eq!(
            snippet_texts("订单 12345 运单 67890", false),
            ["12345", "67890"]
        );
    }

    #[test]
    fn caps_the_number_of_snippets() {
        let text = (10..30)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(" ");

        assert_eq!(snippet_texts(&text, false).len(), MAX_QUICK_SNIPPETS);
    }

    #[test]
    fn splits_han_per_character_and_keeps_latin_words() {
        assert_eq!(
            token_texts("钢化玻璃桌面尺寸 W2000*D800"),
            ["钢", "化", "玻", "璃", "桌", "面", "尺", "寸", "W2000", "*", "D800"]
        );
        assert_eq!(
            token_texts("Hello, world! v1.3.0"),
            ["Hello", ",", "world", "!", "v1.3.0"]
        );
    }

    #[test]
    fn breaks_words_at_colons_and_fullwidth_separators() {
        assert_eq!(
            token_texts("型号SKU：AB123;tel:10086，1，2"),
            ["型", "号", "SKU", "：", "AB123", ";", "tel", ":", "10086", "，", "1", "，", "2"]
        );
        assert_eq!(snippet_texts("SKU:AB123 规格", false), ["AB123"]);
    }

    #[test]
    fn splits_words_at_underscores() {
        let text = "123-456_654-321";

        assert_eq!(
            token_texts(text),
            ["123", "-", "456", "_", "654", "-", "321"]
        );
        assert_eq!(select_words(text, &[0, 1, 2]).as_deref(), Some("123-456"));
    }

    #[test]
    fn merges_repeated_ascii_punctuation() {
        assert_eq!(token_texts("a--b"), ["a", "--", "b"]);
        assert_eq!(
            token_texts("x == y::z..."),
            ["x", "==", "y", "::", "z", "..."]
        );
        assert_eq!(token_texts("a-b"), ["a", "-", "b"]);
    }

    #[test]
    fn marks_tokens_that_start_a_new_line() {
        let breaks: Vec<bool> = split_words("第一行\n\n下一 行")
            .tokens
            .iter()
            .map(|token| token.line_break)
            .collect();

        assert_eq!(breaks, [false, false, false, true, false, false]);
    }

    #[test]
    fn truncates_long_text_before_splitting() {
        let text = "字".repeat(MAX_SPLIT_CHARS + 10);
        let split = split_words(&text);

        assert!(split.truncated);
        assert_eq!(split.tokens.len(), MAX_SPLIT_CHARS);
        assert!(!split_words("短文本").truncated);
    }

    #[test]
    fn selects_adjacent_words_with_the_original_spacing() {
        let text = "hello  world\nagain";

        assert_eq!(select_words(text, &[1, 0]).as_deref(), Some("hello  world"));
        assert_eq!(select_words(text, &[1, 2]).as_deref(), Some("world\nagain"));
    }

    #[test]
    fn joins_separate_runs_by_script() {
        let text = "尺寸W2000*D800 桌面";

        // 尺 寸 W2000 * D800 桌 面
        assert_eq!(select_words(text, &[2, 4]).as_deref(), Some("W2000 D800"));
        assert_eq!(select_words(text, &[0, 5, 6]).as_deref(), Some("尺桌面"));
        assert_eq!(select_words(text, &[2, 5]).as_deref(), Some("W2000桌"));
    }

    // 预览面板按 JS 字符串下标切词：emoji 占两个 UTF-16 码元，后面的词要跟着往后挪。
    #[test]
    fn word_spans_use_utf16_offsets() {
        let text = "好🙏 W2000*D800";
        let (spans, truncated) = word_spans(text);
        assert!(!truncated);
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let words: Vec<String> = spans
            .iter()
            .map(|WordSpan(start, end)| {
                String::from_utf16(&utf16[*start as usize..*end as usize]).unwrap()
            })
            .collect();

        assert_eq!(words, ["好", "🙏", "W2000", "*", "D800"]);
        assert_eq!(spans[2], WordSpan(4, 9));
    }

    #[test]
    fn rejects_empty_or_out_of_range_selection() {
        assert_eq!(select_words("a b", &[]), None);
        assert_eq!(select_words("a b", &[0, 2]), None);
    }
}
