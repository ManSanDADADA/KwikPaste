/**
 * 与 Rust `clipboard::storage::THUMBNAIL_MAX` 保持一致：列表缩略图的最长边像素。
 * 前端只用它为尚未生成的缩略图预留同尺寸占位，图片到达后布局不跳动。
 */
export const IMAGE_THUMBNAIL_MAX_EDGE = 300;
