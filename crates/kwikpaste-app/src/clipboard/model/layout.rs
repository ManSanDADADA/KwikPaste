//! 列表排布：列表风格与密度换算成的尺寸（1.x `useListLayout.ts`、`constants/listLayout.ts`），
//! 以及图片卡片的显示尺寸预测（1.x `ImageCard.tsx` 的 `resolvePlaceholderSize`）。
//!
//! 数值都是 1.x 的设计 px（CSS px）。视图层统一除以 16 换成 rem，所以和 1.x 在 WebView2 里一样
//! 随 Windows「文本大小」整体缩放。

/// 1.x 缩略图的最长边（Rust `THUMBNAIL_MAX` / 前端 `IMAGE_THUMBNAIL_MAX_EDGE`）。
pub const THUMBNAIL_MAX_EDGE: f32 = 300.;

/// 列表风格（设置 `clipboard.display.listStyle`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListStyle {
    #[default]
    Card,
    Seamless,
}

/// 列表密度（设置 `clipboard.display.density`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Density {
    Comfortable,
    #[default]
    Standard,
    Compact,
    Custom,
}

/// 自定义密度（设置 `clipboard.display.customLayout`），默认等于「标准」档。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustomLayout {
    pub header_row: bool,
    pub item_gap: u8,
    pub padding_y: u8,
}

impl Default for CustomLayout {
    fn default() -> Self {
        STANDARD
    }
}

const COMFORTABLE: CustomLayout = CustomLayout {
    header_row: true,
    item_gap: 12,
    padding_y: 8,
};
const STANDARD: CustomLayout = CustomLayout {
    header_row: true,
    item_gap: 8,
    padding_y: 6,
};
const COMPACT: CustomLayout = CustomLayout {
    header_row: false,
    item_gap: 4,
    padding_y: 4,
};

/// 自定义密度可选的条目间距与上下内边距（1.x `LIST_ITEM_GAP_VALUES` / `LIST_PADDING_Y_VALUES`）。
const ITEM_GAP_VALUES: [u8; 6] = [0, 2, 4, 6, 8, 12];
const PADDING_Y_VALUES: [u8; 4] = [2, 4, 6, 8];

/// 卡片内容相关的显示设置（`clipboard.display` 的其余几项）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayOptions {
    /// 文本最多显示几行，1.x 夹到 1–5。
    pub text_max_lines: u8,
    /// 图片最大显示高度（px）。
    pub image_max_height: u16,
}

impl Default for DisplayOptions {
    fn default() -> Self {
        Self {
            text_max_lines: 3,
            image_max_height: 64,
        }
    }
}

/// 换算好的条目排布，单位为设计 px。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutSpec {
    pub seamless: bool,
    /// 有头部行（来源图标、类型在正文上方，时间靠右）；没有时图标并入正文左侧，时间仍在右上角。
    pub header_row: bool,
    /// 条目上方的间距（卡片风格才有）。
    pub item_gap: f32,
    /// 条目左右外边距（卡片风格 12，无间风格 0）。
    pub item_padding_x: f32,
    /// 卡片左右内边距（卡片风格 8，无间风格 12）。
    pub card_padding_x: f32,
    pub card_padding_y: f32,
    /// 头部行高度：舒适档 24，其余 20。
    pub header_height: f32,
    /// 头部行与正文的间距：舒适档 4，其余 2；没有头部行时是图标与正文的横向间距 8。
    pub body_gap: f32,
    /// 选中外环画在内侧（无间风格，或条目间距放不下 2 px 外环时）。
    pub ring_inset: bool,
    pub text_max_lines: usize,
    pub image_max_height: f32,
}

/// 正文一行的高度（`text-sm` 的行高）。
pub const LINE_HEIGHT: f32 = 20.;
/// 卡片描边宽度。
pub const BORDER: f32 = 1.;

impl LayoutSpec {
    pub fn resolve(
        style: ListStyle,
        density: Density,
        custom: CustomLayout,
        display: DisplayOptions,
    ) -> Self {
        let comfortable = density == Density::Comfortable;
        let spec = match density {
            Density::Comfortable => COMFORTABLE,
            Density::Standard => STANDARD,
            Density::Compact => COMPACT,
            Density::Custom => custom,
        };
        let seamless = style == ListStyle::Seamless;
        let item_gap = f32::from(nearest(&ITEM_GAP_VALUES, spec.item_gap));
        let padding_y = f32::from(nearest(&PADDING_Y_VALUES, spec.padding_y));

        Self {
            seamless,
            header_row: spec.header_row,
            item_gap: if seamless { 0. } else { item_gap },
            item_padding_x: if seamless { 0. } else { 12. },
            card_padding_x: if seamless { 12. } else { 8. },
            card_padding_y: padding_y,
            header_height: if comfortable { 24. } else { 20. },
            body_gap: match (spec.header_row, comfortable) {
                (false, _) => 8.,
                (true, true) => 4.,
                (true, false) => 2.,
            },
            ring_inset: seamless || item_gap < 2.,
            text_max_lines: usize::from(display.text_max_lines.clamp(1, 5)),
            image_max_height: f32::from(display.image_max_height.max(1)),
        }
    }

    /// 设置里的列表排布（`clipboard.display` 的风格、密度、自定义尺寸、文本行数与图片高度）。
    pub fn from_settings(display: &kwikpaste_core::settings::Display) -> Self {
        use kwikpaste_core::settings::{ListDensity, ListStyle as SettingStyle};

        let style = match display.list_style {
            SettingStyle::Card => ListStyle::Card,
            SettingStyle::Seamless => ListStyle::Seamless,
        };
        let density = match display.density {
            ListDensity::Comfortable => Density::Comfortable,
            ListDensity::Standard => Density::Standard,
            ListDensity::Compact => Density::Compact,
            ListDensity::Custom => Density::Custom,
        };
        let custom = CustomLayout {
            header_row: display.custom_layout.header_row,
            item_gap: display.custom_layout.item_gap,
            padding_y: display.custom_layout.padding_y,
        };

        Self::resolve(
            style,
            density,
            custom,
            DisplayOptions {
                text_max_lines: display.text_max_lines,
                image_max_height: display.image_max_height,
            },
        )
    }

    /// 未加载行的骨架高度：照两行文本卡片画，标准档是 1.x 的 84 px。
    pub fn placeholder_height(&self) -> f32 {
        let body = 2. * LINE_HEIGHT;
        let content = if self.header_row {
            self.header_height + self.body_gap + body
        } else {
            body
        };
        let border = if self.seamless { BORDER } else { 2. * BORDER };

        self.item_gap + border + 2. * self.card_padding_y + content
    }
}

impl Default for LayoutSpec {
    fn default() -> Self {
        Self::resolve(
            ListStyle::default(),
            Density::default(),
            CustomLayout::default(),
            DisplayOptions::default(),
        )
    }
}

/// 手改配置文件写入档位以外的值时，按最接近的档位渲染（1.x `nearestValue`，相等时取靠前的）。
fn nearest(values: &[u8], value: u8) -> u8 {
    values
        .iter()
        .copied()
        .min_by_key(|candidate| candidate.abs_diff(value))
        .unwrap_or(value)
}

/// 图片卡片的显示框（设计 px）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageBox {
    pub width: f32,
    pub height: f32,
}

/// 按缩略图规则（最长边不超过 300）和最大显示高度，预测图片最终的显示尺寸。
///
/// 照搬 1.x `resolvePlaceholderSize`：尺寸未知时是 `max_height` 见方；结果四舍五入到整 px。
/// 解码前的骨架、解码后的图片都用这个尺寸，行高从头到尾不变（附录 D L6）。
pub fn predict_image_box(width: Option<u32>, height: Option<u32>, max_height: f32) -> ImageBox {
    let (Some(width), Some(height)) = (width.filter(|w| *w > 0), height.filter(|h| *h > 0)) else {
        return ImageBox {
            width: max_height.round(),
            height: max_height.round(),
        };
    };

    let (width, height) = (width as f32, height as f32);
    let scale = (THUMBNAIL_MAX_EDGE / width.max(height)).min(1.);
    let thumb_height = height * scale;
    let display_height = max_height.min(thumb_height);
    let display_width = width * scale * display_height / thumb_height;

    ImageBox {
        width: display_width.round().max(1.),
        height: display_height.round().max(1.),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_card_layout_matches_1x() {
        let spec = LayoutSpec::default();

        assert!(!spec.seamless);
        assert!(spec.header_row);
        assert_eq!(spec.item_gap, 8.);
        assert_eq!(spec.card_padding_y, 6.);
        assert_eq!(spec.header_height, 20.);
        assert_eq!(spec.body_gap, 2.);
        assert!(!spec.ring_inset);
        assert_eq!(spec.placeholder_height(), 84.);
    }

    #[test]
    fn densities_and_styles() {
        let display = DisplayOptions::default();
        let comfortable = LayoutSpec::resolve(
            ListStyle::Card,
            Density::Comfortable,
            CustomLayout::default(),
            display,
        );
        assert_eq!(comfortable.header_height, 24.);
        assert_eq!(comfortable.body_gap, 4.);
        assert_eq!(comfortable.item_gap, 12.);

        let compact = LayoutSpec::resolve(
            ListStyle::Card,
            Density::Compact,
            CustomLayout::default(),
            display,
        );
        assert!(!compact.header_row);
        assert_eq!(compact.body_gap, 8.);
        assert_eq!(compact.placeholder_height(), 4. + 2. + 8. + 40.);

        let seamless = LayoutSpec::resolve(
            ListStyle::Seamless,
            Density::Standard,
            CustomLayout::default(),
            display,
        );
        assert_eq!(seamless.item_gap, 0.);
        assert_eq!(seamless.card_padding_x, 12.);
        assert!(seamless.ring_inset);
    }

    #[test]
    fn settings_map_onto_the_layout() {
        let mut display = kwikpaste_core::settings::Display::default();
        assert_eq!(LayoutSpec::from_settings(&display), LayoutSpec::default());

        display.list_style = kwikpaste_core::settings::ListStyle::Seamless;
        display.density = kwikpaste_core::settings::ListDensity::Compact;
        display.text_max_lines = 2;
        let spec = LayoutSpec::from_settings(&display);
        assert!(spec.seamless);
        assert!(!spec.header_row);
        assert_eq!(spec.text_max_lines, 2);
    }

    #[test]
    fn custom_values_snap_to_the_nearest_step() {
        let spec = LayoutSpec::resolve(
            ListStyle::Card,
            Density::Custom,
            CustomLayout {
                header_row: true,
                item_gap: 1,
                padding_y: 7,
            },
            DisplayOptions {
                text_max_lines: 9,
                image_max_height: 64,
            },
        );

        // 1 离 0 和 2 一样近，取靠前的 0（与 1.x reduce 的比较方式一致）。
        assert_eq!(spec.item_gap, 0.);
        assert!(spec.ring_inset);
        assert_eq!(spec.card_padding_y, 6.);
        assert_eq!(spec.text_max_lines, 5);
    }

    #[test]
    fn image_box_follows_image_card_tsx() {
        // 宽图：缩略图 300×150，显示高 64，宽 128。
        assert_eq!(
            predict_image_box(Some(1200), Some(600), 64.),
            ImageBox {
                width: 128.,
                height: 64.
            }
        );
        // 竖图：缩略图 150×300，显示 32×64。
        assert_eq!(
            predict_image_box(Some(600), Some(1200), 64.),
            ImageBox {
                width: 32.,
                height: 64.
            }
        );
        // 很矮的图不放大：缩略图 300×20，显示 300×20。
        assert_eq!(
            predict_image_box(Some(3000), Some(200), 64.),
            ImageBox {
                width: 300.,
                height: 20.
            }
        );
        // 小图保持原尺寸：40×30。
        assert_eq!(
            predict_image_box(Some(40), Some(30), 64.),
            ImageBox {
                width: 40.,
                height: 30.
            }
        );
        // 尺寸未知：max_height 见方。
        assert_eq!(
            predict_image_box(None, Some(10), 64.),
            ImageBox {
                width: 64.,
                height: 64.
            }
        );
    }
}
