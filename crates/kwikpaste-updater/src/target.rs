//! 安装形态判定与清单里的平台键。
//!
//! - 便携版：exe 旁边有 `portable.txt`；只认 `windows-<arch>-portable`（落到 `windows-<arch>`
//!   就成了 NSIS 安装包，装出来是另一份安装版）。
//! - NSIS 安装版：exe 旁边有 `uninstall.exe`，或者安装程序写下的 `Software\fastthree\KwikPaste`
//!   默认值（当前用户或所有用户）等于 exe 所在目录；先找 `windows-<arch>-nsis` 再找 `windows-<arch>`。
//! - macOS：exe 位于 `*.app/Contents/MacOS`；先找 `darwin-<arch>-app` 再找 `darwin-<arch>`。
//! - 都不是：开发构建或手动拷出来的 exe，不检查也不安装更新。

use std::path::{Path, PathBuf};

use serde::Serialize;

/// NSIS 安装程序在 `SHCTX` 下写安装目录的键（Tauri 模板的 `Software\<publisher>\<product>`）。
#[cfg(target_os = "windows")]
const NSIS_INSTALL_KEY: &str = r"Software\fastthree\KwikPaste";
const NSIS_UNINSTALLER: &str = "uninstall.exe";

/// 当前进程是怎么装到这台电脑上的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum InstallKind {
    /// NSIS 安装版：下载安装包，静默覆盖安装。
    Nsis,
    /// Windows 便携版：从便携包里取出新 exe，原地替换。
    Portable,
    /// macOS：解压新的 `.app` 替换当前的。
    MacApp {
        #[serde(skip)]
        bundle: PathBuf,
    },
    /// 开发构建或手动拷出来的程序：更新器不工作。
    Unmanaged,
}

impl InstallKind {
    /// 按当前 exe 的位置判定。
    pub fn detect() -> Self {
        match std::env::current_exe() {
            Ok(exe) => Self::for_exe(&exe),
            Err(err) => {
                log::warn!("resolve current executable failed: {err}");
                Self::Unmanaged
            }
        }
    }

    /// 按给定 exe 路径判定。
    pub fn for_exe(exe: &Path) -> Self {
        if cfg!(target_os = "macos") {
            return app_bundle(exe).map_or(Self::Unmanaged, |bundle| Self::MacApp { bundle });
        }

        let Some(dir) = exe.parent() else {
            return Self::Unmanaged;
        };
        if kwikpaste_core::portable::data_root_for_exe(exe).is_some() {
            return Self::Portable;
        }
        if dir.join(NSIS_UNINSTALLER).is_file() || registered_install_dir(dir) {
            return Self::Nsis;
        }
        Self::Unmanaged
    }

    pub fn is_managed(&self) -> bool {
        *self != Self::Unmanaged
    }

    /// 清单里依次查找的平台键。
    pub fn platform_keys(&self) -> Vec<String> {
        let arch = arch();
        match self {
            Self::Nsis => vec![format!("windows-{arch}-nsis"), format!("windows-{arch}")],
            Self::Portable => vec![format!("windows-{arch}-portable")],
            Self::MacApp { .. } => vec![format!("darwin-{arch}-app"), format!("darwin-{arch}")],
            Self::Unmanaged => Vec::new(),
        }
    }
}

/// 与 Tauri 相同的架构名。
fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// 扩展不区分安装形态，只使用操作系统与架构。
#[cfg(any(not(debug_assertions), test))]
pub(crate) fn extension_target() -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "windows"
    };
    format!("{os}-{}", arch())
}

/// `/Applications/KwikPaste.app/Contents/MacOS/KwikPaste` → `/Applications/KwikPaste.app`。
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"));
    is_bundle.then(|| bundle.to_path_buf())
}

/// 安装程序登记的安装目录（当前用户或所有用户）是否就是 exe 所在目录。
#[cfg(target_os = "windows")]
fn registered_install_dir(dir: &Path) -> bool {
    [
        windows_registry::CURRENT_USER,
        windows_registry::LOCAL_MACHINE,
    ]
    .iter()
    .filter_map(|root| root.open(NSIS_INSTALL_KEY).ok())
    .filter_map(|key| key.get_string("").ok())
    .any(|installed| same_dir(Path::new(installed.trim()), dir))
}

#[cfg(not(target_os = "windows"))]
fn registered_install_dir(_dir: &Path) -> bool {
    false
}

/// Windows 路径不区分大小写，末尾的分隔符也不算。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn same_dir(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_lowercase()
    };
    normalize(left) == normalize(right)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn platform_keys_follow_the_install_kind() {
        let arch = arch();

        assert_eq!(
            InstallKind::Nsis.platform_keys(),
            [format!("windows-{arch}-nsis"), format!("windows-{arch}")]
        );
        assert_eq!(
            InstallKind::Portable.platform_keys(),
            [format!("windows-{arch}-portable")]
        );
        assert_eq!(
            InstallKind::MacApp {
                bundle: PathBuf::from("/Applications/KwikPaste.app")
            }
            .platform_keys(),
            [format!("darwin-{arch}-app"), format!("darwin-{arch}")]
        );
        assert!(InstallKind::Unmanaged.platform_keys().is_empty());
    }

    #[test]
    fn app_bundle_is_found_from_the_executable() {
        assert_eq!(
            app_bundle(Path::new(
                "/Applications/KwikPaste.app/Contents/MacOS/KwikPaste"
            )),
            Some(PathBuf::from("/Applications/KwikPaste.app"))
        );
        assert_eq!(app_bundle(Path::new("/usr/local/bin/KwikPaste")), None);
        assert_eq!(
            app_bundle(Path::new("/tmp/KwikPaste/Contents/MacOS/KwikPaste")),
            None
        );
    }

    #[test]
    fn windows_paths_compare_without_case_or_trailing_separator() {
        assert!(same_dir(
            Path::new(r"C:\Users\Me\AppData\Local\KwikPaste\"),
            Path::new(r"c:\users\me\appdata\local\kwikpaste")
        ));
        assert!(!same_dir(
            Path::new(r"C:\Program Files\KwikPaste"),
            Path::new(r"C:\Program Files\KwikPaste2")
        ));
    }

    /// 只在临时目录里摆文件判定，不碰本机的安装。
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_kinds_are_detected_from_files_next_to_the_exe() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("KwikPaste.exe");
        fs::write(&exe, b"MZ").unwrap();
        assert_eq!(InstallKind::for_exe(&exe), InstallKind::Unmanaged);

        fs::write(temp.path().join(NSIS_UNINSTALLER), b"MZ").unwrap();
        assert_eq!(InstallKind::for_exe(&exe), InstallKind::Nsis);

        fs::write(temp.path().join("portable.txt"), b"portable").unwrap();
        assert_eq!(InstallKind::for_exe(&exe), InstallKind::Portable);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_kind_needs_an_app_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("KwikPaste.app/Contents/MacOS/KwikPaste");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert_eq!(
            InstallKind::for_exe(&exe),
            InstallKind::MacApp {
                bundle: temp.path().join("KwikPaste.app")
            }
        );
        assert_eq!(
            InstallKind::for_exe(&temp.path().join("KwikPaste")),
            InstallKind::Unmanaged
        );
    }
}
