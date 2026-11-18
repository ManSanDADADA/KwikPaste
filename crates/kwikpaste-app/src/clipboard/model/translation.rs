//! 翻译相关的显示名：语言用它自己的写法。

/// 语言的本名。服务报告的原文语种可能不在设置的 11 种里，不认识的代码原样显示。
pub fn language_name(code: &str) -> &str {
    match code {
        "zh-Hans" => "简体中文",
        "zh-Hant" | "zh-TW" => "繁體中文",
        "zh" => "中文",
        "en" => "English",
        "ja" => "日本語",
        "ko" => "한국어",
        "fr" => "Français",
        "de" => "Deutsch",
        "es" => "Español",
        "ru" => "Русский",
        "pt" => "Português",
        "it" => "Italiano",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_language_has_a_name() {
        for code in kwikpaste_core::translate::LANGUAGES {
            assert_ne!(language_name(code), code);
        }
        assert_eq!(language_name("vi"), "vi");
    }
}
