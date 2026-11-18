// 从 1.x 前端用的 iconify 图标包导出原生版需要、而 gpui-kit-assets 默认图标集里没有的图标，
// 写成同目录的 `<集合>-<名字>.svg`，由 src/assets.rs 用 include_bytes! 内嵌。
//
// 用法：仓库根目录装好前端依赖后运行 `node crates/kwikpaste-ui/icons/export-icons.mjs`。
// 新增图标时把名字加进 ICONS，再在 src/icon.rs 的 IconName（或 src/assets.rs 的分组图标表）里加一项；
// 跑完用 git diff 检查。
import { writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(join(here, "..", "..", "..", "package.json"));

/** `集合:名字`，与 1.x UnoCSS 的 `i-集合:名字` 类名一致。 */
const ICONS = [
  "lucide:circle-check",
  "lucide:clipboard-paste",
  "lucide:clipboard-type",
  "lucide:copy-check",
  "lucide:eye-off",
  "lucide:file-symlink",
  "lucide:folder-open",
  "lucide:grip-vertical",
  "lucide:image-off",
  "lucide:key-round",
  "lucide:keyboard",
  "lucide:languages",
  "lucide:laptop",
  "lucide:list-checks",
  "lucide:mail",
  "lucide:monitor",
  "lucide:smartphone",
  "lucide:notebook-pen",
  "lucide:pencil",
  "lucide:scan-text",
  "lucide:settings-2",
  "lucide:square-arrow-out-up-right",
  "lucide:text-select",
  "lucide:trash",
  "lucide:trash-2",
  "lucide:triangle-alert",
  "ph:push-pin-bold",
  // 搜索框筛选。
  "lucide:list-filter",
  "lucide:filter-x",
  "lucide:clock",
  "lucide:app-window",
  "lucide:shapes",
  "lucide:link",
  "lucide:type",
  // 大图预览。
  "lucide:chevron-left",
  "lucide:chevron-right",
  "lucide:download",
  "lucide:rotate-cw",
  "lucide:scan-qr-code",
  "lucide:shrink",
  "lucide:zoom-in",
  "lucide:zoom-out",
  // 主窗口头部与分组栏。
  "lets-icons:file-dock",
  "lets-icons:folder-file-alt",
  "lets-icons:img-box",
  "lets-icons:pin",
  "lets-icons:setting-line",
  "lets-icons:widget",
  // 自定义分组的预设图标（1.x ClipboardGroupModal 的 PRESET_GROUP_ICONS）。
  "lets-icons:bell",
  "lets-icons:book",
  "lets-icons:bookmark",
  "lets-icons:box",
  "lets-icons:calendar",
  "lets-icons:code",
  "lets-icons:database",
  "lets-icons:folder",
  "lets-icons:link",
  "lets-icons:notebook",
  "lets-icons:star",
];

/** 取图标数据；别名（如 `more-horizontal` → `ellipsis`）取它指向的图标，带变换的别名不支持。 */
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
