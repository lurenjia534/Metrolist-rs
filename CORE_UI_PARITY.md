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
| 迷你播放器 → 当前歌曲动作 | `MiniPlayer.kt` 的 Subscribe、Add to playlist、Favorite | 底栏直接切换歌曲本地收藏；Episode 心形动作切换 Episodes for Later；可打开本地歌单选择器，已登录且有真实 Artist ID 时订阅/取消订阅并可见失败 | 已实现，待真实账号桌面点击验收 |
| 完整播放器控制 | `Player.kt`、`PlayerMenu.kt`、`MiniPlayer.kt`、`ShowMediaInfo.kt` | 封面、标题/歌手、进度、播放/暂停、上一首、下一首、Shuffle、Repeat；Up next 直接显示并操作真实 Sleep Timer（5–120 分钟自定义、15/30/60 分钟预设、当前歌曲结束、取消）；EQ 标签显示真实生效状态，可即时应用并保存 Off/Bass/Vocal/Treble，且可进入完整 AutoEQ/APO/十段设置；当前普通歌曲可本地 Favorite、YT Like、以 fresh feedback token Add/Remove from Library，并可 Pin/Unpin 到持久 Speed Dial，Episode 心形动作切换 Episodes for Later；可打开可见的本地/云端歌单 Picker、订阅真实 Artist、管理 Download/Pause/Resume/Remove offline、复制标准链接、查看本地 Song/Offline 与实时作者频道、上传日期、订阅数、描述及公开 Views/Likes/Dislikes、按 video ID 刷新元数据，并打开所有带真实 ID 的 Artist 及真实 Album 详情 | 已实现，待真实账号桌面点击验收 |
| 完整播放器 → 歌词 | `Player.kt` 的 inline lyrics 与 `Queue.kt` 歌词入口 | 完整播放器内切换歌词、加载/重试、同步行点击 seek | 已实现，待真实桌面点击验收 |
| 完整播放器 → 待播队列 | `Queue.kt`、`SelectionSongsMenu.kt`、`QueueMenu.kt`、`ShowMediaInfo.kt` | 完整播放器 Up next 显示当前项之前、当前项和未来项的完整真实队列；与侧栏 Queue 共用基于 video ID 的 Select、逐项选择、Select all/Clear all、数量和退出状态，并可对所选子集执行 Play、Shuffle、Add to queue、Add to local playlist、Download、Remove selected；仍可逐项播放、改序、移除、清空待播及再次 Play next/Add to queue，歌曲可本地收藏并在登录后切换 YT Like、加入本地或可编辑云端歌单，Episode 可切换 Episodes for Later；另可管理离线下载、复制标准链接、从 YouTube Music 刷新真实元数据、打开本地与实时歌曲详情及所有带真实 ID 的 Artist/Album；Guest 不可执行队列选择写操作 | 已实现，待真实账号桌面队列批量点击验收 |
| 完整播放器 → 相关歌曲 | Android radio/automix 队列逻辑、`QueueMenu.kt` | 完整播放器内可从当前歌曲或任意非单集队列歌曲启动真实 Radio，并展示已进入待播队列的相关推荐 | 已实现，待真实桌面点击验收 |
| 设置 → YouTube Music 登录 | `LoginScreen`、`AccountSettings` | `Sign in` 点击打开系统 WebView，回跳验证并保存会话 | 已实现，待真实账号点击验收 |
| 首页/探索 → 详情或播放 | `HomeScreen.kt`、`ExploreScreen.kt`、`NewReleaseScreen.kt` | 推荐、持久 Song/Album/Artist/Playlist/Podcast/Local Playlist Speed Dial、Quick picks、Daily Discover、Keep listening、Forgotten favorites、账号歌单、Charts/Trending、新发行完整列表及分类可打开详情或建立真实播放队列；Speed Dial 固定项优先并以 Keep Listening/Quick Picks 去重补到最多 27 个媒体项，Randomize 按 Android 的 80% 用户歌曲/20% Home 目录选择并播放或进入详情；Song 固定项可播放、Next、Queue、Download，Browse 固定项点击进入真实在线详情，Local Playlist 固定项读取并进入真实本地歌单，均可原位 Unpin；对应详情可 Pin/Unpin | 已实现，待桌面点击验收 |
| 搜索输入 → 历史/建议/结果 → 播放 | `SearchScreen.kt`、`OnlineSearchSuggestionViewModel.kt`、`OnlineSearchResult.kt`、`LocalSearchScreen.kt`、`LocalSearchViewModel.kt`、`YouTubeUrlParser.kt` | 提交查询自动保存；空输入及前缀搜索可复搜、填充、单删或清空历史；Online 默认 All 在同页展示真实歌曲与 Album/Artist/Playlist/Podcast/Profile 等目录，目录-only 响应不会误显示为空，continuation 对两类结果均可继续追加；另有歌曲/视频/专辑/艺术家/社区与精选歌单/播客/单集/Profile 筛选、建议/详情/播放、YouTube 链接直达；Local 合并收藏、历史、下载、稍后单集及全部本地歌单关联歌曲，提供歌曲/专辑/艺术家/歌单过滤与 All 分组预览；仅存在于本地歌单的歌曲及其带真实 ID 的 Album/Artist 也可搜索、播放或进入详情 | 已实现，待桌面在线混合与本地歌单来源点击验收 |
| Album/Playlist 详情 → 作者/集合动作 | `AlbumScreen.kt`、`OnlinePlaylistScreen.kt`、`YouTubePlaylistMenu.kt` | Header 中带真实 ID 的 Playlist author 或一个/多个 Album artists 可进入 Artist 并 Back；Play all、整组 Play next/Queue all/Add all to local playlist、Download all、标准链接 Copy link 及逐曲 Play/Next/Queue/Download，所有整组本地动作会先跟随真实 continuation 补齐长 collection，加入本地歌单使用单事务；Playlist 优先使用响应真实 Shuffle endpoint 并在存在真实 Radio endpoint 时显示 Radio，缺少服务端 Shuffle 时对补齐后的完整歌曲执行本地随机；Album 保留完整整组 Shuffle，登录后可用真实 playlist ID Save/Remove 并同步 Library Albums | 已实现，待真实账号桌面点击验收 |
| Artist 详情 → About/分区/Radio/Shuffle/订阅 | `ArtistScreen.kt`、`ArtistItemsScreen.kt`、`YouTubeArtistMenu.kt` | About 显示响应原始订阅数、月听众和描述，并可复制真实 channel 标准链接；Songs/Albums/Singles 等分区仅在响应提供真实 `moreEndpoint` 时显示 View all，并复用 browse 分页与 Back；Radio/Shuffle 复用真实 `next` endpoint 并以服务端队列开始播放，加载/失败原位可见；登录后可订阅/取消订阅真实频道 | 已实现，待真实账号桌面点击验收 |
| Podcast 详情 → 保存/频道/单集 | `OnlinePodcastScreen.kt` | 保存/移除节目；按 episode 标题/作者实时搜索并以过滤集合播放；仅用响应真实 channel ID 打开 View channel，Back 恢复已加载 Podcast；单集可播放、Next、Queue、Download、Episodes for Later | 已实现，待真实账号桌面点击验收 |
| 资料库 → 混合入口/歌曲/歌单/专辑/艺术家/播客/历史/下载 | `LibraryScreen.kt`、`LibraryMixScreen.kt`、`LibrarySongsScreen.kt`、`LibraryPlaylistsScreen.kt`、`LibraryAlbumsScreen.kt`、`LibraryArtistsScreen.kt`、`LibraryPodcastsScreen.kt` 与 Android Library 各屏 | Overview 聚合并搜索本地/云端真实歌单、专辑和艺术家，搜索时也返回可操作歌曲；设备已知歌曲包含全部本地歌单关系，因而仅存在于本地歌单的歌曲及其真实 Album/Artist 无需先在线浏览即可在 Overview 和本地 Albums/Artists 中发现；并可刷新账号资料库、创建本地歌单及按来源顺序/名称排序；顶部分类切换；Songs 提供 Liked/Library/Uploaded/Downloaded；Playlists 可统一搜索本地/云端歌单并进入 Liked/Offline/My Top/Uploaded，本地歌单详情显示持久自定义封面并可通过系统图片选择器选择/替换/移除；没有自定义封面时按 Custom position 用前四张真实歌曲封面显示单图或四格，Library、Local Search 与 Home Speed Dial 共用该封面；详情提供 Custom order/Date added/Title/Artist/Play time 与非 Custom 升降序，使用真实加入时间和累计播放时长；Custom 空搜索时可 Move up/Move down 并以 SQLite 单事务修改真实 position；可按歌曲标题/任一歌手实时搜索并显示结果数/无匹配，过滤结果仍从排序后的完整歌单对应索引播放；整组提供 Play、Shuffle、Queue all、Download all 与确认式 Remove downloads、Android 同正文的 Copy track list、原生 Save 对话框 CSV/M3U 导出、Pin、Rename、Delete，并可 Select 子集后 Play、Shuffle、Play next、Queue、Add to playlist、Download 或单事务 Remove selected；逐曲动作保持可用；Albums 提供 Liked/Library/Uploaded，Artists 提供 Liked/Library，Podcasts 提供 Episodes/Channels/Downloaded；各真实来源可进入播放、节目、频道或 Album/Artist 详情，其余本地/远端写操作保持可用 | 已实现，待真实账号与本地歌单来源桌面点击验收 |
| 主导航 → History → 过滤/播放/删除 | `MainActivity.kt` 的 History 图标、`HistoryScreen.kt`、`HistoryViewModel.kt` | 直接入口；Local/YouTube Music 切换及标题/歌手过滤；本地单删/清空，远端仅对携带真实非空 feedback token 的项目显示并执行精确删除，无 token 项不暴露无效动作；两类历史均可播放/入队/下载 | 已实现，待真实账号桌面点击验收 |
| 主导航 → Stats → 榜单播放/详情 | `MainActivity.kt` 的 Stats 图标、`StatsScreen.kt`、`StatsViewModel.kt` | 7 天至全部时间范围，按真实本地播放时长展示总览、Top Songs/Artists/Albums；周期切换采用 latest 请求代际，旧周期成功或失败不能覆盖当前选择；歌曲可播放/入队/下载，歌手与专辑可打开详情 | 已实现，待真实桌面快速周期切换验收 |
| Recognize → 匹配 → 历史/播放/搜索 | `RecognitionScreen.kt`、`RecognitionHistoryScreen.kt`、`MusicRecognitionService`、ShazamKit | 点击录音、真实 Shazam 匹配、Match/No match/Error；成功结果自动保存，可从 History 重新搜索、单删或清空，并进入 YouTube Music 播放或搜索 | 已实现，待真实麦克风点击验收 |
| 设置 → 音频/代理/音质/均衡器 | Android Settings 各屏 | 编辑、保存并即时作用于真实服务 | 已实现，待桌面点击验收 |
| 设置 → 内容语言/国家地区 | `ContentSettings.kt` 的 Content language / Content country，`App.kt` 的 YouTube locale 应用 | 两个可滚动下拉入口分别提供跟随系统及 Android 当前完整选项；保存后统一重建 InnerTube 并让 Home、Explore、Search、Browse、Radio 与账号资料库使用解析后的 `hl/gl`，Reset 只恢复已保存值 | 已实现，待真实桌面区域内容点击验收 |
| 设置 → 内容过滤 | `ContentSettings.kt` 的 Hide explicit / Hide video songs / Hide YouTube Shorts，`YTItem.filterExplicit` / `filterVideoSongs` / `filterYoutubeShorts` | Appearance 提供三个可点击开关，默认关闭；保存后跨重启保留，Reset 恢复已保存值；Home、Explore、Search、Browse、Library 列表与由这些列表发起的 Play all 按 Android 规则隐藏 explicit 项、非 ATV 视频歌、以及去 `VL` 后以 `SS` 开头的歌单；缺少真实标记的项保留；已在播放的队列不因开关被改写；过滤后为空显示空态 | 已实现，待真实桌面列表点击验收 |
| 设置 → 静音暂停 | `PlayerSettings.kt` 的 Pause music when media is muted | 默认音量归零仍继续播放；开启后本地滑杆或系统媒体音量归零会暂停，只有该静音动作造成的暂停会在恢复音量后继续，手动控制和 Guest 音量同步不误触发 | 已实现，待真实桌面音量点击验收 |
| 设置 → 渐进封面 Seek | `PlayerSettings.kt` 的 Progressive seek、`Thumbnail.kt` 的 `onDoubleTap` | 完整播放器封面左右半区双击默认快退/快进 5 秒；开启后，一秒内连续双击按 5、10、15…秒递增，复用真实播放位置与时长裁剪，Guest 不可操作 | 已实现，待真实桌面封面双击验收 |
| 设置 → Skip silence / Instant skip | `PlayerSettings.kt` 的 Skip silence / Instant skip、`MusicService` 的静音处理 | 默认完整播放 PCM；开启后按全声道近静音 frame 保留短静音并压缩持续静音，Instant 在连续 2 秒后直接跳过剩余静音；跳过时长计入真实媒体位置，进度、歌词、历史、外部状态与 Sleep Timer 保持对齐 | 已实现，待真实桌面静音音轨点击验收 |
| 设置 → Crossfade / Gapless albums | `PlayerSettings.kt` 的 Crossfade 开关、1–15 秒时长和 Gapless albums，`MusicService` 的双播放器切换 | 默认关闭；开启后当前歌曲自然进入末尾窗口时，在同一 Rodio mixer 上让下一首从零开始并以二次曲线重叠淡入淡出；同真实 Album ID 且 Gapless 开启时保持原自然切歌，Episode、Guest、曲末 Sleep Timer 和无自动目标时不提前切歌 | 已实现，待真实桌面连续音轨点击验收 |
| 设置 → Sleep Timer 结束行为 | `PlayerSettings.kt` 的 Stop after current song / Fade out、`SleepTimer.kt` | 分钟定时器可选择到点立即暂停或播完当时的当前歌曲后停止；可在真正停止前最后 60 秒线性淡出，直接 End of song 同样生效；淡出不改变用户音量、会话音量或触发静音暂停 | 已实现，待真实桌面定时器点击验收 |
| 设置 → Automatic Sleep Timer | `PlayerSettings.kt` 的 Automatic sleep timer / repeat 日程、`SettingsSleepTimerDialog.kt`、`PlayerConnection.kt` 的 paused→playing 检查 | 默认关闭、30 分钟、每日 22:00–06:00；Settings 可开关、调整 5–120 分钟、应用 Daily/Weekdays/Weekends 预设，并逐日启用及调整独立 HH:MM 开始/结束窗口；支持跨午夜，非 Playing→Playing 命中本地日程且没有活动 timer 时复用现有 Deadline、Finish current song 与 Fade out，Guest 不触发 | 已实现，待真实桌面时区与播放转换点击验收 |
| 设置 → 队列持久化 | `PlayerSettings.kt` 的 Persistent queue | 默认保存并在冷启动恢复队列、当前歌曲、位置与可复用播放源；关闭后当前播放不受影响，但下次启动不恢复上述状态，Repeat/Shuffle 与音量仍保留 | 已实现，待真实桌面重启点击验收 |
| 设置 → 自动播放 | `PlayerSettings.kt` 的 Autoplay | 默认在 Repeat Off 时让自然结束的歌曲进入队列下一首；关闭后停在当前歌曲，Repeat One/All 和所有手动播放控制保持原行为 | 已实现，待真实桌面自然播完点击验收 |
| 设置 → 自动 Radio 队列 | `PlayerSettings.kt` 的 Auto radio queue | 默认从 Search 结果/建议或 Home 单曲点击时以所选歌曲立即播放并建立真实 YouTube Radio；关闭后只建立该单曲队列且不立即补相似内容，Play all 与显式 Radio 不受影响 | 已实现，待真实桌面 Search/Home 点击验收 |
| 设置 → 收藏后自动下载 | `PlayerSettings.kt` 的 Auto-download on like、`MusicService.toggleLike` | 默认只收藏；开启后普通歌曲成功加入本地 Favorites 才进入现有离线下载队列，取消收藏、Episode、写入失败及已完成/已排队的同音质下载不重复触发 | 已实现，待真实桌面收藏/下载点击验收 |
| 设置 → 播放错误自动跳过 | `PlayerSettings.kt` 的 Auto skip to next song when error occurs | 默认在现有离线修复/播放源刷新均失败后停留并显示错误；开启后复用真实队列 Next 跳过失败歌曲，连续第三首失败自动停止，成功播放会清零计数 | 已实现，待真实桌面失败播放点击验收 |
| 设置 → 队列模式持久化 | `PlayerSettings.kt` 的 Persistent shuffle、Remember shuffle and repeat | 可选择普通新队列是否保留已开启的 Shuffle，以及冷启动是否恢复 Shuffle/Repeat；显式 Shuffle、当前运行模式和手动队列控制不被设置保存打断 | 已实现，待真实桌面新队列/重启点击验收 |
| 设置 → 优先随机原始集合 | `PlayerSettings.kt` 的 Shuffle playlist or album first | 默认将全部待播歌曲混合随机；开启后原播放列表/专辑与后来 Queue/Automatic radio 内容分别随机，当前仍在原集合时先播完原集合，Repeat All 新一轮继续保持分组 | 已实现，待真实桌面 Shuffle/Radio 点击验收 |
| 设置 → 防止队列重复歌曲 | `PlayerSettings.kt` 的 Prevent duplicate tracks in queue | 默认允许显式重复入队；开启后单曲/整组 Play next 与 Add to queue 会先移除同 ID 的所有非当前旧项再插入目标位置，当前播放项与既有 Shuffle 行为保持不变 | 已实现，待真实桌面队列点击验收 |
| 设置 → Similar content / Repeat All 自动补充 | `PlayerSettings.kt` 的 Enable similar content、Disable load more when Repeat All | Similar content 独立控制普通队列接近末尾时是否开始首个相似推荐页；Repeat All 可停止新的初始页和续页请求；手动 Radio、已有队列、Repeat 与 Shuffle 状态不受影响 | 已实现，待真实桌面队尾点击验收 |
| 设置 → Auto load more | `PlayerSettings.kt` 的 Auto load more、`MusicService.onMediaItemTransition` | 默认在已有 Radio 队列剩余不超过五首且存在 continuation 时自动请求并去重追加下一页；关闭后当前已加载队列继续播放但不再自动翻页，显式 Radio 与 Similar content 首页行为保持独立 | 已实现，待真实桌面 Radio 队尾点击验收 |
| 设置 → 播放历史时长 | `PlayerSettings.kt` 的 History duration | 以 1–100 秒、步长 1 秒的真实滑杆编辑并持久保存历史门槛；累计真实播放达到生效值后，本地历史/统计与已开启的 YouTube Music 远端注册各执行一次，Reset 恢复已保存值 | 已实现，待真实桌面播放点击验收 |
| 设置 → 隐私历史 | `PrivacySettings.kt` | 保存 Pause listening history / Pause search history 后分别停止新增本地播放统计与搜索记录，已有数据保留；同卡片可确认清空全部本地收听或搜索历史并原位显示状态；恢复记录后继续复用既有 SQLite 写入，远端播放历史由原独立开关控制 | 已实现，待真实桌面历史点击验收 |
| 设置 → 存储清理 | `StorageSettings.kt` | 分别确认并清除播放缓存、图片缓存，或取消活动下载并移除全部显式离线下载；复用真实缓存目录和既有下载/SQLite 状态机，操作状态与失败在 Settings 原位可见 | 已实现，待真实桌面存储点击验收 |

## 每轮收尾审计

每次声称一个核心切片完成前，必须：

1. 搜索 Android 对应界面的点击处理和状态来源。
2. 搜索 Rust 所有同类后端调用，确认每个用户入口都有 UI。
3. 更新上表，不允许用“阶段已完成”替代逐入口状态。
4. 只运行当前改动所需的最小格式/编译检查；完整测试、Clippy 或 release 构建仅在当前切片明确
   要求或发布前运行一次，不重复制造验证证据。
5. 优先完成一次真实桌面点击验收；不为替代人工点击而开发合成输入或临时验证设施。
