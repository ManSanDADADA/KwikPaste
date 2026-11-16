# AGENTS.md

> 本文件是本项目 AI 编码工具的**单一真相源**。其它工具入口若存在，只应引用本文件，不要重复维护规则。

KwikPaste 是跨平台剪贴板管理器，采用 Rust-First 的 Tauri 架构。原生 2.0 正在用 GPUI 重写，代码在根 workspace 的 `crates/`，规则见「原生 2.0（GPUI）」一节；1.x 的 `src/` 与 `src-tauri/` 只修 bug。

## 工作方式

- 默认用中文沟通；代码标识符、命令和模型名保持原文。

## 快速原则

- **Rust-First**：业务、系统能力、数据库与持久化优先放 Rust；前端只做展示与交互。
- **仅支持 macOS + Windows**：不要新增 Linux 代码、依赖、构建产物或文档承诺。例外：根 `Cargo.lock` 里上游 crate 在 Linux/wasm cfg 下的传递依赖不算新增，是否真正引入以 `cargo tree --target <4 个发布目标>` 为准；`third_party/` 里不得新增 Linux 代码。
- **已发布版本按发布数据处理**：数据结构、配置格式、默认值和 migration 变更必须有明确迁移策略，不再直接覆盖已发布数据契约。
- **主动演进当前项目**：实现新能力时以当前代码、产品需求和平台约束为准，把当前仓库作为唯一实现基线。
- **尊重 dirty worktree**：不要回滚或覆盖非本轮改动；需要动到已修改文件时先读清楚。
- **提交与推送分支策略**：直接在 `main` 上提交并推送，不新建工作分支。

## 技术栈

| 维度 | 选型                                            |
| ---- | ----------------------------------------------- |
| 桌面 | Tauri v2                                        |
| 前端 | React 19 + Ant Design v6 + UnoCSS `presetWind4` |
| 状态 | Valtio（仅 UI 状态与设置镜像）                  |
| 后端 | Rust + sqlx + SQLite                            |
| 构建 | Vite + pnpm                                     |
| 质量 | Biome、rustfmt、clippy、cargo test              |

## 架构边界

**必须在 Rust 实现**

- 剪贴板监听、写回剪贴板、模拟粘贴，以及监听回环抑制。
- 所有数据库读写、SQLite FTS5 搜索、历史记录清理。
- 内容类型识别：URL、email、color、path。
- 窗口定位计算、OS 级键盘钩子、全局快捷键、托盘、自启。
- 图片落盘、缩略图、文件元信息读取、设置项持久化。
- Rust 侧直接展示给用户的短文案（托盘、原生右键菜单、命令返回 toast）走 `i18n/` 模块；日志与内部错误上下文不走这里。

**保留在前端**

- 组件渲染、虚拟滚动、瀑布流、动画、列表选中态。
- 主题视觉应用、CSS 变量注入、前端 i18n 文案渲染（Rust 侧文案见上）。
- HTML sanitize 与预览、RTF 渲染、Markdown 渲染。
- 普通键盘交互；Windows 主窗口收不到键时走 Rust `keyboard/` 事件。

**跨端契约**

- 前端通过 `#[tauri::command]` 调 Rust，Rust 用 `emit` 通知刷新。
- 事件名用 `domain://action`，如 `clipboard://updated`、`settings://updated`、`window://visibility`、`keyboard://nav`。
- 命令名、事件名、channel/storage key 等跨端或多处复用字面量必须集中维护：Rust 模块常量 + `src/constants/` 同步更新。

## 目录约定

```text
src-tauri/
  src/
    commands/   # tauri command 入口，只做校验与转发
    db/         # sqlx 仓储、连接池、模型
    clipboard/  # 剪贴板读写、监听、内容识别
    window/     # 窗口管理、定位、平台特化
    keystroke/  # 模拟粘贴按键注入
    keyboard/   # OS 级键盘钩子（仅 windows）
    mouse/      # 全局鼠标钩子，主窗口失焦隐藏（仅 windows）
    shortcut/   # 全局快捷键
    tray/       # 托盘菜单
    menu/       # 列表项右键菜单（macOS muda / Windows webview 窗）
    drag_out/   # OS 级拖出（文件/图片/文本拖到外部应用）
    backup/     # .kwikpastebak 历史备份导出与接收
    i18n/       # Rust 侧用户可见文案（托盘、菜单、命令 toast）
    autostart/  # 开机自启
    settings/   # 设置模型与持久化
    core/       # 错误类型、路径、Windows 便携模式、prevent_default（setup 在 lib.rs）
  migrations/
src/            # 前端 components/pages/stores/hooks/locales/utils
```

## 常用命令

```bash
pnpm install
pnpm tauri dev
pnpm tauri build
pnpm lint
pnpm format

cd src-tauri
cargo fmt
cargo clippy -- -D warnings
cargo test
```

## Rust 约定

- 命令与仓储函数使用 `async`，返回 `Result<T, AppError>`；`AppError` 序列化为 `{ kind, message }`。
- `message` 写用户可读根因，不加 `"xxx failed: {err}"` 动作前缀；动作上下文由前端 toast label 拼接，技术上下文写日志。
- 错误处理用 `thiserror` 定义错误类型、`anyhow` 做内部传播、`tauri-plugin-log` 记录上下文。
- 数据库使用 Tauri `State<SqlitePool>`；不要每次新建连接。
- Cargo 依赖版本不要写 patch 级完整版本；所有依赖优先写主版本号，如 `"2"`，确需收窄时最多写到 minor，如 `"0.9"`，除非有明确锁定原因。例外：`gpui-pre*`、`gpui-base`、`gpui-component` 必须在根 `Cargo.toml` 用 `=` 精确钉版，并与 `third_party/gpui` 副本的版本一致（版本对不上时 `[patch]` 会悄悄失效）；只在 GPUI 升级时改。
- SQL 用 `sqlx::query` / `query_as`，不用 `query!` 宏，避免维护离线缓存。
- 已发布版本的 schema 变更必须新增 migration；已发布 migration 不回改。
- `*.sql` 不加任何 `eol` / `-text` / `binary` 规则，也不引入带 `ignored-chars` 的 `sqlx.toml`：已发布版本在 Windows 存 CRLF 校验和、在 macOS 存 LF，任何一种都会让存量用户启动失败。
- 改 schema 时同步检查所有 `SELECT`、`INSERT`、`UPDATE`、`bind`、测试结构体字面量；`query_as` 字段不匹配可能表现为 UI 空结果。
- 表必须有 `created_at` / `updated_at`，类型 `TEXT NOT NULL`；剪贴板 `updated_at` 表示内容重新使用时间，收藏、置顶、备注等元数据更新不要刷新它。
- `commands/` 保持薄层：参数校验 + 调用下层模块，不写业务逻辑。
- 平台代码用 `#[cfg(target_os = "macos")]` / `#[cfg(target_os = "windows")]` 隔离；新增能力两端同步实现，或显式标注 TODO。

## 前端约定

**React 与组件**

- 组件用 `FC<Props>`；函数体内解构 `props`，不要在参数处解构。
- 解构时需要透传剩余字段就用 `...rest` 收尾。
- React 19 优先用 Actions、`use`、`useOptimistic`、ref as prop；不要新增 `forwardRef`。
- JSX 事件回调提取为命名函数；单一动作用动词名，通用事件用 `handleXxx`。
- 箭头函数一律使用 `{}` 和显式 `return`；不要单表达式隐式返回。
- `useEffect` 只写同步副作用；异步初始化用 `useMount` + `useUnmount`，清理句柄用 `useRef`。

**状态、数据与平台 API**

- Valtio 只存 UI 状态和设置镜像；业务数据从 Rust command 拉取，不在前端建数据库副本。
- 异步统一 `async` / `await` + `try` / `catch`；不要 `.then()` / `.catch()` / `.finally()` 链式写法。
- 表达未定义用 `void 0`，不要写 `undefined`。
- 日志统一走 `@/utils/log`，禁止裸 `console.*`。
- 平台与环境判断统一从 `@/utils/is` 引入。
- 当前窗口统一用 `getCurrentWebviewWindow()`，不要用 `getCurrentWindow()`。

**样式与 UI**

- 优先使用 Ant Design v6 组件；prop 命名用 `open` / `checked` / `disabled` / `onClick`。
- 自定义 antd 内部结构优先用组件 `classNames` / `styles` 语义槽位；谨慎使用 `.ant-*` 全局覆盖。
- 样式使用 UnoCSS；条件 className 统一用 `cn from "@/utils/cn"` + 对象语法，不拼模板字符串或 `+`。
- 颜色只能用 antd token 映射类，如 `text-ant-secondary`、`bg-ant-container`、`border-ant-border`；需要新颜色先扩 `src/unocss/presetAntdColors.ts`。
- 普通文本继承全局 `text-ant-text`；次级信息优先 `text-ant-secondary`，更浅层级需有明确设计理由。
- 字号用标准语义字号：`text-xs`、`text-sm`、`text-base`、`text-lg`；不要用 `text-3` / `text-3.5`。
- 尺寸走 wind4 数字制（1 = 4px），如 `p-1.5`、`gap-2`、`rounded-2.5`、`w-36`；不要写任意 px 类或 inline px。
- 主题通过根部 `ConfigProvider` 的 `theme.algorithm` 切换，同时把 `light` / `dark` 类同步到 `<html>`。

**内容与列表**

- i18n 文案必须同步补齐 `zh-CN`（默认）和 `en-US`。
- 列表使用 `react-virtuoso` 虚拟滚动。
- HTML 内容必须经 DOMPurify sanitize 再渲染。

## 通用代码规范

- 非显然函数 / 方法上方写文档注释：TS/JS 用多行 JSDoc，Rust 用连续 `///`；getter/setter、显然一行包装、纯字面量常量可省。
- 优先早返回，避免把主流程包进嵌套 `if`。
- hooks、变量声明、副作用、不同语义阶段和 `return` 前用空行分组。
- 函数体内少写注释；只解释隐藏约束、反直觉行为或规避原因。
- 不写历史残留注释，不引用 TODO 阶段号或外部行号。
- 不做超出当前需求的抽象、兼容垫片或提前优化；React hook / 工具函数遇到真实复杂度再抽象。
- 提交信息用单行 Conventional Commits，如 `feat:`、`fix:`、`refactor:`、`docs:`。
- `third_party/` 是上游原样源码加补丁，不受 rustfmt、clippy 和注释语言约束；改动只能走 `third_party/gpui/patches/` 补丁队列，改动处加 `[kwikpaste patch NNNN]` 注释。
- 改 UI 后必须实际操作验证主路径与边界，不只靠类型检查。

## 原生 2.0（GPUI）

**布局**

- 根 `Cargo.toml` 是原生 workspace（`members = ["crates/*"]`，排除 `src-tauri` 和 `third_party`），锁文件是根 `Cargo.lock`；`src-tauri` 继续自成 workspace，两边的 target 目录不要共用。
- `crates/kwikpaste-app`：GPUI 应用，二进制名 `KwikPaste`；需要 `gpui::*` 类型的代码都在这里。
- `crates/kwikpaste-ui`：唯一依赖 gpui-component 的隔离层，应用只经它使用 gpui-component，窗口经它的 `open_window` 打开。
- 业务与数据契约放 `crates/kwikpaste-core`（不依赖 tauri / gpui，不用 `cfg!(dev)`），只需原生句柄或 OS API 的集成放 `crates/kwikpaste-os`；这两个 crate 随重写计划加入。
- `third_party/gpui/`：打了补丁的 GPUI crate 与 gpui-component（浮层的描边环和阴影由应用主题提供）。`upstream.json` 记版本、sha256 和补丁顺序；用 `node scripts/gpui/vendor.mjs <crate>` 重建，加 `--check` 校验与“原版 + 补丁”逐字节一致。

**命令**

```bash
cargo run -p kwikpaste-app
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo fmt
```

根目录的 `cargo fmt` 不要加 `--all`，否则会连带格式化 `third_party`。

**约束**

- 永不调用 `activate_window()`、`Window::resize()`、`toggle_fullscreen`，不用 `focus: true` 创建窗口：Windows 面板靠“不激活”工作。
- Windows 平台由应用自己调用 `gpui_windows::WindowsPlatform::new` 创建（拿到 `Result`），不调用 `gpui_platform::application()`。
- `line_clamp(n)` 必须配 `.text_ellipsis()` 或 `.truncate()`。
- 渲染与事件回调里不得 panic（进程会直接以 0xC0000409 退出）；可失败的路径返回 `Result` 并写日志。
- release 是 `panic = "abort"`：任何线程上的 panic 都会终止进程。后台线程与任务同样不得 panic，可失败的路径一律返回 `Result`。
- 不引入 gpui-wry 或任何 WebView。
- 仓库根目录不放 `.cargo/config.toml`（会泄漏进 `src-tauri` 的构建）；release profile 不开 `debug-assertions`。
- 原生界面的颜色只能从 `theme::semantic(cx)` / `theme::components(cx)` 读取；新增颜色或状态必须先进入 palette 或组件 token，不能在视图里写内联色调。
- 自测入口必须同时满足 `--selftest-*` 参数和 `KWIKPASTE_SELFTEST=1`。

## 外部文档

- Ant Design v6：<https://ant.design/components/overview-cn> · <https://ant.design/docs/react/customize-theme-cn>
- UnoCSS：<https://unocss.dev/> · <https://unocss.dev/presets/wind4>
- Tauri v2：<https://tauri.app/llms-full.txt>
