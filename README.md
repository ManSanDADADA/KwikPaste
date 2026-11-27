<div align="center">
  <img src="./public/logo.png" alt="快贴" width="96" height="96" />

# 快贴 KwikPaste

**快速、本地优先的 macOS 与 Windows 剪贴板管理器。**

**官网：[paste.fastthree.com](https://paste.fastthree.com)**

简体中文 | [English](./docs/README.en-US.md)

  <br />

  <img alt="100% Rust" src="https://img.shields.io/badge/Rust-100%25-b7410e?style=flat-square&logo=rust&logoColor=white" />
  <img alt="GPUI" src="https://img.shields.io/badge/UI-GPUI-6e56cf?style=flat-square" />
  <img alt="No WebView" src="https://img.shields.io/badge/WebView-none-2ea44f?style=flat-square" />
  <img alt="macOS" src="https://img.shields.io/badge/macOS-supported-000000?style=flat-square&logo=apple&logoColor=white" />
  <img alt="Windows" src="https://img.shields.io/badge/Windows-supported-0078d4?style=flat-square&logo=windows&logoColor=white" />
  <img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" />
</div>

## 关于

快贴把你复制过的一切——纯文本、富文本、图片和文件——放在一个快捷键之外，并且不会把任何内容发送到别处。历史记录、搜索索引、资源缓存和设置全部保存在本机。

快贴 2.0 用 Rust 从头重写，是一个纯 Rust 的原生应用：界面基于 [GPUI](https://www.gpui.rs) 由 GPU 直接绘制，不再内嵌 WebView。剪贴板采集、存储、搜索、系统集成和界面渲染都在同一个原生进程里完成。

- **内存占用极低**：在 Windows 上窗口隐藏时常驻后台不到 15 MB（实测），1.x 约 370 MB，只有原来的二十分之一左右。
- **随按随开**：按下快捷键，窗口立即出现，不用等网页引擎启动。
- **两端原生**：Windows 上是 Direct3D 11 渲染与 Mica / 磨砂材质，macOS 上是 Metal 渲染与原生面板，都不抢当前应用的焦点。

## 下载

在[官网](https://paste.fastthree.com)或 [Releases](https://github.com/ManSanDADADA/KwikPaste/releases) 页面下载最新安装包：

- **Windows**：`-setup.exe` 安装包，分 x64 和 ARM64 两种。
- **Windows 便携版**：`_portable.zip`，同样分 x64 和 ARM64。解压后直接运行 `KwikPaste\KwikPaste.exe`，历史、设置和日志都保存在同目录的 `data` 文件夹里，整个文件夹可以拷到别的电脑或 U 盘。`portable.txt` 要和 exe 放在一起；便携版和安装版不能同时运行。
- **macOS**：按芯片选择 `.dmg`，Apple 芯片选 `aarch64`，Intel 芯片选 `x64`。

macOS 也可以用 [Homebrew](https://brew.sh) 安装，会按芯片自动选择版本：

```bash
brew install --cask mansandadada/tap/kwikpaste
```

升级和卸载见 [homebrew-tap](https://github.com/ManSanDADADA/homebrew-tap)。

> [!NOTE]
> 安装包暂未经过微软和苹果的代码签名，首次打开时可能出现安全提示。Windows 上点 **更多信息 → 仍要运行**；macOS 上打开 **系统设置 → 隐私与安全性**，点 **仍要打开**。

之后快贴会自动保持最新。每个更新包在安装前都会校验签名，下载走国内 CDN，GitHub 作为备用。

**从 1.x 升级**：1.x 不会自动更新到 2.0，请下载 2.0 直接覆盖安装，记录、分组、设置和图片都会保留。升级后 1.x 无法再打开这份数据，如果之后可能退回 1.x，请先在 1.x 中「导出备份」。

## 使用

| 快捷键 | 作用 |
| --- | --- |
| <kbd>Alt</kbd> + <kbd>C</kbd>（macOS：<kbd>⌥</kbd> + <kbd>C</kbd>） | 打开剪贴板历史 |
| <kbd>Alt</kbd> + <kbd>X</kbd>（macOS：<kbd>⌥</kbd> + <kbd>X</kbd>） | 打开偏好设置 |

两个快捷键都可以在偏好设置里修改。在 Windows 上，还可以让快贴接管系统自带剪贴板面板的 <kbd>Win</kbd> + <kbd>V</kbd>。

## 功能

- 采集纯文本、HTML、RTF、图片、文件和文件夹等剪贴板内容。
- 使用 SQLite FTS5 搜索剪贴板正文与备注。
- 按来源应用和内容类型过滤历史记录。
- 识别并跳过高置信敏感内容，例如私钥、服务 Token、AWS Key 和 JWT。
- 在独立预览窗口中查看文本、图片和文件记录。
- 支持粘贴、复制、复制为纯文本、定位文件、打开链接、添加备注、置顶、收藏、删除，以及将记录拖出到其它应用。
- 通过收藏、置顶、备注、自定义分组和可配置快捷动作组织历史记录。
- 可调整采集顺序、大小限制、保留策略、展示密度、列表排序和窗口行为。
- 局域网同步：用配对码配对同一网络中的设备，复制的文本和图片实时互通，数据不经过任何服务器。
- 全局快捷键支持「粘贴为纯文本」；全屏应用（如游戏）或指定应用在前台时可以自动停用快捷键。
- 支持导出和导入 `.kwikpastebak` 备份，包括加密备份包，也可以只备份收藏或指定分组；还能导出为 Excel 或 Markdown。
- 应用内自动更新，更新包经过签名校验。
- 剪贴板数据、资源缓存和设置均保存在本机。

## 参与贡献

开发环境、架构说明、质量检查和贡献要求请阅读[贡献指南](./docs/CONTRIBUTING.zh-CN.md)。

## 开源协议

快贴基于 [Apache License 2.0](./LICENSE) 开源。

> 快贴 1.x 基于 Apache-2.0 许可的 [EcoPaste](https://github.com/EcoPasteHub/EcoPaste) 二次开发，2.0 起用 Rust 原生重写。
