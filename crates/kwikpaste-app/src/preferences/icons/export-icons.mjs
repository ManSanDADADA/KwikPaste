// 从 1.x 前端用的 iconify 图标包导出偏好窗用到的图标，写成同目录的 `<集合>-<名字>.svg`，
// 由 ../icons.rs 用 include_str! 内嵌、运行时经 `kwikpaste_ui::register_svg` 登记。
//
// 用法：仓库根目录装好前端依赖后运行 `node crates/kwikpaste-app/src/preferences/icons/export-icons.mjs`。
// 新增图标时把名字加进 ICONS，再在 ../icons.rs 的 PrefIcon 里加一项；跑完用 git diff 检查。
import { writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(
  join(here, "..", "..", "..", "..", "..", "package.json"),
);

/** `集合:名字`，与 1.x UnoCSS 的 `i-集合:名字` 类名一致。 */
const ICONS = [
  // 侧栏分类（1.x preferenceSchema 的 tab 图标）。
  "lucide:settings",
  "lucide:keyboard",
  "lucide:palette",
  "lucide:clipboard-plus",
  "lucide:panel-top",
  "lucide:clipboard-paste",
  "lucide:layers",
  "lucide:chart-pie",
  "lucide:database",
  "lucide:info",
  // 侧栏存储卡片、搜索、控件。
  "lucide:hard-drive",
  "lucide:search",
  "lucide:play",
  "lucide:check-circle-2",
  "lucide:loader-circle",
  "lucide:folder-open",
  "lucide:folder-sync",
  "lucide:puzzle",
  "lucide:rotate-ccw",
  "lucide:refresh-cw",
  "lucide:grip-vertical",
  "lucide:arrow-up",
  "lucide:arrow-down",
  "lucide:pencil",
  "lucide:trash-2",
  "lucide:plus",
  "lucide:circle-x",
  "lucide:triangle-alert",
  "lucide:sparkles",
  "lucide:eye-off",
  "lucide:chevron-down",
  // 采集类型（1.x captureKinds）。
  "lucide:files",
  "lucide:file-code-2",
  "lucide:file-image",
  "lucide:file-type",
  "lucide:clipboard-type",
  // 数据概览：卡片标题、数字卡、内容类别与标记。
  "lucide:chart-column",
  "lucide:shapes",
  "lucide:app-window",
  "lucide:circle-help",
  "lucide:folder-tree",
  "lucide:calendar-plus",
  "lucide:repeat",
  "lucide:history",
  "lucide:link",
  "lucide:mail",
  "lucide:star",
  "lucide:pin",
  "lucide:notebook-pen",
  "lucide:shield-alert",
  "lucide:inbox",
  // 侧栏收起、展开。
  "lucide:panel-left-close",
  "lucide:panel-left-open",
  // 快捷键页：触发方式、进入二级页、返回。
  "lucide:mouse",
  "lucide:mouse-pointer-2",
  "lucide:chevron-right",
  "lucide:arrow-left",
  // 侧栏「扩展」下的二级菜单：图片文字识别、微信输入法同步、翻译。
  "lucide:scan-text",
  "lucide:smartphone",
  "lucide:languages",
];

/** 取图标数据；别名取它指向的图标，带变换的别名不支持。 */
const resolve = (set, name, icon) => {
  const data = set.icons[name];
  if (data) {
    return data;
  }

  const alias = set.aliases?.[name];
  if (!alias) {
    throw new Error(`${icon} is missing`);
  }
  if (alias.rotate || alias.hFlip || alias.vFlip) {
    throw new Error(`${icon} is a transformed alias`);
  }

  return { ...resolve(set, alias.parent, icon), ...alias, parent: undefined };
};

for (const icon of ICONS) {
  const [prefix, name] = icon.split(":");
  const set = require(`@iconify-json/${prefix}/icons.json`);
  const data = resolve(set, name, icon);

  const width = data.width ?? set.width ?? 16;
  const height = data.height ?? set.height ?? 16;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">${data.body}</svg>\n`;

  writeFileSync(join(here, `${prefix}-${name}.svg`), svg);
  process.stdout.write(`${prefix}-${name}.svg\n`);
}
