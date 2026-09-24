use crate::i18n::keys::StartupKey as Key;

/// 返回简体中文启动自检弹窗文案。
pub fn label(key: Key) -> &'static str {
    match key {
        Key::DialogTitle => "快贴",
        Key::PortableDirNotWritable => {
            "快贴便携版无法在程序所在的文件夹里保存数据。\n\n请把整个文件夹移到可以写入的位置（比如桌面、D 盘或 U 盘），然后重新打开。"
        }
        Key::WebviewMissing => {
            "这台电脑缺少快贴运行所需的 Microsoft Edge WebView2 组件。\n\n点击“确定”下载安装程序，装好后重新打开快贴即可。"
        }
    }
}
