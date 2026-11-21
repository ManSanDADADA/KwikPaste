//! 合并段落内的换行：PDF 阅读器复制出的文字每个视觉行末都带硬换行。

use super::payload::{ClipboardPayload, TextPayload};
use crate::app_ids::contains_app;

/// 来源应用在「合并换行」列表里时，把文本载荷换成合并后的纯文本。
/// 合并后只保留纯文本表示：html / rtf 里仍是断行的原文，粘贴出去的应是合并结果。
/// 文本没有变化时载荷原样保留。
pub(crate) fn merge_lines_for_app(
    payload: ClipboardPayload,
    merge_app_ids: &[String],
    source_id: Option<&str>,
) -> ClipboardPayload {
    let ClipboardPayload::Text(text) = &payload else {
        return payload;
    };
    if !source_id.is_some_and(|id| contains_app(merge_app_ids, id)) {
        return payload;
    }

    let merged = merge_wrapped_lines(&text.text);
    if merged == text.text {
        return payload;
    }

    ClipboardPayload::Text(TextPayload {
        text: merged,
        html: None,
        rtf: None,
    })
}

/// 合并段落内相邻两行；空白行分隔段落并原样保留。
///
/// 断行处先去掉两侧空白，再按断点两侧的字符连接：行尾「字母 + `-`」接小写字母时去掉连字符，
/// 任一侧是中日韩字符时直接相连，其余情况用一个空格。结果对再次合并不变（回环抑制依赖这一点）。
pub fn merge_wrapped_lines(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    // 当前段落是否已有内容：有则下一行与之合并，否则另起一行。
    let mut in_paragraph = false;

    for (index, line) in text.split('\n').enumerate() {
        if line.trim().is_empty() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(line);
            in_paragraph = false;
            continue;
        }

        if !in_paragraph {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(line);
            in_paragraph = true;
            continue;
        }

        out.truncate(out.trim_end().len());
        let next = line.trim_start();
        let prev_last = out.chars().last();
        let next_first = next.chars().next();
        if ends_with_hyphenated_word(&out) && next_first.is_some_and(|c| c.is_ascii_lowercase()) {
            out.pop();
        } else if !prev_last.is_some_and(is_cjk) && !next_first.is_some_and(is_cjk) {
            out.push(' ');
        }
        out.push_str(next);
    }

    out
}

/// 行尾是「ASCII 字母 + `-`」：PDF 排版的行尾断词。
fn ends_with_hyphenated_word(line: &str) -> bool {
    let mut chars = line.chars().rev();

    chars.next() == Some('-') && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// 汉字、假名、谚文以及中日韩 / 全角标点。
fn is_cjk(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1100..=0x11FF
            | 0x2E80..=0x9FFF
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7FF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF
            | 0x20000..=0x3FFFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_english_lines_with_a_space() {
        assert_eq!(
            merge_wrapped_lines("The quick brown \nfox jumps over\n  the lazy dog."),
            "The quick brown fox jumps over the lazy dog."
        );
    }

    #[test]
    fn rejoins_hyphenated_words() {
        assert_eq!(
            merge_wrapped_lines("an exam-\nple of hyphen-\nAtion"),
            "an example of hyphen- Ation"
        );
    }

    #[test]
    fn joins_cjk_lines_without_space() {
        assert_eq!(
            merge_wrapped_lines("从 PDF 里复制一段\n话，每行末尾都带\n硬换行。"),
            "从 PDF 里复制一段话，每行末尾都带硬换行。"
        );
        assert_eq!(
            merge_wrapped_lines("使用 Rust\n编写，支持\nWindows 平台"),
            "使用 Rust编写，支持Windows 平台"
        );
    }

    #[test]
    fn keeps_blank_line_paragraphs_and_normalizes_crlf() {
        assert_eq!(
            merge_wrapped_lines("first line\r\nsecond line\r\n\r\nnext para\r\ncontinues\r\n"),
            "first line second line\n\nnext para continues\n"
        );
    }

    #[test]
    fn single_line_and_merged_text_are_unchanged() {
        assert_eq!(
            merge_wrapped_lines("  just one line  "),
            "  just one line  "
        );
        let merged = merge_wrapped_lines("a\nb\n\n中\n文\n");
        assert_eq!(merge_wrapped_lines(&merged), merged);
    }

    #[test]
    fn only_merges_text_from_listed_apps() {
        let payload = ClipboardPayload::Text(TextPayload {
            text: "line one\nline two".to_owned(),
            html: Some("<p>line one<br>line two</p>".to_owned()),
            rtf: None,
        });
        let apps = vec!["C:\\Program Files\\Reader\\Reader.exe".to_owned()];

        assert_eq!(
            merge_lines_for_app(payload.clone(), &apps, Some("C:\\Other\\Editor.exe")),
            payload
        );
        assert_eq!(
            merge_lines_for_app(payload, &apps, Some(&apps[0])),
            ClipboardPayload::Text(TextPayload {
                text: "line one line two".to_owned(),
                html: None,
                rtf: None,
            })
        );
    }
}
