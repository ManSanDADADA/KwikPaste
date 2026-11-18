//! 主窗口列表的模型层：不依赖 GPUI，全部可以 headless 跑单测（附录 D §3.1）。
//!
//! - [`item`]：列表条目的视图模型（core 列表载荷的形状）；
//! - [`list_model`]：稀疏分页缓存；
//! - [`controller`]：当前项、键盘移动、数字提示、刷新策略；
//! - [`filter`]：范围、分类、自定义分组、搜索词；
//! - [`freshness`]：数据追没追上 core，作用于当前项的按键何时执行；
//! - [`empty_state`]：空列表的 16 种提示；
//! - [`actions`]：悬停快捷动作、删除保护；
//! - [`menu`]：卡片右键菜单；
//! - [`preview`]：预览窗的几何、文本行与选词；
//! - [`shortcut`]：快捷键的显示文案；
//! - [`selection`]：多选；
//! - [`layout`]：密度换算、图片显示尺寸预测；
//! - [`time_label`]：卡片时间标签；
//! - [`translation`]：翻译相关的显示名；
//! - [`viewer`]：大图预览的缩放、平移与解码尺寸；
//! - [`viewer_text`]：大图预览的文字层（识别行的位置、命中与选区）。

pub mod actions;
pub mod controller;
pub mod empty_state;
pub mod filter;
pub mod freshness;
pub mod item;
pub mod layout;
pub mod list_model;
pub mod menu;
pub mod preview;
pub mod selection;
pub mod shortcut;
pub mod time_label;
pub mod translation;
pub mod viewer;
pub mod viewer_text;
