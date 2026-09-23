use crate::i18n::keys::TrayKey as Key;

/// 返回简体中文系统托盘菜单文案。
pub fn label(key: Key) -> &'static str {
    match key {
        Key::Preference => "偏好设置",
        Key::Exit => "退出应用",
    }
}
