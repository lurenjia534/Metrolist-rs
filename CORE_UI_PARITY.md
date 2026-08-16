# Metrolist 核心 UI 对照门槛

本文件是核心功能完成度的硬门槛。Android 项目只读参考路径为
`/home/luren/Code/Android/Metrolist`。阶段 4 或其他扩展功能的实现数量不能抵消这里的缺口。

## 移植目标与优先级（最高硬约束）

- 本项目当前唯一首要目标是：尽快把 Android Metrolist 已有的功能、真实后端行为和核心 UI
  移植到 Desktop；完成可用的桌面应用优先于安全加固、测试覆盖率、架构优化和扩展能力。
- 实现依据按以下顺序执行：Android 现成功能与交互、Rust 已有后端能力、本文锁定的
  `gpui-component` UI 约束。与这三项无关的工作不得占用当前移植切片。
- 每轮必须优先交付可发现、可点击、接入真实后端的纵向功能闭环；不得用测试、文档、探针、
  验证设施或底层重构代替缺失的功能和 UI。
- 除非当前功能无法工作或用户明确要求，不主动扩展安全审计/加固、通用抽象、性能工程、
  额外兼容层、测试框架、合成输入、临时 UI 钩子或重复的环境验证。
- 测试和构建只做与本轮改动直接相关的最小验证。已有门禁通过后不得为了增加“证据”重复运行；
  不为边缘情况堆叠测试。功能是否完成以真实后端接通和桌面 UI 可操作为主。
- 本节优先级高于其他计划文档中的泛化测试、安全或审计要求；发生冲突时以本节为准。

## 完成判定

一个核心入口只有同时满足以下条件，才允许标记为“完成”：

1. 已定位 Android 中用户实际点击的入口和目标界面。
2. Rust 中存在可发现、可点击的对应入口，而不只是后台函数。
3. 点击后进入真实后端状态机或网络/存储操作，不是静态占位 UI。
4. 加载、成功、空数据和业务失败状态在目标界面可见。
5. 在可用桌面环境完成一次直接点击验收；仅能冷启动不能算点击验证。自动测试不是核心 UI
   完成的独立前置条件，只在能直接防止本轮功能回归时补充最小测试。

任何一项不满足时，`PORTING_PLAN.md` 不得使用“核心 UI 闭环完成”描述该功能。

## UI 实现约束

- 交互与业务行为只读参考 Android Metrolist。
- 桌面 UI 的组件、主题、布局惯例和交互状态以 `Cargo.toml` 锁定提交
  `f3ba893bd6a996ab0699266ba774b5bbb7f0ca1c` 的 Longbridge `gpui-component`
  源码、示例和公开 API 为准。
- 优先复用 `gpui-component` 的 Button、Tabs、Slider、Scroll、Dialog、Theme 等组件；
  组件库没有对应能力时，才使用 GPUI 原语组合。
- 不引入其他 UI 框架的技能、组件语义或设计系统作为实现依据。
- 新界面必须沿用应用当前主题 token，不写死另一套颜色、圆角或排版体系。

## 核心点击路径矩阵

| 核心路径 | Android 入口 | Rust 必需入口与结果 | 当前状态 |
| --- | --- | --- | --- |
| 迷你播放器 → 完整播放器 | `MiniPlayer.kt` 的 `onClick` 展开 `Player.kt` | 点击底栏当前歌曲/封面，进入完整正在播放界面 | 已实现，待真实桌面点击验收 |
| 完整播放器控制 | `Player.kt`、`MiniPlayer.kt` | 封面、标题/歌手、进度、播放/暂停、上一首、下一首、Shuffle、Repeat；当前歌曲可本地/云端收藏、加入歌单、订阅真实 Artist，并打开 Artist/Album 详情 | 已实现，待真实账号桌面点击验收 |
| 完整播放器 → 歌词 | `Player.kt` 的 inline lyrics 与 `Queue.kt` 歌词入口 | 完整播放器内切换歌词、加载/重试、同步行点击 seek | 已实现，待真实桌面点击验收 |
| 完整播放器 → 待播队列 | `Queue.kt` | 完整播放器内查看、选择、改序、移除、清空待播 | 已实现，待真实桌面点击验收 |
| 完整播放器 → 相关歌曲 | Android radio/automix 队列逻辑 | 完整播放器内启动 Radio，并展示已进入待播队列的相关推荐 | 已实现，待真实桌面点击验收 |
| 设置 → YouTube Music 登录 | `LoginScreen`、`AccountSettings` | `Sign in` 点击打开系统 WebView，回跳验证并保存会话 | 已实现，待真实账号点击验收 |
| 首页/探索 → 详情或播放 | `HomeScreen.kt`、`ExploreScreen.kt`、`NewReleaseScreen.kt` | 推荐、Quick picks、Daily Discover、Keep listening、Forgotten favorites、账号歌单、Charts/Trending、新发行完整列表及分类可打开详情或建立真实播放队列 | 已实现，待桌面点击验收 |
| 搜索输入 → 历史/建议/结果 → 播放 | `SearchScreen.kt`、`OnlineSearchSuggestionViewModel.kt`、`OnlineSearchResult.kt`、`LocalSearchScreen.kt`、`LocalSearchViewModel.kt`、`YouTubeUrlParser.kt` | 提交查询自动保存；空输入及前缀搜索可复搜、填充、单删或清空历史；Online 默认 All 混合结果及歌曲/视频/专辑/艺术家/社区与精选歌单/播客/单集/Profile 筛选、建议/详情/播放、YouTube 链接直达；Local 提供歌曲/专辑/艺术家/歌单过滤与 All 分组预览，歌曲播放、歌单及 Album/Artist 详情 | 已实现，待桌面点击验收 |
| Album/Playlist 详情 → 集合动作 | `AlbumScreen.kt`、`OnlinePlaylistScreen.kt` | Play all、Shuffle、Download all 及逐曲 Play/Next/Queue/Download；登录后 Album 可用真实 playlist ID Save/Remove 并同步 Library Albums | 已实现，待真实账号桌面点击验收 |
| 资料库 → 歌曲/歌单/专辑/艺术家/播客/历史/下载 | `LibraryScreen.kt`、`LibrarySongsScreen.kt`、`LibraryPlaylistsScreen.kt`、`LibraryAlbumsScreen.kt`、`LibraryArtistsScreen.kt`、`LibraryPodcastsScreen.kt` 与 Android Library 各屏 | 顶部分类切换；Songs 提供 Liked/Library/Uploaded/Downloaded；Playlists 可统一搜索本地/云端歌单并进入 Liked/Offline/My Top/Uploaded；Albums 提供 Liked/Library/Uploaded，Artists 提供 Liked/Library，Podcasts 提供 Episodes/Channels/Downloaded；各真实来源可进入播放、节目、频道或 Album/Artist 详情，其余本地/远端写操作保持可用 | 已实现，待真实账号桌面点击验收 |
| 主导航 → History → 过滤/播放/删除 | `MainActivity.kt` 的 History 图标、`HistoryScreen.kt`、`HistoryViewModel.kt` | 直接入口；Local/YouTube Music 切换及标题/歌手过滤；本地单删/清空、远端精确删除，并可播放/入队/下载 | 已实现，待桌面点击验收 |
| 主导航 → Stats → 榜单播放/详情 | `MainActivity.kt` 的 Stats 图标、`StatsScreen.kt`、`StatsViewModel.kt` | 7 天至全部时间范围，按真实本地播放时长展示总览、Top Songs/Artists/Albums；歌曲可播放/入队/下载，歌手与专辑可打开详情 | 已实现，待桌面点击验收 |
| Recognize → 匹配 → 历史/播放/搜索 | `RecognitionScreen.kt`、`RecognitionHistoryScreen.kt`、`MusicRecognitionService`、ShazamKit | 点击录音、真实 Shazam 匹配、Match/No match/Error；成功结果自动保存，可从 History 重新搜索、单删或清空，并进入 YouTube Music 播放或搜索 | 已实现，待真实麦克风点击验收 |
| 设置 → 音频/代理/音质/均衡器 | Android Settings 各屏 | 编辑、保存并即时作用于真实服务 | 已实现，待桌面点击验收 |

## 每轮收尾审计

每次声称一个核心切片完成前，必须：

1. 搜索 Android 对应界面的点击处理和状态来源。
2. 搜索 Rust 所有同类后端调用，确认每个用户入口都有 UI。
3. 更新上表，不允许用“阶段已完成”替代逐入口状态。
4. 只运行当前改动所需的最小格式/编译检查；完整测试、Clippy 或 release 构建仅在当前切片明确
   要求或发布前运行一次，不重复制造验证证据。
5. 优先完成一次真实桌面点击验收；不为替代人工点击而开发合成输入或临时验证设施。
