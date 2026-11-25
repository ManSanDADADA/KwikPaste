# KwikPaste 远程同步协议 v1

局域网同步之外的第二条路：设备通过一台同步服务器互通。服务器有两种运行方式，**同一个程序**：

- **官方服务器**（`mode = official`）：我们运行在 `https://sync.fastthree.com`。只做立即同步：
  文本留 24 小时给离线设备补收，图片强制压缩、只实时转发不存储，同步设置，不同步历史和收藏 / 置顶 / 删除。
- **自部署服务器**（`mode = selfhost`）：用户用 Docker 跑在自己的机器上，访问要带服务器令牌。图片质量和大小由用户选，
  可以开「同步全部历史」（全部记录 + 收藏 / 置顶 / 备注 / 删除）。

服务端代码在单独的仓库 `KwikPaste-sync-server`（它要在 Linux / Docker 上跑，不进本仓库）。改协议时先改这里，再同步两边。

## 1. 原则

- **端到端加密**：服务器只存转密文。它能看到的只有组、设备 id、条目 id（带密钥的哈希）、条目类别、时间、大小和 HLC 时间戳。
- **服务器是「加密的后写者赢存储 + 变更流」**：每个条目 id 只保留 HLC 最大的那一版，每次存储分到组内递增的序号 `seq`；
  客户端只记一个游标（已收齐的 `seq`）。
- **没有账号**：第一台设备建组，其他设备用配对码加入（和微信输入法一样）。
- **策略由服务器下发**（§4.1）：能不能传图片、怎么压缩、文本留多久、能不能同步历史，客户端按 `/v1/info` 的 `policy` 执行，
  官方服务器的参数以后不发版也能调。
- 和局域网共用同一套记录还原 / 入库流程（`engine.rs`）与设备身份（局域网身份的设备 id）。

## 2. 传输

- HTTP/1.1，客户端用 `reqwest`（native-tls + system-proxy，已经在安装包里）。上行用 POST，下行用一条长连接的 GET 流（§4.7），
  不用 WebSocket（省一个依赖）。官方服务器在 nginx 后面：`proxy_buffering off`、`proxy_read_timeout 600s`。
- 所有接口的路径都相对服务器地址，例如 `https://sync.fastthree.com/v1/info`。
- 请求头：
  - `X-KwikPaste-Client: KwikPaste/<应用版本> (<windows|macos>)`；
  - 成员接口带 `Authorization: Bearer <设备令牌>`；
  - 自部署服务器的**每个**请求都带 `X-KwikPaste-Server-Token: <服务器令牌>`（官方服务器忽略）。
- 错误：HTTP 状态码 + `{"error": "<code>", "message": "<给日志看的英文>"}`，429 / 503 带 `Retry-After`（秒）。

| `error` | 状态码 | 含义 | 客户端处理 |
| --- | --- | --- | --- |
| `badRequest` | 400 | 请求不合法 | 记日志 |
| `incompatible` | 400 | 协议版本不兼容 | 提示升级（哪边旧按 `/v1/info` 的范围判断） |
| `serverToken` | 401 | 自部署服务器令牌不对 | 提示「服务器令牌不正确」，停止重试 |
| `unauthorized` | 401 | 设备令牌无效（被移出组 / 组已删除） | 清掉本机凭据，提示「本机已不在同步组里」 |
| `notFound` | 404 | 资源不存在 | — |
| `codeInvalid` | 404 | 配对码不存在、过期或次数用完 | 提示「配对码无效或已过期」 |
| `inviterOffline` | 409 | 生成配对码的设备不在线 | 提示「请让显示配对码的设备保持在线」 |
| `pairingTimeout` | 408 | 对方 60 秒内没回应 | 提示超时 |
| `groupFull` | 409 | 组内设备已达上限 | 提示上限 |
| `tooLarge` | 413 | 超过策略上限 | 跳过这条 |
| `policy` | 403 | 策略不允许（如官方服务器传 meta） | 跳过这条 |
| `quota` | 429 | 今日额度用完，`retryAt` 给出恢复时间 | 图片暂停到 `retryAt`，文本照常 |
| `rateLimited` | 429 | 请求太频繁 | 按 `Retry-After` 退避 |

## 3. 帧

上行 `POST /v1/entries` 的请求体和下行事件流都是连续的帧（`Content-Type: application/x-kwikpaste-frames`）：

```
u32 大端：头长度 H（H ≤ 64 KiB）
H 字节：JSON 头（UTF-8，字段 camelCase）
u32 大端：体长度 B（没有体时为 0；上限见策略）
B 字节：体（密文）
```

## 4. 接口

### 4.1 `GET /v1/info`（不需要设备令牌）

```json
{
  "protocol": { "min": 1, "max": 1 },
  "mode": "official",
  "serverVersion": "1.0.0",
  "policy": {
    "history": false,
    "meta": false,
    "settings": true,
    "textRetentionHours": 24,
    "images": "live",
    "imageMaxBytes": 2097152,
    "imageCompress": { "maxSide": 1920, "quality": 70 },
    "maxEntryBytes": 1048576,
    "maxDevices": 10
  }
}
```

- `images`：`"off"` 不收图片；`"live"` 只转发给在线设备、不存储；`"stored"` 存储，可补齐。
- `imageCompress` 非空时客户端**必须**按它压缩（§6.3）；为空时按用户自己的选择。
- `textRetentionHours` 为 `null` 时永久保留；`history` / `meta` 为假时客户端不提供对应选项。
- `imageMaxBytes` / `maxEntryBytes` 都按**密文**（体）长度算。
- 自部署服务器的默认策略：`history: true, meta: true, settings: true, textRetentionHours: null, images: "stored",
  imageMaxBytes: 100 MiB + 64, imageCompress: null, maxEntryBytes: 16 MiB, maxDevices: 20`（可用环境变量改）。

### 4.2 建组 `POST /v1/groups`

请求 `{"deviceId": "<设备 id>", "info": "<base64 密封的设备信息>"}`，响应 `{"groupId": "<uuid>", "deviceToken": "<43 字符 base64url>"}`。
设备 id 就是局域网身份的设备 id（20 个十六进制字符）。官方服务器按 IP 限制每天 10 次。

### 4.3 配对码

- `POST /v1/pairing`（成员）→ `{"code": "123456", "expiresAt": "<RFC3339>"}`：6 位数字，5 分钟有效、最多尝试 5 次；
  每组同一时间只有一个有效码，新申请替换旧的。服务器记下是哪台设备申请的（邀请方）。
- `DELETE /v1/pairing`（成员）：作废当前配对码。

### 4.4 加入 `POST /v1/join`

```
加入方                               服务器                               邀请方（显示配对码）
 POST /v1/join {code, deviceId, spake: A} ─▶ 找码、扣一次次数
                                           事件 pairRequest {session, deviceId, spake: A} ─▶
                                                                          算 SPAKE2，生成 joinSecret
                                           ◀─ POST /v1/pairing/{session}/reply
                                              {spake: B, sealed, joinProof}
 ◀─ 200 {session, spake: B, sealed}（最多等 60 秒）
 解开 sealed 得到组密钥和 joinSecret（解不开 = 配对码不对）
 POST /v1/join/{session}/complete {joinSecret, info} ─▶ 校验 BLAKE3(joinSecret) == joinProof，登记设备，作废配对码
 ◀─ 200 {groupId, deviceToken}
```

- SPAKE2：Ed25519 群，身份 `kwikpaste-remote-joiner`（A）/ `kwikpaste-remote-member`（B），密码是 6 位数字的 ASCII。
- `K = BLAKE3.derive_key("KwikPaste remote pairing v1", SPAKE2 共享密钥 ‖ session 的 UTF-8)`。
- `sealed = base64(nonce(24) ‖ XChaCha20-Poly1305(K, nonce, aad = session 的 UTF-8, JSON {"groupKey": "<base64 32 字节>", "joinSecret": "<hex 32 字节>"}))`。
- `joinProof = hex(BLAKE3(joinSecret 的 32 字节原文))`（先把十六进制解码再哈希），`complete` 里 `joinSecret` 照样传十六进制。
  服务器从头到尾拿不到组密钥，也不知道配对码对不对；只有解开了 `sealed` 的加入方才拿得到 `joinSecret`，才能换到设备令牌。
- `joinSecret` 不对：404 `codeInvalid`，这次会话作废；会话不存在或已过 5 分钟：404 `notFound`。
  已在组里的设备 id 再次加入时替换它的令牌，不重复计数。
- `reply`、`DELETE /v1/pairing`、`PUT /v1/devices/self`、`DELETE /v1/devices/{id}` 成功时都回 200 `{}`。
- 码不存在 / 过期 / 次数用完：404 `codeInvalid`；邀请方不在线（没有事件流）：409 `inviterOffline`；60 秒没有回应：408。
  官方服务器按 IP 限制每小时 30 次加入请求。

### 4.5 设备

- `GET /v1/devices` → `[{"deviceId", "info", "createdAt", "lastSeenAt", "online"}]`（`info` 是密封的设备信息）。
- `PUT /v1/devices/self` `{"info"}`：改名后更新。
- `DELETE /v1/devices/{deviceId}`：任何成员都能移除任何设备（移除自己就是退出）。被移除设备的令牌立即作废，
  在线时收到 `revoked` 事件；组里最后一台设备退出时整组和所有条目一起删除。
- 设备信息（密封前）：`{"name": "...", "platform": "windows" | "macos", "appVersion": "2.0.11"}`。

### 4.6 上传 `POST /v1/entries`（成员）

请求体是一个或多个帧。帧头：

```json
{ "entryId": "<32 个十六进制字符>", "kind": "item" | "meta" | "settings",
  "class": "text" | "image" | "none", "hlc": "<16 个十六进制字符>-<设备 id>",
  "live": true, "skip": ["<设备 id>"] }
```

服务器逐帧处理：

1. 校验策略：`kind = meta` 需要 `policy.meta`；`class = image` 需要 `images != "off"` 且体长 ≤ `imageMaxBytes`；
   其他体长 ≤ `maxEntryBytes`；官方服务器再查图片额度（每组每天 30 MiB 转发量、全站每天 1 GiB，北京时间零点重置）。
2. 后写者赢：同一 `entryId` 已存的 HLC ≥ 这一帧的 HLC 时丢弃（`stale`）。HLC 先比 16 位十六进制数，再比设备 id 字符串。
3. 存储：`images = "live"` 的图片不存；其他条目替换旧版本，分到新的 `seq`，`class = text` 且 `textRetentionHours` 非空时设过期时间。
4. 转发：发给组里除发送方和 `skip` 之外的在线设备（`skip` 用来跳过已经通过局域网直连的设备，省流量）。

响应 `{"results": [{"entryId", "status": "stored" | "forwarded" | "stale" | "rejected", "seq": 12, "error": "quota", "retryAt": "<RFC3339>"}]}`，
`seq` 只在 `stored` 时有。策略、超限、额度、帧头读不懂这类单帧错误都放在 200 响应里的 `rejected` 结果中（额度带 `retryAt`）；
官方服务器整个请求超过 8 MiB 回 413，帧截断回 400（之前的帧已经处理）。自部署不限帧数，只限单帧。

转发出去的条目沿用上传方的 `live`（缺省为真），所以补传历史必须带 `live: false`，只有刚采集的记录才是 `live: true`。
图片额度只按至少有一台设备收到的图片计一次；额度和按 IP 的限制存在内存里，服务器重启后清零。

### 4.7 事件流 `GET /v1/events?after=<seq>&items=all|none`（成员）

长连接，响应体是连续的帧：

1. **补齐**：按 `seq` 从小到大发出所有 `seq > after`、未过期的已存条目（每个条目 id 只有最新一版）：
   头 `{"type": "entry", "seq", "entryId", "kind", "class", "hlc", "from", "live": false}`，体是密文。
   `items=none` 时跳过 `kind = item | meta`，只发设置（第一次连接、不同步历史时用）。
2. `{"type": "synced", "seq": <当前最大 seq>}`：补齐结束，客户端把游标推进到这里。
3. **实时**：新条目 `{"type": "entry", ..., "live": true, "seq": 13 | null}`（不存储的图片 `seq` 为 null）。
4. `{"type": "pairRequest", "session", "deviceId", "spake"}`：有设备用本机申请的配对码加入（§4.4）。
5. `{"type": "devices"}`：成员或在线状态变了，客户端重新拉 `GET /v1/devices`。
6. `{"type": "ping"}`：每 240 秒一次。客户端 300 秒没收到任何字节就断开重连。
7. `{"type": "revoked"}`：本机被移除，之后服务器关闭连接。

- 每台设备只保留一条事件流，新的替换旧的。
- `after` 大于服务器当前最大 `seq`（服务器数据重置过）时按 0 处理。
- 实时条目的 `seq` 非空时，客户端处理完把游标推进到它。

## 5. 加密与标识

- **组密钥** `GK`：建组设备随机生成 32 字节，配对时交给新设备（§4.4），存在本机 `<bootstrap>/sync/remote.json`。
- 派生：`encKey = BLAKE3.derive_key("KwikPaste remote entry key v1", GK)`，`idKey = BLAKE3.derive_key("KwikPaste remote entry id v1", GK)`。
- **条目密文**：`nonce(24) ‖ XChaCha20-Poly1305(encKey, nonce, aad = entryId ‖ 0x00 ‖ kind ‖ 0x00 ‖ hlc, 明文)`；
  明文 = `u32 大端 JSON 长度 ‖ JSON ‖ 二进制`（图片字节；没有时为空）。设备信息用 `aad = "device" ‖ 0x00 ‖ deviceId`。
- **条目 id**：`hex(BLAKE3.keyed(idKey, 定义串))` 的前 32 个字符。定义串：
  - 记录：`"item\0" ‖ itemKey`；
  - 元数据：`"meta\0" ‖ field ‖ "\0" ‖ itemKey`；
  - 设置：`"settings\0" ‖ scope ‖ "\0" ‖ JSON 指针`。
- **itemKey** = `<kind>:<content_hash>`，用**最初采集那台设备**算出的指纹。收到的记录把它存进
  `clipboard_items.sync_key`（本机采集的记录这一列为空，等于自己的 `kind:content_hash`）。
  图片压缩后接收方重新算出的指纹会变，靠 `sync_key` 才能对上同一条记录（之后的元数据、删除、再次复制都按它找）。
- **HLC**：64 位 = `毫秒 Unix 时间 << 16 | 计数`，取 `max(本机时钟, 见过的最大值)`；收到更大的 HLC 时追上去。
  线上格式 `"<16 位小写十六进制>-<设备 id>"`。

## 6. 条目内容

### 6.1 记录（`kind = item`）

JSON `{"item": <WireItem，同 PROTOCOL.md §4>, "itemKey": "...", "capturedAt": "<RFC3339>", "image": {"format": "png" | "webp", "width", "height"}}`，
二进制是图片字节。`class` 按 `item.kind` 取 `text` / `image`。只发本机能同步的记录：文本 / 图片、不是敏感内容、不是文件，
并且对应的「同步文本 / 同步图片」开着。

### 6.2 元数据（`kind = meta`，只在自部署 + 同步全部历史时）

每个字段一个条目：`{"itemKey", "field": "favorite" | "pinned" | "note" | "deleted", "value": true | false | "备注" | null}`。

- 用户手动收藏 / 取消、置顶 / 取消、改备注、删除单条记录时上传。自动清理和「清空历史」只在本机生效，不上传。
  手动排序（`pin_order` / `favorite_order`）和分组不同步。
- 接收方按 itemKey（`COALESCE(sync_key, kind || ':' || content_hash)`）找行，HLC 比本地记录的新才应用。
  还没收到对应记录时先存进 `remote_sync_state`，记录到了再补上。
- `deleted = true` 且它的 HLC 比这条记录最后一次的 HLC 新时删除本地行；之后又收到更新的记录条目（有人再次复制）就重新插入。

### 6.3 图片

- 官方服务器（`imageCompress` 非空）：最长边缩到 `maxSide`（不放大），用 libwebp 有损编码 `quality`，再和无损 WebP 比，取更小的；
  结果超过 `imageMaxBytes` 就不发。
- 自部署：用户在「原图 / 高质量（2560、q85）/ 省流（1920、q70）」里选，「原图」直接发本地 PNG；单张上限用户设（1–100 MB），
  同时不超过服务器的 `imageMaxBytes`。
- 接收方把 WebP 解码后存成 PNG，和局域网收到的图片走同一条入库流程。

### 6.4 设置（`kind = settings`）

- 每个叶子值一个条目：`{"path": "/appearance/theme", "scope": "all" | "windows" | "macos", "value": <JSON>}`。
  对象逐层展开；数组和键值表（如 `/shortcuts/panel`）整体算一个叶子。
- **不同步**（只属于本机）：`/general/autoStart`、`/general/runAsAdmin`、`/onboarding`、`/update`、`/sync`、
  `/extensions/wetype`，以及任何保存本机文件路径的项。
- **只在同平台之间同步**（`scope` = 本机平台）：`/shortcuts`、`/general/trayIcon`、`/general/trayClick`、`/general/dockIcon`，
  以及保存应用 id 的列表（如 `/clipboard/filters/excludedAppIds`）。
- 本机改设置后，对比「上次同步的值」，只上传变了的叶子。收到的叶子 HLC 更新时写进本机设置，写入不再回传。
- 建组设备上传全部叶子；新加入的设备先采用组里已有的值，再把组里没有的叶子传上去。

## 7. 客户端行为

- **本机状态**：`<bootstrap>/sync/remote.json`（服务器类型、地址、服务器令牌、groupId、设备令牌、组密钥、游标），原子写入；
  不进 `settings.json`、备份和反馈上报。`settings.json` 里只放 `sync.remote` 的开关。
- **数据库**：迁移 `0012_remote_sync.sql` 给 `clipboard_items` 加 `sync_key`，新建 `remote_sync_state`（条目 id → 类型、itemKey、字段、值、HLC），
  记后写者赢的本地状态和删除墓碑。覆盖导入备份后清空这张表并把游标归零。
- **何时上传**：本机采集（`sync_seq` 分配后，和局域网同一个钩子）→ 实时记录条目；设置变化 → 设置条目；
  自部署 + 同步全部历史打开时，先把整个历史（记录 + 收藏 / 置顶 / 备注）分批上传，之后跟进元数据变化。
  收到的记录（没有 `sync_seq`）不再上传。上传串行执行，排队的条目合并进下一个请求。
- **何时补齐**：第一次连接用 `after=0`，`items` 按「同步全部历史」取 `all` / `none`；之后都用 `after=游标&items=all`。
- **收到记录**：`live = true` 的按局域网实时推送处理（入库、按设置写剪贴板、60 秒回声过滤）；`live = false` 只在本机没有同内容时插入。
  局域网和远程同时开时同一条会到两次：本机已有同一内容且更新时间不早于 `capturedAt` 就跳过。
- **断线**：5 秒起退避，最长 5 分钟；按 `Retry-After` / `retryAt` 等待；`unauthorized` 清凭据，`serverToken` 停止重试。

## 8. 服务端限额（官方）

| 项 | 值 |
| --- | --- |
| 每组设备数 | 10 |
| 建组 | 每 IP 每天 10 次 |
| 加入请求 | 每 IP 每小时 30 次；每个配对码 5 次、5 分钟 |
| 文本 | 单条密文 ≤ 1 MiB，保留 24 小时 |
| 图片 | 单张密文 ≤ 2 MiB，只转发不存储；每组每天 30 MiB，全站每天 1 GiB |
| 设置 | 跟着组一直保留 |
| 闲置的组 | 90 天没有设备连接就整组删除 |

## 9. 兼容规则

同 PROTOCOL.md §8：不认识的帧类型、字段、`kind` / `class` / `field` 取值都跳过不断开；新增字段必须可选；只有改变已有含义时才升协议版本。

## 10. 测试约定

客户端单元测试覆盖加解密、条目 id、帧编解码、HLC、设置展开与排除、元数据后写者赢。端到端：在 Windows 本机跑一个服务端
（`official` 和 `selfhost` 两种模式），两台隔离的测试实例互相同步。
