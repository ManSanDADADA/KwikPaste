pub use crate::i18n::keys::StartupKey as Key;

use crate::settings::Language;

/// 返回 Tauri 启动前自检弹窗的文案。
pub fn label(lang: Language, key: Key) -> &'static str {
    match lang {
        Language::ZhCN => crate::i18n::zh_cn::startup::label(key),
        Language::EnUS => crate::i18n::en_us::startup::label(key),
    }
}
