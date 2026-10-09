//! macOS 集成。本机没有 Mac，这里只靠 CI 编译和冒烟自测验证。

pub mod apps;
pub mod drag_out;
pub mod keystroke;
pub mod material;
pub mod metal;
pub mod monitor;
pub mod mouse;
pub mod panel;
pub mod permissions;
pub mod single_instance;
pub mod system;
pub mod trigger_pause;
pub mod window;
