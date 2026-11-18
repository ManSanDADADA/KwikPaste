//! 翻译相关的显示名：语言用它自己的写法，翻译服务按界面语言。

use gpui::SharedString;

use crate::i18n::t;

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

/// 翻译服务的名称；AI 接口的 `provider` 是模型名，原样显示。
pub fn provider_name(provider: &str) -> SharedString {
    match provider {
        "volc" => t("clipboard:translation.providers.volc"),
        "transmart" => t("clipboard:translation.providers.transmart"),
        other => SharedString::from(other.to_owned()),
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
