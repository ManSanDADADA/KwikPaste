//! Windows administrator launch support.
//!
//! The persistent setting records intent. The current process token remains the
//! source of truth for whether the app is actually elevated.

#[cfg(target_os = "windows")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
use anyhow::Context;
#[cfg(target_os = "windows")]
use serde::Deserialize;

#[cfg(target_os = "windows")]
use crate::autostart::AUTO_LAUNCH_ARG;
#[cfg(target_os = "windows")]
use crate::core::windows_args;
use crate::core::{AppError, Result};

#[cfg(target_os = "windows")]
const ADMIN_RESTARTED_ARG: &str = "--kwikpaste-admin-restarted";
#[cfg(target_os = "windows")]
const TASK_NAME: &str = "KwikPasteAdmin";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(target_os = "windows")]
const SETTINGS_FILENAME: &str = "settings.json";
#[cfg(target_os = "windows")]
const STORAGE_MANIFEST_FILENAME: &str = "storage.json";
#[cfg(target_os = "windows")]
const DEV_ENV_DIR: &str = "dev";
#[cfg(target_os = "windows")]
const PROD_ENV_DIR: &str = "prod";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminLaunchStatus {
    pub configured: bool,
    pub running_as_admin: bool,
    pub task_ready: bool,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct EarlySettings {
    general: EarlyGeneral,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct EarlyGeneral {
    run_as_admin: bool,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StorageManifest {
    data_dir: PathBuf,
    environment: String,
    version: u16,
}

pub fn status(configured: bool) -> AdminLaunchStatus {
    AdminLaunchStatus {
        configured,
        running_as_admin: is_running_as_admin(),
        task_ready: is_scheduled_task_ready(),
    }
}

#[cfg(target_os = "windows")]
pub fn is_running_as_admin() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token_handle = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION::default();
        let mut return_length = 0_u32;
        let result = GetTokenInformation(
            token_handle,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut return_length,
        );

        let _ = CloseHandle(token_handle);

        result.is_ok() && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(target_os = "windows"))]
pub fn is_running_as_admin() -> bool {
    false
}

pub fn is_scheduled_task_ready() -> bool {
    #[cfg(target_os = "windows")]
    {
        is_scheduled_task_exists() && is_scheduled_task_path_valid()
    }

    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// 便携版不注册计划任务：任务里记着 exe 路径，文件夹挪动或换电脑后就失效，
/// 任务名还会和安装版冲突。
pub fn sync_scheduled_task(configured: bool) {
    #[cfg(target_os = "windows")]
    {
        if crate::core::portable::is_portable() {
            return;
        }

        if configured && is_running_as_admin() {
            if let Err(err) = create_scheduled_task() {
                log::warn!("sync admin scheduled task failed: {err}");
            }
            return;
        }

        if !configured && is_running_as_admin() {
            if let Err(err) = delete_scheduled_task() {
                log::warn!("delete admin scheduled task failed: {err}");
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = configured;
    }
}

pub fn launch_elevated_current_process() -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        if try_launch_elevated_current_process() {
            return Ok(());
        }

        Err(AppError::Other(anyhow::anyhow!(
            "administrator permission request was cancelled or failed"
        )))
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err(AppError::Other(anyhow::anyhow!(
            "administrator launch is only available on Windows"
        )))
    }
}

/// 按设置在启动时自动提权。便携版跳过：设置随文件夹带到别的电脑后不应弹 UAC，
/// 这里读的也是安装版的数据目录。
pub fn handle_startup_auto_elevation() {
    #[cfg(target_os = "windows")]
    {
        if cfg!(debug_assertions) || crate::core::portable::is_portable() {
            return;
        }

        let Ok(configured) = early_run_as_admin_enabled() else {
            return;
        };
        if !configured {
            return;
        }

        if is_running_as_admin() {
            if let Err(err) = create_scheduled_task() {
                log::warn!("startup admin scheduled task sync failed: {err}");
            }
            return;
        }

        if has_admin_restart_marker() {
            return;
        }

        if try_launch_elevated_current_process() {
            std::process::exit(0);
        }
    }
}

#[cfg(target_os = "windows")]
fn is_scheduled_task_exists() -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let output = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    matches!(output, Ok(output) if output.status.success())
}

#[cfg(target_os = "windows")]
fn is_scheduled_task_path_valid() -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let current_exe = match std::env::current_exe() {
        Ok(path) => path.to_string_lossy().to_lowercase(),
        Err(_) => return false,
    };
    let output = Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME, "/FO", "LIST", "/V"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    String::from_utf8_lossy(&output.stdout)
        .to_lowercase()
        .contains(&current_exe)
}

/// 每次提权启动都重建任务，老版本用命令行参数建的任务会在这里换成 XML 定义。
#[cfg(target_os = "windows")]
fn create_scheduled_task() -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let xml_path =
        std::env::temp_dir().join(format!("kwikpaste-admin-task-{}.xml", std::process::id()));
    std::fs::write(&xml_path, utf16le_with_bom(&scheduled_task_xml(&exe)))
        .context("failed to write administrator launch task definition")?;

    let _ = Command::new("schtasks")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let output = Command::new("schtasks")
        .args(["/Create", "/TN", TASK_NAME, "/XML"])
        .arg(&xml_path)
        .arg("/F")
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = std::fs::remove_file(&xml_path);
    let output = output.context("failed to create administrator launch task")?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(AppError::Other(anyhow::anyhow!(
        "failed to create administrator launch task: {stderr}"
    )))
}

#[cfg(target_os = "windows")]
fn delete_scheduled_task() -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let _ = Command::new("schtasks")
        .args(["/Delete", "/TN", TASK_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("failed to delete administrator launch task")?;

    Ok(())
}

/// `/I` 忽略任务条件：老版本建的任务带「仅接通电源时启动」，电池供电时 `/Run`
/// 会被静默跳过却仍返回成功，应用就不会启动。
#[cfg(target_os = "windows")]
fn run_via_scheduled_task() -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let output = Command::new("schtasks")
        .args(["/Run", "/I", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    matches!(output, Ok(output) if output.status.success())
}

#[cfg(target_os = "windows")]
fn try_launch_elevated_current_process() -> bool {
    if can_use_scheduled_task_for_args(std::env::args().skip(1))
        && is_scheduled_task_exists()
        && is_scheduled_task_path_valid()
        && run_via_scheduled_task()
    {
        return true;
    }

    try_launch_with_uac()
}

#[cfg(target_os = "windows")]
fn try_launch_with_uac() -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let params = restart_args()
        .into_iter()
        .map(|arg| windows_args::quote_arg(&arg))
        .collect::<Vec<_>>()
        .join(" ");

    let operation = wide_null("runas");
    let file: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let params = wide_null(&params);

    unsafe {
        let result = ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR(std::ptr::null()),
            SW_SHOWNORMAL,
        );

        result.0 as usize > 32
    }
}

#[cfg(target_os = "windows")]
fn early_run_as_admin_enabled() -> Result<bool> {
    let Some(base) = std::env::var_os("LOCALAPPDATA") else {
        return Ok(false);
    };

    let bootstrap = PathBuf::from(base)
        .join("com.fastthree.kwikpaste")
        .join(env_dir());
    let data_dir = early_data_dir(&bootstrap)?;
    let settings_path = data_dir.join("config").join(SETTINGS_FILENAME);
    if !settings_path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(&settings_path)
        .with_context(|| format!("failed to read early settings at {settings_path:?}"))?;
    let settings: EarlySettings =
        serde_json::from_str(&content).context("failed to parse early settings")?;

    Ok(settings.general.run_as_admin)
}

#[cfg(target_os = "windows")]
fn early_data_dir(bootstrap: &Path) -> Result<PathBuf> {
    let default = bootstrap.to_path_buf();
    let manifest_path = bootstrap.join(STORAGE_MANIFEST_FILENAME);
    if !manifest_path.exists() {
        return Ok(default);
    }

    let content = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read storage manifest at {manifest_path:?}"))?;
    let manifest: StorageManifest =
        serde_json::from_str(&content).context("failed to parse storage manifest")?;
    if manifest.version != 1 || manifest.environment != env_dir() || !manifest.data_dir.exists() {
        return Ok(default);
    }

    Ok(manifest.data_dir)
}

/// 按需启动的提权任务定义，不设触发器。`schtasks /Create` 的默认设置只在接通电源时启动，
/// 还会以低于正常的优先级运行，这里显式改掉，并去掉运行时长上限、允许并行实例。
#[cfg(target_os = "windows")]
fn scheduled_task_xml(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>Parallel</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{}</Command>
      <Arguments>{ADMIN_RESTARTED_ARG}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        xml_escape(&exe.to_string_lossy())
    )
}

#[cfg(target_os = "windows")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// 与 XML 声明的 `encoding="UTF-16"` 保持一致，写成带 BOM 的 UTF-16LE 交给 `schtasks /XML`。
#[cfg(target_os = "windows")]
fn utf16le_with_bom(value: &str) -> Vec<u8> {
    [0xFEFF_u16]
        .into_iter()
        .chain(value.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// `schtasks /Run` 不能传参，只有丢掉参数也无妨的启动才能走计划任务。
/// `--auto-launch` 只在单实例回调里用来识别重复自启，首个实例不读它，
/// 开机自启因此也走任务，不再每次弹 UAC。
#[cfg(target_os = "windows")]
fn can_use_scheduled_task_for_args(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter()
        .all(|arg| arg == ADMIN_RESTARTED_ARG || arg == AUTO_LAUNCH_ARG)
}

#[cfg(target_os = "windows")]
fn has_admin_restart_marker() -> bool {
    std::env::args().any(|arg| arg == ADMIN_RESTARTED_ARG)
}

#[cfg(target_os = "windows")]
fn restart_args() -> Vec<String> {
    let mut args = std::env::args()
        .skip(1)
        .filter(|arg| arg != ADMIN_RESTARTED_ARG)
        .collect::<Vec<_>>();
    args.push(ADMIN_RESTARTED_ARG.to_owned());
    args
}

#[cfg(target_os = "windows")]
fn env_dir() -> &'static str {
    if cfg!(dev) {
        DEV_ENV_DIR
    } else {
        PROD_ENV_DIR
    }
}

#[cfg(target_os = "windows")]
fn wide_null(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn autostart_launch_can_use_the_scheduled_task() {
        assert!(can_use_scheduled_task_for_args(args(&[])));
        assert!(can_use_scheduled_task_for_args(args(&["--auto-launch"])));
        assert!(can_use_scheduled_task_for_args(args(&[
            "--auto-launch",
            ADMIN_RESTARTED_ARG
        ])));
    }

    #[test]
    fn launches_with_other_args_keep_the_uac_path() {
        assert!(!can_use_scheduled_task_for_args(args(&[
            r"C:\Users\me\history.kwikpastebak"
        ])));
        assert!(!can_use_scheduled_task_for_args(args(&[
            "--auto-launch",
            "--unknown"
        ])));
    }

    #[test]
    fn task_xml_overrides_schtasks_defaults() {
        let xml = scheduled_task_xml(Path::new(r"C:\Program Files\KwikPaste\KwikPaste.exe"));

        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(xml.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<Priority>4</Priority>"));
        assert!(xml.contains(r"<Command>C:\Program Files\KwikPaste\KwikPaste.exe</Command>"));
        assert!(xml.contains("<Arguments>--kwikpaste-admin-restarted</Arguments>"));
        assert!(!xml.contains("<Triggers>"));
    }

    #[test]
    fn task_xml_escapes_the_executable_path() {
        let xml = scheduled_task_xml(Path::new(r"D:\Tools & <Apps>\KwikPaste.exe"));

        assert!(xml.contains(r"<Command>D:\Tools &amp; &lt;Apps&gt;\KwikPaste.exe</Command>"));
    }

    #[test]
    fn task_xml_file_is_utf16le_with_bom() {
        assert_eq!(
            utf16le_with_bom("<a/>"),
            [0xFF, 0xFE, b'<', 0, b'a', 0, b'/', 0, b'>', 0]
        );
    }
}
