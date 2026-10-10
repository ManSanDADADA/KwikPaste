//! Windows 的 exe 资源：图标组与 VERSIONINFO。macOS 不需要（图标与版本在 .app 的 Info.plist 里）。
//!
//! - 图标组必须是 ID 1：GPUI 用 `LoadImageW(module, 1, IMAGE_ICON)` 取窗口类图标，取不到时静默不显示；
//!   卸载项、快捷方式、`.kwikpastebak` 文件关联取的是第 0 个图标组，只有这一个组时也是它。
//! - 不嵌 manifest：GPUI 自己已经嵌了 RT_MANIFEST（ID 1），再嵌一份会冲突。
//! - Windows 的 release 构建必须带 `+crt-static`：便携版只有一个 exe，不能依赖 VC++ 运行库。
//!   这里直接让构建失败，免得漏设 `CARGO_TARGET_<triple>_RUSTFLAGS` 时悄悄产出一个依赖运行库的 exe。

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

const ICON: &str = "assets/icons/icon.ico";
const COMPANY: &str = "fastthree";
const COPYRIGHT: &str = "Copyright 2026 FastThree";
const BINARY: &str = "KwikPaste";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICON}");

    // 裁掉没有引用的框架对应的 load command。
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-dead_strip_dylibs");
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    require_static_crt();
    embed_resources();
}

/// release 构建缺 `+crt-static` 时直接失败。
fn require_static_crt() {
    if env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }
    let features = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    if features.split(',').any(|feature| feature == "crt-static") {
        return;
    }

    panic!(
        "Windows release builds of KwikPaste must link the C runtime statically. Set \
         CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS / CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_RUSTFLAGS \
         to `-C target-feature=+crt-static` (not RUSTFLAGS, and not in .cargo/config.toml: that would \
         also change how src-tauri links)."
    );
}

fn embed_resources() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let production = env::var_os("CARGO_FEATURE_PRODUCTION_IDENTITY").is_some();

    let rc = resource_script(
        &manifest_dir.join(ICON).display().to_string(),
        &version,
        production,
    );
    let rc_path = out_dir.join("kwikpaste.rc");
    fs::write(&rc_path, rc).expect("failed to write kwikpaste.rc");

    // 找不到 rc.exe 时 embed-resource 只会跳过；这里要求必须编进去。
    embed_resource::compile_for(&rc_path, [BINARY], embed_resource::NONE)
        .manifest_required()
        .expect("failed to compile the Windows resources");
}

/// 资源脚本。数字版本是 `MAJOR,MINOR,PATCH,0`，字符串 `ProductVersion` 写完整 semver（含预发布后缀）。
fn resource_script(icon: &str, version: &str, production: bool) -> String {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts: Vec<u16> = core
        .split('.')
        .map(|part| part.parse().expect("CARGO_PKG_VERSION is not semver"))
        .collect();
    parts.resize(4, 0);
    let numeric = parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let dotted = parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(".");
    // 开发构建在任务管理器里标出来，免得和本机正在运行的 1.x 混淆。
    let description = if production {
        "KwikPaste"
    } else {
        "KwikPaste (native dev)"
    };

    let strings = [
        ("CompanyName", COMPANY),
        ("FileDescription", description),
        ("FileVersion", dotted.as_str()),
        ("InternalName", BINARY),
        ("LegalCopyright", COPYRIGHT),
        ("OriginalFilename", "KwikPaste.exe"),
        ("ProductName", "KwikPaste"),
        ("ProductVersion", version),
    ];

    let mut rc = String::new();
    let _ = writeln!(rc, "1 ICON \"{}\"", rc_escape(icon));
    let _ = writeln!(rc);
    let _ = writeln!(rc, "1 VERSIONINFO");
    let _ = writeln!(rc, "FILEVERSION {numeric}");
    let _ = writeln!(rc, "PRODUCTVERSION {numeric}");
    let _ = writeln!(rc, "FILEFLAGSMASK 0x3fL");
    let _ = writeln!(rc, "FILEFLAGS 0x0L");
    let _ = writeln!(rc, "FILEOS 0x40004L");
    let _ = writeln!(rc, "FILETYPE 0x1L");
    let _ = writeln!(rc, "FILESUBTYPE 0x0L");
    let _ = writeln!(rc, "BEGIN");
    let _ = writeln!(rc, "  BLOCK \"StringFileInfo\"");
    let _ = writeln!(rc, "  BEGIN");
    let _ = writeln!(rc, "    BLOCK \"040904b0\"");
    let _ = writeln!(rc, "    BEGIN");
    for (key, value) in strings {
        let _ = writeln!(rc, "      VALUE \"{key}\", \"{}\"", rc_escape(value));
    }
    let _ = writeln!(rc, "    END");
    let _ = writeln!(rc, "  END");
    let _ = writeln!(rc, "  BLOCK \"VarFileInfo\"");
    let _ = writeln!(rc, "  BEGIN");
    let _ = writeln!(rc, "    VALUE \"Translation\", 0x409, 1200");
    let _ = writeln!(rc, "  END");
    let _ = writeln!(rc, "END");
    rc
}

/// rc.exe 的字符串字面量：反斜杠要写两次，双引号写成两个双引号。
fn rc_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\"\"")
}
