# Metrolist-rs 活动代码实施计划

本文件回答“当前正在写什么、为什么写、改哪些代码、怎样才算完成”。每次开始新的代码
切片前必须先更新本文件；同一时间只允许一个核心切片处于“进行中”。

UI 完整性逐入口检查见 [`CORE_UI_PARITY.md`](CORE_UI_PARITY.md)，长期阶段和架构见
[`PORTING_PLAN.md`](PORTING_PLAN.md)。

## 总目标

交付可日常使用的 GPUI YouTube Music 桌面客户端：用户能从真实 UI 完成登录、首页浏览、
搜索、打开详情、播放、查看完整播放器、控制进度与音量、管理队列、查看歌词、维护资料库、
下载和修改核心设置。

Android Metrolist 只读参考业务行为和点击路径；桌面 UI 以锁定版本的 Longbridge
`gpui-component` 为唯一组件库依据。

## 硬优先级

1. P0 核心点击闭环：登录、首页/探索、搜索/详情、完整播放器、队列、歌词、资料库、下载、设置。
2. P1 核心跨平台可运行：Linux、Windows、macOS 的构建、系统 WebView、凭据库、音频和媒体控制。
3. P2 非核心但独立可用：播客、歌曲识别、Last.fm、Discord、高级 DSP。
4. P3 后续扩展：“一起听”公网互操作及其他协作能力。

P2/P3 已存在的代码只做回归维护，在 P0 矩阵全部完成前不得继续扩展。

## 已实现切片：Automatic Sleep Timer Schedule

状态：功能、持久化与 UI 已实现，待真实桌面时区/播放转换点击验收。

Android 会在播放由暂停切到 Playing 时，根据本地星期与时间窗口自动启动默认时长的 Sleep Timer；
支持每日、工作日/周末及自定义日期和跨午夜窗口。Desktop 已有真实 Deadline、End of song、Fade out
计时器及 Settings 行为，但计划明确记录自动日程尚未移植。本切片把日程持久化、可点击设置和现有
计时器触发链接通，不建立第二套计时服务。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `Cargo.toml` / `src/config.rs` | 跨平台本地日期时间与可校验的七日日程模型 |
| `src/storage/sqlite.rs` | 持久化自动日程并升级 schema v46，旧设置默认关闭 |
| `src/ui/shell.rs` | Settings 可编辑启用、时长、日期和逐日时间窗口；Playing 转换触发现有 Sleep Timer |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记自动日程真实闭环与剩余时区/桌面点击验收 |

### 完成定义

1. Settings 可发现并启用 Automatic sleep timer，默认关闭；默认 30 分钟、每日 22:00–06:00，所有值
   跨重启保留且 Reset 恢复已保存值。
2. 可选择每日、工作日、周末及自定义星期组合，并可编辑每个启用日的开始/结束时间；支持跨午夜窗口，
   非法时长、日期或时间不能保存。
3. 本地播放从非 Playing 进入 Playing 时，若当前本地星期/时间命中日程且没有活动计时器，自动复用
   现有 minute Deadline，并继承 Finish current song 与 Fade out 设置。
4. 同一活动计时器不会因轮询或切歌重复启动；暂停后在窗口内恢复且计时器已取消时可再次自动启动。
5. Listen Together Guest 不自动创建本地计时器；关闭日程、窗口外、未选日期均不改变播放。
6. 旧 v45 数据库无损升级到 v46 且日程默认关闭；不修改 Android 仓库，不新增计时线程、平行播放器、
   测试或验证设施。
7. 仅执行一次 `cargo fmt --all && cargo check --all-targets` 最小门禁。

完成情况：新增可序列化并校验的七日 Automatic Sleep Timer schedule，默认关闭、30 分钟且每日窗口
为 22:00–06:00；每个星期可独立启用并保存 start/end minute，匹配同时支持当日窗口和前一日跨午夜
延续。Settings 现可开关日程、以 5 分钟调整 5–120 分钟时长、应用 Daily/Weekdays/Weekends 预设，
并逐日开关及以 30 分钟调整显示为 HH:MM 的开始/结束时间。播放 observed state 仅在非 Playing 转为
Playing 时检查本地时间，命中、无活动 timer 且非 Guest 才复用现有 Deadline timer，因此轮询与切歌
不会重启，Finish current song 与 Fade out 继续由同一计时器实现。SQLite v46 以 JSON TEXT 保存完整
日程，v45 升级默认关闭；增加锁定 `chrono` 直接依赖读取跨平台本地星期/时间。没有新增计时线程、
播放器、测试或验证设施。
仅执行一次 `cargo fmt --all && cargo check --all-targets` 并通过，只保留既有
`render_favorites_section` 未使用警告。

## 已实现切片：Stats Period Request Freshness

状态：功能与异步状态归属已实现，待真实桌面快速周期切换验收。

Android `StatsViewModel` 以 latest 语义响应周期切换。Desktop 的 `reload_stats()` 在已有请求时直接返回，
因此用户快速切换周期后，旧周期查询可能在新 Tab 下完成并覆盖界面。本切片给现有统计请求增加代际/
周期归属，只允许最新选择更新 UI，不改统计 SQL 或聚合模型。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Stats 请求代际与周期核对，快速切换立即发起最新查询并丢弃旧结果 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记周期 latest 语义与剩余桌面点击验收 |

### 完成定义

1. 每次有效周期切换都以新代际请求所选周期，不因旧任务仍在运行而忽略用户选择。
2. 旧周期成功或失败结果均不能覆盖当前周期的 Loading、Loaded 或 Failed 状态。
3. 最新请求完成后清除忙碌状态，Top Songs/Artists/Albums 与总览继续来自该周期现有真实 SQL。
4. 初次进入、Retry、重复点击当前周期和离开/返回 Stats 的现有行为保持稳定。
5. 不修改统计查询、schema、播放历史写入、测试或验证设施。
6. 仅执行一次 `cargo fmt --all && cargo check --all-targets` 最小门禁。

完成情况：Stats 现维护请求 generation；初次进入与 Retry 仍沿用既有防重复入口，有效周期切换则
立即递增代际并请求新周期，不再被旧任务阻塞。任意旧代际的成功或失败返回都会在更新状态前被丢弃，
也不能清除最新 `stats_task`；只有最新请求能写入 Loading 后的 Loaded/Failed。重复点击当前周期不发
请求，现有 `listening_stats` SQL、总览与 Top Songs/Artists/Albums 聚合均未修改，也没有新增 schema、
测试或验证设施。
仅执行一次 `cargo fmt --all && cargo check --all-targets` 并通过，只保留既有
`render_favorites_section` 未使用警告。

## 已实现切片：Remote History Delete Eligibility

状态：功能与 UI 已实现，待真实账号远端历史点击验收。

Android 只在远端历史项携带 `historyRemoveToken` 时展示删除动作。Desktop 允许解析缺少 feedback token
的历史项，却始终显示 Remove remote；点击后因为没有 token 而静默无操作。本切片让 UI 可发现性与
真实后端资格一致，不修改历史解析或远端写入协议。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 仅对携带真实 feedback token 的远端历史项显示 Remove remote |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记远端精确删除资格与剩余账号点击验收 |

### 完成定义

1. 携带非空 feedback token 的远端历史项继续显示 Remove remote，并复用现有认证 feedback 删除链路。
2. token 缺失或为空的历史项不显示删除动作，不保留可点击但无操作的占位入口。
3. 远端行的播放、Next、Queue、Download、过滤、加载、失败和刷新保持不变。
4. 不修改 InnerTube 解析、网络协议、存储、schema、测试或验证设施。
5. 仅执行一次 `cargo fmt --all && cargo check --all-targets` 最小门禁。

完成情况：远端 History 行现仅在 `feedback_token` 存在且不是空白字符串时渲染 Remove remote 及其
分隔线，有效 token 继续进入既有认证 feedback 精确删除链路；无 token 的行仍保留播放、Play next、
Add to queue 和 Download，但不再显示点击无反应的删除入口。本地 History 删除及所有加载、过滤、
失败和刷新状态保持不变；没有修改解析、协议、存储、schema、测试或验证设施。
仅执行一次 `cargo fmt --all && cargo check --all-targets` 并通过，只保留既有
`render_favorites_section` 未使用警告。

## 已实现切片：Local Playlist Songs Discoverability

状态：功能、存储读取与 UI 合并已实现，待真实桌面本地搜索/资料库点击验收。

Android Library Overview 与 Local Search 会把本地资料库歌曲和已收藏歌单歌曲合并。Desktop 的
`local_known_songs()` 当前只合并 Favorites、History、Downloads、Episodes for Later，导致只加入
本地歌单、未进入这些来源的歌曲及其 Album/Artist 在 Local Search 与 Library Overview 中完全不可
发现。本切片从现有本地歌单关系读取真实 Song 并合入现有本地来源，不建立第二份索引。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` / `src/storage/mod.rs` | 读取所有本地歌单关联的去重真实歌曲，不修改 schema |
| `src/ui/shell.rs` | 加载并合并该来源到 `local_known_songs()`，供 Local Search 与 Library Overview 共用 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单歌曲可发现性与剩余点击验收 |

### 完成定义

1. 任意歌曲只要仍存在于至少一个本地歌单，就进入现有本地已知歌曲集合；跨歌单重复按 video ID 去重。
2. Local Search 的 Songs 与 All 能按标题/歌手找到仅存在于本地歌单的歌曲，并从当前过滤结果建立真实
   播放队列。
3. Library Overview 搜索能返回这些歌曲；歌曲携带的真实 Album/Artist 信息继续进入现有本地目录
   派生与详情入口，不要求用户先浏览在线页面。
4. 从最后一个本地歌单移除歌曲或删除歌单并刷新后，该歌曲若不属于 Favorites、History、Downloads、
   Episodes for Later 等其他来源，就不再由歌单来源保留。
5. SQLite 读取失败沿用现有本地分区失败/重试状态，不新增 schema、缓存表、后台索引、测试或验证设施。
6. 仅执行一次 `cargo fmt --all && cargo check --all-targets` 最小门禁。

完成情况：`DesktopStore` 现通过单个 `SELECT DISTINCT` 查询读取全部本地歌单关系关联的真实 Song，
跨歌单按 video ID 去重且不修改 schema。独立的 `local_playlist_songs_state` 已接入启动加载、本地资料库
失败重试，以及添加、单项/批量移除和删除歌单后的统一刷新，并合入 `local_known_songs()`。Local Search
Songs/All 和 Library Overview 现可发现仅存在于本地歌单的歌曲；Local Search Albums/Artists、Library
Overview 及 Library 的本地 Albums/Artists 也统一从真实 Song 的 album/artist ID 派生并进入既有 Browse。
最后一个歌单关系消失后，下次刷新会自然移除该来源；其他本地来源仍可继续保留同一歌曲。没有新增
schema、缓存表、后台索引、测试或验证设施。
仅执行一次 `cargo fmt --all && cargo check --all-targets` 并通过，只保留既有
`render_favorites_section` 未使用警告。

## 已实现切片：Online Search All Mixed Results

状态：功能与 UI 已实现，待真实桌面混合结果点击验收。

Android `OnlineSearchResult.kt` 的 All 是歌曲与 Album、Artist、Playlist、Podcast、Profile 等目录项的
混合结果。Desktop 已解析 `songs` 与 `items`，但 All 的空态只看歌曲，结果主体也只渲染歌曲，因此
真实响应只有目录项时会被错误显示为空。本切片直接补齐 All 的真实混合结果 UI，不改搜索协议或建立
第二套结果模型。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | All 同时判断并渲染歌曲与目录结果，歌曲继续复用现有动作，目录进入既有 Browse |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 All 混合结果真实闭环与剩余点击验收 |

### 完成定义

1. 默认 All 对歌曲或目录任一非空都显示真实结果，不再把目录-only 响应判为空。
2. All 同时展示响应中的歌曲和 Album、Artist、Playlist、Podcast、Profile 等目录项；点击目录继续进入
   既有 Browse，点击歌曲继续复用现有播放、Next、Queue、Download 和收藏链路。
3. All 的 continuation 继续追加到同一结果；追加歌曲或目录任一有进展都不得被误判为没有结果。
4. Songs、Videos、Albums、Artists、Community/Featured playlists、Podcasts、Episodes、Profiles 等
   显式筛选的现有布局与请求参数保持不变。
5. 加载、失败、空态和 Load more 保持当前页面可见，不新增搜索服务、数据库、测试或验证设施。
6. 仅执行一次 `cargo fmt --all && cargo check --all-targets` 最小门禁。

完成情况：默认 All 现仅在歌曲与目录均为空时显示空态，并在同一结果页先后渲染真实歌曲及 Album、
Artist、Playlist、Podcast、Profile 等目录项；总数覆盖两类结果。歌曲继续复用现有播放、Next、
Queue、Download 与收藏入口，目录继续进入既有 Browse。continuation 合并后的进展同时按歌曲和目录
数量判断，目录-only 追加不会被提前截断；所有显式筛选保持原请求和布局。没有新增搜索服务、结果
模型、数据库、测试或验证设施。
仅执行一次 `cargo fmt --all && cargo check --all-targets` 并通过，只保留既有
`render_favorites_section` 未使用警告。

## 已实现切片：Queue Selection and Full Queue Visibility

状态：功能与 UI 已实现，待真实桌面队列批量点击验收。

反向审计 Android `Queue.kt` 与 `SelectionSongsMenu.kt` 后确认，Desktop 文档把“队列选择”误记为
已实现：当前 Queue / Up next 只有单项播放、改序和移除，没有 Android 的选择模式、全选和批量动作；
完整播放器还从当前索引开始构造虚拟列表，直接隐藏已经播放过但仍在真实队列中的项目。本切片只补齐
这两个 P0 队列入口，继续复用现有队列、歌单 Picker、下载和会话持久化状态机。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Queue / Up next 共用 Select 状态、逐项选择/全选和批量动作；完整播放器显示完整真实队列 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 修正此前“队列选择已完成”的误标并登记真实批量闭环 |

### 完成定义

1. 侧栏 Queue 与完整播放器 Up next 均有可发现的 Select 入口，并共享同一组基于真实 video ID 的选择。
2. 选择模式可逐项选择、取消、Select all / Clear all、显示数量并退出；重复 video ID 与 Android 一样
   作为同一选择身份处理。
3. 非空选择可按当前队列顺序 Play、Shuffle、Add to queue、Add to local playlist、Download，以及
   Remove selected；全部复用现有真实后端和失败显示。
4. Remove selected 可删除任意非当前/当前组合；当前项被删时只切换并载入最终相邻项目一次，全部删空
   时停止播放并保存空会话。
5. Listen Together Guest 不进入选择模式或执行批量队列写操作；单项 Guest 边界保持不变。
6. 完整播放器 Up next 显示当前项之前、当前项和未来项的完整真实队列，不再用当前索引裁掉历史项。
7. 退出 Queue / 完整播放器、替换队列或批量动作完成后清空选择，不新增数据库、队列类型或测试设施。
8. 只执行一次 `cargo fmt --all && cargo check --all-targets` 最小门禁。

完成情况：侧栏 Queue 与完整播放器 Up next 现共用基于 video ID 的选择模式，可逐项切换、全选/
清空、查看选择数量及退出；重复 video ID 按同一身份选择。非空选择复用现有播放、队列、本地歌单
Picker 与下载状态机执行 Play、Shuffle、Add to queue、Add to local playlist、Download 和 Remove
selected。批量删除以倒序移除真实索引，若包含当前项只在全部删除完成后载入一次最终相邻项，删空时
停止播放并保存空会话。Guest 无法进入或执行选择写操作；退出相关覆盖层、替换队列和完成批量动作
会清空选择。完整播放器不再从当前索引裁切 Up next，而是显示完整真实队列。没有新增数据库、队列
类型、测试或验证设施。仅执行一次 `cargo fmt --all && cargo check --all-targets` 并通过，只保留既有
`render_favorites_section` 未使用警告。

## 已实现切片：Content Language / Country

状态：功能、持久化与真实 InnerTube 请求上下文已实现，待真实桌面区域内容点击验收。

Android `ContentSettings.kt` 提供跟随系统或显式选择 YouTube Music 内容语言、国家/地区；Desktop 的
`InnerTubeSession` 已把 `hl/gl` 传给所有 Home、Search、Browse、Radio 和账号资料库请求，但当前创建
服务时始终保留固定 `en/US`，Settings 也没有对应入口。本切片只接通现有请求链，不新增区域服务、
缓存层或测试设施。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 保存内容语言与国家/地区，支持跟随系统，并升级 schema v45 |
| `src/services/innertube.rs` | 创建客户端时把解析后的语言与地区写入现有 `InnerTubeSession` |
| `src/ui/shell.rs` | Settings 增加两个下拉入口，应用后复用现有服务热替换与页面刷新 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记可点击入口、覆盖请求和边界 |

### 完成定义

1. Settings 可分别选择跟随系统或 Android 当前支持的全部内容语言、国家/地区，并跨重启保留。
2. 跟随系统读取跨平台操作系统 locale，匹配受支持语言与两位国家码；不可识别时回退 `en/US`。
3. 保存应用后，现有 InnerTube 服务统一携带解析后的 `hl/gl`，Home、Explore、当前 Search、账号状态、
   后续详情与 Radio 使用同一上下文；不复制第二套客户端或请求逻辑。
4. 未保存的选择不影响当前网络服务；Reset 恢复已保存值，非法持久值通过现有设置校验显示错误。
5. 不扩展为界面语言、VPN/代理地区、内容过滤、账号迁移、自动化测试或验证设施。
6. 只执行一次 `cargo fmt --all && cargo check --all-targets` 最小门禁。

完成情况：`AppSettings` 与 SQLite v45 现分别保存内容语言和国家/地区，默认均跟随系统；跨平台
系统 locale 会优先匹配 Android 当前支持的完整语言与地区代码，不可识别时回退 `en/US`。Settings
新增两个可滚动下拉入口，明确显示 draft 与活动请求值，Reset 恢复已保存选择。保存应用继续复用
原服务热替换；所有由 `InnerTubeClient::with_settings` 建立的匿名或登录会话都会把解析值写入现有
`hl/gl`，Home、Explore、当前 Search、账号检查以及后续 Browse、Radio 和账号资料库统一生效。
没有新增区域服务、请求副本、内容过滤、自动化测试或验证设施。仅执行一次
`cargo fmt --all && cargo check --all-targets` 并通过，只保留既有 `render_favorites_section` 未使用警告。

## 已实现切片：Crossfade / Gapless Albums

状态：功能与 UI 已实现，待真实桌面连续音轨点击验收。

Android `PlayerSettings.kt` 已提供 Crossfade 开关、1–15 秒时长与 “Gapless albums” 选项；
`MusicService.kt` 会在自然播完前创建同一输出上的第二播放器，以二次曲线重叠淡出当前歌曲和淡入
下一首，同专辑且 Gapless 开启时保持原始无缝衔接。Desktop 当前只在音轨完全结束后才解析并载入
下一首，因此还没有真实重叠播放或对应 Settings 入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Crossfade 开关、秒数与 Gapless albums，并升级 schema |
| `src/services/playback.rs` / `src/services/audio.rs` | 在现有输出 mixer 上载入第二播放器，重叠执行淡入淡出并继续暴露新歌曲位置 |
| `src/ui/shell.rs` | 在自然播完前按队列状态触发 Crossfade，并增加 Settings 可点击入口 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记真实音频行为、入口和限制 |

### 完成定义

1. Settings 可发现 Crossfade，默认关闭并跨重启保留；开启后可设置 1–15 秒，默认 5 秒。
2. 当前歌曲自然播放进入末尾窗口时，下一首在同一 Rodio mixer 上从零开始播放；旧歌淡出与新歌
   淡入真实重叠，时长随媒体播放速度缩放。
3. Gapless albums 默认开启；当前与下一首具有相同真实 Album ID 时不做 Crossfade，仍走原自然切歌。
4. Repeat One/All、Shuffle 与 Autoplay 继续由现有队列状态机选择目标；Autoplay 关闭、队尾停止、
   Episode、Listen Together Guest 或等待曲末的 Sleep Timer 不提前切歌。
5. Crossfade 开始后 UI、歌词、历史、Last.fm、Discord、媒体会话和持久会话切到新歌曲及其真实位置；
   暂停、继续、音量、临时 Sleep Timer 音量乘数、EQ、变速、Skip silence 与输出设备切换保持可用。
6. 下一首解析或载入失败继续进入现有可见失败/重试/自动跳过流程，不新增预取服务、播放器页面、
   自动化测试或验证设施。
7. 只执行格式化和全目标编译门禁；直接编译错误只做对应修正后必要重跑。

完成情况：`AppSettings` 与 SQLite v44 现持久保存 Crossfade、1–15 秒时长和 Gapless albums，
默认分别为关闭、5 秒和开启。Settings 可直接编辑三项；自然播放进入末尾窗口后，Shell 使用现有
Repeat/Shuffle/Autoplay 队列状态选择真实目标，Rodio 在同一 mixer 上保留旧 Player 并启动新
Player，以新歌曲媒体位置执行二次曲线重叠淡出/淡入。最终目标若与当前歌曲具有相同真实 Album ID
且 Gapless 开启，或任一侧为 Episode，则恢复队列并等待原自然切歌；Guest、曲末 Sleep Timer、
Autoplay 关闭及无目标状态也不提前切歌。新歌曲继续使用原播放源解析、缓存、离线、标准化、EQ、
变速和 Skip silence 链，暂停、音量与 Sleep Timer 临时乘数同时作用于两路；解析/载入失败沿用现有
可见失败、刷新与自动跳过流程。没有新增预取服务、第二套音频服务、页面、自动化测试或验证设施。
仅执行 `cargo fmt --all && cargo check --all-targets` 门禁；首次发现 Shell 漏导入已有
`PlaybackSnapshot`，直接补齐后必要重跑通过，仅保留既有未使用方法警告。

## 已实现切片：Skip Silence / Instant Skip

状态：功能与 UI 已实现，待真实桌面静音音轨点击验收。

Android `PlayerSettings.kt` 直接提供 Skip silence，并在启用后允许 Instant skip；`MusicService.kt`
把前者应用到真实播放器，把后者用于持续静音超过 2 秒后的快速跳跃。Desktop 的 Rodio 解码链已有
音量标准化、EQ、变速和准确 seek/位置状态，但还会原速播放所有静音 PCM，也没有对应 Settings 入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Skip silence / Instant skip 并升级 schema |
| `src/services/audio.rs` | 在现有解码链加入按完整 PCM frame 判断的静音压缩，并把丢弃时长计入媒体位置 |
| `src/ui/shell.rs` | Settings 增加可发现开关，Instant skip 只在 Skip silence 开启时可选 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记真实音频后端与 UI 行为 |

### 完成定义

1. Settings 可发现 Skip silence，默认关闭并跨重启保留；关闭时现有 PCM、位置和音质不变。
2. 开启后以 Android 静音检测阈值对应的全声道近静音 frame 为准，保留开头短静音并压缩持续静音，
   有声 frame 不丢弃。
3. Instant skip 默认关闭且仅在 Skip silence 开启时可操作；开启后持续静音达到 2 秒即丢弃其余静音，
   比普通压缩更快进入下一段有声内容。
4. 被跳过的 frame 时长进入真实播放位置，因此进度、歌词、历史、Last.fm、Discord、会话保存和
   Sleep Timer 曲末淡出继续使用媒体时间而不是缩短后的输出时间。
5. seek、切歌、服务热替换和输出设备切换会重置或保留正确静音状态；音量标准化、EQ、变速与离线
   播放继续走同一音频链。
6. 不新增播放器、页面、后台扫描、音频预分析、自动化测试或验证设施。
7. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v43 现持久保存 Skip silence 和 Instant skip，默认均关闭；Settings
中 Instant skip 仅在 Skip silence 开启时可操作。现有 Rodio 解码链会按所有声道低于 Android 对应
阈值的完整 PCM frame 判断近静音，保留开头 150 ms，普通模式随后保留 20% frame，Instant 模式在
连续静音达到 2 秒后直接丢弃其余静音直到恢复有声。丢弃的原始 frame 会换算为媒体时间并补入现有
播放位置，seek 会清空累计值；进度、歌词、历史、Last.fm、Discord、会话与 Sleep Timer 继续使用
原始时间轴。服务热替换、输出设备切换、音量标准化、EQ、变速、缓存和离线来源继续复用同一音频链。
没有新增播放器、页面、后台扫描、音频预分析、自动化测试或验证设施；只执行一次
`cargo fmt --all && cargo check --all-targets` 并通过，仅保留既有未使用方法警告。

## 已实现切片：Sleep Timer Finish Current Song / Fade Out

状态：功能与 UI 已实现，待真实桌面 Sleep Timer 点击验收。

Android `PlayerSettings.kt` 和 `SleepTimer.kt` 允许普通倒计时到点后改为播完当前歌曲再暂停，并可在
真正停止前 60 秒逐渐降低输出音量。Desktop 已有完整播放器与队列侧栏可点击的 15/30/60 分钟、
自定义分钟和曲末 Sleep Timer，也已有统一播放轮询与真实音量后端，但尚未移植这两个设置和行为。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Sleep Timer 的播完当前歌曲与淡出偏好并升级 schema |
| `src/services/playback.rs` / `src/services/audio.rs` | 在现有逻辑音量之上增加临时输出乘数，不改变用户音量或会话音量 |
| `src/ui/shell.rs` | 复用现有 Sleep Timer 状态机执行到点转曲末、最后 60 秒淡出，并增加 Settings UI |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记可点击入口和真实播放行为 |

### 完成定义

1. Settings 可分别发现 Finish current song 与 Fade out，默认均关闭并跨重启保留。
2. Finish current song 关闭时，分钟定时器到点立即暂停；开启时到点改为当前歌曲自然结束后停止，
   不进入队列下一首。
3. Fade out 开启时，仅在实际停止前最后 60 秒线性降低输出；分钟定时器等待曲末时，倒计时到点前
   不淡出，转为曲末后按歌曲剩余时长淡出。
4. 直接选择 End of song 时同样应用 Fade out；取消或完成定时器立即恢复完整输出音量。
5. 淡出使用独立输出乘数，不移动音量滑杆、不触发静音暂停，也不把衰减后的音量写入会话。
6. Listen Together Guest 继续不能创建或取消 Sleep Timer；Repeat、Autoplay、Queue 与手动播放控制
   保持现有行为。
7. 不新增播放器、计时任务、页面、自动化测试或验证设施。
8. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v42 现持久保存 Finish current song 和 Fade out，默认均关闭。
完整播放器与 Queue 侧栏现有分钟/自定义/End of song 入口会在启动时读取这两项设置：普通 deadline
可立即暂停或到点转为当时当前歌曲结束后停止，曲末状态也会在手动切歌时停止新歌曲而不是继续计时；
Fade out 仅在真正停止前最后 60 秒线性调整音频线程的临时输出乘数。取消、完成及音频服务热替换
都会恢复或保留正确乘数，逻辑音量、滑杆、静音暂停、桌面媒体同步和会话保存不记录衰减值。Guest、
Repeat、Autoplay 与队列行为继续复用原状态机。没有新增播放器、计时任务、页面、自动化测试或验证
设施；只执行一次 `cargo fmt --all && cargo check --all-targets` 并通过，仅保留既有未使用方法警告。

## 已实现切片：Auto Load More

状态：功能与 UI 已实现，待真实桌面 Radio 队尾点击验收。

Android `PlayerSettings.kt` 将 Auto load more 与 Enable similar content 分开：前者只决定已有 Radio
队列接近末尾时是否请求 continuation，后者决定普通队列能否自动开始相似内容。Desktop 当前把首次
相似推荐和 Radio continuation 都绑定在 `auto_radio`，关闭 Similar content 会连已有 Radio 的自动
翻页一并关闭，两个语义尚未独立。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Auto load more 偏好并升级桌面 schema |
| `src/ui/shell.rs` | 将首次相似推荐与已有 Radio continuation 分别按两个设置控制，并增加 Settings UI |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记独立自动翻页行为与入口 |

### 完成定义

1. Settings 可发现 Auto load more，默认开启并跨重启保留。
2. 已有 Radio 队列剩余不超过五首且存在有效 continuation 时，开启设置会自动请求并追加下一页。
3. 关闭时不再自动请求 continuation，当前已加载队列、当前播放和手动控制保持不变。
4. Similar content 只控制普通队列是否自动开始首个推荐页；显式 Radio 仍可加载首个页面。
5. Listen Together Guest 与 Repeat All 禁止加载设置继续阻止首次推荐和 continuation。
6. 追加歌曲继续复用现有去重、Shuffle 与原始队列边界行为。
7. 不新增 Radio 后端、队列类型、页面、请求系统或测试设施。
8. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v41 现独立持久保存 Auto load more，默认开启。Settings 可直接
选择自动加载或停在当前已加载页面；普通合格队列仍只由 Similar content 决定是否请求首个推荐页，
已有 Radio 则仅在 Auto load more 开启、剩余不超过五首且 continuation 有效时复用现有请求并追加。
关闭不会修改当前队列或显式 Radio，Repeat All、Guest、跨页去重、Shuffle 与原始队列边界继续使用
既有状态机；队列侧栏会显示当前 Radio 是否继续自动翻页。没有新增 Radio 后端、队列类型、页面、
请求系统或测试设施；只执行一次 `cargo fmt --all && cargo check --all-targets` 并通过，仅保留既有
未使用方法警告。

## 已实现切片：Auto Radio Queue

状态：功能与 UI 已实现，待真实桌面 Search/Home 单曲点击验收。

Android `PlayerSettings.kt` 将 Auto radio queue 与 Enable similar content 分开：前者决定从 Search/Home
点击单曲时建立真实 YouTube Radio 队列还是只播放该曲；后者负责既有队列末尾的相似内容。
Desktop 当前只有一个 `auto_radio` 偏好，Search 点击还会错误地把整页结果当作播放队列，两个语义尚未
独立。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Auto radio queue 偏好并升级桌面 schema |
| `src/ui/shell.rs` | Search/Home 单曲点击按设置进入真实 Radio 或单曲队列；现有队尾补充明确为 Similar content |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记两个独立队列行为与入口 |

### 完成定义

1. Settings 可分别发现 Auto radio queue 与 Similar content，默认均开启并跨重启保留。
2. Auto radio queue 开启时，Search 结果/建议与 Home 单曲点击立即播放所选歌曲并请求其真实 Radio。
3. 关闭时同一入口只建立所选单曲队列，不把整页 Search/Home 卡片集合加入队列，也不立即补相似内容。
4. Home/Search 的 Play all、显式 Radio、Play next、Add to queue 和 YouTube URL 直达保持原行为。
5. Similar content 继续独立控制其他队列接近末尾时的自动推荐补充及 Repeat All 限制。
6. Listen Together Guest 继续不能从这些入口控制播放。
7. 不新增 Radio 后端、队列类型、页面或测试设施。
8. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v40 现独立持久保存 Auto radio queue，默认开启；原
`auto_radio` 设置在 UI 中明确为 Similar content。Search 结果/建议以及 Home 的推荐、Quick picks、
Speed Dial、Recent、Daily Discover 等普通单曲入口开启时复用现有真实 Radio 请求，关闭时建立严格
单曲队列并阻止该队列立即补相似内容；Episode 始终走单集队列。Play all、显式 Radio、Next、Queue
与 YouTube URL 直达保持原行为，Guest 仍由统一播放控制拒绝。没有新增 Radio 后端、队列类型、页面
或测试设施；只执行一次 `cargo fmt --all && cargo check --all-targets` 并通过，仅保留既有未使用方法
警告。

## 已实现切片：Auto-download on Like

状态：功能与 UI 已实现，待真实桌面收藏/下载点击验收。

Android `PlayerSettings.kt` 提供 Auto-download on like，`MusicService.toggleLike` 只在歌曲变为 liked
且偏好开启时提交下载请求。Desktop 已有跨页面统一的本地 Favorite 写入入口、持久下载记录、并发
下载队列和可见下载状态，但收藏成功后尚未触发下载。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Auto-download on like 偏好并升级桌面 schema |
| `src/ui/shell.rs` | Settings 增加开关，并在 Favorite 写库成功后复用现有下载队列 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记收藏自动下载闭环 |

### 完成定义

1. Settings 可发现 Auto-download on like，默认关闭并跨重启保留。
2. 关闭时 Favorite 只保持现有收藏、Last.fm 与 Daily Discover 行为。
3. 开启后，普通歌曲从未收藏变为 Favorite 且本地写库成功时进入现有离线下载队列。
4. 取消收藏不移除既有下载，再次收藏已完成或已排队的同音质歌曲不重复下载。
5. Episode 的 Episodes for Later 行为不触发歌曲自动下载。
6. 收藏写入失败不启动下载；下载失败继续使用现有可见下载错误与重试入口。
7. 不新增下载器、队列、页面或测试设施。
8. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v39 现持久保存 Auto-download on like，默认关闭。Queue
persistence 设置卡片可直接选择 Off / Download favorites；所有普通歌曲的本地 Favorite 入口继续汇入
统一写库回调，只有写入成功且歌曲刚变为 Favorite 时才复用现有当前音质下载队列。取消收藏、Episode
和收藏失败不触发；既有下载入口继续对已完成、活动或排队中的同音质歌曲去重，下载错误沿用现有可见
状态和重试入口。没有新增下载器、队列、页面或测试设施；只执行一次
`cargo fmt --all && cargo check --all-targets` 并通过，仅保留既有未使用方法警告。

## 已实现切片：Progressive Artwork Seek

状态：功能与 UI 已实现，待真实桌面封面双击验收。

Android `Thumbnail.kt` 在完整播放器封面左右半区响应双击，默认每次快退/快进 5 秒；启用
`SeekExtraSeconds` 后，一秒内连续触发会按 5、10、15…秒递增。Desktop 已有真实 seek 后端和完整
播放器封面，但封面尚无该手势，Settings 也没有对应开关。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Progressive seek 偏好并升级桌面 schema |
| `src/ui/shell.rs` | 在现有完整播放器封面接入左右双击 seek、递增反馈与 Settings UI |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记封面 seek 设置闭环 |

### 完成定义

1. 完整播放器封面左/右半区双击分别快退/快进，默认每次 5 秒。
2. Settings 可发现 Progressive seek，默认关闭并跨重启保留。
3. 开启后，一秒内连续双击按 5、10、15…秒递增；间隔超过一秒或关闭设置后恢复 5 秒。
4. seek 复用现有播放位置、时长裁剪、会话保存与可见提示，不增加第二套进度状态。
5. Listen Together Guest 不允许通过封面控制房主播放。
6. 不新增播放器、计时任务、页面或测试设施。
7. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v38 现持久保存 Progressive seek，默认关闭。完整播放器封面
左右半区通过 GPUI 原生双击计数分别复用现有 seek 快退/快进，默认固定 5 秒；开启设置后，一秒内
连续操作按 5、10、15…秒递增，现有时长裁剪、会话保存和原位提示保持生效。Listen Together Guest
不挂载封面手势。没有新增播放器、计时任务、页面或测试设施；只执行一次
`cargo fmt --all && cargo check --all-targets` 并通过，仅保留既有未使用方法警告。

## 已实现切片：Pause Music When Muted

状态：功能与 UI 已实现，待真实桌面音量点击验收。

Android `PlayerSettings.kt` 提供 Pause music when media is muted；`MusicService.onDeviceVolumeChanged`
只在播放中归零时暂停并记住来源，音量恢复后仅自动恢复这次静音暂停，用户手动播放控制会取消
自动恢复资格。Desktop 的音量滑杆与系统媒体音量目前只设置增益。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化静音暂停偏好并升级桌面 schema |
| `src/ui/shell.rs` | 统一本地音量入口、静音暂停来源与手动控制取消语义，并增加 Settings UI |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记静音暂停设置闭环 |

### 完成定义

1. Settings 可发现 Pause music when muted，默认关闭并跨重启保留。
2. 关闭时音量归零继续只改变增益，不改变播放状态。
3. 开启且当前正在播放时，本地音量归零后暂停并保留当前歌曲/位置/队列。
4. 仅由该静音动作暂停时，音量恢复为正数才自动继续；原本已暂停的歌曲不自动播放。
5. 静音暂停后用户手动 Play/Pause/Stop 或选择新歌曲会取消自动恢复资格。
6. 系统媒体 SetVolume 与应用音量滑杆共用行为；会话恢复、服务热替换和 Guest 房主音量同步不触发。
7. 不新增音频后端、设备监听器、播放状态机、页面或测试设施。
8. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v37 现持久保存 Pause music when muted，默认关闭。Volume
normalization 卡片可选择 Keep playing / Pause and resume；应用音量滑杆与桌面媒体 SetVolume 共用本地
音量入口，播放中归零会暂停并记录来源，恢复正音量时只继续该静音造成的暂停。用户手动
Play/Pause/Stop 或选择新歌曲会清除自动恢复资格；会话恢复、服务热替换和 Guest 房主音量同步继续
只设置增益。没有新增后端、设备监听器、状态机、页面或测试设施。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Shuffle Playlist or Album First

状态：功能与 UI 已实现，待真实桌面 Shuffle/Radio 点击验收。

Android `PlayerSettings.kt` 提供 Shuffle playlist or album first；`MusicService.applyShuffleOrder`
以原始队列边界分别随机原播放列表/专辑和后来加入的相似内容，当前仍在原始部分时优先播完原始
歌曲。Desktop 当前 Shuffle 会把 Automatic radio 和显式 Queue 新增项直接混入全部待播歌曲。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化优先随机原始集合偏好并升级桌面 schema |
| `src/services/playback.rs` / `src/ui/shell.rs` | 记录原始队列边界，复用现有物理队列执行分段 Shuffle，并增加 Settings UI |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记优先随机原始集合设置闭环 |

### 完成定义

1. Settings 可发现 Shuffle playlist or album first，默认关闭并跨重启保留。
2. 关闭时继续随机全部尚未播放歌曲，包括后来加入的 Queue/Automatic radio 内容。
3. 开启且当前仍在原始集合时，原始待播歌曲与新增内容分别随机，原始歌曲始终排在新增内容前。
4. 当前进入新增内容后只随机剩余新增部分，不重播已经播放过的原始歌曲。
5. Add to queue、Automatic radio、开启 Shuffle 和 Repeat All 新一轮共用同一原始边界；新建/恢复队列重置边界。
6. Play next、当前歌曲、已播放历史、Repeat/Shuffle 状态与 Guest 行为保持不变。
7. 不新增第二套队列、持久队列项类型、播放后端、页面或测试设施。
8. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v36 现持久保存 Shuffle playlist or album first，默认关闭。
Queue persistence 卡片可选择 Mix all / Original first；队列在新建、恢复或 Guest 替换时记录原始
集合边界，开启后 Add to queue、Automatic radio、启用 Shuffle 与 Repeat All 新一轮均分别随机原始
待播和新增部分。当前进入新增部分后不会重播旧的原始歌曲；Play next、当前项和已播放历史不变。
没有新增第二套队列、持久队列项类型、后端、页面或测试设施。首次 `fmt + check` 直接指出分段切片
的借用冲突，按该诊断缓存长度后立即重跑同一门禁并通过，仅保留既有未使用方法警告；未运行测试、
Clippy 或 release。

## 已实现切片：Auto Skip Next on Playback Error

状态：功能与 UI 已实现，待真实桌面失败播放点击验收。

Android `PlayerSettings.kt` 提供 Auto skip to next song when error occurs；`MusicService` 先执行既有
播放恢复，确认不可恢复且开关开启后才跳到下一首，并以连续错误计数阻止整条队列失控跳过。
Desktop 已有失败后刷新一次播放源的恢复链，但终态失败只能停留在当前歌曲。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化错误自动跳过偏好并升级桌面 schema |
| `src/ui/shell.rs` | Settings 增加开关，并在现有恢复链终态接入下一首与连续失败上限 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记错误自动跳过设置闭环 |

### 完成定义

1. Settings 可发现 Auto skip on playback error，默认关闭并跨重启保留。
2. 关闭时终态失败继续停在当前歌曲并显示既有错误。
3. 开启时 Cache repair / source refresh 等既有恢复机会不变，仅在最终失败后进入下一首。
4. 下一首继续复用 Shuffle/Repeat All/队列选择与真实播放解析，不建立第二套切歌路径。
5. 连续两首终态失败可继续跳过；第三首失败停止并保留错误，任一歌曲成功播放后计数清零。
6. 无下一首、Repeat Off 的队尾、Guest 模式与手动 Next 行为保持不变。
7. 不新增播放后端、错误分类、重试框架、页面或测试设施。
8. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v35 现持久保存 Auto skip on playback error，默认关闭。
Queue persistence 卡片可选择 Stop on error / Skip next；开启后，离线副本修复和一次网络播放源刷新
仍优先执行，最终失败才复用现有 Next、Shuffle、Repeat All 与真实播放解析进入下一首。连续两首失败
可继续跳过，第三首失败停下并保留错误；成功进入 Playing/Paused 会清零，Guest 与无下一首的 Repeat
Off 队尾不受影响。没有新增后端、错误分类、重试框架、页面或测试设施。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Prevent Duplicate Tracks in Queue

状态：功能与 UI 已实现，待真实桌面队列点击验收。

Android `PlayerSettings.kt` 提供 Prevent duplicate tracks in queue，`MusicService.playNext` 与
`addToQueue` 会在开关开启时先移除本次歌曲 ID 对应的所有非当前队列项，再把歌曲移动到新的
Next/Queue 位置；当前正在播放项不会被移除。Desktop 当前所有显式 Next/Queue 都允许重复插入。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化防重复队列偏好并升级桌面 schema |
| `src/services/playback.rs` / `src/ui/shell.rs` | 统一移除匹配的非当前项，并接入单曲/整组 Next 与 Queue 及 Settings UI |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记防重复队列设置闭环 |

### 完成定义

1. Settings 可发现 Prevent duplicate tracks in queue，默认关闭并跨重启保留。
2. 关闭时继续允许显式 Next/Queue 插入重复歌曲。
3. 开启时，单曲与整组 Next/Queue 都先移除本次歌曲 ID 的所有非当前旧项，再插入到目标位置。
4. 当前正在播放的同 ID 项不移除；本次输入自身的顺序和重复项按 Android 行为保留。
5. Play next 仍紧随当前项，Add to queue 仍进入队尾并保持既有 Shuffle 行为。
6. Automatic radio 继续使用既有跨页去重，不受该显式入队设置影响。
7. 不新增队列状态机、播放服务、设置页面或测试设施。
8. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v34 现持久保存 Prevent duplicate tracks in queue，默认关闭。
Queue persistence 卡片可选择 Allow duplicates / Move existing；开启后，单曲与整组 Play next / Add to
queue 会统一移除同 video ID 的所有非当前旧项，再按原有 Next、队尾及 Shuffle 语义插入本次歌曲。
当前播放项、本次输入内部顺序/重复项、Automatic radio、Guest 限制与会话持久化均保持既有行为。
没有新增队列状态机、服务、页面、测试或验证设施。`cargo fmt --all && cargo check --all-targets`
已通过，仅保留既有未使用方法警告。

## 已实现切片：Disable Automatic Radio in Repeat All

状态：功能与 UI 已实现，待真实桌面 Repeat All 队尾点击验收。

Android `PlayerSettings.kt` 提供 Disable load more when Repeat All；`MusicService` 在 Repeat All 时按该
设置跳过自动补充与相似内容。Desktop 已有 Automatic radio，但当前 Repeat All 仍会继续补充队列。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Repeat All 自动补充偏好并升级桌面 schema |
| `src/ui/shell.rs` | Automatic radio 卡片增加开关，并门控唯一自动补充入口 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Repeat All 自动补充设置闭环 |

### 完成定义

1. Settings 的 Automatic radio 卡片可发现 Repeat All behavior，默认 Keep loading 并跨重启保留。
2. 选择 Do not load 且 Repeat All 生效时，不启动新的自动 Radio 初始页或 continuation。
3. Repeat Off/One、设置为 Keep loading 时继续使用现有五首阈值自动补充。
4. 手动 Radio、Retry、Artist/Playlist 服务端 Radio 与已入队歌曲不受影响。
5. 切换设置不清空队列、不改变 Repeat/Shuffle，也不取消已发出的请求。
6. 不新增 Radio 状态、请求后端、队列服务或设置页面。
7. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v33 现持久保存 Disable load more when Repeat All，默认关闭。
Automatic radio 卡片可选择 Keep loading / Do not load；选择后者且 Repeat All 生效时，唯一自动补充
入口会跳过新的初始页和 continuation。Repeat Off/One、手动 Radio、Retry、Artist/Playlist 服务端
Radio、既有队列和已发出的请求均保持原行为。没有新增 Radio 状态、后端、队列服务、页面或测试设施。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Queue Mode Persistence

状态：功能与 UI 已实现，待真实桌面新队列/重启点击验收。

Android `PlayerSettings.kt` 提供 Persistent shuffle across queues 与 Remember shuffle and repeat。
Desktop 当前每次建立新队列都关闭 Shuffle，并且冷启动始终恢复上次 Shuffle/Repeat，Settings 无法选择。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化两个队列模式偏好并升级桌面 schema |
| `src/ui/shell.rs` | Settings 增加两个开关，门控新队列 Shuffle 与冷启动模式恢复 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记队列模式设置闭环 |

### 完成定义

1. Settings 可发现 Keep Shuffle across new queues 与 Remember Shuffle and Repeat，默认分别关闭/开启并跨重启保留。
2. Keep Shuffle 关闭时，普通新队列继续关闭 Shuffle；开启且当前 Shuffle 已开启时，新队列保留开启并随机化当前项之后的歌曲。
3. 显式 Shuffle collection 始终建立随机队列并开启 Shuffle，不依赖 Keep Shuffle。
4. Remember modes 开启时冷启动恢复 Repeat/Shuffle；关闭时以 Off/Off 启动，但保存设置不会改变当前运行模式。
5. 关闭 Remember modes 后持久会话立即写入 Off/Off；重新开启则立即保存当前运行模式，不复活旧状态。
6. Persistent queue 关闭时仍按其既有规则不恢复歌曲；两个设置不影响手动 Next、点选歌曲、Radio 或房客控制。
7. 不新增队列状态、随机算法、会话文件、后台服务或设置页面。
8. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v32 现持久保存 Persistent shuffle across queues 和 Remember
shuffle and repeat，默认分别关闭与开启；两项选择与 Queue persistence、Autoplay 共用一个设置卡片。
普通新队列在前者开启且当前 Shuffle 已开启时保留模式并只随机化当前项之后的歌曲，默认仍关闭模式；
显式 Shuffle 始终建立随机队列。后者关闭时，session 保存为 Repeat Off / Shuffle Off，冷启动不恢复
旧模式，但保存设置不改变当前运行状态；重新开启会立即保存当前模式。实现复用现有队列与 session，
没有新增随机算法、会话文件、后台服务、页面或测试设施。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Autoplay

状态：功能与 UI 已实现，待真实桌面自然播完点击验收。

Android `PlayerSettings.kt` 提供 Autoplay，默认开启；`MusicService` 只在 Repeat Off 且该开关开启时，
让自然结束的歌曲进入下一首。Desktop 当前在队列存在下一项时始终自然前进，Settings 无法关闭。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Autoplay 并升级桌面 schema |
| `src/ui/shell.rs` | Settings 增加 Autoplay 开关，并门控 Repeat Off 的自然队列推进 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Autoplay 设置闭环 |

### 完成定义

1. Settings 可发现 Autoplay On/Off，默认开启并跨重启保留。
2. 开启且 Repeat Off 时，当前歌曲自然结束后继续进入队列下一首；现有默认行为不变。
3. 关闭且 Repeat Off 时，自然结束停在当前歌曲，不改变队列选择；再次点击播放重播当前歌曲。
4. Repeat One 仍重播当前歌曲，Repeat All 仍前进或回绕，不受 Autoplay 关闭影响。
5. 手动 Next、直接点选歌曲、建立新队列、Radio 加载与 Listen Together 房客控制保持现有行为。
6. 不新增播放器、定时器、队列状态或设置页面。
7. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v31 现持久保存 Autoplay，默认开启；Queue persistence 卡片中可直接
切换 On/Off。播放器复用唯一的自然结束推进入口：Repeat Off 时按开关决定是否进入下一首，关闭时保留
当前歌曲和队列选择供用户重播；Repeat One/All、手动 Next、歌曲点选、新队列、Radio 加载及 Listen
Together 房客控制均保持原行为。没有新增播放器、定时器、队列状态、设置页面或测试设施。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Persistent Queue

状态：功能与 UI 已实现，待真实桌面重启点击验收。

Android `PlayerSettings.kt` 提供 Persistent queue，关闭后应用重启不恢复上次待播列表。Desktop 已有
完整 SQLite 播放会话保存/恢复，但当前始终持久化并恢复队列，Settings 无法选择。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 Persistent queue 并升级桌面 schema |
| `src/ui/shell.rs` | Settings 增加队列持久化开关，门控会话队列/位置/播放源保存与启动恢复 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Persistent queue 闭环 |

### 完成定义

1. Settings 可发现 Persistent queue 的 Restore last queue / Do not restore 选择，默认开启并跨重启保留。
2. 开启时继续保存和恢复完整队列、当前索引、播放位置与安全播放源元数据。
3. 关闭并 Save 后立即把持久快照改为空队列；当前运行队列和正在播放歌曲不被清空或中断。
4. 关闭状态重启时不恢复任何歌曲、位置或播放源；音量与 Repeat/Shuffle 仍使用现有会话字段恢复。
5. 再次开启后立即保存当前运行队列，后续重启恢复最新状态，不复活关闭前的旧队列。
6. 不新增队列文件、后台服务、关闭钩子或设置页面。
7. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v30 现持久保存 Persistent queue，默认开启。Settings 可选择恢复或
不恢复上次队列；关闭并保存会立即以空队列覆盖持久会话，但不清空或中断当前运行队列。关闭状态冷启动
只恢复音量、Repeat 与 Shuffle，不恢复歌曲、索引、位置或播放源；再次开启会立即保存当前运行队列，
不会复活关闭前的旧状态。实现完全复用现有会话存储和设置保存流程，没有新增队列文件、后台服务、关闭
钩子、设置页面或测试设施。`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法
警告。

## 已实现切片：Listening History Duration

状态：功能与 UI 已实现，待真实桌面播放门槛点击验收。

Android `PlayerSettings.kt` 提供 1–100 秒 History duration，并让本地历史/统计与 YouTube Music 播放
注册共用该真实播放时长门槛。Desktop 目前将两者集中接线但固定为 30 秒，Settings 无法修改。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化 1–100 秒历史门槛并升级桌面 schema |
| `src/ui/shell.rs` | Settings 增加真实 GPUI 滑杆，替换固定 30 秒判断并支持 Reset |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记历史时长设置闭环 |

### 完成定义

1. Settings 可发现 Listening history duration 滑杆，范围 1–100 秒、步长 1 秒并显示当前草稿值。
2. Save and apply 后跨重启保留；Reset unsaved changes 同步恢复已保存值与滑杆位置。
3. 新门槛同时控制本地 `play_history` /累计统计与已开启的 YouTube Music 远端历史注册。
4. 已暂停本地历史时继续跳过本地记录；远端同步仍由其独立开关决定，两者不被合并。
5. 当前歌曲只在累计真实播放达到生效门槛后记录一次；暂停、seek 和界面停留不伪造播放时长。
6. 默认保持 Android 同值 30 秒；不新增历史计时器、播放服务或设置页面。
7. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite v29 现持久保存 1–100 秒历史门槛，默认 30 秒。Settings 使用锁定
`gpui-component` 的真实水平滑杆，步长 1 秒并显示草稿值；Save and apply 后立即成为生效值，Reset
同时恢复草稿和滑杆位置。播放器继续使用既有 `played_this_track` 累计真实 Playing 时长，在达到生效
门槛后只进入一次共用历史处理：本地记录仍受 Pause listening history 控制，远端注册仍受 YouTube
Music history sync 控制。没有新增计时器、播放服务、设置页面或测试设施。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Privacy History Clear Actions

状态：功能与 UI 已实现，待真实桌面历史清空点击验收。

Android `PrivacySettings.kt` 将 Clear listen history 与 Clear search history 和对应 Pause 开关放在同一
设置页。Desktop 已有两项真实 SQLite 清空后端及其他页面入口，但刚补齐的 Privacy history 卡片仍缺少
这两个直接操作。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Settings 复用现有两类历史确认框/清空任务，并显示记录数、忙碌、成功和失败 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Privacy Settings 清空入口闭环 |

### 完成定义

1. Settings 的 Privacy history 卡片可发现 Clear listening history 与 Clear search history。
2. 数量为零时对应按钮禁用；确认框取消不改变任何状态。
3. 确认后分别复用现有 `clear_history` / `clear_search_history` 与 SQLite 后端，不复制删除逻辑。
4. 收听历史清空继续同步刷新当前 History、Keep listening、Forgotten favorites 与 Stats 状态；搜索历史
   清空继续同步刷新搜索历史空态。
5. Settings 内进行中、成功和业务失败原位可见；收藏、歌单、队列、下载及远端历史不受影响。
6. 不新增数据库迁移、清理服务、历史页面或测试设施。
7. 只执行一次格式化和全目标编译检查。

完成情况：Settings 的 Privacy history 卡片现直接提供 Clear listening history 与 Clear search history；
空集合禁用，点击沿用现有危险确认框，取消不改变状态。确认后分别进入既有 `clear_history` 与
`clear_search_history` SQLite 任务，收听历史继续同步清空 History、Keep listening、Forgotten favorites
并使 Stats 下次进入时刷新，搜索历史立即切换为空态。进行中锁定 Settings 写操作，成功或业务失败在
Settings 原位显示；收藏、歌单、队列、下载与远端历史不受影响。没有新增迁移、后端或测试设施。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Privacy History Controls

状态：功能与 UI 已实现，待真实桌面历史点击验收。

Android `PrivacySettings.kt` 直接提供 Pause listen history 与 Pause search history；Desktop 虽可分别
清空两类历史，但 Settings 还不能阻止后续写入。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/config.rs` / `src/storage/sqlite.rs` | 持久化两个历史暂停开关并升级桌面 schema |
| `src/ui/shell.rs` | Settings 增加两个开关，并接入现有本地收听/搜索历史写入入口 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Privacy 历史控制闭环 |

### 完成定义

1. Settings 可发现 Pause listening history 与 Pause search history，保存后跨重启保留。
2. 暂停收听历史时不再新增本地 `play_history` 或累计本地播放统计；已有历史、收藏、歌单和队列保留。
3. 暂停搜索历史时，手工提交、建议提交和链接直达都不新增本地搜索记录；搜索请求和结果照常工作。
4. 两个开关恢复后，后续达到已保存历史时长门槛的播放和有效搜索继续使用现有 SQLite 写入链路。
5. YouTube Music 远端播放历史仍只由现有独立同步开关控制，不把本地隐私开关扩成云端行为。
6. 不实现 Android 截图限制，不新增历史服务、事件总线或测试设施。
7. 只执行一次格式化和全目标编译检查。

完成情况：`AppSettings` 与 SQLite 现持久保存两项暂停开关，Settings 的 Privacy history 卡片可
分别选择继续记录或暂停本地收听/搜索历史。搜索暂停在既有统一提交入口跳过 SQLite 写入，不影响搜索、
建议和链接直达；收听暂停在已保存历史时长门槛跳过本地历史及累计统计，同时保留独立的 YouTube Music
远端历史注册开关。已有历史、收藏、歌单和队列不被修改；恢复记录后继续使用原写入链路。没有实现
Android 专属截图限制，也没有增加历史服务、事件总线或测试设施。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Storage Clear Actions

状态：功能与 UI 已实现，待真实桌面存储点击验收。

Android `StorageSettings.kt` 直接提供 Clear all downloads、Clear song cache 与 Clear image cache；Desktop
设置页目前只能修改缓存目录/容量，离线下载也只能到 Library 逐项移除，缺少三项真实存储操作入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/cache.rs` | 为现有播放 LRU 缓存增加整库清空操作 |
| `src/services/thumbnail.rs` | 为现有远端缩略图磁盘缓存增加整库清空操作 |
| `src/ui/shell.rs` | Settings 增加确认式清缓存/移除全部下载，并显示忙碌、成功和失败 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Storage 清理入口闭环 |

### 完成定义

1. Settings 的 Storage 区可发现 Clear playback cache、Clear artwork cache 与 Remove all downloads。
2. 两项缓存操作只清对应缓存目录；不删除 SQLite、本地歌单、自定义封面或显式下载。
3. Remove all downloads 逐项复用现有取消、资源删除和 SQLite 下载记录状态机，不复制下载后端。
4. 三项操作使用现有 GPUI 确认框；取消不改变状态，进行中、成功和业务失败在 Settings 可见。
5. 清图片缓存同时丢弃内存缩略图并重新加载当前可见图片；本地 `file:` 自定义封面源文件不删除。
6. 不新增设置字段、清理服务、定时任务或缓存扫描 UI。
7. 只执行一次格式化和全目标编译检查。

完成情况：Settings 已增加独立的播放缓存、图片缓存和全部离线下载清理入口，三项操作均使用现有
GPUI 确认框并原位显示进行中、成功或业务失败。播放缓存和图片缓存只清理各自现有目录；图片清理后
同时释放内存缩略图并重载当前可见图片，不触碰本地自定义封面源文件。Remove all downloads 逐项复用
既有活动任务取消、下载资源删除和 SQLite 记录状态机，保留歌单、收藏、历史、播放缓存与云端数据。
没有新增设置字段、清理服务、定时任务、缓存扫描 UI 或测试设施。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Local Playlist Automatic Cover Mosaic

状态：功能与 UI 已实现，待真实桌面多封面点击验收。

Android `Playlist.thumbnails` 在没有自定义封面时读取歌单歌曲封面；`LocalPlaylistHeader` 对一张封面
整图显示，对多张封面显示最多四格。Desktop 刚补齐自定义封面，但未设置时仍只显示 BookOpen，占位
没有反映歌单真实内容。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 随本地歌单查询读取 Custom position 前四首真实歌曲封面 |
| `src/ui/shell.rs` | 统一渲染单图/四格自动封面，并在歌曲增删、重排后刷新所有入口 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单自动封面行为 |

### 完成定义

1. 自定义封面存在时始终优先，不同时加载或覆盖自动封面。
2. 无自定义封面且只有一张可用歌曲封面时整图显示；两张以上时按 Custom position 显示最多四格。
3. 没有可用歌曲封面时继续使用现有 BookOpen 主题占位；不伪造图片 URL。
4. 详情使用当前真实歌曲集合，Library Overview/Playlists、Local Search 与 Home Speed Dial 使用同一
   SQLite 预览字段。
5. 加入、删除、批量删除或 Custom 重排后更新当前详情与已固定歌单预览；不新增 schema、图片表或缓存。
6. 只执行一次格式化和全目标编译检查。

完成情况：每次本地歌单与 Speed Dial join 查询现按真实 Custom position 读取前四个非空歌曲 thumbnail
URL，不增加 schema 或图片表。共用渲染器让自定义封面始终优先；没有自定义封面时，一张歌曲图整铺，
两至四张按 Android 标题区语义显示四格，完全无图片才显示 BookOpen。详情直接使用当前已加载歌曲，
Library Overview/Playlists、Local Search 与 Home Speed Dial 使用同一 SQLite 预览；加入、删除、批量
删除及相邻重排后同时刷新歌单集合、已固定项和可见缩略图。没有新增缓存或图片后端。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Local Playlist Custom Cover

状态：功能与 UI 已实现，待真实桌面图片选择点击验收。

Android `LocalPlaylistScreen.kt` 在本地歌单标题区显示真实封面，并允许从系统图片选择器设置、替换或
移除自定义封面；未设置时继续使用歌曲封面或默认占位。Desktop 目前所有本地歌单只显示 BookOpen
占位，缺少这项直接可见的歌单管理能力。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 持久化本地歌单自定义封面 URI，并随歌单查询返回 |
| `src/services/thumbnail.rs` | 让现有缩略图管线读取系统选择器返回的本地图片 URI |
| `src/ui/shell.rs` | 详情提供选择/替换/移除封面，并在资料库、搜索和 Speed Dial 显示 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单自定义封面闭环 |

### 完成定义

1. 本地歌单详情始终显示封面区域；没有自定义封面时显示现有主题占位。
2. Choose/Replace cover 调用 GPUI 原生单文件选择器，取消静默结束，图片读取或存储失败原位可见。
3. 支持现有缩略图管线可解码的本地图片；成功后 URI 写入 SQLite，并立即刷新当前详情与歌单集合。
4. Remove cover 只清空当前歌单封面，不修改歌曲、顺序、下载、收藏或播放状态。
5. 自定义封面同时用于 Library Overview/Playlists、Local Search 与 Home Speed Dial；不新增图片编辑器或
   平行缓存实现。
6. 只执行一次格式化和全目标编译检查。

完成情况：SQLite v27 已为本地歌单保存可选封面 URI，创建、重命名、排序和 Speed Dial join 均返回
同一字段。详情固定显示封面区域，并提供原生单文件选择器驱动的 Choose/Replace cover 与 Remove
cover；选择前复用现有缩略图解码确认图片可显示，取消静默结束，失败和成功留在当前详情可见。更新会
立即同步当前详情、Library Overview/Playlists、Local Search 与 Home Speed Dial；移除只清空封面
字段，不触碰歌曲、顺序、下载、收藏或播放状态。没有新增依赖、图片编辑器或平行缓存。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Local Playlist Remove Downloads

状态：功能与 UI 已实现，待真实桌面下载点击验收。

Android `LocalPlaylistMenu` 根据整组下载状态显示 Download、Downloading 或 Remove download；后两种
点击同一确认框，逐首取消进行中任务并删除已保存离线内容。Desktop 详情已有 Download all，但必须
去全局 Downloads 才能移除这张歌单已有的离线记录。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 检测当前歌单真实下载记录，增加确认后整组取消/删除入口 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单下载生命周期闭环 |

### 完成定义

1. 当前歌单任一歌曲存在下载记录时显示 Remove downloads；无记录时不显示伪入口。
2. 确认框明确删除离线音频或进度，但保留歌单、收藏、历史及云端资料。
3. 已完成/暂停/失败记录复用现有缓存与 SQLite 删除；Queued/Downloading 先取消，再由同一状态机删除。
4. 删除中按钮显示 Removing，失败继续显示在详情既有 Download error 区；Download all 仍可补齐缺失项。
5. 不新增下载表、批量后端、缓存实现或设置。
6. 只执行一次格式化和全目标编译检查。

完成情况：当前歌单只要有一首真实下载记录，详情便显示 Remove downloads；确认框列出记录数并明确
保留歌单、收藏、历史和云端资料。确认后逐首复用现有 `remove_download`：活动任务设置取消与待删除，
Queued 从队列移除，其他状态直接删除缓存资源和 SQLite 记录；进行中显示 Removing，任一失败继续进入
详情 Download error。Download all 保留，可补齐尚未下载的歌曲。没有新增表、缓存或平行状态机。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Local Playlist CSV / M3U Export

状态：功能与 UI 已实现，待真实桌面 Save 对话框点击验收。

Android `PlaylistMenu.kt` / `PlaylistExporter.kt` 可把当前本地歌单顺序保存或分享为 CSV、M3U；CSV
包含 Title、Artist、Album、YouTube Video ID，M3U 包含 EXTINF 与标准 YouTube watch URL。Desktop
目前只有复制标题列表，缺少真实文件导出。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 生成 Android 对应 CSV/M3U，并调用 GPUI 三平台原生 Save 对话框写入用户路径 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单文件导出闭环 |

### 完成定义

1. 非空本地歌单可直接发现 Export CSV 与 Export M3U，内容使用当前排序后的完整集合。
2. CSV 正确转义双引号，包含 Title/Artist/Album/YouTube Video ID；M3U 使用 EXTINF 与 watch URL。
3. 使用锁定 GPUI 的原生 Save 对话框和系统默认 Downloads/Documents 目录，不新增依赖。
4. 取消选择不报错；对话框失败、文件写入失败及成功路径在当前详情可见。
5. 文件写入不修改 SQLite、播放队列、下载状态或歌单顺序。
6. 只执行一次格式化和全目标编译检查。

完成情况：非空详情现直接提供 Export CSV 与 Export M3U，并按当前排序后的完整集合生成文件。CSV
使用 Android 的四列和双引号转义；M3U 输出 EXTINF、歌手/标题及真实 YouTube watch URL，未知时长
使用标准 `-1`。保存复用锁定 GPUI 的三平台原生路径选择器，默认从 Downloads/Documents 开始；取消
静默结束，对话框或写入失败显示 Library error，成功显示格式和最终路径。未新增依赖、SQLite 或队列
写入。`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Local Playlist Custom Reorder

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `LocalPlaylistScreen.kt` 在 Custom sort、非搜索和非选择状态提供 drag handle，拖动结束后在
数据库 transaction 中调用 `move(playlistId, from, to)`。Desktop 已显示 Custom order，但没有修改
该真实顺序的入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 增加按真实相邻 position 交换本地歌单歌曲的单事务命令 |
| `src/ui/shell.rs` | Custom order 下增加 Move up/Move down，并刷新详情与歌单排序状态 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单自定义重排闭环 |

### 完成定义

1. 只有 Custom order、Search 为空且未处于 Select 模式时显示 Move up/Move down。
2. 首项禁用 Move up、末项禁用 Move down；点击按真实 video ID 与相邻 SQLite position 交换。
3. 交换在一个 transaction 内完成，避开 `(playlist_id, position)` 唯一约束并更新歌单时间。
4. 成功后刷新详情顺序及歌单列表；失败保留当前详情并在既有 Library error 区显示。
5. 不增加拖拽框架、排序持久化副本或新 schema；桌面按钮是同一 Android 功能的直接可点击入口。
6. 只执行一次格式化和全目标编译检查。

完成情况：Custom order、空 Search、非 Select 状态的歌曲行现显示 Move up/Move down，首尾边界直接
禁用。点击以真实 video ID 在存储线程 transaction 内寻找相邻 SQLite position，借用同歌单尾部临时
位置避开唯一约束后完成交换，并更新歌单时间。操作中详情显示 Updating custom order，成功刷新详情
和歌单列表，失败保留当前内容并显示 Library error。没有引入拖拽依赖、顺序副本或 schema。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Local Playlist Multi-select Actions

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `LocalPlaylistScreen.kt` 长按歌曲进入多选，顶部可全选并打开 `SelectionSongMenu`；所选歌曲可
Play、Shuffle、Play next、Add to queue、Add to playlist、Download，并可从当前本地歌单批量删除。
Desktop 详情已有单曲及整组动作，但没有选择子集的入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 增加本地歌单多首删除的单事务命令 |
| `src/ui/shell.rs` | 增加可发现 Select 模式、逐行选择/全选和真实批量动作 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单多选闭环 |

### 完成定义

1. 非空本地歌单提供 Select；进入后逐行选择、取消、Select all/Clear all 和选中数量可见。
2. 选择身份使用真实 `video_id`，排序或搜索变化不会选错歌曲；动作按当前排序后的完整集合顺序取子集。
3. Play、Shuffle、Play next、Queue 使用现有真实队列和 Guest 边界；Add to playlist 复用现有批量 Picker。
4. Download 逐首进入现有持久下载队列；Remove selected 在存储线程单事务删除并刷新详情及歌单计数。
5. 打开/关闭歌单或批量删除成功后退出选择状态，不新增选择持久化或新的播放/下载后端。
6. 只执行一次格式化和全目标编译检查。

完成情况：非空本地歌单详情现有可发现的 Select 入口，选择模式显示逐行 Selected 状态、选中数量、
Select all/Clear all 与 Done。选择以真实 video ID 保存，排序和 Search 改变后仍按当前排序后的完整
集合顺序生成子集。Play、Shuffle、Play next、Queue、Add to playlist、Download 均复用既有状态机；
Remove selected 通过存储线程单事务删除并刷新详情、元数据和歌单计数。打开/关闭详情及删除成功会
清除会话内选择，没有新增持久化 schema、播放或下载后端。`cargo fmt --all && cargo check
--all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Local Playlist Song Sort

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `LocalPlaylistViewModel.kt` / `SortHeader` 提供 Custom、Create date、Name、Artist、Play time
及非 Custom 的升降序切换；排序后的完整集合决定行顺序和点击播放队列。Desktop 已补详情搜索，但
仍固定使用 SQLite position 顺序。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 为歌单歌曲读取真实 `added_at_ms` 与全量历史累计播放时长 |
| `src/ui/shell.rs` | 增加五种详情排序、方向切换，并让搜索/集合动作/逐曲播放共享排序后完整集合 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单歌曲排序闭环 |

### 完成定义

1. Custom 始终使用 SQLite `position ASC`，不显示或应用方向；其他四种支持 Ascending/Descending。
2. Date added 使用 `local_playlist_song.added_at_ms`，Play time 使用全量 `play_history.play_time_ms` 聚合。
3. Title 与 Artist 大小写不敏感并以原 Custom 位置稳定消歧，不修改数据库中的自定义顺序。
4. Search 在排序后的完整集合上过滤；点击结果用排序后完整集合和该行索引建立真实播放队列。
5. Play、Queue all、Download all、Copy track list 使用当前排序后的完整集合；Shuffle 仍随机化同一集合。
6. 排序选择在当前 Desktop 会话跨本地歌单保留，不新增设置 schema 或数据库写入。
7. 只执行一次格式化和全目标编译检查。

完成情况：本地歌单详情现提供 Custom order、Date added、Title、Artist、Play time 五种排序；Custom
直接保留 SQLite position，其他排序可切换升降序。Date added 读取歌单关系的真实加入时间，Play time
聚合完整本地播放历史；同值项稳定回到 Custom 位置。搜索、列表行、逐曲播放队列及 Play、Queue all、
Download all、Copy track list 共享当前排序后的完整集合，Shuffle 随机化同一集合。选择只在 Desktop
会话内保留，没有新增设置或数据库写入。`cargo fmt --all && cargo check --all-targets` 已通过，仅保留
既有未使用方法警告。

## 已实现切片：Local Playlist Song Search

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `LocalPlaylistScreen.kt` 在歌单详情提供 Search，按歌曲标题或任一歌手名称实时过滤；点击过滤
结果时仍按完整歌单顺序建立队列，并以该歌曲在完整歌单中的位置开始。Desktop 本地歌单详情没有
搜索入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 增加详情 Search Input、实时标题/歌手过滤、结果计数/空状态及清理行为 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单详情搜索闭环 |

### 完成定义

1. 已加载且非空的本地歌单直接显示可发现的搜索框，输入变化不访问网络或数据库。
2. 非空查询按大小写不敏感的歌曲标题或任一歌手名称过滤，保持当前详情排序。
3. 点击过滤结果仍用排序后的完整歌单建立真实队列，并以该歌曲在该集合中的索引开始，符合 Android 行为。
4. 当前结果数、无匹配与 Clear filter 均在详情可见；移除歌曲继续使用原始歌曲身份。
5. 打开新歌单或关闭详情会清空查询和输入，不把上一歌单过滤条件带到下一歌单。
6. 不增加全文索引、搜索表、选择模式或新的播放后端；详情排序由后续 Song Sort 切片补齐。
7. 只执行一次格式化和全目标编译检查。

完成情况：已加载且非空的本地歌单详情现直接显示 Search Input；输入按大小写不敏感的标题或任一
歌手名称实时过滤，保留当前详情排序，并显示结果数、Clear filter 和无匹配状态。结果行继续携带
排序后的完整歌单及对应索引，因此点击不会把过滤子集误建成队列；移除、Next、Queue、Download 仍
针对真实歌曲身份。打开新歌单或关闭详情会清空输入。没有增加网络请求、全文索引或平行后端。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Local Playlist Collection Actions

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `LocalPlaylistHeader` 直接提供 Play/Shuffle，`LocalPlaylistMenu` 对同一真实歌曲集合提供 Add to
queue、Download 和 Share track list。Desktop 本地歌单详情目前只有 Play、Rename、Delete 与逐曲
动作，缺少这些集合级入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 本地歌单详情增加 Shuffle、Queue all、Download all、Copy track list 与失败显示 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记本地歌单集合动作点击闭环 |

### 完成定义

1. 非空本地歌单可按当前详情顺序 Play，并可对同一完整集合本地随机后 Shuffle 播放。
2. Queue all 复用现有队列语义：空队列直接播放，非空队列追加；Shuffle 开启时进入未播放随机区。
3. Download all 逐首进入现有持久下载队列，已完成或已排队项继续由既有去重逻辑跳过，失败在当前页可见。
4. Copy track list 按 Android 本地 Share 的正文语义复制按歌单顺序排列的歌曲标题，不伪造在线链接。
5. Guest 禁用 Play、Shuffle 与 Queue all；下载、复制、重命名和删除保持既有边界。
6. 不增加导出文件格式、文件选择器、同步或新的队列/下载后端。
7. 只执行一次格式化和全目标编译检查。

完成情况：非空本地歌单详情现直接提供 Play、Shuffle、Queue all、Download all 与 Copy track list。
前三个播放/队列动作复用现有 Guest 边界和真实会话持久化，批量下载逐首进入既有持久下载队列并
在当前详情显示失败；复制正文与 Android 一样按歌单顺序仅包含歌曲标题。动作行允许窄窗口换行，
没有增加导出器、文件选择器、同步或平行后端。`cargo fmt --all && cargo check --all-targets` 已通过，
仅保留既有未使用方法警告。

## 已实现切片：Home Speed Dial Recommendations and Randomize

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `HomeViewModel.speedDialItems` 会先放持久固定项，再以 Keep Listening、Quick Picks 去重补到
27 项；`HomeScreen.kt` 首屏保留 Randomize 可点击槽，`getRandomItem()` 按 80% 用户歌曲、20% 其他
Home 目录选择并分别播放或进入详情。Desktop 已有三类持久固定项，但尚未恢复补位和 Randomize。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 合并现有 Keep Listening/Quick Picks 补位，增加 Randomize 卡片和真实播放/详情动作 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Speed Dial 推荐补位与 Randomize UI |

### 完成定义

1. 固定项优先，按真实 ID 去重后先取 Keep Listening、再取现有 Quick Picks，媒体项总数最多 27。
2. 补位项只存在于 Home 当前状态，不写 SQLite、不显示 Unpin；Song 点击建立真实单曲播放队列。
3. Randomize 使用现有歌曲和 Home Browse 项；候选都有时按 Android 的 80%/20% 选择，分别播放或打开详情。
4. Listen Together Guest 或无候选时禁用 Randomize；不增加一秒假等待、动画状态、推荐请求或设置项。
5. 已固定 Song/Browse/Local Playlist 的持久化与点击语义保持不变。
6. 只执行一次格式化和全目标编译检查。

完成情况：Home 现将持久固定项放在前面，再按真实 ID 先合并 Keep Listening、后合并既有 Quick
Picks，媒体项总数限制为 27；补位歌曲只存在于当前状态，点击建立真实单曲播放队列，并继续提供
Next、Queue 与 Download。Randomize 卡片从现有歌曲及当前 Home Browse 项中按 Android 80%/20%
语义选择，分别播放或进入既有详情；Guest 和无候选状态不可点击。没有增加等待动画、请求、存储或
设置。`cargo fmt --all && cargo check --all-targets` 在修正锁定组件库的直接图标名称后通过，仅保留
既有未使用方法警告。

## 已实现切片：Local Playlist Speed Dial

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `PlaylistMenu.kt` 可把本地歌单以 `LOCAL_PLAYLIST` 固定到 Speed Dial，`HomeScreen.kt` 点击该
类型会进入 `local_playlist/{id}`。Desktop 已有本地歌单详情和刚完成的 Song/Browse Speed Dial，但
本地歌单没有 Pin 入口、持久关系或 Home 点击路径。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | SQLite v26 增加带删除级联的 Local Playlist Speed Dial 关系与有序读取 |
| `src/ui/shell.rs` | 本地歌单详情增加 Pin/Unpin，Home 展示、打开和移除本地歌单固定项 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 更新 Speed Dial 完整类型覆盖与数据库版本 |

### 完成定义

1. 本地歌单详情直接显示 Pin/Unpin，点击写入真实 SQLite 状态，忙碌、成功与失败在现有界面可见。
2. SQLite v26 原位保留 v25 Song/Browse 固定项；删除本地歌单自动移除其固定关系，重命名后读取最新名称。
3. Home 展示固定的本地歌单，点击复用现有 `open_playlist` 详情和真实歌曲读取，并可原位 Unpin。
4. 本地歌单无独立封面字段时使用现有 BookOpen 占位，不伪造远端 BrowseItem 或复制歌曲快照。
5. 本切片当时不实现推荐填满、Randomize 槽或新的 Speed Dial 设置；前两项已由本文顶部的后续切片
   补齐。
6. 只执行一次格式化和全目标编译检查。

完成情况：SQLite v26 已增加随本地歌单删除自动级联的固定关系，并通过 join 读取最新名称与真实
歌曲数。Local Playlist 详情提供 Pin/Unpin；Home 展示本地固定项，点击复用现有 `open_playlist` 和
SQLite 歌曲读取，且可原位 Unpin。重命名和删除会同步当前 Home 状态；无独立封面时沿用 BookOpen
占位，没有伪造 BrowseItem 或歌曲快照；推荐填充与 Randomize 已由本文顶部的后续切片补齐。
`cargo fmt --all && cargo check
--all-targets` 已通过。

## 已实现切片：Browse Speed Dial

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `SpeedDialItem.kt` / `HomeScreen.kt` 支持 Song、Album、Artist、Playlist/Podcast 固定项：Song
直接播放，其余点击进入对应详情。Desktop 目前 SQLite v24 和 Home 仅保存/展示 Song；现有 Browse
详情、嵌套 Back 与真实 endpoint 已完成，可以直接恢复非 Song 固定入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | SQLite v25 增加 Browse Speed Dial 元数据与 Pin/Unpin/有序读取 |
| `src/ui/shell.rs` | Album/Artist/Playlist/Podcast 详情增加 Pin/Unpin，Home 展示、打开和移除 Browse 固定项 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 更新 Speed Dial 类型覆盖与数据库版本 |

### 完成定义

1. Browse 详情仅为 Album、Artist、Playlist、Podcast 显示 Pin/Unpin；Category 不显示。
2. 固定时保存真实 `BrowseItem` ID、kind、标题、副标题、缩略图与 params，不依赖内存目录。
3. SQLite v25 原位升级并保留 v24 Song Speed Dial；Browse 类型和 ID 组成稳定唯一身份。
4. Home 同时展示 Song 与 Browse 固定项；Song 继续播放，Browse 点击复用现有真实详情，均可 Unpin。
5. 固定顺序按各表 `pinned_at` 稳定恢复；本切片当时不实现 Android Randomize 槽或 Local Playlist
   类型，后者已由本文顶部的后续切片补齐。
6. 只执行一次格式化和全目标编译检查。

完成情况：SQLite v25 已为 Album、Artist、Playlist 与 Podcast 保存真实 Browse 固定元数据，并保留
v24 Song Speed Dial。对应 Browse 详情提供 Pin/Unpin，Category 不显示入口；Home 同时恢复 Song 与
Browse 固定项，Song 继续建立播放队列，Browse 点击进入既有真实详情，全部类型可原位 Unpin。未扩展
Randomize、推荐填充或新的设置面板；Local Playlist 类型已由本文顶部的后续切片补齐。`cargo fmt --all && cargo check
--all-targets` 已通过。

## 已实现切片：Collection Add All to Local Playlist

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `YouTubePlaylistMenu.kt` 对整个 collection 提供 Add to playlist。Desktop 本地 Playlist Picker
只接受单曲，Album/Playlist 详情没有整组入口；对长 collection 直接使用首屏会漏曲，逐首独立写库又会
暴露部分成功状态。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 增加按原序、去重语义不变的本地歌单批量事务 |
| `src/ui/shell.rs` | 本地 Picker 支持一首或完整 collection，详情增加 Add all to playlist |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记整组本地歌单写入入口 |

### 完成定义

1. Album/Playlist 详情显示 Add all to playlist，先复用真实 continuation 补齐完整 collection。
2. 补齐成功后打开现有本地歌单 Picker，并显示待加入曲目数量；无本地歌单沿用现有 Library 引导。
3. 用户选择目标后在存储线程的单个 SQLite transaction 中按 collection 原序插入。
4. 现有 `INSERT OR IGNORE` 去重语义保持；失败整体回滚并通过现有 Library error 可见。
5. 单曲 Add to playlist 复用同一批量路径，云端 Picker 和云端非幂等写入不变。
6. 只执行一次格式化和全目标编译检查。

完成情况：Album/Playlist 详情现有 Add all to playlist；点击先补齐真实 continuation，再打开既有
本地 Picker 并显示曲目数。用户选定目标后，存储线程以单个 transaction 按原序 upsert Song 并
`INSERT OR IGNORE` 歌单关系，任一错误整体回滚；单曲入口也复用一项批量路径。无本地歌单引导、
Library error 与云端 Picker 行为保持不变。`cargo fmt --all && cargo check --all-targets` 已通过，
仅保留既有未使用方法警告。

## 已实现切片：Artist About Metadata and Copy Link

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `ArtistScreen.kt` 的 About 区从 browse header 显示 subscriber count、monthly listeners 与
description，`YouTubeArtistMenu.kt` 用真实 Artist ID 分享 channel 链接。Desktop 目前仅保留 description，
另两个真实字段和 Share 入口均被丢弃。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/domain/browse.rs` / `src/services/innertube.rs` | 保留 Artist header 的 subscriber/monthly listener 文本 |
| `src/ui/shell.rs` | Artist 详情展示 About 元数据并增加标准 channel Copy link |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Artist About 与分享入口 |

### 完成定义

1. 按 Android immersive header 的 subscriptionButton2/subscriptionButton 路径解析 subscriber 文本。
2. 按 `monthlyListenerCount` 原字段显示月听众，不解析或伪造数值。
3. 仅 Artist 详情展示返回的非空元数据，description 现有显示不变。
4. Copy link 使用真实 browse ID 生成 Android 同源 `music.youtube.com/channel/{id}` 并写系统剪贴板。
5. 不增加请求、设置项、缓存或平台分支。
6. 只执行一次格式化和全目标编译检查。

完成情况：Artist browse 现按 Android 路径保留 subscriber count 与 monthly listener count 原始文本，
详情与 description 一起形成 About artist 区；缺失字段不显示。Copy link 使用真实 Artist browse ID
生成 Android 同源 channel URL，并复用现有 GPUI 系统剪贴板。没有新增请求、缓存、设置或平台分支。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Collection Copy Link

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `YTItem.kt` 为 Album/Playlist 定义标准 `music.youtube.com/playlist?list=` share link，
`YouTubePlaylistMenu.kt` 暴露 Share。Desktop 已有歌曲 Copy link 与系统剪贴板能力，但 collection
详情没有对应入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Album/Playlist 详情增加标准链接 Copy link 按钮 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Collection 分享入口及真实 ID 规则 |

### 完成定义

1. Playlist 使用真实 browse ID，生成链接前只移除协议层 `VL` 前缀。
2. Album 仅使用 browse 响应解析出的真实 playlist ID；缺失时不显示伪链接按钮。
3. 点击复用现有 GPUI 系统剪贴板能力，不增加平台分支或外部服务。
4. Artist/Podcast 和逐曲 Copy link 语义不变。
5. 只执行一次格式化和全目标编译检查。

完成情况：Album/Playlist 详情现显示 Copy link；Playlist 按 Android shareLink 规则只去除 browse
请求使用的 `VL` 前缀，Album 只使用响应真实 playlist ID。点击复用既有 GPUI 系统剪贴板，缺少
Album playlist ID 时不生成按钮；其他详情和逐曲链接不变。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Complete Collection Play Shuffle and Download

状态：功能与 UI 已实现，待真实桌面点击验收。

Desktop Album/Playlist 的 Play all、本地 Shuffle 和 Download all 目前直接使用已加载 `songs`；长
collection 尚有 continuation 时会静默漏掉后续曲目。Android 的结果语义是对完整 collection 操作，
上一切片已接通受限完整 browse 合并，本切片让三个现有按钮直接复用该路径。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 扩展既有 collection action 为 Play all、完整本地 Shuffle 和 Download all |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 锁定长 collection 的完整动作语义 |

### 完成定义

1. Album/Playlist 的 Play all 在开始播放前补齐真实 continuation，失败不替换当前队列。
2. Album 及无服务端 endpoint 的 Playlist 本地 Shuffle 先补齐完整 collection，再随机并播放。
3. Download all 先补齐完整 collection，再逐曲进入现有持久下载队列；Guest 不受播放控制限制影响。
4. 加载时对应按钮显示状态且全部 collection 动作互斥，失败在详情原位可见并可重试。
5. Artist/Podcast/Category 的既有 Play all 语义和 Playlist 服务端 Shuffle/Radio 不变。
6. 只执行一次格式化和全目标编译检查。

完成情况：Album/Playlist 的 Play all、本地 Shuffle 与 Download all 现与整组 Next/Queue 共用完整
browse completion；长 collection 会先在现有重复 token、无进展及 64 页上限内补齐，成功后才播放、
随机或排入下载。对应按钮显示加载且与其他 collection 动作互斥，失败回到 Browse 详情原位显示；
Download all 不受 Guest 播放控制限制。Playlist 服务端 Shuffle/Radio 及其他页面 Play all 保持原链路。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Collection Play Next and Add to Queue

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `YouTubePlaylistMenu.kt` 对整个 Online Playlist 提供 Play next / Add to queue，必要时通过
`completed()` 取得歌曲；Desktop 详情目前只有逐曲 Next/Queue。直接对首屏循环会让长 collection
静默缺少 continuation 曲目，因此本切片复用现有完整 browse 合并后再执行整组队列操作。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 公开现有受 continuation 上限保护的 BrowsePage 完整合并能力 |
| `src/ui/shell.rs` | Album/Playlist 增加整组 Play next / Add to queue、加载和原位失败状态 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Collection 整组队列入口和长列表语义 |

### 完成定义

1. Album/Playlist 详情显示 Play next all 与 Queue all，Guest 使用现有播放控制限制。
2. 有 continuation 时先从真实 browse 后端依次补齐，重复 token、无进展和页数上限沿用现有保护。
3. Play next 保持 collection 原顺序并紧接当前项；Queue all 在 Shuffle 关闭时按原顺序追加到尾部，
   Shuffle 开启时沿用现有语义参与未播放部分随机。
4. 当前队列为空时，两种动作均直接建立 collection 队列并从第一首播放。
5. 加载期间按钮禁用，失败在当前详情可见且再次点击重试；失败不改变现有队列。
6. 只执行一次格式化和全目标编译检查。

完成情况：Album/Playlist 详情现提供 Play next all 与 Queue all；点击先通过现有受 64 页、重复
token 和无进展保护的 browse completion 补齐长 collection，成功后才改变队列。Play next 按原序
紧接当前项，Queue all 在 Shuffle 关闭时原序追加、开启时参与未播放部分随机；空队列直接建立并
播放。加载、失败重试和 Guest 限制均在当前详情可见。`cargo fmt --all && cargo check --all-targets`
已通过，仅保留既有未使用方法警告。

## 已实现切片：Collection Creator Navigation

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `OnlinePlaylistScreen.kt` 的 creator 行会用真实 `author.id` 进入 Artist，`AlbumScreen.kt`
也允许点击带真实 ID 的 artists。Desktop browse 已从 collection header 提取这些 Artist credit 作为
歌曲元数据兜底，但详情头部只显示不可点击 subtitle，入口被丢弃。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/domain/browse.rs` / `src/services/innertube.rs` | 在 Album/Playlist BrowsePage 保留 header 中带真实 ID 的 creator links |
| `src/ui/shell.rs` | Collection 详情头部显示可点击 creator 按钮，复用现有 Browse 与 Back 栈 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Album/Playlist → Artist 点击路径 |

### 完成定义

1. 只使用 collection header 已有的真实 Artist browse ID，不从显示文本猜测身份。
2. Playlist 支持作者入口，Album 支持一个或多个 artist 入口；相同 ID 去重。
3. 点击复用现有 Artist browse 后端，详情内 Back 恢复已加载 collection。
4. 没有真实 ID 时不显示伪 creator 按钮，现有 subtitle 仍保留。
5. 不增加请求端点、缓存或独立页面。
6. 只执行一次格式化和全目标编译检查。

完成情况：Album/Playlist browse 现把 header 中带真实 ID 的 Artist credit 保存为 creator links；
Playlist author 与 Album 一个或多个 artists 在详情以可点击行显示，相同 ID 去重。点击复用现有 Artist
Browse 和内存 Back 栈；无真实 ID 时不显示伪入口，原 subtitle 不变。没有新增请求、缓存或页面。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Online Playlist Server Shuffle and Radio

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `YouTubePlaylistMenu.kt` 仅在 Online Playlist 响应携带真实 `shuffleEndpoint` / `radioEndpoint`
时显示 Shuffle 和 Start radio，并通过 `YouTubeQueue` 请求服务端队列。Desktop Playlist 当前只有
对已加载歌曲执行本地随机的 Shuffle，没有 Playlist Radio，且丢弃了两个 header menu endpoint。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 按 Playlist header 菜单图标解析真实 Shuffle / Radio endpoint |
| `src/ui/shell.rs` | Playlist 详情优先显示服务端 Shuffle，并增加 Radio；复用现有 endpoint 队列及状态 UI |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Online Playlist 真实服务端队列入口 |

### 完成定义

1. 对照 Android header menu 的 `MUSIC_SHUFFLE` 与 `MIX` 项读取原始 watch-playlist endpoint。
2. Playlist 有真实 Shuffle endpoint 时点击请求服务端顺序；没有时保留并明确使用现有已加载曲目随机。
3. Radio 只在真实 endpoint 存在时显示，不使用首曲电台伪造 Playlist Radio。
4. 成功后才替换队列并播放；加载、失败、重试及 Guest 限制复用现有详情状态机。
5. Album 现有本地整组 Shuffle 和 Download all 语义不变。
6. 只执行一次格式化和全目标编译检查。

完成情况：Online Playlist browse 现按 Android header 菜单图标保留真实 Shuffle / Radio endpoint；
服务端 Shuffle 存在时详情按钮请求其真实队列，Radio 只在真实端点存在时显示。缺少 Shuffle endpoint
时原按钮明确标为 Shuffle loaded，继续只作用于已加载歌曲；两种服务端动作均成功后才替换并播放，
沿用现有加载、失败重试和 Guest 限制。`cargo fmt --all && cargo check --all-targets` 已通过，
仅保留既有未使用方法警告。

## 已实现切片：Artist Section View All

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `ArtistScreen.kt` 会把 Artist 的 Songs、Albums、Singles 等 section 标题连接到响应携带的
`moreEndpoint`，进入 `ArtistItemsScreen` 查看真实完整列表。Desktop 已能请求并分页同类 browse
响应，但 Artist 解析只保留扁平歌曲和目录，用户无法发现这些完整分区。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/domain/browse.rs` / `src/services/innertube.rs` | 保留 Artist shelf/carousel 的标题及真实 `browseId + params` |
| `src/ui/shell.rs` | Artist 详情显示可点击 View all 分区，复用现有 browse、Back 和 continuation UI |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Artist 完整分区入口 |

### 完成定义

1. 按 Android `ArtistPage.fromSectionListRendererContent` 的 shelf/carousel 路径读取 section 标题。
2. 仅保留响应真实 `moreEndpoint`，没有 endpoint 的 section 不显示伪 View all。
3. 点击进入现有 Browse 详情并发送原始 `browseId + params`；歌曲、专辑等继续复用既有真实解析。
4. 详情内 Back 恢复已加载 Artist，目标页沿用现有加载、失败、空数据和 continuation 状态。
5. 不增加请求端点、缓存层或独立 ArtistItems 页面。
6. 只执行一次格式化和全目标编译检查。

完成情况：Artist browse 现按 Android 的 shelf/carousel 路径保留标题与真实 `moreEndpoint`，详情以
View all 列表公开这些入口；点击发送原始 `browseId + params` 并复用既有 Browse 歌曲/目录解析、
continuation 和详情内 Back 栈。没有 endpoint 的 section 不显示伪入口，也没有新增网络端点或页面。
门禁首次发现一处渲染调用位置接线错误，原位修正后 `cargo fmt --all && cargo check --all-targets`
复查通过，仅保留既有未使用方法警告。

## 已实现切片：Artist Radio and Shuffle

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `ArtistScreen.kt` 从 Artist 详情头部的真实 `radioEndpoint` 与 `shuffleEndpoint` 建立
YouTube Music 队列。Desktop Artist 详情已经展示真实歌曲、目录和订阅，但丢弃了 browse 响应中的
两个播放端点，只能逐曲播放。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/domain/browse.rs` / `src/services/innertube.rs` | 保留 Artist 头部真实播放端点并复用既有 `next` 队列请求 |
| `src/ui/shell.rs` | Artist 详情增加 Radio / Shuffle 按钮及加载、失败状态，成功后播放服务端返回队列 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Artist 详情真实集合播放入口 |

### 完成定义

1. 只按 Android 现有解析路径读取 Artist `startRadioButton` 与 `playButton` 的真实 endpoint。
2. 仅在响应携带对应 endpoint 时显示 Radio / Shuffle，不使用首曲电台或本地随机伪造结果。
3. 点击后复用现有 InnerTube `next` 请求和队列歌曲解析，成功才替换当前队列并开始播放。
4. 加载时对应按钮禁用并显示进度文案，失败在 Artist 详情原位可见并允许再次点击重试。
5. Listen Together Guest 继续使用既有播放控制限制。
6. 只执行一次格式化和全目标编译检查。

完成情况：Artist browse 现按 Android 路径保留头部真实 Radio / Shuffle endpoint；仅在端点存在时
显示按钮，点击复用既有 `next` 队列请求与 Song 解析，成功后才以服务端队列替换当前播放。对应按钮
具有加载文案和禁用态，请求失败在 Artist 详情原位显示，再次点击即可重试；Guest 限制保持不变。
`cargo fmt --all && cargo check --all-targets` 已通过，仅保留既有未使用方法警告。

## 已实现切片：Podcast Episode Search

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `OnlinePodcastScreen.kt` 可在当前 Podcast 内按 episode 标题或作者实时搜索，并以过滤结果
建立播放队列。Desktop Podcast 详情已展示真实 episodes，但没有详情内搜索入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Podcast 详情增加实时过滤、结果计数、清空和无匹配状态 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Podcast episode 搜索与播放语义 |

### 完成定义

1. 仅 Podcast 详情显示 episode 搜索输入，按标题或 `artist_line` 大小写不敏感匹配。
2. 输入变化立即过滤已加载 episodes，不发新请求、不复制 Song 数据。
3. 结果显示匹配数/总数；无匹配与节目本身无 episodes 使用不同空状态，可一键清空查询。
4. 点击任一结果以过滤后的完整集合和对应 index 建立真实播放队列，与 Android 一致。
5. 每行继续复用现有 Play、Later、Local/YT playlist、Download、Next 和 Queue 动作。
6. continuation 继续作用于原详情，加载更多后当前查询自动重新过滤。
7. 只执行一次格式化和全目标编译检查。

完成情况：Podcast 详情现有独立 episode 搜索输入，按 Android 同样规则对标题和作者执行大小写
不敏感过滤；输入即时更新匹配数/总数，提供 Clear，并区分无匹配与节目无 episodes。点击结果会以
过滤后的完整 Song 集合和对应 index 建立真实队列，逐集 Later、Playlist、Download、Next、Queue
继续复用既有动作；continuation 追加后当前查询自动重新计算，没有新增请求或数据副本。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Podcast View Channel and Nested Browse Back

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `OnlinePodcastScreen.kt` 的播客详情头部使用真实 author/channel ID 提供 `View channel`。
Desktop 已从同一详情响应解析 `channel_id`，但只用于保存同步，没有可点击频道入口；现有详情到
Related/Artist/Album 的跳转也会覆盖当前详情，Back 直接退出而不能返回上一层。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 播客详情增加真实 View channel，并为既有 Browse 状态增加最小内存返回栈 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记播客频道入口和详情返回语义 |

### 完成定义

1. 播客详情仅在响应提供非空真实 `channel_id` 时显示 View channel，不猜测频道身份。
2. 点击复用现有 Artist/Channel browse 后端与详情 UI，不增加请求端点或平行页面。
3. Channel Back 恢复刚才的 Podcast 详情及其已加载 episodes，不重复请求。
4. 同一返回栈也服务现有详情内 Related/Artist/Album 跳转；最外层 Back 仍回到原 Home/Search/Library。
5. 新的顶层详情会清空旧栈；栈只在内存中保存已加载 `BrowsePage`，不落库。
6. 加载、失败、重试继续使用现有 Browse 状态。
7. 只执行一次格式化和全目标编译检查。

完成情况：Podcast 详情现仅在详情响应提供真实非空 `channel_id` 时显示 View channel，并复用
现有 Artist/Channel Browse 请求和页面。详情内跳转会把已加载 `BrowsePage` 压入最小内存返回栈，
Channel Back 直接恢复 Podcast 与 episodes；既有 Related/Artist/Album 嵌套跳转也获得相同返回
语义，最外层仍回原路由。没有新增端点、页面、存储或持久栈。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Full Player Equalizer

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `PlayerMenu.kt` 从当前播放器直接打开 Equalizer。Desktop 已有十段预设、AutoEQ/APO 档案、
即时音频链应用、SQLite 保存和失败回滚，但完整播放器没有可发现入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 完整播放器增加 EQ 标签，并让预设复用现有即时应用/保存/回滚状态机 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记完整播放器 Equalizer 点击闭环 |

### 完成定义

1. 完整播放器直接显示 EQ 标签和当前生效状态，不要求先退出播放器寻找设置。
2. Off/Flat、Bass、Vocal、Treble 点击后立即重建真实音频链并保存 SQLite。
3. 保存失败恢复此前音频设置，Applying、成功和失败状态在 EQ 标签内可见。
4. 已应用 AutoEQ/APO 档案显示真实档案名，并可进入既有完整 Settings 继续导入、选择或编辑十段 EQ。
5. Listen Together 不限制本机 DSP；不新增音频后端、数据库表或平行 Equalizer 状态。
6. 只执行一次格式化和全目标编译检查。

完成情况：完整播放器现有独立 EQ 标签，显示 Off、十段预设、自定义十段或当前真实 AutoEQ/APO
档案名；Off、Bass、Vocal、Treble 直接复用既有音频链即时重建、SQLite 保存和保存失败回滚，
Applying、成功、失败均原位可见。完整导入、档案选择和十段编辑继续由已有 Settings 提供并可直接
进入，没有新增音频后端、数据库表或 Equalizer 状态机。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Custom Sleep Timer Duration

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `Player.kt` 的 Sleep Timer 允许以 5 分钟步进选择 5–120 分钟。Desktop 已接入真实
deadline/EndOfSong 状态机，但仍只有 15、30、60 分钟三个固定预设。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 增加共享的 5–120 分钟选择值，并在完整播放器与 Queue 侧栏启动真实 deadline |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 更新 Sleep Timer 自定义时长能力 |

### 完成定义

1. 用户可按 5 分钟步进在 5–120 分钟间调整时长，默认 30 分钟。
2. Start 直接写入现有 `SleepTimer::Deadline`，倒计时和到时暂停继续由真实播放轮询处理。
3. 完整播放器与侧栏 Queue 共享同一选择值和当前计时状态，切换入口不会丢失选择。
4. 现有 15/30/60、End of song、Cancel 保持可用；Guest 与无歌曲禁用边界不变。
5. 不新增数据库字段、持久化默认值、自动日程、淡出或第二套计时器。
6. 只执行一次格式化和全目标编译检查。

完成情况：完整播放器与侧栏 Queue 现共享默认 30 分钟的自定义值，可用 −5/+5 在 5–120 分钟
边界内调整并直接 Start；启动后写入既有 `SleepTimer::Deadline`，剩余状态及到时暂停继续由真实
播放轮询处理。固定预设、End of song、Cancel、Guest 和无歌曲边界保持不变，没有增加数据库、
自动日程、淡出或计时服务。`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Full Player Sleep Timer Entry

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `Player.kt` 在完整播放器直接显示 Sleep Timer 状态并允许启动或取消；Desktop 已有真实的
定时暂停与当前歌曲结束暂停状态机，但入口只在侧栏 Queue，展开完整播放器后不可直接发现或操作。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 完整播放器 Up next 接入现有 Sleep Timer 状态、预设、当前歌曲结束和取消 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记完整播放器计时暂停点击闭环 |

### 完成定义

1. 完整播放器默认可见的 Up next 内直接显示 Sleep Timer 当前状态。
2. 15、30、60 分钟复用现有 deadline 状态机，到时暂停真实音频且不继续自动切歌。
3. End of song 在当前项目自然结束时暂停并阻止队列前进；Cancel 立即撤销。
4. 侧栏 Queue 与完整播放器读取同一状态，任一入口修改后另一处立即同步。
5. Listen Together Guest 禁止修改本地计时器，没有当前歌曲时禁用启动操作。
6. 不增加自动日程、淡出、持久化或平行计时服务。
7. 只执行一次格式化和全目标编译检查。

完成情况：完整播放器默认可见的 Up next 现直接显示同一 Sleep Timer 状态，可选择 15、30、
60 分钟或当前歌曲结束并可取消；所有操作复用侧栏 Queue 已有的 deadline/EndOfSong 状态机，计时
到达暂停真实音频，当前歌曲结束模式阻止队列前进。Guest 与无当前歌曲禁用边界保持一致，没有新增
自动日程、淡出、持久化或计时服务。`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Song Speed Dial

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `PlayerMenu.kt` 可把当前歌曲 Pin/Unpin from Speed Dial，`HomeScreen.kt` 会在 Speed Dial
展示持久固定项并允许直接播放。Desktop 当前没有任何 Speed Dial 存储、播放器入口或 Home 区块，
该核心 Home 点击路径完全缺失。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | SQLite v24 增加最小 `speed_dial_song` 关系与读写 |
| `src/ui/shell.rs` | 当前播放器 Pin/Unpin，Home 展示/播放/取消固定及加载/空/失败状态 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 登记 Song Speed Dial 点击闭环和后续类型边界 |

### 完成定义

1. 当前歌曲可从完整播放器 Pin/Unpin，状态来自 SQLite 而不是临时 UI 标记。
2. Home 在同一启动会话立即更新，并在重启后按固定时间顺序恢复真实 Song 元数据。
3. Home 固定歌曲可直接建立真实播放队列，并继续提供现有 Next、Queue、Download；可原位 Unpin。
4. Loading、空状态、失败重试、写入成功/失败均在当前播放器或 Home 可见。
5. SQLite 只保存 Song 外键与固定时间，复用既有 `song` / `song_album` 元数据，不复制 JSON 快照。
6. 本切片当时只移植 Song Speed Dial；Album、Artist、Playlist、Podcast 后续已由本文顶部的 Browse
   Speed Dial 切片通过对应真实详情入口补齐，Episode 仍不冒充 Song 或 Podcast 固定项。
7. 不增加推荐填充、随机入口或 Speed Dial 设置；这些不是当前最小点击闭环。
8. 只执行一次格式化和全目标编译检查。

完成情况：完整播放器现可依据 SQLite v24 的真实固定状态 Pin/Unpin 当前普通歌曲；Home 会在当前
会话立即刷新并跨启动按固定时间恢复 Song 与既有 Album 元数据。固定项可直接建立播放队列，并复用
现有 Next、Queue、Download 和原位 Unpin；Loading、空数据、失败重试及写入结果均在现有界面可见。
本切片没有扩展其他媒体类型、推荐填充、随机入口、设置或平行状态机。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Now Playing Song Library and Episode Later

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `PlayerMenu.kt` 为当前普通歌曲提供 Add/Remove from Library，并由 `toggleSongLibrary` 先通过
登录态 `/next` 获取最新 BOOKMARK/LIBRARY feedback token 再提交；当前项是 Episode 时，播放器的
心形动作切换 Episodes for Later。Desktop 已能加载账号 Library Songs，也有完整播客保存状态机，
但当前播放器没有歌曲 Library 写入口，完整/迷你播放器的 Episode 心形动作仍误写普通 Favorite。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 增加 Android 同源的 fresh-token Song Library feedback 写入 |
| `src/ui/shell.rs` | 当前歌曲 Library 状态/动作及完整、迷你播放器 Episode Later 语义 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录当前歌曲 Library 与 Episode 保存闭环 |

### 完成定义

1. 已登录的当前普通歌曲可依据真实 Library Songs 快照显示 Add/Remove from Library。
2. 点击后按真实 video ID 请求登录态 `/next`，只解析同一歌曲的 BOOKMARK/LIBRARY toggle token，
   再调用现有认证 `feedback` 端点，不复用缓存 token。
3. UI 先乐观更新 Library Songs，失败回滚并在当前播放器可见；成功后 Library Songs 入口同步更新。
4. 完整与迷你播放器中的 Episode 心形动作读取并切换现有 Episodes for Later，不写普通 Favorite；
   播客操作成功或失败在当前播放器可见。
5. 普通 Favorite、YT Like 与 Song Library 保持三个既有独立语义，不增加数据库表。
6. 不实现 Speed Dial、系统 EQ 或其他 PlayerMenu 扩展。
7. 只执行一次格式化和全目标编译检查。

完成情况：已登录的当前普通歌曲现从真实 Library Songs 快照显示 Library + / In library；点击会用
登录态 `/next` 只定位同一 video ID 的 BOOKMARK/LIBRARY toggle，每次取得 fresh add/remove token
后提交认证 feedback。账号 Library Songs 先乐观更新，失败恢复完整旧快照并在播放器显示错误。
完整与迷你播放器的 Episode 心形动作现复用既有 Episodes for Later 本地/账号同步状态，成功和失败
可见，不再写入普通 Favorite；未增加 token 缓存、数据库表或 PlayerMenu 扩展。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Player Playlist Pickers and Queue Cloud Actions

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `QueueMenu.kt` 的普通歌曲 Like 会通过现有账号同步写入 YouTube Music，Add to playlist
也可选择带远端 browse ID 的账号歌单。Desktop 已有完整的 YT Like、可编辑云端歌单和两个共用
Picker 状态机，但 Queue / Up next 行只暴露本地 Favorite 与本地歌单；同时从完整播放器调用本地或
云端 Picker 时没有退出全屏覆盖层，已设置的侧栏实际上不可见。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Queue / Up next 增加现有 YT Like 与云端歌单入口，并确保播放器内 Picker 可见 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录队列账号写操作和 Picker 点击闭环 |

### 完成定义

1. 已登录且账号资料库可用时，任意非 Episode 队列歌曲可切换真实 YouTube Music Like。
2. 任意非 Episode 队列歌曲可打开现有云端歌单 Picker，并写入用户选择的可编辑歌单。
3. 从完整播放器当前歌曲或 Up next 打开本地/云端 Picker 时退出覆盖层，目标侧栏立即可见；播放和
   队列保持不变。
4. YT Like 继续使用现有乐观更新/失败回滚，云端歌单继续使用现有加载、空数据与失败状态。
5. Episode 保持 Episodes for Later 语义，不显示普通歌曲的 YT Like/云端歌单入口。
6. 不增加后端请求、数据库表或平行 Picker 状态机。
7. 只执行一次格式化和全目标编译检查。

完成情况：侧栏 Queue 与完整播放器 Up next 的共用行现会在账号可用时为普通歌曲展示现有
YT Like 与可编辑云端歌单入口，分别复用乐观更新/失败回滚和云端 Picker 写入状态机；Episode
继续只使用 Episodes for Later。两个共用 Picker 入口从完整播放器触发时会先退出覆盖层，使本地或
云端目标侧栏立即可见，当前播放与队列不变；没有新增请求、存储或 Picker 状态。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Queue Reinsert and Episode Later Actions

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `QueueMenu.kt` 对任意队列项继续提供 Play next 和 Add to queue，允许用户主动把同一首歌
重新插入待播位置；队列项是 Episode 时，头部收藏动作实际切换 Episodes for Later，而不是歌曲
Favorite。Desktop 共用 Queue 行已有播放、改序、移除及其他歌曲动作，但这两个队列插入入口缺失，
Episode 的心形按钮也仍错误调用普通歌曲 Favorite。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Queue / Up next 共用行复用现有 Next、Queue 与 Episodes for Later 状态机 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录队列重复插入与 Episode 保存语义 |

### 完成定义

1. 任意 Queue / Up next 项可复用现有 Play next 与 Add to queue，允许主动加入重复项。
2. Play next 精确插入当前项之后，Add to queue 遵循现有 Shuffle/队尾语义，不切换当前播放。
3. Episode 心形动作显示并切换真实 Episodes for Later 本地/账号同步状态；普通歌曲继续切换 Favorite。
4. Listen Together Guest 继续禁止队列插入；歌曲资料库和播客忙碌状态分别禁用对应动作。
5. 不增加队列、播客或收藏的平行状态机，不修改数据库结构。
6. 只执行一次格式化和全目标编译检查。

完成情况：侧栏 Queue 与完整播放器 Up next 的共用行现直接复用既有 Play next / Add to queue，
允许同一项目按现有重复项、Shuffle 和会话持久化语义再次入队，Listen Together Guest 仍禁用。
Episode 的心形动作改为读取并切换现有 Episodes for Later 本地/账号同步状态，普通歌曲继续使用
Favorite；两类忙碌状态分别禁用，不增加数据库或平行状态机。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Live Song Details

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `QueueMenu.kt` 的 Details 会打开 `ShowMediaInfo(videoId)`：除本地 Song 外，还通过 WEB
`/next` 读取真实标题、作者、频道、上传日期、订阅数与描述，并读取 Return YouTube Dislike 的观看、
喜欢和不喜欢计数。Desktop 当前完整播放器 Details 只有本地 Song/Offline 字段，任意 Queue 歌曲也
没有详情入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 增加 Android 同源 WEB `/next` MediaInfo 与公开计数读取/解析 |
| `src/ui/shell.rs` | 完整播放器 Details 接入真实加载状态，Queue 行打开共用歌曲详情侧栏 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录真实歌曲详情闭环 |

### 完成定义

1. 当前歌曲和任意 Queue / Up next 歌曲均可按真实 video ID 请求 MediaInfo。
2. 详情显示本地 Song、Album、时长、Offline、链接，以及真实作者频道、上传日期、订阅数、描述、
   Views、Likes、Dislikes；服务未返回的字段明确省略，不编造数值。
3. Loading、Failure、Retry 与成功内容在当前目标界面可见；迟到响应不能替换另一首歌曲详情。
4. 完整播放器继续使用 Details 标签；非当前 Queue 歌曲进入共用详情侧栏，播放与队列不改变。
5. 每个详情值可复制，标准歌曲链接继续使用 GPUI 原生剪贴板。
6. 不持久化实时统计、不增加数据库表或影子 MediaInfo 缓存。
7. 只执行一次格式化和全目标编译检查。

完成情况：当前歌曲的 Details 标签和任意 Queue / Up next 行的共用详情侧栏现按真实 video ID
调用 Android 同源 WEB `/next` 与 Return YouTube Dislike；本地 Song、Album、时长、Offline、标准
链接和实时作者频道、上传日期、订阅数、描述、Views/Likes/Dislikes 在同一详情中展示且均可复制。
Loading、失败重试和迟到响应隔离已接入现有 UI，打开详情不改变播放或队列，实时结果不落库。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Song Metadata Refresh

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `QueueMenu.kt` 的 Refetch 会通过 `YouTube.queue(videoId)` 重新取得真实歌曲元数据并更新已有
歌曲。Desktop 已有队列歌曲解析器，但当前歌曲与 Queue 行没有刷新入口，旧标题、
Artist、Album、时长或封面只能等其他在线入口偶然覆盖。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 按 Android 同源 `music/get_queue` 与队列解析取得指定 video ID 的最新 Song |
| `src/ui/shell.rs` | 当前歌曲和共用 Queue 行增加 Refresh，更新内存队列与持久会话 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录歌曲元数据刷新入口 |

### 完成定义

1. 当前歌曲及任意 Queue / Up next 歌曲均可按真实 video ID 请求最新 Song 元数据。
2. 成功后更新队列中相同 video ID 的项目；若是当前歌曲，同时刷新播放器标题、Artist、Album、
   时长和封面，但不重载或中断音频。
3. 更新通过现有播放会话保存，重启读回同一份 Song，不增加影子缓存或数据库表。
4. Loading 禁止重复刷新，成功和失败在当前播放器/底栏可见；Listen Together Guest 不修改房主队列。
5. 请求按 Android 同源 `music/get_queue` 实现，并复用现有队列歌曲解析，不建立影子元数据协议。
6. 只执行一次格式化和全目标编译检查。

完成情况：InnerTube 现可用指定 video ID 调用 Android 同源 `music/get_queue` 并从同一队列解析器返回匹配 Song；
完整播放器与侧栏 Queue / Up next 共用行均提供 Refresh。成功后队列中全部同 ID 项及当前歌曲会更新
真实标题、Artist、Album、时长和封面，并由既有会话保存写回 SQLite，音频不中断；Loading、成功、
失败及 Guest 禁用状态均在现有播放器 UI 可见。`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Song Artist and Album Navigation

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `QueueMenu.kt` 可从任意歌曲打开其 Artist 或 Album；多位 Artist 会先让用户选择。Desktop
完整播放器只显示第一位 Artist 的详情入口，侧栏 Queue 与完整播放器 Up next 的共用歌曲行则没有
Artist/Album 导航，用户仍需先播放或重新搜索歌曲。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 当前歌曲与共用 Queue 行增加所有真实 Artist 及真实 Album 的现有 Browse 入口 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录歌曲上下文导航闭环 |

### 完成定义

1. 当前歌曲和任意队列歌曲的所有真实 Artist ID 均可直接打开，不再只保留第一位 Artist。
2. Song 含真实 Album credit 时可从当前歌曲和任意队列歌曲直接打开 Album。
3. 导航退出 Queue/完整播放器覆盖层并进入既有 Browse 状态机，当前播放与队列不改变。
4. 缺少真实 ID 时不显示伪入口，不猜测 Artist 或 Album 身份。
5. 两处入口复用同一身份转换与 Browse 状态，不新增平行页面或请求。
6. 只执行一次格式化和全目标编译检查。

完成情况：完整播放器当前歌曲与侧栏 Queue / Up next 共用歌曲行现会从 Song 的真实 credit 生成
导航入口；所有非空且去重后的 Artist ID 均可按名字选择，真实 Album credit 可直接打开。点击会关闭
Queue/完整播放器覆盖层并进入既有 Browse 请求与页面，当前播放和队列保持不变；缺少真实 ID 时不
显示入口。`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Queue Song Radio

状态：功能与 UI 已实现，待真实桌面 Radio 点击验收。

Android `QueueMenu.kt` 可从任意待播歌曲启动 Radio：当前项无缝替换未来队列，非当前项则以该歌曲
建立新的 Radio 队列。Desktop 只能从当前歌曲或 Queue 顶部启动，任意队列行尚无入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 共用 Queue 行增加任意歌曲 Radio，并复用现有请求/队列状态机 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录队列歌曲 Radio 入口 |

### 完成定义

1. 当前歌曲的 Queue Radio 继续请求真实 Radio 并在成功后替换未来队列。
2. 非当前歌曲先成为新队列首项并开始播放，再用其真实 video ID 请求 Radio。
3. Episode、Listen Together Guest 与正在加载的同 seed Radio 明确禁用。
4. Loading、Active、Failure 继续由现有 Radio UI 和重试路径展示，不新增平行状态。
5. 侧栏 Queue 与完整播放器 Up next 复用同一按钮实现。
6. 只执行一次格式化和全目标编译检查。

完成情况：共用 Queue 行现按每首歌的真实 video ID 启动 Radio。点击当前项沿用既有
`replace_future` 行为；点击非当前项先通过现有集合播放建立单曲新队列，再进入同一个 Radio 请求与
continuation 状态机。Episode、Guest 和同 seed Loading 均禁用，Active/Failure 继续在现有 UI 可见。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Queue Song Actions

状态：功能与 UI 已实现，待真实桌面队列点击验收。

Android `QueueMenu.kt` 允许对任意待播歌曲收藏、加歌单、管理下载和分享。Desktop 侧栏 Queue 与
完整播放器 Up next 共用同一行，但目前只有选中播放、改序和移除，必须先播放该歌曲才能执行日常
歌曲操作。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 两处队列共用行增加 Favorite、Playlist、下载生命周期和 Copy link |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录待播歌曲动作入口 |

### 完成定义

1. 队列任意歌曲可调用现有 SQLite Favorite 与本地歌单选择器，不改变当前播放项。
2. 队列任意歌曲可 Download/Pause/Resume/Remove offline，复用现有持久下载与确认框。
3. Copy link 写入该队列歌曲的标准 YouTube Music 链接，并在底栏显示反馈。
4. Up/Down/Remove 仍只修改队列；Listen Together Guest 的队列播放/编辑限制保持不变。
5. 侧栏 Queue 与完整播放器 Up next 复用同一实现，不复制状态机。
6. 只执行一次格式化和全目标编译检查。

完成情况：共用 Queue 行现为任意待播歌曲提供本地 Favorite、现有本地歌单选择器、完整
Download/Pause/Resume/Remove offline 生命周期和标准链接复制；动作不会先切换当前歌曲。原有
Play、Up、Down、Remove 及 Guest 禁用规则保持独立，复制提示和动作错误由底栏副标题显示。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Now Playing Link and Details

状态：功能与 UI 已实现，待真实桌面剪贴板点击验收。

Android `PlayerMenu.kt` 提供 Copy link 和 Details。Desktop 当前歌曲已有播放、收藏、歌单、订阅、
详情导航与下载动作，但不能复制标准 YouTube Music 链接，也没有集中查看当前真实 Song 元数据的
入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 完整播放器增加 Copy link 动作及 Details 标签 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录当前歌曲链接与详情入口 |

### 完成定义

1. Copy link 写入 `https://music.youtube.com/watch?v=<video_id>` 到 GPUI 原生剪贴板并显示提示。
2. Details 展示现有 Song 的标题、Artist、Album、类型、时长、Video ID 与标准链接。
3. Details 同时显示现有 `AudioDownload` 状态，不发新请求或建立影子元数据。
4. 切歌清除上一首复制提示，歌词加载/取消逻辑不受 Details 标签影响。
5. 只执行一次格式化和全目标编译检查。

完成情况：完整播放器动作区现可复制标准 YouTube Music watch 链接并显示成功提示；Details 按现有
Song 展示标题、Artists、Album、歌曲/单集类型、时长、Video ID、链接，并从现有下载状态展示
Offline。切换歌曲会清除旧复制提示，Details 不触发任何网络或存储请求。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Now Playing Download Lifecycle

状态：功能与 UI 已实现，待真实桌面下载点击验收。

Android `PlayerMenu.kt` 可从当前歌曲直接开始或移除下载。Desktop 现有列表行有 Download，资料库
也有完整下载管理，但完整播放器缺少当前歌曲入口；通用行按钮在 Completed、Queued、Downloading
状态会禁用，不能承担暂停与移除。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 完整播放器增加 Download / Pause / Resume / Remove 当前歌曲下载动作 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录当前歌曲下载生命周期入口 |

### 完成定义

1. 无下载记录时调用现有持久下载排队；Paused/Failed 时调用现有断点恢复。
2. Queued/Downloading 时可暂停；Completed 时复用现有确认框精确移除离线副本。
3. Loading/Removing 状态禁用重复动作，下载失败在完整播放器可见。
4. 不新增下载存储、调度器或平行状态机。
5. 只执行一次格式化和全目标编译检查。

完成情况：完整播放器当前歌曲动作区会按现有 `AudioDownload` 状态显示 Download、Pause download、
Resume download 或 Remove offline；完成项复用现有确认框，进行中复用取消/暂停标志，失败与暂停项
复用断点恢复。加载、暂停中、移除中禁止重复操作，`download_error` 在当前页面直接可见。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Mini Player Song Actions

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `MiniPlayer.kt` 新版底栏在当前歌曲旁直接提供 Subscribe、Add to playlist、Favorite。
Desktop 已在完整播放器接通这些真实状态机，但底栏只能展开播放器，日常操作多一步且与 Android
核心入口不一致。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 底栏增加当前歌曲本地收藏、本地歌单及真实 Artist 订阅动作 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Mini Player 直接歌曲动作 |

### 完成定义

1. 有当前歌曲时底栏可直接 Favorite / Unfavorite，并调用现有 SQLite 收藏状态机。
2. Add to playlist 打开现有本地歌单选择器，不复制歌单写入逻辑。
3. 已登录且第一位 Artist 有真实 ID 时显示 Subscribe / Subscribed，调用现有账号后端及回滚。
4. 操作失败可在底栏元数据区看到；没有当前歌曲或真实 Artist ID 时不显示伪入口。
5. 控件使用紧凑图标与 tooltip，不挤掉已有播放、进度、音量、Radio、Lyrics、Queue。
6. 只执行一次格式化和全目标编译检查。

完成情况：底栏当前歌曲旁现有独立紧凑动作组，可直接切换本地 Favorite、打开现有本地歌单
选择器；账号资料库已加载且首位 Artist 有真实 ID 时显示订阅按钮并复用现有乐观更新/失败回滚。
收藏或订阅错误会优先显示在底栏当前歌曲副标题，不再只在完整播放器可见。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Library Mix Actions and Sort

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `LibraryMixScreen.kt` 在默认资料库入口直接提供刷新、创建歌单和排序。Desktop Mixed
Overview 已显示真实内容，但刷新仍藏在 Playlists 云端区，创建本地歌单也必须先猜测切换分类；
混合结果本身还不能切换名称排序。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Mix 增加真实云端刷新、进入本地歌单创建表单及 Recent/Name 排序方向 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录默认资料库入口的操作闭环 |

### 完成定义

1. 已登录时 Refresh 调用现有账号资料库刷新状态机，并显示 Loading/Failure。
2. New playlist 从 Mix 进入现有本地歌单创建表单，不复制存储或创建逻辑。
3. Recent 使用后端与 SQLite 的既有来源顺序，Name 按真实标题排序；不伪造跨类型更新时间。
4. 排序方向可切换，搜索结果保持 Android 的类型优先、同类型名称排序。
5. 只执行一次格式化和全目标编译检查。

完成情况：Mix 顶部现可用现有账号状态机 Refresh；New playlist 在当前页展开共享名称输入并调用
既有 SQLite 创建函数，成功后自动收起。空查询可按既有来源顺序或真实名称升降序排列；搜索时按
Playlist、Song、Artist、Album 优先级及名称排列。没有为 BrowseItem 编造创建或更新时间。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Library Mixed Overview

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `LibraryScreen.kt` 默认进入 `LibraryMixScreen`，把真实歌单、专辑和艺术家集中在一个可搜索
入口；搜索时也返回真实歌曲。Desktop Overview 目前只是若干独立区块，缺少这一默认混合浏览路径。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Overview 增加混合搜索与真实歌曲/歌单/专辑/艺术家结果 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Android Library Mix 默认入口的桌面闭环 |

### 完成定义

1. 空查询展示已有云端/本地真实歌单、专辑与艺术家，并可打开现有详情。
2. 非空查询同时搜索真实歌曲，歌曲复用现有播放、收藏、歌单、下载和队列动作。
3. Liked、Offline、My Top、Uploaded 快捷入口继续指向现有真实来源，不伪造 Cached 集合。
4. 加载、空数据和已有后端失败状态在 Overview 可见。
5. 只执行一次格式化和全目标编译检查。

完成情况：Overview 现聚合账号歌单、Liked/Uploaded Albums、Library Artists、本地 Album/Artist
目录和本地歌单；搜索非空时还会合并账号与设备已知真实歌曲。所有结果复用现有播放或详情状态机，
Signed out、Loading、Empty 和后端错误直接显示在混合入口。自动歌单继续复用真实 Songs/Stats 来源，
未伪造 Cached 集合；现有 Downloads 管理区继续保留暂停、恢复、移除和离线播放动作。
`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Persistent Album Navigation

状态：功能与 UI 已实现，待真实桌面重启后点击验收。

SQLite v23 已保存真实 song→album 映射，但历史、收藏、Episodes for Later、本地歌单、下载和冷启动
队列读回 Song 时尚未恢复 Album credit。结果是同一歌曲首次在线播放时完整播放器有 Album 入口，
重启或从本地资料库播放后入口消失。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 所有持久 Song 读取同时恢复 v23 Album credit |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Album 入口跨本地来源与重启保持 |

### 完成定义

1. History、Favorites、Episodes for Later、Local Playlists、Downloads 和恢复队列读回真实 Album。
2. 没有关联的旧 Song 继续返回 `album: None`，不猜测或阻塞其他字段。
3. 完整播放器从上述本地来源播放后仍可打开真实 Album 详情。
4. 只执行一次格式化和全目标编译检查。

完成情况：所有调用 `song_from_row` 的历史、推荐、本地收藏、稍后播放、歌单、下载和恢复队列查询
都会同时读取 SQLite v23 `song_album`；无映射的旧 Song 保持 `album: None`。因此真实 Album 入口不再
只存在于首次在线解析的内存 Song。`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Now Playing Song Actions

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `MiniPlayer.kt` 在当前歌曲旁直接提供 Subscribe、Add to playlist、Favorite；通用歌曲元数据
也可进入 Artist/Album。Desktop 完整播放器目前只展示标题和歌手，用户必须返回搜索结果才能完成
这些日常操作。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 完整播放器增加本地收藏/歌单、YouTube Music 喜欢/歌单操作 |
| `src/ui/shell.rs` | 第一位真实 Artist 可直接订阅或打开，真实 Album credit 可直接打开 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录完整播放器歌曲动作闭环 |

### 完成定义

1. Favorite 与 Add to playlist 复用现有本地存储和选择器。
2. 登录后 YT Like / YT Playlist / Subscribe 调用现有真实账号后端，并保持乐观状态与失败回滚。
3. 只有真实 Artist ID / Album browse ID 才显示对应 Open 操作。
4. 打开详情时退出完整播放器并进入现有 browse 状态机；播放不中断。
5. 只执行一次格式化和全目标编译检查。

完成情况：完整播放器现可直接 Favorite、加入本地歌单、YT Like、加入可编辑云端歌单、订阅首位
真实 Artist，并打开真实 Artist/Album 详情。订阅同时乐观更新 Artist 详情与 Library Artists，失败
精确回滚并在播放器显示错误。`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Library Playlists Daily Entry

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `LibraryPlaylistsScreen.kt` 在同一页面搜索歌单，并直接提供 Liked、Offline、My Top、
Uploaded 等自动歌单。Desktop 已有这些真实歌曲/统计来源和本地/云端歌单，但 Playlists 分类没有
搜索，自动入口也分散在 Songs、Stats 等页面。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Playlists 分类增加统一搜索，并过滤现有本地与云端歌单 |
| `src/ui/shell.rs` | 增加 Liked / Offline / My Top / Uploaded 快捷入口，直接切换现有真实来源 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Playlists 日常入口闭环 |

### 完成定义

1. 搜索同时作用于当前本地和云端歌单，空结果、加载、未登录与失败沿用现有可见状态。
2. Liked、Offline、Uploaded 进入现有 Library Songs 真实来源，My Top 进入现有 Stats。
3. 不为 Android Cached 入口伪造缓存歌单；当前没有可枚举的真实缓存集合时不显示。
4. 保留创建、排序、打开、收藏移除、云端重命名和删除操作。
5. 只执行一次格式化和全目标编译检查。

完成情况：Playlists 分类现在用一个搜索框同时过滤本地与云端真实歌单；Liked、Offline、
My Top、Uploaded 分别切换到已有 Library Songs 或 Stats 后端。没有为不可枚举的缓存虚构
Cached 歌单。`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Stats Top Albums

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `StatsViewModel.mostPlayedAlbums` 通过真实 `song_album_map` 按播放时长聚合专辑，
`StatsScreen` 展示排行并点击进入 Album。Desktop Stats 目前只有 Top Songs / Artists，且文档明确
记录歌曲模型尚无专辑关系。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/domain/song.rs` / `src/services/innertube.rs` | Song 携带解析得到的真实 Album browse ID 与标题 |
| `src/storage/sqlite.rs` / `src/storage/mod.rs` | SQLite v23 持久化歌曲专辑关系，并按播放记录聚合 Top Albums |
| `src/ui/shell.rs` | Stats 展示可点击的 Top Albums 排行 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 移除 Top Albums 缺口说明并记录闭环 |

### 完成定义

1. 只使用 InnerTube 明确返回或 Album 详情上下文提供的真实 Album browse ID，不按标题猜测。
2. 播放历史按真实 Album ID 聚合次数和播放时长，旧数据无 Album 时继续正常显示 Songs/Artists。
3. Top Albums 显示标题、封面、次数、时长，点击进入现有 Album browse 详情。
4. 只执行一次格式化和全目标编译检查，不新增与本切片无直接关系的测试。

完成情况：Song 会携带 renderer 明确返回或 Album 详情上下文提供的真实 Album credit；SQLite v23
持久化 song→album 映射，Stats 按历史事件聚合 Top Albums 并进入现有 Album 详情。旧历史没有专辑
映射时仍正常显示 Songs/Artists。`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Album Library Toggle

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `AlbumPage.getPlaylistId()` 从专辑响应提取真实 playlist ID，`AlbumEntity.toggleLike()`
随后调用 `YouTube.likePlaylist()`；Desktop Album 详情已有 Play / Shuffle / Download all，但没有
收藏或取消收藏写操作。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/domain/browse.rs` / `src/services/innertube.rs` | 在 Album 详情解析真实 playlist ID |
| `src/ui/shell.rs` | Album 详情增加 Save/Remove、乐观状态和失败回滚 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Album 收藏闭环 |

### 完成定义

1. 只对真实解析到 playlist ID 的 Album 显示账号收藏按钮。
2. Save/Remove 调用现有 YouTube Music `like/like` / `like/removelike` 后端。
3. Library Albums 乐观更新，失败时回滚并显示现有云端错误。
4. 不影响 Playlist 收藏与 Artist 订阅；只运行一次格式与编译检查。

完成情况：Album browse 响应会从 canonical URL 或播放端点提取真实 playlist ID；登录后的
Album 详情据此显示 Save/Remove，调用现有 YouTube Music playlist like 后端并同步 Library Albums。
失败沿用云端错误提示并精确回滚。`cargo fmt --all && cargo check --all-targets` 已通过。

## 已实现切片：Library Podcasts Sources

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `LibraryPodcastsScreen.kt` 在 Podcasts 分类提供 Episodes / Channels / Downloaded 三个
入口。Desktop 已有保存节目、Episodes for Later 与已下载单集的真实状态，但全部堆在一个区块，
且已下载播客只能从 Overview 的通用下载列表寻找。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 增加 Episodes / Channels / Downloaded 切换及对应真实列表 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Podcasts 三来源入口 |

### 完成定义

1. Episodes 展示保存节目与 Episodes for Later，保留同步、播放和详情动作。
2. Channels 按真实 channel ID 聚合并进入 Artist 详情。
3. Downloaded 只展示已完整下载的播客单集，并支持排序、播放和 Shuffle。
4. 每个来源的加载、失败和空结果状态可见。
5. 只运行一次格式与编译检查。

## 已实现切片：Library Albums and Artists Sources

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `LibraryAlbumsScreen.kt` 提供 Liked / Library / Uploaded，`LibraryArtistsScreen.kt`
提供 Liked / Library；两页都可搜索、排序并进入详情。Desktop 当前只有远端 Liked Albums 与
Subscribed Artists 的单一列表，已持久化的本地真实目录也没有 Library 入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 加载 Uploaded Albums；为 Albums/Artists 增加来源、搜索、排序与详情列表 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Albums/Artists 的来源与 UI |

### 完成定义

1. Albums 可切换 Liked / Library / Uploaded；Artists 可切换 Liked / Library。
2. Liked/Uploaded 使用真实账号端点，Library 使用设备已持久化的真实目录，不生成占位条目。
3. 两页均可按标题/副标题搜索，并按来源顺序、标题或副标题升降序排序。
4. 每个结果点击进入现有真实 browse 详情；加载、失败、登录和空结果状态可见。
5. 只运行一次格式与编译检查。

## 已实现切片：Library Songs Sources

状态：功能与 UI 已实现，待真实账号桌面点击验收。

Android `LibrarySongsScreen.kt` 直接提供 Liked / Library / Uploaded / Downloaded 四个真实来源，
并支持搜索、排序、整组播放和随机播放。Desktop 当前把 `FEmusic_liked_videos` 统一标成 Liked，
也会在 Songs 分类同时堆叠云端收藏、本地收藏和下载，无法按 Android 的日常入口切换。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 分别加载 `LM`、`FEmusic_liked_videos` 与上传歌曲标签页 |
| `src/ui/shell.rs` | Songs 分类增加四来源筛选、搜索、排序、播放与 Shuffle |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Library Songs 的真实来源与 UI |

### 完成定义

1. Liked、Library、Uploaded 使用登录后的真实 YouTube Music 数据，Downloaded 使用本地完成下载。
2. 来源切换不混淆数据；失败、加载、空结果在 Songs 页面可见。
3. 可按标题/歌手搜索，按来源顺序、标题、歌手或实际本地播放时长排序。
4. 单曲、Play all 和 Shuffle 复用现有播放、队列及下载链路。
5. 只运行一次格式与编译检查。

## 已实现切片：Dedicated Listening History

状态：功能与 UI 已实现，待真实桌面点击验收。

Android 主界面顶部有直接进入 `HistoryScreen` 的入口，并支持 Local / Remote 切换、标题或歌手
过滤、本地单条删除/清空，以及远端单条移除。Desktop 已有这些历史后端的大部分能力，但入口
埋在 Library Overview，且本地历史没有过滤和单删。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/app/router.rs` | 增加直接可发现的 History 路由 |
| `src/storage/sqlite.rs` / `src/storage/mod.rs` | 增加本地历史单条删除 |
| `src/ui/shell.rs` | 独立 History 页面、Local/YouTube Music、过滤、单删/清空、播放与队列动作 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录主导航 History 点击闭环 |

### 完成定义

1. 侧栏可直接进入 History，不必先经过 Library。
2. Local 与登录后的 YouTube Music 历史继续使用现有真实数据源，并可按标题/歌手过滤。
3. 本地记录可单删或确认清空；远端记录继续按 feedback token 精确删除。
4. 历史歌曲保留播放、Next、Queue、Download；只运行格式与编译检查。

## 已实现切片：Listening Stats

状态：功能与 UI 已实现，待真实桌面点击验收。

Android 主界面顶部公开 `StatsScreen`，按时间范围展示真实播放时长、歌曲播放次数、Top Songs
和 Top Artists，并可从歌曲榜直接建立播放队列。Desktop 已有按实际播放达到已保存门槛（默认 30 秒）写入的
`play_history`，但没有 Stats 入口或聚合页面。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` / `src/storage/mod.rs` | 按时间范围聚合总时长、播放次数、唯一歌曲/歌手及榜单 |
| `src/app/router.rs` | 增加可发现的 Stats 路由 |
| `src/ui/shell.rs` | 时间范围、加载/空/失败、总览、Top Songs 播放/随机播放及 Top Artists 详情 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Stats 点击闭环及真实数据边界 |

### 完成定义

1. 侧栏可点击 Stats，并按 7 天、30 天、3/6 个月、1 年和全部时间切换。
2. SQLite 以真实 `play_history.play_time_ms` 聚合总览、Top Songs 和 Top Artists。
3. Top Songs 可单曲、整组或随机播放，并保留 Next、Queue、Download；有 artist ID 时可打开真实详情。
4. 加载、空数据和失败均在 Stats 页面可见；只运行格式与编译检查。

## 已实现切片：Search History

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `SearchScreen` / `OnlineSearchSuggestionViewModel` 会把用户提交的搜索写入本地历史：
空输入展示全部历史，输入时展示同前缀最近 3 条；历史可点击重新搜索、填回输入框和单条删除。
Desktop 当前只有在线建议与结果，没有搜索历史存储或 UI。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | SQLite v21 搜索历史保存、读取、单删和清空 |
| `src/storage/mod.rs` | 公开搜索历史条目 |
| `src/ui/shell.rs` | 提交搜索自动保存；在线搜索页展示、复搜、填充、单删和清空 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录搜索历史点击闭环 |

### 完成定义

1. 用户按 Enter、Search 或点击建议提交有效查询时自动保存，内部刷新/筛选切换不重复保存。
2. 空输入展示最近历史；输入时展示同前缀最近 3 条。
3. 历史项可立即复搜、填入输入框或删除，并可经确认清空全部历史。
4. 只运行格式与编译检查。

## 已实现切片：Recognition History

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `RecognitionScreen` 顶部可进入 `RecognitionHistoryScreen`；每次 Match 自动保存，历史项
可重新搜索，并支持单条删除和清空。Desktop 现已补齐同一用户闭环。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | SQLite v20 保存、读取、单删和清空识曲历史 |
| `src/storage/mod.rs` | 公开识曲历史条目 |
| `src/ui/shell.rs` | Match 自动保存；Recognize 页提供 History/Back、重新搜索、删除和清空 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录识曲历史闭环 |

### 完成定义

1. 每次真实 Match 成功后自动写入本地历史。
2. Recognize 可点击 History，显示封面、标题、歌手和识别时间。
3. 历史项可重新搜索；单条删除和清空使用现有确认对话框。
4. 只运行格式与编译检查。

## 已实现切片：Real Music Recognition

状态：功能与 UI 已实现，待真实麦克风点击验收。

Desktop 主导航已有 Recognize，但当前只生成本地指纹并明确不请求匹配，用户拿不到歌曲结果；
Android `MusicRecognitionService` 会把同格式签名发送到 Shazam 并展示可播放/搜索的真实匹配。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/recognition.rs` | 接入 Android 同 Shazam discovery 请求并解析歌曲元数据 |
| `src/services/mod.rs` | 将识曲客户端纳入现有 DesktopServices/代理配置 |
| `src/ui/shell.rs` | Listening → Matching → Match/No match/Error，显示封面元数据并接入 YouTube Music 播放/搜索 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 删除“仅验证本地指纹”的旧描述，记录真实识曲闭环 |

### 完成定义

1. 点击 Recognize 录音并生成签名后发送真实 Shazam 匹配请求。
2. Match 显示标题、歌手、专辑、封面，并能直接播放关联视频或搜索 YouTube Music。
3. No match、网络失败、麦克风失败和取消均有明确状态，可重试。
4. 只运行格式与编译检查。

## 已实现切片：Online Search Featured Playlists and Profiles

状态：功能与 UI 已实现，待真实桌面点击验收。

Android Online Search 还公开 Featured playlists 与 Profiles。Rust 的目录解析和 Artist/Playlist
详情已经支持这些 renderer，但筛选枚举与 UI 没有入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 增加 Android Featured playlist / Profile 原始 filter 参数 |
| `src/ui/shell.rs` | 通过既有 filter 列表自动展示两个入口，结果打开真实详情 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 更新 Online Search 分类范围 |

### 完成定义

1. Online Search 可选择 Featured playlists 和 Profiles。
2. 两类请求使用 Android 原始 params，并复用目录结果、continuation 与 browse 详情。
3. 现有 Playlists 保持社区歌单参数及行为。
4. 只运行格式与编译检查。

## 已实现切片：Online Search All

状态：功能与 UI 已实现，待真实桌面点击验收。

Android Online Search 默认以无 filter 的 All 展示歌曲与目录混合结果；Rust 默认直接进入 Songs，
虽然现有解析器已支持混合结果，却没有可点击入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 增加默认 All filter；请求时不发送 params，沿用混合结果解析与 continuation |
| `src/ui/shell.rs` | Online Search 筛选栏增加并默认选中 All |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 All 混合搜索入口 |

### 完成定义

1. 新打开 Search 时默认选择 All。
2. All 发送无 filter 的真实搜索请求，同时展示可播放歌曲和可打开目录。
3. 切换其他筛选及分页逻辑保持现有行为。
4. 只运行格式与编译检查。

## 已实现切片：Online Search Videos

状态：功能与 UI 已实现，待真实桌面点击验收。

Android Online Search 直接提供 Videos 筛选，参数为 `FILTER_VIDEO`。Rust 当前只有 Songs、
Albums、Artists、Playlists、Podcasts、Episodes，无法直接搜索音乐视频。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 增加 Android 对应 Video filter 参数并作为可播放歌曲结果解析 |
| `src/ui/shell.rs` | Online Search 筛选栏公开 Videos，复用现有结果播放/队列/下载动作 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 更新搜索内容范围 |

### 完成定义

1. Online Search 可点击 Videos 并发送 Android 对应真实参数。
2. 视频结果进入现有歌曲结果状态、分页和真实播放队列。
3. 加载、空数据和失败状态沿用搜索现有实现。
4. 只运行格式与编译检查。

## 已实现切片：Browse Collection Actions

状态：功能与 UI 已实现，待真实桌面点击验收。

Android Album / Playlist 详情可整组播放、Shuffle 与下载。Rust 详情已有逐曲动作和 Play all，
但缺少整组 Shuffle / Download；新发行等 Category 页还会错误显示“无可播放曲目”。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Album/Playlist 详情增加真实 Shuffle 与 Download all；Category 有目录项时不显示空曲目提示 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录详情集合动作 |

### 完成定义

1. Album / Playlist 的 Shuffle 建立真实随机队列并进入播放。
2. Download all 将未下载曲目交给现有下载队列与并发限制。
3. 只有 songs 与 related 均为空时才显示 Category 空状态。
4. 只运行格式与编译检查。

## 已实现切片：Library Content Filters

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `LibraryScreen.kt` 在资料库主入口直接切换 Playlists / Songs / Albums / Artists /
Podcasts。Rust 当前把所有分区纵向堆叠，内容虽可用但缺少对应的可发现分类入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 增加 Overview 与 Android 五类资料库筛选按钮，只显示对应真实分区 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Library 分类入口 |

### 完成定义

1. Library 顶部可切换 Overview / Playlists / Songs / Albums / Artists / Podcasts。
2. 每个分类复用已加载的真实云端或本地数据和既有详情/播放操作。
3. 进入详情后 Back 回到原分类，不引入新路由或存储。
4. 只运行格式与编译检查。

## 已实现切片：Library Albums and Artists

状态：功能与 UI 已实现，待真实桌面点击验收。

Android Library 提供 Albums 与 Artists 主分类，并从 `FEmusic_liked_albums`、
`FEmusic_library_corpus_artists` 同步真实账号内容。Rust 云端资料库当前只加载歌曲和歌单。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 云端资料库并行加载 albums/artists，显示可点击分区与真实缩略图 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Library Albums / Artists 入口 |

### 完成定义

1. 登录后的 Library 同步并显示 liked albums 与 library artists。
2. Album / Artist 点击进入既有真实 browse 详情；加载、空数据和失败沿用云端资料库状态。
3. 不新建存储表或平行详情页。
4. 只运行格式与编译检查。

## 已实现切片：Explore New Releases View All

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `ExploreScreen.kt` 的 New releases 标题可进入 `NewReleaseScreen.kt`，后者加载
Explore shelf 返回的 `FEmusic_new_releases_albums` 完整列表。Rust 当前只能打开首屏专辑。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/domain/home.rs` | Explore 状态保留 New releases 的 More browse endpoint |
| `src/services/innertube.rs` | 从真实 Explore 响应提取 `browseId` 与 `params` |
| `src/ui/shell.rs` | New releases 标题增加 View all，进入既有 browse/continuation 详情 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录完整新发行入口 |

### 完成定义

1. Explore 首屏继续展示真实新发行专辑。
2. View all 使用服务端返回的 endpoint，展示完整专辑列表并沿用现有加载、空数据和失败状态。
3. 单张专辑仍进入真实专辑详情。
4. 只运行格式与编译检查。

## 已实现切片：Search URL Direct Open

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `SearchScreen.kt` 使用 `YouTubeUrlParser` 识别 video、playlist、album 与 artist 链接，
直接播放或打开详情。Rust 当前会把这些链接作为普通搜索关键词发送。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 解析 YouTube / YouTube Music watch、shorts、playlist、channel、browse URL |
| `src/ui/shell.rs` | Video 通过现有 Radio 元数据链路播放；其他类型进入既有 browse 详情 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录搜索链接直达行为 |

### 完成定义

1. 支持 Android 对应的 `youtube.com/watch`、`youtu.be`、`youtube.com/shorts`、playlist、
   `music.youtube.com/channel` 和 `music.youtube.com/browse/MPRE…` 形态。
2. Video 获取真实歌曲元数据后播放，不显示占位标题；Playlist/Album/Artist 打开真实详情。
3. 已识别链接不再发送普通关键词建议或搜索请求。
4. 只运行格式与编译检查。

## 已实现切片：Local Search

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `SearchScreen.kt` 可在 Online 与 Local 间切换，`LocalSearchScreen.kt` 会搜索设备中的歌曲
和歌单并进入真实播放或本地详情。Rust 当前只有匿名 YouTube Music 搜索，但本地收藏、历史、
下载、稍后播放单集和歌单状态已经存在。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 增加 YouTube Music / On this device 搜索来源切换 |
| `src/ui/shell.rs` | 从现有本地状态聚合歌曲并搜索本地歌单 |
| `src/ui/shell.rs` | 本地结果接入真实播放、Next、Queue、Download 和歌单详情 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 更新搜索核心入口 |

### 完成定义

1. Search 可发现地切换 Online 与 Local，Local 输入时不发网络建议或搜索请求。
2. Local 的 All / Songs / Playlists 过滤器实时搜索标题、歌手和歌单名。
3. 歌曲结果建立真实本地结果队列；歌单结果进入现有本地歌单详情。
4. 加载、空结果及已有数据错误状态在本地结果区可见。
5. 只运行格式与编译检查。

## 已实现切片：Local Search Albums and Artists

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `LocalSearchViewModel.kt` 的本地搜索范围是 Songs / Albums / Artists / Playlists，All
模式每类预览 3 条；Album 与 Artist 点击后分别进入真实详情。Desktop 已有歌曲与歌单搜索，
但还缺这两个可发现的筛选和结果组。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 持久化搜索、首页、探索与云端资料库已经返回的真实 Album / Artist 目录项 |
| `src/ui/shell.rs` | 增加 Albums / Artists 筛选、All 分组预览、本地 Artist 派生及详情点击 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 更新 Local Search 的真实数据范围 |

### 完成定义

1. Local 提供 All / Songs / Albums / Artists / Playlists 五个筛选。
2. Artist 可由本地歌曲的真实 artist ID 立即发现；Album / Artist 目录项跨启动保留。
3. All 每类最多预览 3 条，单类筛选展示全部匹配结果。
4. Album / Artist 点击复用现有真实 browse 详情，不增加占位页。
5. 不猜测歌曲所属专辑，只运行格式与编译检查。

## 已实现切片：Home Quick Picks

状态：功能与 UI 已实现，待真实桌面点击验收。

Android Quick picks 组合 related 映射、Forgotten favorites 和最近歌曲的实时相似推荐。Rust
已经具备 Daily Discover 的实时 Radio/Related 推荐、Forgotten favorites、Keep listening 与
最近播放，因此直接组合这些真实来源，不新增 `related_song_map` 影子存储。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 对已有 Home 推荐来源按 `video_id` 去重并生成 Quick picks |
| `src/ui/shell.rs` | 复用歌曲卡片并增加整组 Play all |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Quick picks 入口 |

### 完成定义

1. 组合 Daily Discover、Forgotten favorites、Keep listening 与最近播放，去重后最多 20 首。
2. Home 提供 Play all、Play、Next、Queue、Download，全部进入现有真实后端。
3. 各来源变化后 UI 随 Shell 状态直接更新，不增加独立持久化层。
4. 只运行格式与编译检查。

## 已实现切片：Home Daily Discover

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `HomeViewModel.getDailyDiscover()` 从本地收藏选择种子，通过 YouTube Related 为每个种子
挑一首推荐，并在 Home 展示“Because you listen to …”。Rust 已有本地收藏和真实 Radio/Related
请求，因此直接复用这两条链路。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 从本地收藏选择种子并并行请求现有 Radio 后端 |
| `src/ui/shell.rs` | Home 展示 Daily Discover 推荐、种子说明和真实歌曲操作 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Daily Discover 入口 |

### 完成定义

1. 最多选择 5 首本地收藏作为种子，每个种子取一首不同的非播客推荐并去重。
2. Home 显示推荐与对应种子，支持 Play all、Play、Next、Queue、Download。
3. 收藏列表变化会刷新推荐；无收藏或无结果时不阻塞其他 Home 内容。
4. 只运行格式与编译检查。

## 已实现切片：Home Keep Listening

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `HomeViewModel` 从最近两周播放事件按累计播放时间取高频歌曲（跳过最靠前的 5 首），
在 Home 的 Keep listening 区块提供直接播放。Rust 已有同等 `play_history.play_time_ms` 数据。

`Quick picks` 审计确认依赖 Android 专有的 `related_song_map`；当前 Rust 没有该映射，因此不为
这一切片新建重复推荐存储，先完成可直接复用现有后端的 Keep listening。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 按近两周累计播放时间查询 Keep listening 歌曲 |
| `src/ui/shell.rs` | 加载并在 Home 接入 Play、Next、Queue、Download |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Home Keep listening 入口 |

### 完成定义

1. 使用最近 14 天 `play_time_ms` 聚合，按 Android 行为跳过前 5 首并取后续 15 首。
2. Home 仅在有结果时展示，列表操作进入现有真实队列/下载后端。
3. 新播放历史和清空历史同步更新该区块。
4. 只运行格式与编译检查。

## 已实现切片：Home Forgotten Favorites

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `HomeViewModel.forgottenFavorites()` 使用播放事件筛选“30 天前累计播放明显高于最近
30 天”的歌曲，并在 Home 提供整组播放和逐曲入口。Rust 已有对应 `play_history` 与 `song`
数据，只缺查询和 Home 呈现。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/storage/sqlite.rs` | 复用现有播放历史表增加 Forgotten favorites 查询 |
| `src/ui/shell.rs` | 加载该列表并在 Home 接入 Play、Next、Queue、Download |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Home 本地历史推荐入口 |

### 完成定义

1. 查询语义与 Android 一致，以 30 天为新旧播放窗口并按旧播放权重筛选。
2. Home 仅在有结果时显示，不阻塞其他本地或匿名内容。
3. 整组播放和逐曲操作进入现有真实队列/下载后端。
4. 新播放历史和清空历史会同步更新该区块。
5. 只运行格式与编译检查。

## 已实现切片：Home Account Playlists

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `HomeScreen.kt` 会把已登录账号的 `accountPlaylists` 直接展示在 Home；Rust 已经通过
云端资料库后端加载同一批歌单，但当前只能到 Library 才能看到，Home 缺少入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | Home 展示真实账号歌单卡片，点击进入现有在线歌单详情 |
| `src/ui/shell.rs` | Home 路由把歌单与账号头像纳入现有缩略图加载管线 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Home 账号歌单入口 |

### 完成定义

1. 已登录且云端歌单非空时，Home 可直接看到账号歌单。
2. 点击歌单进入现有真实 browse 详情，Back 返回 Home。
3. 未登录、加载或失败时不复制 Library 的账号管理 UI，不阻塞匿名 Home 内容。
4. 只运行格式与编译检查。

## 已实现切片：Explore Charts / Trending

状态：功能与 UI 已实现，待真实桌面点击验收。

Android `ExploreScreen.kt` 会先加载 `FEmusic_charts`，把 Trending、Top 等歌曲榜单直接放在
Explore 首屏；Rust 当前只展示 New releases 与 Moods & genres，缺少这段主要内容。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/services/innertube.rs` | 复用现有 browse 请求和 Home shelf 解析器读取 `FEmusic_charts` |
| `src/domain/home.rs` | 在 Explore 页面模型中携带榜单 sections |
| `src/ui/shell.rs` | 在 Explore 首屏展示榜单，并复用 Play、Next、Queue、Download 与详情入口 |
| `PORTING_PLAN.md` / `CORE_UI_PARITY.md` | 记录 Explore 真实入口状态 |

### 完成定义

1. Explore 与 Android 一样请求并展示 `FEmusic_charts` 的歌曲 sections。
2. 榜单歌曲直接接入现有真实播放队列，并具有 Next、Queue、Download 操作。
3. 榜单封面进入现有缩略图加载管线，加载/失败沿用 Explore 页面状态。
4. 只运行格式与编译检查，不新增与该功能无直接关系的测试。

## 已实现切片：完整“正在播放”界面

状态：功能与 UI 已实现，待一次真实桌面人工点击验收；不为此开发合成输入或临时验证设施，
继续推进下一项 P0 移植。

遗漏原因：此前把底栏播放器、歌词侧栏和队列侧栏误判为播放器 UI 闭环，没有覆盖 Android
`MiniPlayer.kt` 点击展开 `Player.kt` 的核心入口。

### 代码改动

| 文件 | 改动 |
| --- | --- |
| `src/ui/shell.rs` | 增加完整播放器显示状态、当前标签状态及底栏点击入口 |
| `src/ui/shell.rs` | 使用 `gpui_component::tab::{Tab, TabBar}` 构建 Up next / Lyrics / Related 标签 |
| `src/ui/shell.rs` | 左侧接入当前封面、标题、作者；底部沿用真实播放/进度/音量控制 |
| `src/ui/shell.rs` | Up next 接入现有队列选择、改序、移除、清空待播、Shuffle、Repeat |
| `src/ui/shell.rs` | Lyrics 接入现有歌词加载、失败重试、同步高亮和点击 seek |
| `src/ui/shell.rs` | Related 接入现有匿名 Radio 请求，并显示真实推荐歌曲操作 |
| `PORTING_PLAN.md` | 撤销“核心 UI 已闭环”的过早结论并记录真实状态 |
| `CORE_UI_PARITY.md` | 将完整播放器各点击路径逐项更新为真实验收状态 |

### 完成定义

1. 点击底栏当前歌曲或显式入口能展开完整播放器，Back 能返回原页面。
2. 切歌后封面、标题、作者、歌词和相关内容不会停留在旧歌曲。
3. Up next 中的选择、改序、移除、清空待播实际修改播放队列并持久化。
4. Lyrics 标签实际加载当前歌曲歌词；同步行点击调用真实 seek。
5. Related 标签实际请求 Radio；推荐结果能 Play、Next 或 Queue。
6. 窗口最小尺寸 720×520 和默认尺寸均不遮挡退出入口及主要控制。
7. 只做与改动直接相关的最小格式/编译检查；完整门禁留到发布前统一运行一次。
8. 在可渲染桌面完成“底栏 → 完整播放器 → 三个标签 → Back”的人工点击验证；
   只有 Xvfb 冷启动时必须明确标记为尚未完成点击验收。

## 紧随其后的切片

完整播放器完成后，不继续高级功能，按以下顺序执行：

1. 用 `CORE_UI_PARITY.md` 反向审计 Android P0 点击入口，补齐遗漏项。
2. 由用户在新 release 上点击 Sign in，完成真实 Google 回跳、账号验证、凭据保存、重启恢复和退出登录验收。
3. 在真实 Linux 桌面完成首页 → 搜索 → 详情 → 播放 → 完整播放器 → 队列/歌词 → 资料库/下载的连贯冒烟测试。
4. 再处理 Windows WebView2/Credential Manager 与 macOS WKWebView/Keychain 的 P1 实机验证。

## 每次代码切片流程

1. 先更新本文件的当前切片、范围和完成定义。
2. 只读定位 Android 点击入口及其业务状态来源。
3. 定位 Rust 现有后端，先复用，再补真正缺失的业务能力。
4. 依据锁定的 `gpui-component` 源码/示例实现 UI；不读取或套用其他 UI skill。
5. 更新 `CORE_UI_PARITY.md` 的逐入口状态。
6. 做与本轮改动直接相关的最小验证，并单独记录是否完成真实点击验收。
