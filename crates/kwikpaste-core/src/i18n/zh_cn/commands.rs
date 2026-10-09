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

        Key::PasteBusy => "上一次粘贴或复制尚未结束，请稍后再试；本次未写入剪贴板",
        Key::PasteHandoffFailed => {
            "内容已复制到剪贴板；目标窗口或键盘交接未就绪，自动粘贴未完成，请手动粘贴"
        }
        Key::PastePermissionMissing => {
            "未获得辅助功能权限，内容已复制到剪贴板，请手动粘贴；请在系统设置的辅助功能中允许快贴"
        }

        Key::DragSourceFilesMissing => "拖拽源文件已不存在",
        Key::DragImageMissing => "图片文件已不存在",
        Key::DragTextEmpty => "文本内容为空",
        Key::ExternalUrlUnsupported => "只能打开 http 或 https 开头的链接",
        Key::FragmentUnavailable => "所选内容已不在这条记录中",
        Key::SplitTextOnly => "只有文本记录可以拆词",
        Key::SplitSensitiveRedacted => "敏感内容已脱敏显示，不能拆词",
        Key::PortableStorageFixed => "便携版的数据固定保存在程序文件夹的 data 目录里",
        Key::StorageInsufficientSpace => "目标磁盘的可用空间不足，无法迁移数据",
        Key::StorageSpaceUnavailable => "无法读取目标磁盘的可用空间，请检查目录权限或连接状态",
        Key::StorageTargetHasData => "目标目录里已有快贴数据",
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
