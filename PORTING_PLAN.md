# Metrolist GPUI 桌面版移植计划

## 1. 项目目标

在 `/home/luren/Code/rust/Metrolist-rs` 中实现一个面向 Linux、Windows 和 macOS 的 Metrolist 桌面客户端，使用 Rust、GPUI 和 GPUI Component 构建界面，满足日常搜索和播放 YouTube Music 的需求。

Android 原项目 `/home/luren/Code/Android/Metrolist` 只作为业务行为、协议模型和交互设计参考，不在移植过程中修改。

核心 UI 完整度以 [`CORE_UI_PARITY.md`](CORE_UI_PARITY.md) 的逐点击入口矩阵为硬门槛；高级功能数量不能抵消核心入口或目标界面的缺失。

当前代码切片、修改范围和完成定义以 [`IMPLEMENTATION_LPLAN.md`](IMPLEMENTATION_PLAN.md) 为准；每次开始写代码前先更新该活动计划。

首要原则是先完成可用的听歌闭环，再逐步追求 Android 版本的功能覆盖：
核心诉求是把 Metrolist 的实际音乐功能后端和可用 UI 做完整，不是继续堆异常兜底、看门狗或平台防御代码。

```text
搜索歌曲 → 获取播放地址 → 播放 → 控制进度和音量 → 管理队列 → 保存历史
```

## 2. 当前状态

Rust 项目目前已经完成：

- 引入 `gpui` 和 `gpui_platform`。
- 引入 `gpui-component`，精确固定到提交 `f3ba893bd6a996ab0699266ba774b5bbb7f0ca1c`。
- Linux 启用了 `wayland` 和 `x11` 后端。
- macOS 启用了 `font-kit` 和 `runtime_shaders`。
- `gpui_platform::application()` 可以启动最小 GPUI Component 窗口。
- 当前 GPUI 解析到 Zed 提交 `9f164a0d2eec5805fc66a9e9a8d624fbf0ef24e1`，由 `Cargo.lock` 固定。

Android 参考项目大致规模：

- 541 个 Kotlin 文件。
- App 代码约 12 万行。
- InnerTube 协议和解析代码约 1.1 万行。
- 184 个 UI 文件、31 个 ViewModel。
- Media3/ExoPlayer 播放栈。
- Room SQLite schema v38。
- 播放地址解析包含 PoToken、`n` 参数转换、签名解密和多个 YouTube 客户端回退策略。

因此，本项目属于跨平台重写，不是 Kotlin 到 Rust 的逐文件翻译。

## 3. 约束和原则

### 3.1 平台

- Linux 是首个开发和运行验证平台。
- 架构从第一天保持 Windows 和 macOS 可实现，禁止把 Linux API 泄漏到领域层。
- 平台媒体键、通知、托盘和凭据存储通过独立适配层实现。

### 3.2 上游项目

- Android 仓库保持只读。
- 不修改 Android 数据库 schema、Markdown、版本号或构建配置。
- 可以参考协议、模型、算法和用户体验，但不能依赖 Android/JVM 运行时。

### 3.3 许可证

Android Metrolist 使用 GPL-3.0。桌面版按 GPL-3.0 兼容方式管理，并保留必要的来源和版权说明。GPUI 与 GPUI Component 的 Apache-2.0 许可证可与 GPL-3.0 项目组合使用。

### 3.4 交付策略

- 每个阶段必须形成可运行的纵向切片。
- 不在播放闭环完成前批量移植所有页面。
- 外部服务通过 trait 隔离，UI 不直接发送网络请求或操作数据库。
- 后台任务不能阻塞 GPUI 主线程。
- 依赖选择优先考虑三平台可构建、可分发和长期维护。

## 4. 目标架构

项目最终建议拆成 Cargo workspace：

```text
Metrolist-rs
├── crates/
│   ├── metrolist-app/        GPUI 启动、窗口、路由和应用状态
│   ├── metrolist-ui/         页面、组件、主题和资源
│   ├── metrolist-domain/     Song、Album、Artist、Queue 等领域模型
│   ├── metrolist-innertube/  YouTube Music 请求、响应和解析
│   ├── metrolist-playback/   播放器状态机、解码、输出和队列
│   ├── metrolist-storage/    SQLite、设置、历史、歌单和缓存索引
│   └── metrolist-platform/   媒体键、通知、托盘和平台路径
├── assets/                   图标、占位图和应用资源
└── src/main.rs               最薄的桌面入口
```

初期不必一次拆完；先保证模块边界清晰，在代码增长后再移动为独立 crate。

### 4.1 状态流

```text
GPUI View
   │ action
   ▼
AppModel / PageModel
   │ command
   ├──────────► InnerTubeService ─────► YouTube Music
   ├──────────► PlaybackService ──────► Audio backend
   └──────────► StorageService ───────► SQLite / cache
   ▲
   │ state/event
   └───────────────────────────────────────────────
```

UI 只读取可观察状态并发送 action。网络、播放和数据库状态通过实体更新回 GPUI，不允许从后台线程直接操作 View。

## 5. 技术方向

### 5.1 UI

- GPUI 负责窗口、渲染、输入和状态实体。
- GPUI Component 提供 `Root`、按钮、输入框、列表、弹窗、菜单和主题基础。
- 桌面布局采用左侧导航、主内容区、右侧可选队列，以及常驻底部播放器。
- 首批页面：搜索、搜索结果、专辑/歌单详情、播放队列和设置。
- 图片加载必须异步，并带内存缓存、磁盘缓存、取消和失败占位。

### 5.2 InnerTube

- 使用 Rust HTTP 客户端和 `serde` 重写请求/响应模型。
- 首批只迁移搜索、搜索建议、歌曲元数据、播放响应和队列所需模型。
- JSON 模型以宽松解析为原则：未知字段忽略，可选字段不得导致整页失败。
- Cookie、visitor data、locale、代理和客户端头部由统一 session 管理。
- 解析层输出稳定领域模型，不把 YouTube renderer 结构暴露给 UI。

### 5.3 播放地址解析

这是移植中风险最高的模块，需要单独做技术验证：

- 获取 `/player` 响应和音频格式。
- 选择合适的 WebM/Opus 或 M4A/AAC 音频流。
- 支持 `signatureCipher` 解密。
- 支持 `n` 参数转换。
- 支持 PoToken/BotGuard，或提供可靠的非 PoToken 客户端回退。
- 处理流 URL 过期、HTTP 403、区域限制和客户端回退。
- 把 Android WebView 依赖替换成跨平台 WebView 或受控 JavaScript 执行环境。

可以使用 `yt-dlp` 作为开发期结果对照工具，但默认不把外部命令作为最终产品的核心运行依赖。

### 5.4 音频后端

播放模块先定义稳定接口：

```rust
trait AudioPlayer {
    fn load(&mut self, source: PlaybackSource) -> Result<()>;
    fn play(&mut self) -> Result<()>;
    fn pause(&mut self) -> Result<()>;
    fn seek(&mut self, position: Duration) -> Result<()>;
    fn set_volume(&mut self, volume: f32);
    fn snapshot(&self) -> PlaybackSnapshot;
}
```

优先验证纯 Rust 路线是否满足需求：

- HTTP Range 流读取和分段缓存。
- WebM/Opus、M4A/AAC 解封装和解码。
- 跨平台音频输出。
- seek、暂停恢复、队列切歌和错误恢复。

如果纯 Rust 路线无法在合理复杂度内稳定覆盖三平台，再评估 libmpv 或 GStreamer 适配器。具体后端必须通过真实 YouTube 音频流验证后决定。

### 5.5 存储

- 使用 SQLite 保存歌曲、歌手、专辑、歌单、队列、历史和缓存索引。
- 初期建立桌面版自己的迁移体系，不直接修改或复用 Android Room 数据库文件。
- 后期以显式导入器支持 Android 备份迁移。
- 数据库和文件 I/O 全部在后台线程执行。
- Cookie/session 只写入平台安全存储（Linux Secret Service、macOS Keychain、Windows Credential Manager），禁止写入 SQLite 或日志；安全存储不可用时保持匿名能力并显示可恢复错误，不回退到明文文件。

## 6. 功能阶段

### 阶段 0：工程基线

目标：形成可持续开发的 GPUI 桌面工程。

- 建立模块目录和基础错误类型。
- 加入日志、配置和资源加载。
- 实现应用 Shell、窗口尺寸、主题和桌面导航。
- 实现 Home/Search/Library/Settings 占位路由。
- 实现底部播放器外观和空状态。
- 建立测试夹具，保存脱敏后的 InnerTube JSON 样本。

验收：

- Linux 下可启动、切换页面和调整窗口尺寸。
- UI 主线程无阻塞。
- `cargo fmt --check`、`cargo check` 和基础测试通过。

### 阶段 1：最小听歌闭环

目标：用户可以搜索并播放一首真实歌曲。

- 实现匿名 InnerTube session。
- 实现搜索和搜索结果解析。
- 显示标题、作者、封面和时长。
- 实现播放地址解析技术验证。
- 实现至少一种主流音频格式的播放。
- 实现播放/暂停、seek、音量、上一首、下一首。
- 实现底部播放器和队列。
- 处理加载、无结果、网络错误和播放错误。

验收：

- 从冷启动完成“输入关键词 → 选择歌曲 → 听到声音”。
- 连续播放多首歌曲时 UI 保持响应。
- URL 过期或 403 时能刷新或给出明确错误。
- 播放状态和界面状态一致。

### 阶段 2：日常可用

目标：形成适合个人日常使用的桌面播放器。

- 首页和探索页。
- 专辑、歌手、歌单详情。
- 自动续播和电台队列。
- 同步歌词和歌词滚动。
- 播放历史、收藏和本地歌单。
- 图片与音频缓存。
- 恢复上次队列和播放位置。
- 代理、音质、缓存路径和主题设置。
- 桌面媒体键与基础通知。

### 阶段 3：账号与同步

- 点击登录与高级 Cookie/session 导入：主入口已按 Android 行为改为隔离系统 WebView 登录，Google 回跳 `music.youtube.com` 后提取、验证并安全保存会话；手工导入保留为高级恢复入口（Linux WebKitGTK 登录页实机渲染已验证，Windows WebView2/macOS WKWebView 原生运行待对应平台验证）。
- YouTube Music 资料库和历史同步（喜欢的歌曲、歌单和播放历史核心读写链路已完成）。
- 喜欢、订阅和在线歌单编辑（核心写链路、乐观状态与失败回滚已完成）。
- 安全凭据存储（平台适配基础与 Linux Secret Service 实机保存/恢复/删除已完成，Windows/macOS 实机验证待完成）。
- 账号失效与重新认证处理（启动探测、强类型失效状态、云端操作隔离、匿名回退、重试验证、替换和删除凭据已完成）。

### 阶段 4：高级能力

- 离线下载（基础纵向切片已完成）。
- 音量标准化、十段均衡器、变速和变调（基础纵向切片已完成；本地与在线 AutoEQ/APO、20 段参数滤波、档案管理和频率响应图也已完成）。
- Last.fm Scrobble（基础纵向切片已完成）。
- Discord Rich Presence（基础纵向切片已完成）。
- 一起听（基础纵向切片已完成并保持维护；作为后续扩展，不占用当前核心功能与 UI 完整性工作的优先级；公网双端互操作和长会话稳定性仍待受控验证）。
- 播客（匿名搜索、节目详情、单集队列/播放/下载、本地优先的节目收藏与 Episodes for Later、登录后写同步、既有远端资料库对账及逐集进度恢复均已完成基础纵向切片）。
- 歌曲识别（PCM 重采样、Shazam 签名、可取消麦克风采集、真实 Shazam 匹配、自动保存识别历史及 YouTube Music 播放/搜索已完成；真实设备权限验收仍待后续）及其他非核心 Android 功能。

## 7. 跨平台要求

### Linux

- 同时支持 Wayland 和 X11。
- 点击登录依赖系统 WebKitGTK 4.1；开发与打包环境必须提供对应头文件和运行库。
- 验证 Vulkan/软件渲染失败时的错误提示。
- 支持常见桌面环境的音频输出与媒体键。

### Windows

- 使用 MSVC 工具链和 Windows SDK。
- 验证窗口、字体、音频设备切换和媒体键。
- 后期提供可携带的安装包或压缩包。

### macOS

- 使用 `font-kit` 保证文字渲染。
- 验证 Apple Silicon，必要时补充 Intel 构建。
- 验证 Metal shader、系统媒体控制和应用签名路径。

平台专用实现必须放在 `cfg(target_os = ...)` 边界内，并为不支持的平台提供清晰的编译期或运行时降级。

## 8. 测试和质量门槛

每个可交付阶段至少执行：

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --no-deps
```

还需覆盖：

- 使用固定 JSON fixture 测试 InnerTube 解析。
- 使用本地音频 fixture 测试解封装、解码和 seek。
- 用模拟服务测试播放状态机和队列。
- 对搜索到真实播放做人工冒烟测试。
- 后续建立 Linux、Windows、macOS CI 构建矩阵。
- 禁止在测试日志中输出 Cookie、PoToken、签名或完整播放 URL。

## 9. 主要风险

| 风险 | 影响 | 应对 |
|---|---|---|
| YouTube renderer 和客户端参数变化 | 搜索或页面解析失效 | 宽松模型、fixture、分层解析、客户端回退 |
| PoToken、签名和 `n` 转换变化 | 无法播放或出现 403 | 独立 resolver、运行时刷新、真实流集成测试 |
| 三平台音频后端差异 | 某平台无声、seek 异常 | 播放 trait、后端验证、设备切换测试 |
| GPUI 仍处于快速演进阶段 | 上游更新导致破坏性改动 | 保留 `Cargo.lock`，升级时单独验证 |
| 一次迁移过多页面 | 长期没有可用成果 | 严格按纵向切片推进 |
| Android schema 直接复用 | 数据损坏或迁移困难 | 桌面独立 schema，后期显式导入 |

## 10. 首个 Goal 建议

建议 Goal 模式先使用以下目标：

> 在 `/home/luren/Code/rust/Metrolist-rs` 中完成 Metrolist GPUI 桌面版，并持续推进真实功能、后端与 UI 纵向切片。以 `/home/luren/Code/Android/Metrolist` 为只读参考；不得修改 Android 仓库。优先保证 Linux 可运行，同时维持 Windows/macOS 的平台边界。每轮只做与当前功能直接相关的格式化和编译检查；不以测试、安全工程或验证设施替代缺失的功能和 UI，完成度以真实后端接通及 Desktop UI 可操作为准。

首个 Goal 的最低完成条件：

1. 桌面应用 Shell 和路由可用。
2. 模块边界和错误处理建立。
3. 搜索接口及解析具备 fixture 测试，或明确记录真实 API 阻塞证据。
4. 播放地址解析和音频后端至少完成一个可运行技术验证。
5. 不破坏当前 GPUI Component 精确提交固定。
6. `cargo fmt --check`、`cargo check`、`cargo test` 通过。

## 11. 非目标

首个 Goal 不要求：

- 完整复刻 Android 的全部页面。
- 账号登录和云端资料库同步。
- Android Auto、Cast、小组件、闹钟或 Quick Settings。
- 一起听、歌曲识别、AI 翻译、均衡器和年度总结。
- 生产级安装包和应用商店发布。

这些能力在最小听歌闭环稳定后按优先级逐步加入。

## 12. 实施进度

截至 2026-08-16，首个 Goal 已启动并完成以下工作：

- 阶段 0 工程基线：建立 `app`、`domain`、`services`、`ui`、配置和统一错误边界；实现可启动的 GPUI 桌面 Shell、首页/探索/搜索/资料库/设置路由、主题切换、播放器空态和日志初始化。侧栏账号卡片现直接反映未登录、登录中、检查中、已登录、凭据过期和故障状态，已移除早期“Desktop preview / Anonymous session / stage 3”占位文案，避免已实现账号功能仍被 UI 误报为未来能力；整张账号卡片可直接进入设置页处理登录或重新认证。
- 阶段 3 点击登录：只读对照 Android `LoginScreen` 与 `AccountSettings` 后，将账号主入口从“粘贴 Cookie 后 Verify and save”改为真正的 `Sign in` 按钮；手工 Cookie/Android session template 仍作为 `Advanced session import` 保留。主进程点击后以同一可执行文件启动独立登录子进程，由 Wry/Tao 使用 Linux WebKitGTK、Windows WebView2 或 macOS WKWebView 展示 Google 登录；WebView 使用临时数据上下文，不把网页登录 Cookie 留在磁盘，只允许 Google/YouTube 受信 HTTPS 导航并拒绝弹窗。回跳 `music.youtube.com` 后读取适用于该 origin 的 Cookie，通过页面 IPC取得 visitor data/data sync ID，经现有 `SAPISID` 结构校验和 `accountInfo()` 网络验证成功后才写入平台凭据库；Cookie 只经父子进程管道传递，stdout 结果有大小边界且缓冲区随后清零，不进入命令行、SQLite 或日志。取消会恢复原账号状态，替换失败不会破坏已有有效会话，登录任务被取消时会终止其子进程；无认证代理会传入 WebView，Wry 不支持的带认证应用代理会给出明确错误而不是泄露代理密码。另修复 Android 实际导出字段 `***DATASYNC ID*** =` 与 Rust 旧拼写不一致导致的高级导入兼容问题，同时保留旧拼写。隔离 Xvfb/X11 验证已实际创建 960×760 的 “Sign in to YouTube Music — Metrolist” 窗口并完整渲染 Google 邮箱/手机号登录页；本轮未输入真实 Google 凭据，因此尚未把该启动证据冒充为真实账号端到端授权、回跳和凭据落库证明，Windows/macOS 原生 WebView 也仍待对应平台验证。
- 阶段 1 搜索切片：接入匿名 YouTube Music InnerTube 搜索及 `music/get_search_suggestions`，使用宽松递归解析转换为稳定 `Song`/`BrowseItem` 模型，不向 UI 暴露 renderer。建议请求与 Android 一致使用 `WEB_REMIX` 且即使账号已登录也不携带 Cookie、Authorization 或 `onBehalfOfUser`；搜索框输入采用 250 ms 防抖和查询代际校验，编辑、提交、切页会取消旧任务，迟到响应不能覆盖新查询。建议面板支持点击文本立即搜索、只填入输入框、直接播放推荐歌曲以及打开推荐专辑/艺术家/歌单等页面，并具有加载、失败、重试和缩略图状态；正式搜索保留加载、空结果、失败、重试、分类结果及 continuation。对照 Android 搜索历史，用户提交的有效查询自动写入 SQLite v21，空输入展示最近历史，输入时展示同前缀最近 3 条，可复搜、填充、单删或确认清空；筛选切换、失败重试和账号客户端刷新不会制造重复记录。
- 阶段 1 播放解析验证：匿名 session 会刷新 visitor data，并使用 visionOS 客户端回退解析播放信息；当前只选择可由纯 Rust 解码栈处理的 AAC-LC/M4A 直链，且不会在调试输出中暴露完整 URL。
- 阶段 1 解码与输出：实现带播放请求头、重定向和 512 KiB 对齐块的 HTTP Range reader，并用真实歌曲验证分段读取、ISO MP4 解封装和至少 4096 帧 AAC 解码。Rodio/CPAL 后端在独立线程中管理默认输出设备，支持加载、播放、暂停、停止、seek、音量和状态快照；静音实播测试已证明系统输出回调会消费真实流，并在 seek 到 30 秒后继续推进。
- 阶段 1 播放可靠性：专用音频线程改为通过惰性工厂创建可注入 `AudioPlayer` 后端，生产环境仍使用相同 Rodio/CPAL 实现。新增 0.5 秒 AAC-LC/M4A 本地 fixture，离线覆盖加载、播放、进度推进、暂停、seek、结束、停止、音量、根因错误保留和新加载恢复；测试不需要网络或输出设备。另以可控虚拟时钟验证 12 小时会话：逐小时推进、暂停不漂移、连续 257 次 seek 的命令顺序、结尾状态和停止清理均保持一致，测试无需真实等待。
- 阶段 2 音频输出设置：通过 Rodio 重导出的 CPAL API 枚举带稳定 `DeviceId` 的输出端点，并在设置页展示当前输出、系统默认项、刷新/切换忙碌态和独立错误状态。设备切换会先建立替代 sink，再恢复当前 source、音量、进度及播放/暂停状态，全部成功后才替换旧后端；失败会回滚并继续旧设备播放。Linux 实机验证还据此过滤了无可用输出配置的 ALSA 虚拟声道、折叠 `hw`/`plughw` 重复别名、标明 USB profile，并在系统默认项不可用时优先回退 PipeWire/PulseAudio。
- 阶段 2 桌面媒体集成：只读参考 Android `MediaLibrarySession` 与低重要性播放通知语义，使用 Souvlaki 建立独立平台适配层（Linux MPRIS、Windows SMTC、macOS Now Playing）。系统播放、暂停、切换、停止、相对/绝对 seek、音量、唤起和退出事件全部回到既有 Shell/队列状态机；标题、歌手、封面、时长、播放态、进度和音量向系统发布，进度限制为至多每秒更新一次，每首歌只异步发送一次基础“正在播放”通知。媒体会话或通知服务不可用时仅记录降级，不影响音频播放；Linux 现在会在启动 Souvlaki 线程前探测 session D-Bus，不可用时直接禁用 MPRIS，避免第三方 zbus 后台线程因连接失败产生 panic。
- 阶段 1 UI 闭环：搜索结果会组成播放队列并接入真实 resolver 和音频后端；底部播放器展示当前歌曲、错误、播放/暂停状态、时间、可拖动 seek 进度和音量。上一首、下一首、任意队列选中、队列侧栏和自动续播已接线。底栏与队列侧栏现均提供 Shuffle 和 Repeat 控制：Repeat 按 Android 的 Off→All→One→Off 顺序循环，One 在自然播完后重播当前项目，All 在队尾回到队首；开启 Shuffle 只随机化当前项目之后尚未播放的部分，保留历史和当前项，Repeat All 开启时新一轮会重新洗牌。房客模式统一锁定这些本地队列控制；自动电台追加的新项目会在 Shuffle 开启时只重排未播放尾部。Repeat 模式跨新队列保留，Shuffle 在显式建立新队列时安全关闭，两者均随播放会话即时持久化；播客单集的重复或回绕不会错误恢复到已保存的末尾位置。队列每项另提供带文字标签和边界禁用态的上移、下移与移除操作；改序始终保持当前歌曲身份，移除当前项会切换到相邻项目，移除最后一项会停止播放并以空会话覆盖旧队列/播放源，避免重启复活。所有编辑即时持久化并由房主状态追踪同步，房客不可操作。首页推荐/最近播放、搜索建议/结果、在线详情与播客单集、可编辑云端歌单、本地收藏/历史/歌单、远端历史和下载列表的全部逐曲入口现统一提供 Next 与 Queue：空队列会直接建立播放，有当前项时 Next 插入当前项之后且不打断播放，Queue 加入队列；显式插入允许重复歌曲，Shuffle 关闭时 Queue 位于末尾，开启时新项目参与尚未播放部分的随机顺序，tooltip 会明确提示这一差异。为避免这些次要动作在 720×520 最小窗口挤压标题，详细歌曲行已改为可换行容器，首页固定宽度卡片也按 gpui-component 的组合方式拆分为元数据区与独立可换行动作区，窄宽度下动作会自动下沉；按钮继续保留文字标签、tooltip 和明确禁用态。
- 阶段 1 缓存与恢复：Range 块以稳定 `video_id + audio quality + content length + byte offset` 落入系统缓存目录，默认容量 512 MiB，完整块才能命中，损坏块会失效，超限时按最近访问时间淘汰旧块；不同音质使用独立 namespace，缓存文件名不含播放签名 URL。同一缓存实例的读取、原子提交和容量淘汰已串行化，写入使用进程内唯一的 `create_new` 临时文件、`fsync` 和 rename，避免并发写同一块或读/淘汰竞态留下半块和临时文件。cache-only 播放会先逐块核对完整资源，完整缓存可在全新进程中断网读取且不发 HTTP 请求，任一缺块则立即请求刷新播放源；已知长度的截断 206 响应会以 `UnexpectedEof` 拒绝且不落盘。播放期间的 HTTP 403/410 和其他 Range 读取失败会从 Rodio/Symphonia 数据源显式回传到 Shell，不再被误判为自然播完；刷新会保留失败位置并重新解析一次 visitor data 和直链，仍失败才保留错误供手动重试。
- 阶段 2 存储基线：建立桌面独立 SQLite v23 schema 和迁移检查，包含歌曲及播客单集身份、播放历史、识曲历史、搜索历史、队列项、单例播放会话及 Shuffle/Repeat 模式、当前播放缓存元数据、收藏、本地歌单、有序歌单项、播客收藏及取消墓碑、Episodes for Later、逐集播放位置、规范化歌词文档/行、单例应用设置、离线下载状态和参数均衡器档案；v1 至 v22 数据库会原位升级并保留已有数据，程序拒绝比当前版本更新的 schema。v18→v19 增加 Repeat/Shuffle 会话状态，v19→v20 增加识曲历史，v20→v21 增加搜索历史，v21→v22 增加本地 Album/Artist 目录，v22→v23 增加真实 song→album 映射。数据库运行在专用线程，通过异步回复与 GPUI 交互，不读取或复用 Android Room 文件。
- 阶段 2 历史、统计与恢复：实际播放满 30 秒后写入本地历史。对照 Android `HistoryScreen`，侧栏提供独立 History 入口：Local 与已登录 YouTube Music 历史可切换并按标题/歌手过滤，本地记录可单删或确认清空，远端按 feedback token 精确删除；两者都复用播放、Next、Queue 和 Download。Library Overview 仍保留历史区，但不再是唯一入口。对照 Android `StatsScreen`，侧栏 Stats 按 7 天、30 天、3/6 个月、1 年或全部时间聚合 `play_history.play_time_ms`，展示总览、Top Songs、Top Artists 和 Top Albums；歌曲接入播放操作，真实歌手/专辑 ID 可进入详情。队列与当前播放状态定期保存；冷启动异步恢复但不会擅自播放。
- 阶段 2 收藏与本地歌单：搜索结果可收藏或取消收藏，并可从侧栏选择目标歌单；资料库支持创建、打开、播放、移除歌曲和删除本地歌单。收藏与歌单均通过专用存储线程持久化，歌单内歌曲保持插入顺序且重复添加幂等。
- 阶段 2 资料库管理：只读对照 Android 的隐私设置、歌单页和 `PlaylistSortType` 语义，新增只清除 `play_history` 的历史清理操作，收藏、歌曲元数据和本地歌单不受影响；清空历史和删除歌单均使用带取消按钮、不可点击背景关闭的危险操作确认框。歌单详情支持预填原名的重命名并校验空名、大小写不敏感重名与已删除歌单；列表支持按创建时间、名称、歌曲数、最近更新排序及升降序切换，默认与 Android 一致为创建时间降序。所有资料库写操作共用明确的忙碌态，失败会保留现有内容并在当前页面回显。收藏、歌单、本地历史、下载、已存播客或 Episodes for Later 的初始读取若有任一分区失败，资料库页现在提供统一恢复入口，只把失败分区切回加载态并分别重读；正常分区不被清空，重试期间的播客写入或远端同步通过修订号阻止旧读取结果覆盖新状态。
- 阶段 2 首页与探索：主页接入真实的最近播放去重列表；冷启动恢复出当前歌曲时会显示继续播放入口，并沿用已保存的播放位置，不会自动出声。匿名 `FEmusic_home` 推荐现按 carousel shelf 展示，支持 chip 筛选、混合歌曲/专辑/歌手/歌单项目、按 shelf 播放、更多详情、continuation 追加、跨页去重和失败原位重试；探索路由通过 `FEmusic_explore` 展示新发行专辑及带原始 `params` 的心情/流派入口。两页复用现有详情和缩略图管线，未知或残缺 renderer 只跳过对应项目。三份固定 Home/continuation/Explore fixture 与真实匿名 Home/Explore 并发请求均已通过。
- 阶段 2 同步歌词：只读对照 Android 的 provider、LRCLIB 匹配和播放器时间轴语义，新增独立 `LyricsClient` 与稳定 `LyricsDocument`/`LyricsLine` 模型。LRCLIB 查询会清理标题噪声、提取主艺术家、依次尝试元数据/标题/自由文本策略，已知时长允许 ±5 秒并优先同步结果；LRC 解析支持 BOM、元数据、正负 offset、百分秒/毫秒、逗号小数、多时间标签和基础 HTML 实体，纯文本结果明确降级。歌词侧栏按需加载，具有加载、无匹配、失败和重试状态；当前行带 100 ms 提前量随播放位置更新并自动滚动，同步行可点击 seek。切歌会取消旧任务并再次核对 `video_id`，旧响应不能覆盖新歌。SQLite v4 规范化缓存同步/纯文本行，重启优先离线命中，手动 Refresh 才绕过缓存；混合、空白或乱序时间线会被拒绝。固定 LRCLIB JSON、解析/匹配/Unicode/竞态/缓存测试和不输出正文的真实 LRCLIB 请求均已通过。
- 阶段 2 在线详情：搜索页增加歌曲、专辑、艺术家和公共歌单四类筛选，保留 `browseId` 并通过统一匿名 `browse` 请求打开详情。专辑曲目、艺术家热门歌曲、在线歌单曲目、描述和关联专辑/歌单均由宽松解析器转换；详情页支持整页或单曲播放、收藏以及加入本地歌单，请求切换和返回会取消过期任务。搜索和详情现可用 `continuation`/`ctoken` 增量加载后续歌曲或目录项，兼容 shelf continuation 与 append action 等响应容器；跨页按 `videoId`/`browseId` 去重，空页、无进展页和重复 token 会停止，追加失败保留已有内容并原位重试。六份目录固定 JSON fixture 与真实三类目录搜索、详情及 continuation 请求均已通过。
- 阶段 2 图片加载：歌曲、专辑、艺术家、歌单详情和底部播放器已显示真实缩略图；每个 URL 使用可取消的 GPUI 后台任务，切换查询、详情或路由时立即丢弃不再可见的请求，加载中和网络/解码失败都有明确占位。解码源在内存中最多保留 256 张；原始图片使用独立的 256 MiB 磁盘 LRU 缓存，文件名只含 URL 的稳定 128 位散列，文档校验格式、长度、内容散列和实际可解码性，损坏时删除并回源。离线测试覆盖冷启动磁盘命中、损坏剔除和容量淘汰，真实 YouTube JPEG 下载与解码也已通过。
- 阶段 2 日常设置：只读对照 Android 的 HTTP/SOCKS 代理及 Auto/Low/High 音质语义，新增强类型 `AppSettings`、SQLite 单例持久化（当前 schema v19）和完整设置页。应用会在任何首页网络请求前读取设置；代理（含遮罩认证字段）统一作用于 InnerTube、LRCLIB、缩略图、音频 Range 和 Last.fm 客户端，固定本地代理测试已证明请求实际经过代理而不只是保存配置。Low 选择不高于 128 kbps 的最佳直连 AAC，High 优先服务端高质量标记，Auto 选择最高码率；切换音质会清除当前安全源元数据、按新音质重新解析并保留进度和播放/暂停状态。缓存根目录、音频容量、音量标准化级别、十段/参数均衡器、播放速度/移调模式、自动电台、YouTube Music 播放历史同步、Last.fm 行为、Discord Rich Presence 开关、“一起听”服务器/显示名/自动审批/房主音量同步和主题均可持久化；自定义根目录同时承载 `audio`/`thumbnails` 子缓存。保存会先验证代理 URL、绝对且非文件系统根的缓存路径、容量、Last.fm 阈值以及无凭据的 `ws://`/`wss://` 房间服务器，再在后台完整构造新服务并写库，任一步失败都保留旧服务；成功后才原子替换，旧音频线程也在后台退出，避免阻塞 GPUI；活动下载期间要求先暂停再应用网络或缓存设置，房间内也禁止切换服务器，避免两个服务实例并发操作同一持久目录或会话。
- 阶段 2 自动电台：只读对照 Android `YouTubeQueue`、`MusicService.startRadioSeamlessly` 和队列恢复语义，匿名调用 InnerTube `next`，完整携带 `videoId`、`playlistId`、`playlistSetVideoId`、`params`、`index` 与 continuation。歌曲电台优先请求 `RDAMVM{videoId}`，空电台回退单 `videoId`，仍无推荐时再使用 Related 页面；每段最多尝试 4 次，并兼容 automix preview、playlist panel continuation、selected index 和残缺 renderer。播放至队尾五首以内会自动补充，跨页按 `videoId` 去重，重复 token、空 token 或续页无进展会停止；新队列和服务切换使用代际号取消旧响应，失败保留现有队列并在队列侧栏提供重试。播放器与队列侧栏均可手动从当前歌曲启动无缝电台，请求成功后才裁掉未来旧项目。SQLite 只持久化已经入队的歌曲，不保存临时 endpoint/continuation，重启恢复后不会擅自播放；这与 Android 将持久化 YouTube 队列降级为普通列表的行为一致。
- 阶段 3 账号基础：只读对照 Android 的 Cookie 模板、`SAPISIDHASH`、`dataSyncId` 和 `account/account_menu` 行为，支持直接 Cookie header 与 Android session 模板导入；输入长度、控制字符和 `SAPISID` 均在联网前校验，SHA-1 签名按请求时间动态生成，临时明文和 session 析构时主动清零，`Debug`、协议错误与测试输出均不暴露 Cookie。普通搜索即使存在 session 仍保持匿名；需要账号的 browse、continuation、next 与账号探测才携带登录头和 `onBehalfOfUser`。导入流程先探测账号、后写系统凭据库，失败保留旧 session；启动会恢复并重新探测，退出先删除系统凭据再切回匿名，并明确保留本地收藏、歌单、历史和缓存。设置页提供遮罩输入、账号头像/名称、重试、替换和带确认的退出操作；凭据库不可用时仅匿名降级，绝不回退到 SQLite 或明文文件。
- 阶段 3 云端资料库与写同步：账号验证成功后并发读取 `FEmusic_liked_videos` 与 `FEmusic_liked_playlists`，每个端点跟随最多 64 页 continuation，按稳定 ID 去重并在重复 token 或无进展时安全停止。资料库页具有未登录、加载、成功、失败和原位重试状态；喜欢的歌曲可直接播放，在线歌单复用既有详情页。只读对照 Android InnerTube 请求后，现已支持歌曲和公共歌单的喜欢/取消喜欢、艺术家订阅/取消订阅，以及私有在线歌单的创建、加歌、按 `setVideoId` 精确移除重复曲目、改名和远端删除。喜欢与订阅是目标状态幂等写入，网络失败、429 或 5xx 最多重试 3 次；创建和歌单编辑等结果不确定的写入只发送一次，并要求刷新确认后再试。UI 对喜欢、订阅、移歌、改名和删除使用乐观状态，失败精确回滚；401/403 会进入账号失效提示。云端数据仍不复制进本地 SQLite，远端删除也明确不影响本地收藏、歌单、历史和缓存。
- 阶段 3 云端播放历史：只读对照 Android `FEmusic_history`、`REMOVE_FROM_HISTORY` feedback 和 30 秒后注册 `videostatsPlaybackUrl` 的行为，认证读取按远端 shelf 标题保留日期分组，并跟随最多 64 页 continuation。同一首歌的不同反馈 token 视为不同播放记录，重放不会被 `videoId` 去重；重复页则按 token 停止。资料库用 Local/YouTube Music 两个来源页签明确隔离：本地清空只删 SQLite，远端单条移除只提交对应 feedback token，失败乐观回滚，任何远端历史都不复制进本地库。累计实际播放 30 秒后仍照常并行写本地历史；登录且持久化开关开启时，另以同一 16 字符 `cpn` 注册一次远端播放，网络失败、429 或 5xx 最多重试 3 次。跟踪 URL 仅接受 HTTPS 的 YouTube playback 精确路径，所有跟踪 URL 与 feedback token 都只驻留内存并在 `Debug` 中遮罩；401/403 进入账号失效提示。匿名真实 `player` 已证明当前 visionOS 响应提供受信跟踪端点，但未使用真实账号执行历史读取、注册或删除，避免未经确认改动用户资料。
- 阶段 3 账号失效恢复：Cookie 导入校验、远端 401/403/无活动账号和系统凭据库故障现分别使用 `Credential`、`SessionExpired` 与 `CredentialStore` 强类型错误。只有持有 session 且账号探测成功的 `SignedIn` 状态可以读取或修改云端资料库、历史、喜欢、订阅和在线歌单；一旦远端拒绝，应用保留系统凭据供重试、替换或显式删除，但立即切换到匿名客户端、关闭云端歌单选择器、切回本地历史并停止远端历史上报。网络等非认证失败仍只作用于当前请求，不会误判为账号过期。设置页为过期状态提供独立说明、重新验证、替换和删除过期 session；退出不会自动删除本地数据。平台凭据测试为每次运行生成独立 service/account，并用清理守卫验证空条目、保存、恢复、删除和再次为空，绝不访问生产条目；Linux Secret Service 实机已通过，Windows Credential Manager 与 macOS Keychain 保留同一测试入口等待对应平台执行。
- 阶段 4 离线下载：只读对照 Android `DownloadUtil`、`ExoDownloadService` 和自动下载语义，建立与 512 MiB 播放 LRU 缓存完全分离的持久下载目录；显式下载不会被缓存淘汰。下载先解析当前音质的短期播放地址，按稳定 `video_id + quality + content length` 身份写入 512 KiB 原子块，可复用播放器缓存，最多并行 3 首，支持排队、暂停、断点续传、失败重试和精确删除。只有全部块齐全、持久进度达到总长度且 AAC/M4A 媒体探测成功后，SQLite 才允许进入 Completed；坏的完整文件会被丢弃，启动会核对磁盘并自动续传此前 Queued/Downloading 项，运行中离线读取失败也会降级为可修复状态。首页、搜索、在线详情、收藏、历史、本地歌单及云端歌单均提供统一下载状态入口，资料库集中展示进度、离线播放、暂停/恢复、Play all 和带确认的删除；删除只影响本机离线副本。离线播放优先命中持久下载，即使播放缓存被淘汰或临时 URL 过期也不发网络请求；下载线程与 GPUI/异步执行器隔离，错误与调试输出不暴露完整 URL。
- 阶段 4 音量标准化：只读对照 Android `VolumeNormalizationAudioProcessor`、`MusicService` 和响度级别偏好，优先读取 `perceptualLoudnessDb`，否则将相对 `loudnessDb` 换算为实测 LUFS；Aggressive/Loud/Balanced/Quiet 分别以 −7/−11/−14/−19 LUFS 为目标，默认启用 Balanced。Rodio 解码样本进入播放器前按 `10^(gain_mB/2000)` 独立增益，目标差值限制在 −15 dB 至 +3 dB，并把输出硬限幅到 −1…1；用户音量仍由独立播放器音量控制。当前增益进入播放快照和底栏提示，设置保存、音频服务重建、输出设备切换、过期源缓存恢复及显式离线下载都会保留响度元数据；SQLite v9 仅保存有界整数响度，不保存临时播放地址。fixture、DSP、seek、下载传递、会话往返和 v8→v9 无损迁移测试已覆盖，真实匿名完整下载也确认当前 visionOS 响应携带响度元数据。
- 阶段 4 均衡器：只读对照 Android `ParametricEQParser`、`EQProfileRepository`、`CustomEqualizerAudioProcessor`、`GitHubAutoEqSearch`、`WizardViewModel`、`EqFrequencyResponseGraph` 与 RBJ Audio EQ Cookbook 算法，保留标准化之后的 31/62/125/250/500 Hz 和 1/2/4/8/16 kHz 十段 peaking biquad；每段允许 −12…+12 dB，提供 Flat/Bass/Vocal/Treble 预设和逐段 1 dB 调整，最高正增益自动转换为负 preamp headroom。高级模式可通过三平台原生文件选择器导入 AutoEQ ParametricEQ/Equalizer APO 文本，解析 `Preamp:` 与 `Filter N: ON PK/LSC/HSC Fc ... Gain ... Q ...`，忽略 OFF 段，拒绝 LPQ/HPQ、畸形单位、非有限数、超过 1 MiB 的文件及超过 20 个启用滤波器；预放大范围为 −50…+50 dB，频率为 `(0, 100000] Hz`，增益为 −30…+30 dB，Q 为 `(0, 20]`，并以定点整数序列化避免跨平台浮点漂移。DSP 对 PK/LSC/HSC 使用 Android 对应的 RBJ 系数，参数档案使用文件显式 preamp，高于 Nyquist 的段运行时跳过；所有滤波器按声道隔离历史，seek、重载和设备切换会清空或重建状态，最终输出限幅。设置页列出档案、段数、preamp 和三类滤波器数量，支持导入、立即选择/禁用、二次确认删除；活动档案必须先禁用再删除。即时选择在音频线程重建同一 mixer 上的链路并保留音量、位置和播放/暂停态，后端或 SQLite 失败会回滚；尚未播放时只延迟保存目标档案，不为设置操作强行打开音频设备。SQLite v16 新增档案表和活动档案快照，v15 升级默认无档案且保留旧十段设置。

  在线向导现使用 Android 相同的 `ndellagrotte/AutoEq` GitHub Tree/Raw 数据源：Tree 响应限制为 32 MiB，只索引 `results/** ParametricEQ.txt` blob，逐路径段编码下载并拒绝绝对路径、反斜杠、空段及 `.`/`..` 穿越；索引、`name_index.tsv` 和参数文件进入自定义缓存根下独立 `autoeq` namespace，以 24 小时 TTL、临时文件完整写入和陈旧有效缓存回退工作。GitHub 的 `truncated` 标志、下载/新鲜/陈旧来源与短 revision 会在界面显示，网络失败不会破坏已有离线索引。模型名按 Android 语义移除括号变体，搜索以精确、前缀、子串和名称顺序返回最多 100 个模型；第二步按 source/rig/form 展示版本并支持多选，目录未知 rig 会按需查询各来源 `measurements/.../name_index.tsv`，选中档案逐个下载解析后通过单次 SQLite 事务原子批量保存，部分下载失败会明确报告，活动档案禁止被远端刷新以保持音频与持久快照一致。频率响应图以和实际 DSP 共用的 RBJ 系数在 48 kHz 下生成 200 个 20 Hz…20 kHz 对数点，使用 2.5 dB 对称量程并显示 100 Hz/1 kHz/10 kHz 网格；Rust 端把显式 preamp 纳入曲线，修复 Android 当前函数接收 preamp 却未计入总响应的显示偏差。解析往返、全部三类频率响应、显式 preamp、Nyquist 过滤、左右声道/seek、20 段与参数边界、路径安全、搜索排序、缓存命中/陈旧回退、档案下载、批量事务、档案 CRUD/活动删除保护、v15→v16、热切换/回滚及无设备延迟应用均有测试；真实 GitHub 完整 Tree 和参数档案烟测也已单独通过。
- 阶段 4 变速与变调：只读对照 Android `TempoPitchDialog`、`SpeedDialog`、`PlayerMenu` 和 `MusicService`，普通模式支持 0.25× 至 2.00×、每次 0.05× 的保调变速及 −12 至 +12 半音独立移调；Varispeed 模式让音高与速度同步，默认仍为普通 1.00×/0 半音。音频链在标准化和均衡器之后使用固定版本的纯 Rust WSOLA 流处理器保留音高，再用采样率映射实现目标移调；默认参数完全绕过 WSOLA，最慢速度与最高升调造成的 0.125 stretch ratio 会拆为两个合法阶段，所有输出再次限幅。媒体时钟独立按原歌曲时间推进，seek 会清空所有 WSOLA 缓冲并重置时钟；设置保存重建服务和输出设备切换都保留歌曲、原始时间位置及播放/暂停状态，底栏显示活动参数。SQLite v11 保存 Varispeed 模式、有界整数千分比速度和半音值，并为 v10 升级提供 Android 默认值；桌面端设置页保存后应用完整参数，因此也会跨重启恢复。播放器底栏现提供与 Android 调整器同范围的即时侧面板，每次增减都通过带回执的音频工作线程重建 DSP 链，旧流在新链加载、seek 和恢复播放态成功前保持可回滚；GPUI 线程不执行解码或设备操作。音频成功后才保存设置，SQLite 写入失败会再切回旧参数；后端无法启动或热切换失败时恢复旧快照、位置和播放态。普通/Varispeed 切换会清除不再适用的旧 transpose，重置恢复 1.00×/0 半音；现有“一起听”协议不携带速度或音高，故任何房间角色都明确锁定此面板，Discord 和房主时钟改读实际播放器快照。时长/频率独立性、Varispeed、声道隔离、seek 确定性、极端两级处理、媒体时钟、即时步进与模式切换、工作线程成功/失败回滚、后端启动失败快照恢复、非法设置回滚、跨重启往返和 v10→v11 迁移均有测试。
- 阶段 4 Last.fm：只读对照 Android `LastFM.kt`、`ScrobbleManager`、设置和 `MusicService` 生命周期，新增官方表单 API 客户端；按参数名排序后拼接共享密钥并计算 MD5 `api_sig`，支持 `auth.getMobileSession`、`track.updateNowPlaying`、`track.scrobble` 以及 love/unlove，并对网络失败、HTTP 429/5xx 和 API 11/16/29 做最多 3 次有界重试。应用 API key/shared secret 只从 `LASTFM_API_KEY` 与 `LASTFM_SHARED_SECRET`（兼容 Android 的 `LASTFM_SECRET`）读取；密码提交后立即清空且不保存，username/session key 仅写系统凭据库，调试输出和协议错误会遮罩密钥。播放追踪以实际 Playing 墙钟累计，暂停不计时、seek 不伪造播放时长、切歌重置；Now Playing 每首一次，Scrobble 在歌曲长于可配最短时长后，于“已播放比例”和最大延迟的较早者触发一次，默认与 Android 一致为 30 秒/50%/180 秒，设置范围为 10…60 秒、30%…95%、30…360 秒。SQLite v12 只持久化三个行为开关与有界整数阈值，并为 v11 升级提供关闭同步的安全默认值；本地收藏成功后可独立同步 love/unlove，Last.fm 失败不会回滚本地收藏。固定 HTTP fixture 覆盖签名、登录表单、瞬态重试、session 失效、accepted scrobble、敏感信息遮罩和追踪器一次性语义；未使用真实用户凭据执行远端写入。
- 阶段 4 Discord Rich Presence：只读对照 Android `DiscordActivityBuilder`、默认模板和 `MusicService` 的切歌/播放态去重，但不复制 Android 通过用户 token 连接 Gateway 的高风险链路。桌面端使用 Discord 官方支持的本机 IPC 无认证模式和 Android 相同的公开 application ID，不请求 Discord OAuth、不读取或保存用户 token，也不发起 Gateway 连接；由于 Rich Presence 会公开到用户资料，SQLite v13 的开关默认关闭并由设置页明确选择。启用后向本机 Discord desktop 发布 Listening 活动、曲名、歌手、HTTPS 封面、按实际播放速度校正的秒级开始/结束时间，以及 YouTube Music/Metrolist 两个按钮；暂停立即移除计时，持续暂停 60 秒、停止、播完、失败或关闭开关时清除活动。独立 IPC 线程与播放器/UI 隔离，Discord 未运行、连接丢失或命令队列繁忙只显示降级状态，播放不受影响；同曲同态去重并每 30 秒进行一次有界重试，应用退出时尽力清除。固定测试覆盖官方 `SET_ACTIVITY` JSON 字段、Listening 类型、秒级时间戳、UTF-8 有界文本、暂停清时钟、30 秒去重/重试、60 秒清除以及 v12→v13 默认关闭迁移；为避免未经确认公开用户活动，没有对真实 Discord 账号执行发布测试。
- 阶段 4 一起听：只读审计 Android `Protocol`、`MessageCodec`、`ListenTogetherClient`、`ListenTogetherManager`、服务器目录及 gitlink 固定的 `metroproto` 提交 `e7c5e3d811af21b66bfe8e88de87777fcde16f90`，按相同字段号手工建立 Prost 消息和二进制 WebSocket envelope；超过 100 字节时仅在 gzip 确实更小时压缩，并对帧、解压大小、队列、文本、标识符、房间号、修订号、位置和音量设置边界。后台有界命令线程惰性连接默认 `wss://metroserverx.meowery.eu/ws`，支持建房/入房、人工或自动审批、房主/房客角色、移除成员、转交房主、十分钟内存 session 重连、15 次有抖动退避、单调 ping 时钟、按 revision 丢弃旧状态、缓冲 ready/complete 屏障、曲目/队列/播放/暂停/seek/可选音量同步、四秒房主心跳及歌曲建议/审批；房主心跳按本地 tempo 修正媒体位置。设置页可完成服务器与显示名配置、八位房间码操作、连接状态、成员管理、申请与建议审批；房客的底栏、队列、歌词 seek、自动电台、媒体键和自动切歌入口统一锁定，只由房主协议事件驱动。服务器只收到歌曲元数据与时序，不接收 Cookie、播放 URL、缓存或音频；入站封面拒绝本机/IP 主机并只保留常见 YouTube 图片 CDN，session token 不落库。SQLite v14 只保存五个非敏感偏好并有 v13→v14 默认迁移测试；本机回环测试已用真实 WebSocket 握手验证建房、申请、审批和成员事件。Rust `TrackInfo` 现以向后兼容的 protobuf tag 8 携带 `is_episode`；鉴于固定 Android 协议及当前 Go 服务端只有前七个字段且服务端解码/重编码会丢弃未知字段，发送端还把类型写入受信 YouTube 缩略图 URL 的 fragment，接收端验证 CDN 后移除该标记并恢复当前曲目、队列及建议中的单集语义；无封面单集使用按稳定视频 ID 构造的安全 CDN 占位地址。固定测试模拟旧服务端丢弃 tag 8，证明 Rust 房主到 Rust 房客仍保持单集类型和非音乐隔离；旧 Android 客户端会忽略扩展字段，仍需上游协议/客户端采用该字段才能获得跨客户端单集语义。尚未在默认公网服务器上创建真实房间，也未做两台真实客户端和十分钟以上断线/缓冲抖动验证，避免未经确认写入外部服务，不能把本地协议测试等同于公网互操作证明。该功能现按后续扩展处理：保留现有基础行为、UI 和回归测试，不继续挤占搜索、播放、账号、资料库、下载及桌面适配等核心能力的实现与验证优先级。
- 阶段 4 播客：只读对照 Android `PodcastPage`、`OnlinePodcastViewModel`、`LibraryPodcastsScreen`、`SyncUtils`、`MusicService`、`EpisodeItem`、`PodcastEntity`、`SongEntity.isEpisode/playbackPosition` 及搜索筛选值，在统一目录模型中增加 Podcast 类型和 Podcasts/Episodes 两个匿名筛选，解析节目详情的 `musicMultiRowListItemRenderer` 单集并复用现有队列、播放源解析、本地收藏/歌单及离线下载。当前服务端会对部分匿名 WEB_REMIX 会话把 Android 的 Episode 筛选 token 解释为 Profiles；客户端会先验证响应是否真为单集，失败时只追加一次未筛选请求，并依据 `PODCAST_EPISODE`、Episode 标签、非音乐音轨页或播客详情链接筛出可播放单集，绝不把频道资料当作单集。`Song.is_episode` 经 SQLite v15 持久化，v14→v15 让既有歌曲安全默认为 false；单集不会请求歌词或自动电台，也不会触发 Last.fm now-playing/scrobble/love 或普通 YouTube Music 歌曲喜欢/加歌单写入。

  SQLite v18 新增独立的节目收藏、节目取消墓碑、Episodes for Later 和逐集位置表；保存节目或单集始终先更新本地，所以未登录也可用，资料库集中展示节目与单集并支持打开、播放、下载和移除。账号已验证时，节目按 Android 语义移除 `MPSP` 前缀后调用 `likePlaylist`，单集保存写入 `SE`，移除前读取并完整翻页 `VLSE` 取得精确 `setVideoId`；远端失败不会回滚已完成的本地操作，并在当前页面明确提示。登录验证成功及手动刷新时会同时读取 `FEmusic_library_non_music_audio_list`、`FEmusic_library_non_music_audio_channels_list` 和完整翻页的 `VLSE`；只有三个端点全部成功才以一次 SQLite 事务执行服务端优先对账，导入或更新远端项目、清理远端已不存在的本地保存状态，同时保留逐集播放位置。节目取消墓碑会阻止最近在本机移除的节目被旧远端状态重新加入，用户显式重新收藏时才清除墓碑。播放器只在单集实际位置达到 3 秒后保存，每 15 秒播放中、暂停、播完及切换时写入；重新选择同一单集会先异步读取位置并核对队列代际，过期查询不能跳转新的当前项目，冷启动的当前会话位置仍具有优先语义。固定搜索/详情 fixture、筛选回退、Last.fm 隔离、认证请求形状、三端点认证快照、本地收藏/稍后列表/位置独立性、墓碑、原子失败、v17→v18 迁移及“一起听”旧服务端兼容标记均已通过；真实匿名测试已成功完成播客搜索、节目详情、单集搜索和实际音频播放源解析。为避免未经确认读取或修改用户资料，本轮没有使用真实账号验证远端播客对账，也没有创建公网房间；Rust 客户端之间已能携带单集类型，但固定 Android 客户端尚未采用扩展字段，因此不能据此宣称所有 Android 跨客户端播客场景均已覆盖。
- 阶段 4 歌曲识别纵向切片：只读对照 Android `MusicRecognitionService`、`RecognitionScreen`、`RecognitionHistoryScreen`、`AudioResampler`、`VibraSignature`、纯 Kotlin `ShazamSignatureGenerator` 与 ShazamKit 请求，移植 12 秒可取消麦克风采集、16 kHz 重采样和 `data:audio/vnd.shazam.sig` 生成，并接入 Android 同 `amp.shazam.com/discovery/v5` 匹配请求。UI 区分 Listening、Matching、Match、No match、取消及麦克风/网络失败，Match 展示标题、歌手、专辑、流派和封面，并可直接播放关联 YouTube 视频或发起 YouTube Music 搜索。每次 Match 自动写入 SQLite v20；Recognize 页可进入 History，查看封面、歌曲、歌手和识别时间，重新搜索，并经确认单删或清空。代码链路已完成，仍待真实 Linux/Windows/macOS 输入设备点击验收。
- 依赖约束：保留 `gpui`、`gpui_platform`、`gpui-component` 的精确 Git revision，并让资源 crate 使用相同 revision。

已执行的质量门槛：

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets --all-features --no-fail-fast
cargo clippy --all-targets --all-features -- -D warnings
cargo test anonymous_live_search_returns_real_songs -- --ignored --nocapture
cargo test services::innertube::tests::anonymous_live_search_suggestions_return_supported_models --lib -- --ignored --exact --nocapture
cargo test anonymous_live_catalog_filters_open_pages_with_playable_tracks -- --ignored --nocapture
cargo test anonymous_live_home_and_explore_return_shelves -- --ignored --nocapture
cargo test anonymous_live_radio_returns_recommendations_and_can_continue -- --ignored --nocapture
cargo test anonymous_live_podcast_and_episode_filters_open_playable_catalog -- --ignored --nocapture
cargo test live_lrclib_returns_timed_lyrics_without_exposing_their_text -- --ignored --nocapture
cargo test live_youtube_thumbnail_downloads_and_decodes -- --ignored --nocapture
cargo test live_player_source_ranges_and_decodes_as_aac -- --ignored --nocapture
cargo test live_audio_backend_advances_playback_position -- --ignored --nocapture
cargo test live_audio_output_switch_preserves_playback -- --ignored --nocapture
cargo test real_system_credential_store_round_trip_uses_isolated_entry -- --ignored --nocapture
cargo test live_audio_download_completes_in_isolated_storage -- --ignored --nocapture
cargo test live_github_index_and_parametric_profile_smoke_test --lib -- --ignored --nocapture
cargo build --bin metrolist
cargo run --bin metrolist
```

常规测试共 231 项通过，14 项显式标记为依赖真实平台凭据库、真实网络或真实音频设备的忽略测试；新增搜索建议测试覆盖宽松查询/推荐项解析、空白与控制字符前置拒绝、Android 请求体、已登录状态仍保持匿名和查询代际防旧响应，真实匿名建议测试已显式执行并通过。新增队列模式测试覆盖 Android 顺序的 Repeat 状态循环、Shuffle 对当前项/历史/完整集合的保持、自动电台尾部洗牌、Off/All/One 的自然播完动作以及会话持久化；v18→v19 回归验证旧会话安全默认关闭两种模式。新增队列编辑测试覆盖向前/向后改序时保持当前歌曲、越界拒绝、删除当前/前项/尾项后的有效相邻选择，以及空会话覆盖旧队列和播放源。显式队列插入测试另验证 Next 精确落在当前项之后、保持当前歌曲、允许用户主动加入重复曲目，并在空队列中建立首项。新增本地资料库恢复状态测试验证只选择失败分区且不把正在加载或已经正常的数据误判为重试目标。账号与云端测试现同时覆盖点击登录 helper 的受信导航边界、精确 `music.youtube.com` 完成条件、`SAPISID` Cookie 要求、session payload 二次校验、取消结果、无密代理端点及错误脱敏，以及 Android/直接 Cookie 解析与拒绝规则、真实 Android `DATASYNC ID` 拼写、旧拼写兼容、确定性 `SAPISIDHASH`、存储序列化/调试脱敏、认证 continuation、资料库和在线歌单写入、历史 feedback、幂等重试、强类型 session 失效、乐观回滚以及远端历史分组/重放语义。Last.fm 测试覆盖 Android 默认阈值和边界、排序签名、表单编码、登录/session 解码、API/HTTP 瞬态重试、强类型失效、敏感信息遮罩、暂停安全计时、每首一次事件和播客单集拒绝；Discord 测试覆盖官方 RPC 活动 JSON、Listening 类型、秒级时间戳、按钮/封面、UTF-8 文本边界、30 秒去重/重试、暂停清时钟及 60 秒清除。“一起听”相关 17 项测试覆盖 Android wire tag、gzip 往返和上限、输入与入站封面边界、单调服务器时钟、revision 顺序、播放动作往返、房主曲目/播放/队列/seek/音量/心跳追踪、房客追踪器隔离、房间状态一致性、正式单集字段、旧服务端未知字段丢弃后的兼容恢复、房客队列单集保真、本机真实 WebSocket 建房/审批往返以及 v13→v14 无 session secret 迁移。播客测试覆盖脱敏搜索/详情 fixture、服务端筛选漂移后的匿名回退、单集类型持久化、节目 `MPSP` 归一化与 `SE` 请求形状、三个认证资料库端点的合并快照、本地节目收藏/Episodes for Later/逐集位置的独立持久化和 3 秒阈值、服务端优先清理、取消墓碑、原子失败、非音乐集成隔离，以及真实匿名节目、单集和音频源解析；歌曲识别测试只使用合成 PCM，覆盖重采样、签名结构、CRC、确定性峰值、12 秒录音形状、交错多声道混音/截断、采集时长边界、可注入取消与敏感调试遮罩，不访问麦克风或外部识别服务；macOS bundle 测试验证 `NSMicrophoneUsageDescription` 已接入清单且文案没有超出实际本地处理范围。隔离 Linux Secret Service 测试使用进程唯一 service/account 完成空条目、保存、恢复、删除和再次为空的往返，生产条目未被访问。存储测试覆盖 v19 建库、v18→v19、v17→v18、v16→v17、v15→v16、v14→v15、v13→v14、v12→v13、v11→v12、v10→v11、v9→v10、v8→v9 与 v7→v8 无损迁移，历史/收藏/歌单、播客本地状态和远端对账、下载状态约束、设置跨重启替换和非法写入回滚、参数均衡器档案 CRUD/活动删除保护、AutoEQ 批量事务、敏感字段约束以及歌词缓存；Discord 开关迁移默认关闭，“一起听”迁移采用 Android 默认服务器、关闭自动审批、开启房主音量同步且没有 token 列，既有歌曲升级后默认不是播客单集。缓存、下载和播放测试继续覆盖并发原子块、LRU 隔离、断点续传、媒体探测、响度/十段与参数 EQ/WSOLA DSP、seek、设备切换、过期源离线恢复、Range 错误、队列电台、Shuffle/Repeat 队尾行为与会话恢复、即时速度/音高和均衡器热切换与回滚、无设备延迟应用，以及 12 小时虚拟会话。AutoEQ/APO 测试另覆盖规范文本往返、OFF 段、PK/LSC/HSC 响应、显式 preamp、Nyquist 过滤、文件/数值/20 段边界、不支持滤波器拒绝、GitHub Tree 路径解析与编码、防穿越、模型归一化与排序、24 小时缓存命中、陈旧回退、远端文件缓存和 name index rig 解析；真实 GitHub Tree 与一个 ParametricEQ 档案的忽略烟测已在本轮显式执行并通过。目录、Home/Explore、电台、播客、歌词、缩略图、代理及桌面媒体适配器的 fixture、真实网络和 Linux 实机证据保持通过，真实请求和日志不输出 Cookie、歌词正文、反馈 token、跟踪 URL 或完整播放 URL。近期构建已在独立 XDG 数据/缓存/配置目录、无凭据 D-Bus 和 Xvfb/X11 中分别运行至 8 秒和 6 秒预设超时；本轮搜索建议改动后的隔离 X11 进程也成功建立 1180×760 主窗口并维持运行。本轮 Shuffle/Repeat 改动后的隔离冷启动进程也保持运行至 8 秒预设超时；新增队列编辑控件、Next/Queue 主要入口及歌曲行动作自适应换行后的三次隔离 GPUI 进程均继续保持运行至各自 6 秒预设超时，日志未出现 panic、fatal 或应用级 ERROR；隔离 SQLite schema version 为 19、`PRAGMA integrity_check` 为 `ok`，`playback_session.repeat_mode` 与 `shuffle_enabled` 均存在且旧库默认值为 0，`podcast_subscription`、`podcast_subscription_tombstone`、`episode_for_later` 和 `episode_playback_position` 四张表仍存在。无凭据 D-Bus 现在会在创建 MPRIS 线程前被探测并明确降级，日志不再出现 zbus 后台线程 panic，应用主进程保持运行；新增侧栏交互和资料库恢复状态已进入实际 GPUI 渲染路径且未引入启动异常。Xvfb 缺少 DRI3/Vulkan presentation，因此既有 GPUI 主窗口证据只覆盖窗口生命周期、数据迁移和降级路径，本轮黑帧同样不能冒充搜索建议、Shuffle/Repeat、队列编辑、Next/Queue 或窄窗动作换行的像素级 UI/点击验收；账号登录 helper 使用 GTK/WebKit 独立渲染，已另外取得 Google 登录页像素证据且 stderr 只有 Xvfb 缺少 DRI3 的 EGL 警告。此前含新增识别路由、即时参数面板和 AutoEQ 向导的隔离启动证据也继续成立，未读取真实系统凭据、未点击识别按钮、未打开麦克风。所有临时探针、截图与 XDG 目录随后均被精确删除，Android 参考仓库保持干净。Windows 目标静态检查已推进到依赖 C 构建阶段，当前 Linux 主机缺少 MSVC/Windows SDK 的 `windows.h`；Windows WebView2/凭据库原生验证与 macOS WKWebView/Keychain/原生构建仍需在对应 CI 或实机完成。

本轮补充证据：逐项审计所有 `play_song_collection` 调用后，确认首页推荐与最近播放是仅有的两个未提供 Next/Queue 的逐曲入口，并已补齐；集合级 Play/Play all 保持建立整组队列的独立语义。首页歌曲及目录卡片现拆分为元数据区与动作区，动作区允许换行，Shuffle 开启时 Queue tooltip 不再错误承诺固定队尾。`cargo fmt --all -- --check`、`cargo check --all-targets`、`cargo test --all-targets --all-features --no-fail-fast`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo build --release --bin metrolist` 与 `git diff --check` 均通过；常规结果仍为 231 项通过、14 项忽略。重建 release 在全新 XDG 目录和强制 X11 的 Xvfb 中保持运行至 8 秒预设超时，未出现 panic、fatal 或应用级 ERROR；迁移版本为 19 且 `PRAGMA integrity_check` 为 `ok`。该隔离环境没有可用 PulseAudio/ALSA 输出，CPAL 打印了虚拟设备时间戳诊断但没有终止应用；Xvfb 仍缺少 DRI3/Vulkan presentation，所以这项证据只证明冷启动、网络首页数据路径、迁移和进程生命周期，不能替代真实桌面的像素级布局、点击或音频验收。

完整播放器移植进度：底栏当前歌曲现可展开完整播放器并返回；封面、标题、作者、真实播放控制、
进度和音量均接入现有播放器状态。Up next、Lyrics、Related 三个标签分别复用现有队列、歌词和
Radio 后端，支持队列选择/改序/移除/清空、歌词加载/重试/同步行 seek，以及相关推荐的
Play/Next/Queue；当前歌曲还可直接 Favorite、加入本地歌单、YT Like、加入云端歌单、订阅首位
真实 Artist，并打开真实 Artist/Album 详情，订阅失败会回滚详情与 Library Artists。切歌时歌词和
Related 状态按当前 `video_id`/Radio seed 隔离。代码闭环已完成，
仍待一次真实桌面人工点击验收，因此不标记为“核心 UI 闭环完成”。

Explore 对照 Android 补齐 Charts/Trending：桌面端现与 `ExploreScreen.kt` 一样并行请求
`FEmusic_charts` 和 `FEmusic_explore`，在新发行与 Moods & genres 之前展示真实榜单歌曲；
榜单复用现有封面、播放队列、Next、Queue、Download 和详情入口，不另建重复后端。

Explore 的 New releases 标题现保留服务端 More endpoint 并提供 View all；点击进入既有 browse
详情状态机，完整专辑网格、加载/空数据/失败状态及 continuation 均复用真实后端，单张专辑仍进入
原有专辑详情，不新增平行页面或数据源。

Home 对照 Android 补齐 Account Playlists：已登录时复用现有云端资料库快照，在 Home 直接展示
账号头像、名称和真实 YouTube Music 歌单；点击进入既有在线 browse 详情，Back 返回 Home。
未登录、云端加载或失败不会阻塞匿名推荐内容，也没有复制新的账号或存储后端。

Home 对照 Android 补齐 Forgotten favorites：SQLite 直接基于既有 `play_history`，以 30 天为
新旧窗口筛选旧累计播放时间超过近期五倍的歌曲；Home 有结果时展示整组 Play all 及逐曲
Play、Next、Queue、Download。写入新历史后会重新查询，清空历史则同步清空该区块。

Home 对照 Android 补齐 Keep listening：SQLite 按最近 14 天 `play_time_ms` 聚合歌曲，跳过
最高频的前 5 首并取后续 15 首；Home 复用已有歌曲卡片接入 Play、Next、Queue、Download。
新历史和清空历史会同步刷新该区块。Quick picks 依赖 Android 的 `related_song_map`，Rust 尚无
该映射，因此没有为本切片新建重复推荐存储。

Home 对照 Android 补齐 Daily Discover：从本地收藏中最多选择 5 首非播客种子，并行复用现有
Radio/Related 后端为每个种子选择一首不同推荐；Home 展示对应的“Because you listen to …”
说明，以及 Play all、Play、Next、Queue、Download。收藏变化会重新加载，无收藏或无推荐时
不阻塞其他 Home 内容。

Home 对照 Android 补齐 Quick picks：直接组合 Daily Discover 的实时相似推荐、Forgotten
favorites、Keep listening 与最近播放，按 `video_id` 去重并限制为 20 首；Home 提供 Play all
及逐曲 Play、Next、Queue、Download。该区块随已有 Shell 状态更新，不新增影子推荐表。

Search 对照 Android 补齐 Local 来源：Search 现在可在 YouTube Music 与 On this device 间明确
切换；Local 模式不会发网络建议或搜索请求，而是实时聚合本地收藏、最近历史、下载、Episodes
for Later，并搜索本地歌单。All/Songs/Playlists 过滤器、加载/错误/空状态均在同一页面可见；
歌曲进入真实结果队列并保留 Next、Queue、Download 等操作，歌单打开既有本地详情。

Search 对照 Android 补齐 YouTube URL 直达：支持 `youtube.com/watch`、`youtu.be`、Shorts、
普通 playlist，以及 `music.youtube.com` 的 playlist、channel 与 `browse/MPRE…` 链接。Video
先通过现有 Radio 链路取得真实歌曲元数据再播放，不使用占位标题；Playlist/Album/Artist
进入既有 browse 详情。已识别链接不再作为普通关键词请求建议或结果。

Search 对照 Android 补齐本地搜索历史：Enter、Search 和建议提交会保存有效查询，空输入展示
最近历史，输入时展示同前缀最近 3 条；历史可立即复搜、填回输入框、单删或经确认清空。
筛选切换、失败重试及账号/设置导致的内部刷新只重跑现有请求，不会污染历史。

Local Search 现对照 Android 补齐 Albums / Artists：提供 All / Songs / Albums / Artists /
Playlists 五类筛选，All 每类预览 3 条；本地歌曲携带的真实 artist ID 可直接形成 Artist 结果，
搜索、首页、探索和云端资料库已经返回的真实 Album / Artist 目录会写入 SQLite v22 并跨启动
保留。点击复用现有 browse 详情；Song 尚无专辑关系，因此不根据标题猜测本地专辑。

主导航对照 Android 补齐 Stats：时间范围切换直接查询本地实际播放事件，总览与歌曲/歌手/专辑榜
均来自累计播放时长；Top Songs 可整组或随机播放，并复用逐曲队列和下载动作。InnerTube renderer
明确提供或 Album 详情上下文确定的真实专辑身份会随 Song 写入 SQLite v23 song→album 映射；
Top Albums 据此聚合次数与时长、显示封面并进入现有 Album 详情，不根据标题猜测专辑。

主导航对照 Android 补齐独立 History：不再要求先进入 Library；Local 与 YouTube Music
历史共享标题/歌手过滤，保留播放、Next、Queue 和 Download。本地 SQLite 事件支持单条删除
及确认清空，远端继续使用服务端 feedback token 精确移除，不复制到本地数据库。

Library 对照 Android 补齐云端 Albums / Artists：账号资料库现在与 liked songs、playlists 一起
并行加载 `FEmusic_liked_albums` 和 `FEmusic_library_corpus_artists`，显示真实数量、缩略图与空数据
状态；点击专辑或艺术家复用既有 browse 详情，不新增影子存储或平行详情页。

Library 顶部现对照 `LibraryScreen.kt` 提供 Overview / Playlists / Songs / Albums / Artists /
Podcasts 分类按钮；分类只组织现有云端与本地真实状态，详情返回时保留当前分类。Overview 继续
集中展示下载、历史等桌面已有入口，没有为筛选复制请求或存储。

Library Songs 现对照 `LibrarySongsScreen.kt` 区分四个真实来源：Liked 从 `LM` 完整歌单加载，
Library 从 `FEmusic_liked_videos` 加载，Uploaded 精确解析
`FEmusic_library_privately_owned_tracks` 的歌曲标签页，Downloaded 只展示本地完整下载。Songs
分类可按标题/歌手过滤，并按来源顺序、标题、歌手或本地播放时长升降序排列；Play all、Shuffle
与逐曲动作全部复用现有播放、队列及下载链路。

Library Playlists 现对照 `LibraryPlaylistsScreen.kt` 增加统一搜索，同时过滤已有本地与云端真实歌单；
Liked、Offline、My Top、Uploaded 快捷入口分别切换到现有 Library Songs 或 Stats 数据源，保留本地
创建/排序/详情和云端打开/移除/改名/删除。当前播放缓存没有可枚举的真实歌曲集合，因此不伪造
Android Cached 自动歌单。

Library Albums / Artists 现继续对照 Android 的来源切换：Albums 提供 Liked / Library /
Uploaded，Artists 提供 Liked / Library。Liked 与订阅列表来自现有账号端点，Uploaded Albums
来自 `FEmusic_library_privately_owned_releases`，Library 使用 SQLite v22 已持久化的真实目录并
合并本地歌曲携带的 Artist ID；两页均可按标题/详情过滤和升降序排列，点击复用真实 browse 详情。

Album 详情现对照 Android 从 browse 响应的 canonical URL 或播放端点提取真实 playlist ID；登录后
显示 Save/Remove，复用 YouTube Music playlist like/removelike 后端并乐观同步 Library Albums，
失败时回滚并显示现有云端错误。没有真实 playlist ID 的 Album 不显示伪收藏入口。

Library Podcasts 现对照 `LibraryPodcastsScreen.kt` 提供 Episodes / Channels / Downloaded：
Episodes 保留本地保存节目、Episodes for Later，并可打开账号的 RDPN New episodes 与 SE；
Channels 按真实 channel ID 去重并进入 Artist 详情；Downloaded 仅展示已完整落盘且标记为播客
单集的歌曲，支持来源顺序、标题、作者或本地播放时长排序以及 Shuffle。

Album / Playlist 在线详情现补齐整组 Shuffle 与 Download all：Shuffle 使用完整真实歌曲集合
建立随机队列并立即播放，Download all 逐曲进入既有持久下载队列及三路并发调度。Category
详情有专辑目录时不再误报“无可播放曲目”，而是直接展示可打开的 Albums。

Online Search 现增加 Android `FILTER_VIDEO` 对应的 Videos 分类；点击后发送原始 InnerTube 参数，
结果作为可播放视频歌曲进入既有分页、Play、Next、Queue、Download 与本地收藏链路。

Online Search 现与 Android 一样默认选择 All：请求不携带筛选 params，现有混合解析同时展示
歌曲与 Album/Artist/Playlist 等目录，分别进入真实播放队列或 browse 详情；其他筛选保持可切换。

Online Search 继续补齐 Android 的 Featured playlists 与 Profiles 原始筛选参数；既有 Playlists
明确对应 Community playlists。三类目录结果均复用当前 continuation 与 browse 详情，不复制页面。

下一纵向切片按以下顺序推进：

1. 继续以搜索、播放、账号、资料库、下载和设置为核心，优先修复状态与 UI 不一致、平台服务不可用时的异常路径，并在具备 DRI3/Vulkan 与真实音频输出的 Linux 桌面完成页面切换、窗口缩放、搜索到播放、队列/seek 和恢复流程的人工冒烟验证；由用户亲自操作新增登录窗口完成一次受控真实账号回跳、`accountInfo()` 校验、凭据落库、重启恢复和退出登录，自动化过程不得代填或记录 Google 凭据。
2. 在 Windows Credential Manager/WebView2 与 macOS Keychain/WKWebView 上执行现有隔离凭据往返和点击登录窗口测试，并补齐两端原生构建、媒体集成和下载目录行为验证；这些是当前跨平台核心完整性的主要外部缺口。
3. 播客匿名及本地核心链路已完成；在获得明确授权后，使用受控真实账号验证远端资料库读取、写同步和逐集恢复。歌曲识别及历史 UI 已接入 Android 同 Shazam 链路，下一步只需在真实输入设备验收权限、取消、Match/No match、历史保存与播放/搜索。
4. “一起听”保持现有基础核心行为和 UI 回归，不作为当前推进项；公网双端、长会话及 Android 跨客户端扩展验证统一留到上述核心能力和跨平台验证完成之后。
