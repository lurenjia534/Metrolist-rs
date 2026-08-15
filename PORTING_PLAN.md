# Metrolist GPUI 桌面版移植计划

## 1. 项目目标

在 `/home/luren/Code/rust/Metrolist-rs` 中实现一个面向 Linux、Windows 和 macOS 的 Metrolist 桌面客户端，使用 Rust、GPUI 和 GPUI Component 构建界面，满足日常搜索和播放 YouTube Music 的需求。

Android 原项目 `/home/luren/Code/Android/Metrolist` 只作为业务行为、协议模型和交互设计参考，不在移植过程中修改。

首要原则是先完成可用的听歌闭环，再逐步追求 Android 版本的功能覆盖：

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
- 凭据和 Cookie 后期接入平台安全存储，禁止以明文写入日志。

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

- Cookie 登录/session 导入。
- YouTube Music 资料库和历史同步。
- 喜欢、订阅和在线歌单编辑。
- 安全凭据存储。
- 账号失效与重新认证处理。

### 阶段 4：高级能力

- 离线下载。
- 音量标准化、均衡器、变速和变调。
- Last.fm Scrobble。
- Discord Rich Presence。
- 一起听。
- 歌曲识别、播客和其他非核心 Android 功能。

## 7. 跨平台要求

### Linux

- 同时支持 Wayland 和 X11。
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

> 在 `/home/luren/Code/rust/Metrolist-rs` 中完成 Metrolist GPUI 桌面版的阶段 0 工程基线，并尽可能推进阶段 1 的真实搜索到播放纵向切片。以 `/home/luren/Code/Android/Metrolist` 为只读参考；不得修改 Android 仓库。优先保证 Linux 可运行，同时维持 Windows/macOS 的平台边界。所有实现必须经过格式化、编译、测试和实际启动验证；遇到播放地址解析或音频后端不确定性时，先构建最小技术验证并用证据决定方案，不用静态假数据掩盖未完成的核心链路。

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
