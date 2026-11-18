//! 合成数据：确定性的列表条目生成器和它用到的 PNG（缩略图、原图、应用图标、文件图标）。
//!
//! 只写到临时目录，绝不读本机 1.x 或开发版的数据库与图片。同一个下标每次生成的内容相同，
//! 跑分和截图可以复现。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context as _;
use chrono::{DateTime, Duration, TimeZone as _, Utc};
use kwikpaste_core::clipboard::{QuickSnippet, SnippetKind};

use crate::clipboard::model::item::{
    FileRow, FilesPreview, ItemAction, ItemKind, ListItem, Platform, SubKind,
};

/// 合成资源的默认目录：`<临时目录>/kwikpaste-native-fixtures`。
pub fn default_assets_dir() -> PathBuf {
    std::env::temp_dir().join("kwikpaste-native-fixtures")
}

/// 一张合成图片：文件名（记录的 `content`）、原图尺寸、缩略图路径。
#[derive(Clone, Debug)]
pub struct SyntheticImage {
    pub file_name: Arc<str>,
    pub width: u32,
    pub height: u32,
    pub thumbnail: Arc<str>,
}

/// 生成器用到的全部资源。
#[derive(Clone, Debug)]
pub struct AssetSet {
    pub root: PathBuf,
    pub images: Vec<SyntheticImage>,
    /// 原图（单图文件记录直接指向它们）。
    pub originals: Vec<SyntheticImage>,
    pub app_icons: Vec<Arc<str>>,
    pub file_icons: Vec<Arc<str>>,
}

impl AssetSet {
    /// 缩略图目录：夹具数据源按文件名在这里找缩略图。
    pub fn thumbnails_dir(&self) -> PathBuf {
        self.root.join("thumbs")
    }
}

/// 示例夹具（`fixtures/list-sample.json`）引用的固定图片：文件名与原图尺寸，缩略图按 core 规则生成。
const SAMPLE_IMAGES: &[(&str, u32, u32)] = &[
    ("sample-wide.png", 1920, 1080),
    ("sample-tall.png", 900, 1600),
    ("sample-strip.png", 2400, 160),
];
/// 示例夹具里单图文件记录指向的原图。
const SAMPLE_ORIGINAL: (&str, u32, u32) = ("sample-photo.png", 1600, 1000);

const IMAGE_COUNT: u64 = 48;
const ORIGINAL_COUNT: u64 = 4;
const APP_ICON_COUNT: u64 = 12;
const FILE_ICON_COUNT: u64 = 8;
const THUMBNAIL_MAX_EDGE: u32 = 300;

/// 生成（已存在就复用）合成资源。首次在 debug 构建里要几秒，之后直接复用。
pub fn ensure_assets(root: &Path) -> anyhow::Result<AssetSet> {
    let thumbs = root.join("thumbs");
    let originals_dir = root.join("originals");
    let icons = root.join("icons");
    for dir in [&thumbs, &originals_dir, &icons] {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }

    let mut images = Vec::new();
    for index in 0..IMAGE_COUNT {
        let seed = mix(index ^ 0xA11CE);
        let (width, height) = original_size(seed);
        let (thumb_width, thumb_height) = thumbnail_size(width, height);
        let file_name = format!("img-{index:03}.png");
        let path = thumbs.join(&file_name);
        write_png(&path, thumb_width, thumb_height, seed)?;
        images.push(SyntheticImage {
            file_name: file_name.into(),
            width,
            height,
            thumbnail: path_text(&path),
        });
    }

    for (index, (file_name, width, height)) in SAMPLE_IMAGES.iter().enumerate() {
        let (thumb_width, thumb_height) = thumbnail_size(*width, *height);
        write_png(
            &thumbs.join(file_name),
            thumb_width,
            thumb_height,
            mix(index as u64 ^ 0x5A3),
        )?;
    }
    let (file_name, width, height) = SAMPLE_ORIGINAL;
    write_png(&originals_dir.join(file_name), width, height, mix(0x0516))?;

    let mut originals = Vec::new();
    for index in 0..ORIGINAL_COUNT {
        let seed = mix(index ^ 0x0516);
        let (width, height) = (1200 + (seed % 5) as u32 * 120, 800);
        let file_name = format!("photo-{index}.png");
        let path = originals_dir.join(&file_name);
        write_png(&path, width, height, seed)?;
        originals.push(SyntheticImage {
            file_name: file_name.into(),
            width,
            height,
            thumbnail: path_text(&path),
        });
    }

    let mut app_icons = Vec::new();
    for index in 0..APP_ICON_COUNT {
        let path = icons.join(format!("app-{index:02}.png"));
        write_png(&path, 32, 32, mix(index ^ 0xB0B))?;
        app_icons.push(path_text(&path));
    }

    let mut file_icons = Vec::new();
    for index in 0..FILE_ICON_COUNT {
        let path = icons.join(format!("file-{index:02}.png"));
        write_png(&path, 40, 40, mix(index ^ 0xF11E))?;
        file_icons.push(path_text(&path));
    }

    Ok(AssetSet {
        root: root.to_path_buf(),
        images,
        originals,
        app_icons,
        file_icons,
    })
}

fn path_text(path: &Path) -> Arc<str> {
    Arc::from(path.to_string_lossy().as_ref())
}

/// 原图尺寸：横图、竖图、方图、细长图都有。
fn original_size(seed: u64) -> (u32, u32) {
    let long = 400 + (seed % 3200) as u32;
    let short = 24 + ((seed >> 12) % 1400) as u32;
    match seed % 5 {
        0 => (short.min(long), long),
        1 => (long, long),
        _ => (long, short.min(long)),
    }
}

/// core 生成缩略图的规则：最长边缩到 300，不放大。
fn thumbnail_size(width: u32, height: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= THUMBNAIL_MAX_EDGE {
        return (width, height);
    }

    let scale = THUMBNAIL_MAX_EDGE as f32 / longest as f32;
    (
        ((width as f32 * scale).round() as u32).max(1),
        ((height as f32 * scale).round() as u32).max(1),
    )
}

/// 写一张条纹渐变 PNG；文件已存在且尺寸一致时跳过。
fn write_png(path: &Path, width: u32, height: u32, seed: u64) -> anyhow::Result<()> {
    if image::image_dimensions(path).ok() == Some((width, height)) {
        return Ok(());
    }

    let base = [
        (seed % 200) as u8 + 30,
        ((seed >> 8) % 200) as u8 + 30,
        ((seed >> 16) % 200) as u8 + 30,
    ];
    let stripe = 6 + (seed % 10) as u32;
    let pixels = image::RgbaImage::from_fn(width, height, |x, y| {
        let t = (x as f32 / width.max(1) as f32 + y as f32 / height.max(1) as f32) * 0.5;
        let band = if ((x + y) / stripe).is_multiple_of(2) {
            24
        } else {
            0
        };
        let [r, g, b] = base;
        image::Rgba([
            (f32::from(r) * (1. - t) + 30. * t) as u8 + band,
            (f32::from(g) * t + 20.) as u8,
            b.saturating_sub(band),
            255,
        ])
    });
    pixels
        .save(path)
        .with_context(|| format!("could not write {}", path.display()))
}

/// splitmix64：下标到伪随机数的确定映射。
pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn pick<T: Clone>(items: &[T], seed: u64) -> Option<T> {
    let len = u64::try_from(items.len()).ok()?;
    if len == 0 {
        return None;
    }

    items.get(usize::try_from(seed % len).ok()?).cloned()
}

const ZH: &[&str] = &[
    "会议纪要：下周一上午十点讨论剪贴板同步方案，请提前准备数据",
    "把这段文字复制到周报里",
    "请在今天下班前确认发布清单，并同步给测试同学",
    "收货地址：上海市浦东新区世纪大道 100 号 20 楼",
    "这个接口的返回值需要再核对一下，尤其是分页字段",
    "设计稿已经更新，请查看最新版本",
    "今晚八点线上评审，记得带上数据",
    "中文与 English 混排的剪贴板条目",
];
const EN: &[&str] = &[
    "cargo build --release --target x86_64-pc-windows-msvc",
    "The quick brown fox jumps over the lazy dog, again and again until the line wraps",
    "SELECT id, content, updated_at FROM clipboard_items ORDER BY updated_at DESC LIMIT 50;",
    "Remember to rotate the signing key before the next release",
    "fn main() { println!(\"hello, world\"); }",
    "Order #48213 shipped, tracking code 1Z999AA10123456784",
];
const APP_NAMES: &[&str] = &[
    "Editor", "Browser", "Terminal", "微信", "Mail", "Notes", "Figma", "Slack", "Excel", "Word",
    "Finder", "VS Code",
];
const NOTES: &[&str] = &[
    "发布前要用的命令",
    "客户的收货地址",
    "Review comments for the list rewrite",
    "下周复盘时再看",
];
const COLORS: &[&str] = &[
    "#1677ff",
    "#52c41a",
    "rgb(255 136 0)",
    "hsl(280, 60%, 55%)",
    "#ff4d4f",
    "rgba(250, 173, 20, 0.6)",
];
const FILE_NAMES: &[&str] = &[
    "项目计划书_final_v3.docx",
    "screenshot-2026-09-30.png",
    "design-tokens.json",
    "季度报表.xlsx",
    "README.md",
    "安装包",
];

/// 生成器选项。
#[derive(Clone, Copy, Debug)]
pub struct GenerateOptions {
    pub rows: usize,
    /// 开头的置顶条数。
    pub pinned: usize,
    /// 一部分图片记录不带缩略图路径，卡片要先向数据源要（走骨架和后台生成）。
    pub missing_thumbnails: bool,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            rows: 10_000,
            pinned: 2,
            missing_thumbnails: true,
        }
    }
}

/// 最新一条的创建时间；往后每条早 7 分钟，覆盖今天、今年、往年三种时间标签。
fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 2, 8, 0, 0)
        .single()
        .unwrap_or_default()
}

/// 生成 `options.rows` 条记录，顺序即列表顺序（置顶在前，与 core 的 `is_pinned DESC` 一致）。
pub fn generate(assets: &AssetSet, options: GenerateOptions) -> Vec<Arc<ListItem>> {
    (0..options.rows)
        .map(|index| Arc::new(item(assets, index, options)))
        .collect()
}

/// 第 `index` 条记录。
pub fn item(assets: &AssetSet, index: usize, options: GenerateOptions) -> ListItem {
    let id = index as u64;
    let r = mix(id);
    let r2 = mix(r);
    let r3 = mix(r2);
    let pinned = index < options.pinned;

    let (kind, sub_kind) = match r % 100 {
        0..=54 => (ItemKind::Text, None),
        55..=61 => (ItemKind::Text, Some(SubKind::Url)),
        62..=63 => (ItemKind::Text, Some(SubKind::Email)),
        64..=67 => (ItemKind::Text, Some(SubKind::Color)),
        68..=69 => (ItemKind::Text, Some(SubKind::Path)),
        70..=72 => (ItemKind::Text, Some(SubKind::Html)),
        73..=74 => (ItemKind::Text, Some(SubKind::Rtf)),
        75..=87 => (ItemKind::Image, None),
        _ => (ItemKind::Files, None),
    };
    let is_sensitive = kind == ItemKind::Text && sub_kind.is_none() && r3.is_multiple_of(40);

    let summary = match (kind, sub_kind) {
        (ItemKind::Text, None) if is_sensitive => {
            Some("sk-l****************************7Qx2".to_owned())
        }
        (ItemKind::Text, None) => Some(paragraph(r2, index)),
        (ItemKind::Text, Some(SubKind::Url)) => Some(format!(
            "https://example.com/items/{index}?ref=clipboard&page={}",
            r2 % 97
        )),
        (ItemKind::Text, Some(SubKind::Email)) => Some(format!("user{}@example.com", r2 % 1000)),
        (ItemKind::Text, Some(SubKind::Color)) => pick(COLORS, r2).map(str::to_owned),
        (ItemKind::Text, Some(SubKind::Path)) => Some(format!(
            "C:/Users/demo/Documents/{}",
            pick(FILE_NAMES, r2).unwrap_or_default()
        )),
        (ItemKind::Text, Some(SubKind::Html | SubKind::Rtf)) => Some(paragraph(r2 >> 3, index)),
        _ => None,
    };

    let image = (kind == ItemKind::Image)
        .then(|| pick(&assets.images, r2))
        .flatten();
    let (content, width, height, thumbnail) = match &image {
        Some(image) => {
            let missing = options.missing_thumbnails && r3.is_multiple_of(3);
            (
                image.file_name.clone(),
                Some(image.width),
                Some(image.height),
                (!missing).then(|| image.thumbnail.clone()),
            )
        }
        None => (Arc::from(""), None, None, None),
    };

    let (file_entries, files_preview_kind) = if kind == ItemKind::Files {
        files(assets, r2, r3)
    } else {
        (None, None)
    };

    let synced = !pinned && r3.is_multiple_of(23);
    let app = (!synced).then(|| r % APP_NAMES.len() as u64);
    let note = (kind == ItemKind::Text && (r3 >> 16) % 100 < 6)
        .then(|| pick(NOTES, r3).map(Arc::from))
        .flatten();
    let quick_snippets =
        if kind == ItemKind::Text && sub_kind.is_none() && !is_sensitive && (r3 >> 8) % 100 < 30 {
            vec![
                QuickSnippet {
                    text: format!("{}", 100_000 + mix(r3) % 900_000),
                    kind: SnippetKind::Number,
                },
                QuickSnippet {
                    text: "1Z999AA10123456784".to_owned(),
                    kind: SnippetKind::Number,
                },
                QuickSnippet {
                    text: "example.com/abc".to_owned(),
                    kind: SnippetKind::Link,
                },
            ]
        } else {
            Vec::new()
        };

    ListItem {
        id: Arc::from(format!("syn-{index:06}")),
        kind,
        sub_kind,
        group_id: None,
        content,
        summary: summary.map(Arc::from),
        width,
        height,
        is_favorite: pinned || r2.is_multiple_of(10),
        is_pinned: pinned,
        is_sensitive,
        platform: if r.is_multiple_of(2) {
            Platform::Windows
        } else {
            Platform::Macos
        },
        note,
        created_at: base_time() - Duration::minutes(7 * index as i64),
        source_app_id: app.map(|app| Arc::from(format!("app.synthetic.{app}"))),
        source_app_name: app.and_then(|app| pick(APP_NAMES, app).map(Arc::from)),
        source_app_icon_path: app.and_then(|app| pick(&assets.app_icons, app)),
        origin_device_id: synced.then(|| Arc::from("device-synthetic")),
        origin_device_name: synced.then(|| Arc::from("Pixel 9")),
        image_thumbnail_path: thumbnail,
        file_entries,
        files_preview_kind,
        available_actions: actions(kind, sub_kind, is_sensitive),
        color_preview: (sub_kind == Some(SubKind::Color))
            .then(|| summary_color(r2))
            .flatten(),
        quick_snippets,
        image_display: None,
        has_image_text: false,
        image_text_snippet: None,
        translation: None,
        translating: false,
    }
}

fn summary_color(seed: u64) -> Option<Arc<str>> {
    pick(COLORS, seed).map(Arc::from)
}

/// 1–4 行中英混排文本，行之间是硬换行（摘要保留换行）。
fn paragraph(seed: u64, index: usize) -> String {
    let lines = 1 + (seed % 4) as usize;
    let mut text = format!("#{index} ");
    for line in 0..lines {
        if line > 0 {
            text.push('\n');
        }
        let s = mix(seed ^ line as u64);
        let source = if s.is_multiple_of(2) { ZH } else { EN };
        text.push_str(pick(source, s >> 1).unwrap_or_default());
    }
    text
}

fn files(assets: &AssetSet, r2: u64, r3: u64) -> (Option<Vec<FileRow>>, Option<FilesPreview>) {
    // 约 1/6 的文件记录是单个存在的图片：按图片卡片显示原图。
    if r3.is_multiple_of(6)
        && let Some(original) = pick(&assets.originals, r2)
    {
        let row = FileRow {
            path: original.thumbnail.clone(),
            name: original.file_name.clone(),
            is_dir: false,
            is_image: true,
            exists: true,
            modified: None,
            icon_path: pick(&assets.file_icons, r2),
            width: Some(original.width),
            height: Some(original.height),
        };
        return (Some(vec![row]), Some(FilesPreview::ImagePreview));
    }

    let count = 1 + (r2 % 3) as usize;
    let rows = (0..count)
        .map(|k| {
            let seed = mix(r2 ^ k as u64);
            let name = pick(FILE_NAMES, seed).unwrap_or_default();
            FileRow {
                path: Arc::from(format!("C:/Users/demo/Desktop/{name}")),
                name: Arc::from(name),
                is_dir: name == "安装包",
                is_image: name.ends_with(".png"),
                exists: !(seed >> 4).is_multiple_of(7),
                modified: None,
                icon_path: pick(&assets.file_icons, seed),
                width: None,
                height: None,
            }
        })
        .collect();

    (Some(rows), Some(FilesPreview::List))
}

/// 与 core `compute_available_actions` 同样的取舍（不含平台差异的细节）。
fn actions(kind: ItemKind, sub_kind: Option<SubKind>, is_sensitive: bool) -> Vec<ItemAction> {
    let mut actions = vec![ItemAction::Paste];
    match kind {
        ItemKind::Text => actions.push(ItemAction::PasteAsPlainText),
        ItemKind::Files => actions.push(ItemAction::PasteAsPath),
        ItemKind::Image => {}
    }
    actions.push(ItemAction::Copy);
    if kind == ItemKind::Image {
        actions.push(ItemAction::SaveImage);
    }
    if kind == ItemKind::Text && !is_sensitive {
        actions.push(ItemAction::SplitWords);
    }
    match sub_kind {
        Some(SubKind::Url) => actions.push(ItemAction::OpenLink),
        Some(SubKind::Email) => actions.push(ItemAction::SendEmail),
        Some(SubKind::Path) => actions.push(ItemAction::RevealInExplorer),
        _ if kind == ItemKind::Files => actions.push(ItemAction::RevealInExplorer),
        _ => {}
    }
    actions.extend([
        ItemAction::ToggleFavorite,
        ItemAction::TogglePinned,
        ItemAction::EditNote,
        ItemAction::Select,
        ItemAction::Delete,
    ]);
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> AssetSet {
        AssetSet {
            root: PathBuf::from("/fixtures"),
            images: (0..4)
                .map(|index| SyntheticImage {
                    file_name: Arc::from(format!("img-{index}.png")),
                    width: 1200,
                    height: 600,
                    thumbnail: Arc::from(format!("/fixtures/thumbs/img-{index}.png")),
                })
                .collect(),
            originals: Vec::new(),
            app_icons: vec![Arc::from("/fixtures/icons/app.png")],
            file_icons: vec![Arc::from("/fixtures/icons/file.png")],
        }
    }

    #[test]
    fn generation_is_deterministic_and_covers_every_kind() {
        let assets = assets();
        let options = GenerateOptions {
            rows: 600,
            ..GenerateOptions::default()
        };
        let first = generate(&assets, options);
        let second = generate(&assets, options);
        assert_eq!(first, second);

        let has = |predicate: &dyn Fn(&ListItem) -> bool| first.iter().any(|item| predicate(item));
        assert!(has(&|item| item.kind == ItemKind::Image));
        assert!(has(
            &|item| item.kind == ItemKind::Image && item.image_thumbnail_path.is_none()
        ));
        assert!(has(&|item| item.kind == ItemKind::Files));
        assert!(has(
            &|item| item.sub_kind == Some(SubKind::Color) && item.color_preview.is_some()
        ));
        assert!(has(&|item| item.note.is_some()));
        assert!(has(&|item| item.shows_sensitive_mark()));
        assert!(has(&|item| item.origin_device_name.is_some()));
        assert!(has(&|item| !item.quick_snippets.is_empty()));

        assert!(first.iter().take(2).all(|item| item.is_pinned));
        assert!(first.iter().skip(2).all(|item| !item.is_pinned));
        assert!(first.windows(2).all(|pair| match pair {
            [newer, older] => newer.created_at > older.created_at,
            _ => true,
        }));
    }

    #[test]
    fn thumbnails_follow_the_core_rule() {
        assert_eq!(thumbnail_size(1200, 600), (300, 150));
        assert_eq!(thumbnail_size(200, 900), (67, 300));
        assert_eq!(thumbnail_size(120, 40), (120, 40));
    }
}
