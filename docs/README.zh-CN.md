<div align="center">
  <img src="../public/logo.png" alt="快贴" width="96" height="96" />

# 快贴 KwikPaste

**快速、本地优先的 macOS 与 Windows 剪贴板管理器。**

[English](../README.md) | 简体中文

  <br />

  <img alt="Tauri v2" src="https://img.shields.io/badge/Tauri-v2-24c8db?style=flat-square" />
  <img alt="Rust first" src="https://img.shields.io/badge/Rust-first-b7410e?style=flat-square" />
  <img alt="React 19" src="https://img.shields.io/badge/React-19-61dafb?style=flat-square" />
  <img alt="macOS" src="https://img.shields.io/badge/macOS-supported-000000?style=flat-square&logo=apple&logoColor=white" />
  <img alt="Windows" src="https://img.shields.io/badge/Windows-supported-0078d4?style=flat-square&logo=windows&logoColor=white" />
  <img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" />
</div>

## 关于

快贴把你复制过的一切——纯文本、富文本、图片和文件——放在一个快捷键之外，并且不会把任何内容发送到别处。历史记录、搜索索引、资源缓存和设置全部保存在本机。

快贴采用 Rust-First 的 Tauri 架构：剪贴板采集、存储、搜索和系统集成由 Rust 承担，React 前端专注于界面展示与交互。因此它体积小、响应快，在 macOS 和 Windows 上都用得顺手。

## 下载

在 [Releases](https://github.com/ManSanDADADA/KwikPaste/releases) 页面下载最新安装包：

- **Windows**：`-setup.exe` 安装包，分 x64 和 ARM64 两种。
- **macOS**：按芯片选择 `.dmg`，Apple 芯片选 `aarch64`，Intel 芯片选 `x64`。

> [!NOTE]
> 安装包暂未经过微软和苹果的代码签名，首次打开时可能出现安全提示。Windows 上点 **更多信息 → 仍要运行**；macOS 上打开 **系统设置 → 隐私与安全性**，点 **仍要打开**。

之后快贴会自动保持最新。每个更新包在安装前都会校验签名，下载走国内 CDN，GitHub 作为备用。

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
- 支持导出和导入 `.kwikpastebak` 备份，包括加密备份包。
- 应用内自动更新，更新包经过签名校验。
- 剪贴板数据、资源缓存和设置均保存在本机。

## 参与贡献

开发环境、架构说明、质量检查和贡献要求请阅读[贡献指南](./CONTRIBUTING.zh-CN.md)。

## 开源协议

快贴基于 [Apache License 2.0](../LICENSE) 开源。

> 快贴基于 Apache-2.0 许可的 [EcoPaste](https://github.com/EcoPasteHub/EcoPaste) 二次开发。
