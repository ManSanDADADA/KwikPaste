//! 语种识别和翻译方向。

use whatlang::Lang;

/// 这一次请求怎么翻。
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// 译成 `target`。`recheck`：语种没拿准，服务报告原文就是首选语言时要改译成第二语言。
    Translate { target: String, recheck: bool },
    /// 自动模式下不翻：原文是首选语言，或语种拿不准。
    Skip,
}

pub fn plan(text: &str, first: &str, second: &str, auto: bool) -> Plan {
    let detected = whatlang::detect(text).filter(|info| info.is_reliable());
    match detected {
        Some(info) if same_language(base(info.lang()), first) => {
            if auto {
                Plan::Skip
            } else {
                Plan::Translate {
                    target: second.to_owned(),
                    recheck: false,
                }
            }
        }
        Some(_) => Plan::Translate {
            target: first.to_owned(),
            recheck: false,
        },
        None if auto => Plan::Skip,
        None => Plan::Translate {
            target: first.to_owned(),
            recheck: true,
        },
    }
}

/// whatlang 的语种换成 BCP 47 的主语言子标签；列表外的语种给出它的三字母代码，只用来和首选语言比较。
fn base(lang: Lang) -> &'static str {
    match lang {
        Lang::Cmn => "zh",
        Lang::Eng => "en",
        Lang::Jpn => "ja",
        Lang::Kor => "ko",
        Lang::Fra => "fr",
        Lang::Deu => "de",
        Lang::Spa => "es",
        Lang::Rus => "ru",
        Lang::Por => "pt",
        Lang::Ita => "it",
        other => other.code(),
    }
}

/// 两个语言代码是否同一种语言：只比主语言子标签，`zh-Hans` 和 `zh-Hant` 算同一种。
pub fn same_language(a: &str, b: &str) -> bool {
    fn primary(code: &str) -> &str {
        code.split('-').next().unwrap_or(code)
    }
    primary(a).eq_ignore_ascii_case(primary(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZH: &str = "剪贴板管理器可以保存你复制过的内容，方便以后再找出来。";
    const EN: &str =
        "The clipboard manager keeps everything you copy so that you can find it again later.";

    fn to(target: &str, recheck: bool) -> Plan {
        Plan::Translate {
            target: target.into(),
            recheck,
        }
    }

    #[test]
    fn direction_follows_the_first_and_second_language() {
        // 原文是首选语言：手动译成第二语言，自动跳过。繁体首选时简体内容也算首选语言。
        assert_eq!(plan(ZH, "zh-Hans", "en", false), to("en", false));
        assert_eq!(plan(ZH, "zh-Hans", "en", true), Plan::Skip);
        assert_eq!(plan(ZH, "zh-Hant", "en", true), Plan::Skip);
        // 原文是别的语言：译成首选语言。
        assert_eq!(plan(EN, "zh-Hans", "en", false), to("zh-Hans", false));
        assert_eq!(plan(EN, "zh-Hans", "en", true), to("zh-Hans", false));
        assert_eq!(plan(ZH, "en", "zh-Hans", true), to("en", false));
        // 语种拿不准（短文本、纯数字）：手动先译成首选语言并复核，自动跳过。
        for unsure in ["Hello world", "12345"] {
            assert_eq!(plan(unsure, "zh-Hans", "en", false), to("zh-Hans", true));
            assert_eq!(plan(unsure, "zh-Hans", "en", true), Plan::Skip);
        }
    }
}
