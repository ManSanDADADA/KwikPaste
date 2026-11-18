use crate::i18n::keys::CommandKey as Key;

/// 返回简体中文 操作失败的根因文案。
pub fn label(key: Key) -> &'static str {
    match key {
        Key::ExportNoGroups => "请选择至少一个分组或未分组",
        Key::ExportPreviewChanged => "导出内容已变化，请重新预览后确认",
        Key::ExportEmpty => "所选范围没有可导出的记录",
        Key::ExportInvalidTarget => "请选择有效的导出目录或对应格式的文件路径",
        Key::ExportExcelLimit => {
            "内容超过 Excel 的单元格或行数限制，请改用 Markdown 或缩小导出范围"
        }
        Key::BackupPartialOverwrite => "这个备份只包含部分记录，请用合并导入",

        Key::DragSourceFilesMissing => "拖拽源文件已不存在",
        Key::DragImageMissing => "图片文件已不存在",
        Key::DragTextEmpty => "文本内容为空",
        Key::ExternalUrlUnsupported => "只能打开 http 或 https 开头的链接",
        Key::FragmentUnavailable => "所选内容已不在这条记录中",
        Key::SelectionMixedKinds => "文本不能和图片、文件一起合并",
        Key::SelectionFilesMissing => "选中的图片和文件都已不存在",
        Key::QrCodeNotFound => "图片里没有识别到二维码",
        Key::SplitTextOnly => "只有文本记录可以拆词",
        Key::SplitSensitiveRedacted => "敏感内容已脱敏显示，不能拆词",
        Key::TranslateUnavailable => "翻译扩展没有启用",
        Key::TranslateNotTranslatable => {
            "这条记录不能翻译：只支持 5000 个字符以内、没有标记为敏感的文本"
        }
        Key::TranslateNotConfigured => "请先在「偏好设置 → 扩展 → 翻译」里填写 AI 接口的地址和模型",
        Key::TranslationMissing => "这条记录还没有译文",
        Key::PortableStorageFixed => "便携版的数据固定保存在程序文件夹的 data 目录里",
        Key::StorageInsufficientSpace => "目标磁盘的可用空间不足，无法迁移数据",
        Key::StorageSpaceUnavailable => "无法读取目标磁盘的可用空间，请检查目录权限或连接状态",
        Key::StorageTargetHasData => "目标目录里已有快贴数据",
        Key::StorageInsideInstallDir => {
            "数据目录不能放在快贴的安装目录里，卸载或重装时可能被一并删除"
        }
        Key::StorageCustomUnavailable => "自定义数据目录当前不可用，请重新连接后重启快贴再更改目录",
        Key::SyncNotRunning => "请先开启局域网同步",
        Key::SyncInvalidCode => "配对码是 6 位数字",
        Key::SyncWrongCode => "配对码不正确",
        Key::SyncPairingUnavailable => "对方的配对码已失效，请在对方设备上刷新配对码",
        Key::SyncUnreachable => "连不上这台设备，请确认两台设备在同一网络，且对方已开启局域网同步",
        Key::SyncInvalidAddress => "地址格式不正确，例如 192.168.1.8:41573 或 [fe80::1%12]:41573",
        Key::SyncIncompatible => "对方的快贴版本与本机不兼容，请把两台设备都更新到最新版",
        Key::SyncPeerOutdated => "对方的快贴版本过旧，请先在对方设备上升级",
        Key::SyncSelfOutdated => "本机的快贴版本过旧，请先升级本机",
        Key::SyncNotPaired => "这台设备还没有与本机配对，请先用配对码配对",
        Key::SyncDeviceNotFound => "这台设备已不在附近设备列表中",
        Key::SyncSelfPairing => "不能与本机配对",
    }
}
