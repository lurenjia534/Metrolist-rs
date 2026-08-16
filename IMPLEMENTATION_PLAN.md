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
和 Top Artists，并可从歌曲榜直接建立播放队列。Desktop 已有按实际播放满 30 秒写入的
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
