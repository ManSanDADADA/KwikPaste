//! 隔离的 OCR 内存测量；图片生成也在一次性子进程里，主进程只持有路径。
use kwikpaste_core::{
    AppEnv, AppInfo, Core, CoreEvent, CoreOptions, CorePaths, CoreRuntime,
    clipboard::MemoryClipboard,
    db::models::{ClipboardItem, ClipboardKind, Platform},
};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const ARG: &str = "--selftest-ocr-memory";
const GENERATE: &str = "--selftest-ocr-generate=";
const SIZES: [(u32, u32); 3] = [(1920, 1080), (2560, 1440), (3840, 2160)];

/// 仅自测开关和环境变量同时出现时执行；不建托盘、监听、系统剪贴板或生产身份目录。
pub fn run_if_requested() -> anyhow::Result<bool> {
    if std::env::var("KWIKPASTE_SELFTEST").as_deref() != Ok("1") {
        return Ok(false);
    }
    let args: Vec<_> = std::env::args().collect();
    if let Some(root) = args.iter().find_map(|arg| arg.strip_prefix(GENERATE)) {
        generate(std::path::Path::new(root))?;
        return Ok(true);
    }
    if !args.iter().any(|arg| arg == ARG) {
        return Ok(false);
    }
    let on = match args.iter().find_map(|arg| arg.strip_prefix("--ocr-mode=")) {
        Some("on") => true,
        Some("off") => false,
        _ => anyhow::bail!("use --ocr-mode=off or --ocr-mode=on"),
    };
    let temp = tempfile::Builder::new()
        .prefix("kwikpaste-selftest-ocr-memory-")
        .tempdir()?;
    let runtime = CoreRuntime::new()?;
    let local = temp
        .path()
        .join("com.fastthree.kwikpaste.selftest-ocr-memory");
    let paths = CorePaths::new(AppEnv::Dev, local.clone(), local.join("logs"), None);
    let (sender, receiver) = mpsc::channel();
    let core = runtime.handle().block_on(Core::start(
        AppInfo {
            name: "KwikPaste OCR selftest",
            identifier: "com.fastthree.kwikpaste.selftest-ocr-memory",
            version: semver::Version::new(2, 0, 0),
            env: AppEnv::Dev,
        },
        paths,
        CoreOptions {
            fixture_apps: true,
            ..Default::default()
        },
        Arc::new(move |event: CoreEvent| {
            if matches!(event, CoreEvent::OcrChanged) {
                let _ = sender.send(());
            }
        }),
        runtime.handle(),
    ))?;
    core.set_clipboard_provider(Arc::new(MemoryClipboard::new()));
    let example = core.image_store().origin_path(&file_name(0));
    let origin = example
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| anyhow::anyhow!("invalid image root"))?;
    use std::os::windows::process::CommandExt as _;
    let status = std::process::Command::new(std::env::current_exe()?)
        .arg(format!("{GENERATE}{}", origin.display()))
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .status()?;
    if !status.success() {
        anyhow::bail!("fixture generator failed: {status}");
    }
    for index in 0..30 {
        let (width, height) = dimensions(index);
        let name = file_name(index);
        let now = chrono::Utc::now() + chrono::Duration::milliseconds(index as i64);
        let item = ClipboardItem {
            id: format!("ocr-memory-{index}"),
            kind: ClipboardKind::Image,
            sub_kind: None,
            group_id: None,
            source_app_id: None,
            content_hash: format!("ocr-measurement-{index}"),
            content: name,
            search_text: None,
            summary: None,
            file_types: None,
            size: None,
            width: Some(width.into()),
            height: Some(height.into()),
            use_count: 1,
            is_favorite: false,
            is_pinned: false,
            is_sensitive: false,
            platform: Platform::Windows,
            note: None,
            created_at: now,
            updated_at: now,
            origin_device_id: None,
            source_app_name: None,
            source_app_icon_file: None,
        };
        runtime.handle().block_on(core.store_item(item, None))?;
    }
    // 两种模式都走相同设置写入和 status 查询，避免把测量驱动的页缓存当成 OCR 的增量。
    #[cfg(debug_assertions)]
    if on {
        runtime.handle().block_on(core.install_extension_from_dev(
            "ocr",
            "1.0.0",
            kwikpaste_core::extensions::OCR_PROTOCOL,
        ))?;
    }
    #[cfg(not(debug_assertions))]
    if on {
        let sibling = std::env::current_exe()?
            .with_file_name(kwikpaste_core::extensions::executable_name("ocr")?);
        let staged = tempfile::NamedTempFile::new()?;
        std::fs::copy(sibling, staged.path())?;
        runtime.handle().block_on(core.install_extension(
            "ocr",
            "1.0.0",
            kwikpaste_core::extensions::OCR_PROTOCOL,
            staged.path(),
        ))?;
    }
    if on {
        let support = runtime.handle().block_on(core.ocr_support())?;
        if !matches!(support, kwikpaste_core::OcrSupport::Available { .. }) {
            anyhow::bail!("OCR unavailable: {support:?}");
        }
        let deadline = Instant::now() + Duration::from_secs(1800);
        loop {
            let status = runtime.handle().block_on(core.ocr_status())?;
            if !status.running && status.pending == 0 {
                if status.recognized != 30 || status.with_text != 30 {
                    anyhow::bail!("recognition acceptance failed: {status:?}");
                }
                break;
            }
            if Instant::now() >= deadline {
                anyhow::bail!("OCR queue did not drain: {status:?}");
            }
            receiver.recv_timeout(Duration::from_secs(60))?;
        }
    } else {
        let status = runtime.handle().block_on(core.ocr_status())?;
        if status.running || status.recognized != 0 || status.pending != 30 {
            anyhow::bail!("disabled OCR performed work: {status:?}");
        }
    }
    std::thread::sleep(Duration::from_secs(20));
    let (working_set, private_usage) = process_memory()?;
    println!(
        "{}",
        serde_json::json!({"mode":if on {"on-after-drain"} else {"off"},"images":30,"settle_seconds":20,"PrivateWorkingSetSize":working_set,"PrivateUsage":private_usage,"HelperPeakPrivateUsage":core.ocr_helper_peak_private_usage(),"data_dir":temp.path()})
    );
    runtime.handle().block_on(core.shutdown())?;
    Ok(true)
}

fn file_name(index: usize) -> String {
    format!("{index:064x}.png")
}
fn dimensions(index: usize) -> (u32, u32) {
    if index == 29 {
        (1080, 8000)
    } else {
        SIZES[index % 3]
    }
}

/// 图像字节只在隔离生成进程中存在；两种测量模式使用同一确定性图集。
fn generate(root: &std::path::Path) -> anyhow::Result<()> {
    let text = image::load_from_memory(include_bytes!(
        "../../kwikpaste-ext-ocr/fixtures/ocr/chinese-english.png"
    ))?
    .to_rgba8();
    for index in 0..30 {
        let (width, height) = dimensions(index);
        let mut image =
            image::RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
        for y in (32..height.saturating_sub(220)).step_by(700) {
            image::imageops::overlay(&mut image, &text, 32, i64::from(y));
        }
        image.put_pixel(0, 0, image::Rgba([index as u8, 0, 0, 255]));
        let name = file_name(index);
        let path = root.join(&name[..2]).join(name);
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("invalid fixture path"))?,
        )?;
        image.save(path)?;
    }
    Ok(())
}

/// 仅读取当前自测进程的内存计数，不加载 OCR 库。
fn process_memory() -> anyhow::Result<(usize, usize)> {
    use windows_sys::Win32::System::{
        ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
        },
        Threading::GetCurrentProcess,
    };
    let mut memory: PROCESS_MEMORY_COUNTERS_EX2 = unsafe { std::mem::zeroed() };
    memory.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
    if unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut memory).cast::<PROCESS_MEMORY_COUNTERS>(),
            memory.cb,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((memory.PrivateWorkingSetSize, memory.PrivateUsage))
}
