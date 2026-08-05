# pan-tui Provider TUI 设计文稿

- 文档版本：v4.0
- 状态：Provider TUI、Google Installed App OAuth、SQLite runtime、daemon/IPC 与通知核心已实现；真实验收和 Settings 收尾仍在进行
- 当前实现基线：Phase 20，Google/Aliyun Provider TUI；长期运行能力按 Phase 21–26 交付
- 日期：2026-08-05
- 语言规则：设计与计划文稿使用中文；README、Rust 代码及程序界面使用英文

## 1. 设计审阅结论

本版合并 Provider、Tab/Panel 和并行下载三部分设计，并修正旧设计中的概念冲突：

1. `Files / Logs / Settings` 是顶层 Tab，不再用一个 `ContentView` 同时表达页面和焦点。
2. `[` / `]` 只切换顶层 Tab；`Tab` / `Shift-Tab` 只切换当前 Tab 内的上、下 Panel。
3. Files 页的下方 Panel 必须显示全部并行 Transfer，不能只显示“最近更新的单个文件”。
4. Provider 使用编译期注册器和统一运行时接口；第一批实现为 Google Drive 与阿里云盘。
5. 递归计划、并发队列、本地路径、`.part`、日志和进度属于公共核心，不属于 Provider。
6. 配置分为全局公共配置和 Provider 私有配置。核心不解析 Google 或阿里云盘专属字段。
7. 每个 Provider、Profile 和 Source 保存独立浏览会话；切换后不得丢失目录栈和选中项。
8. 单个 Provider 配置或连接失败不得阻止其他 Provider 进入 TUI。

本文描述的是目标行为。README 在目标实现完成前继续描述当前可用行为，避免把设计当成已交付功能。

## 2. 目标与非目标

### 2.1 目标

1. 提供从网盘逐层浏览并下载到本地的统一 TUI。
2. 首批支持 Google Drive 和阿里云盘，且核心模块不依赖任何一个 Provider 的 API 类型。
3. 支持多个保存的 Source，并允许在 Provider/Source 之间切换而不丢失浏览状态。
4. 文件夹确认后递归建立下载计划；普通文件建立单文件计划。
5. 单个下载 Job 内允许多个文件并行传输，并在 TUI 中逐行展示每个 Transfer。
6. 保持远端相对目录结构，默认下载到 `~/Downloads`。
7. 保留 `.part` 续传、安全路径、原子完成、日志轮转和失败可见等现有语义。
8. Provider 配置、认证材料和可变 token 状态相互隔离，并按最小权限保存。

### 2.2 非目标

1. 第一版不实现改名、移动、复制、权限修改或文件多选；Provider 接口保留创建目录、上传、删除和异步任务能力，具体 UI 可按阶段启用。
2. TUI 不作为常驻进程；多个 Job 由独立 DownloadManager 或可选 daemon 持有。
3. 第一版不加载动态库或第三方运行时插件；Provider 在编译时注册。
4. 第一版不要求为不同 Provider 伪造完全相同的高级能力；不支持的能力必须显式显示原因。
5. 第一版不自动改写或删除旧配置文件。
6. Google Docs/Sheets/Slides 等需要导出的对象仍可由 Google Provider 标记为不支持下载。

## 3. 统一概念

| 概念 | 定义 |
|---|---|
| Tab | 顶层工作页面：`Files`、`Logs`、`Settings` |
| Panel | 一个 Tab 内可获得键盘焦点的上、下区域 |
| Provider | 网盘实现类型，例如 `google`、`aliyun` |
| Profile | 某 Provider 的一个认证实例；第一版每个 Provider 默认一个 Profile |
| Source | Profile 下保存的远端入口，例如共享文件夹或指定目录 |
| ProviderSession | Provider/Profile/Source 的连接状态、目录栈和选择状态 |
| DownloadJob | 用户对一个高亮文件或文件夹提交的一次下载 |
| Transfer | DownloadJob 中一个可下载普通文件的传输状态 |

稳定身份使用组合键，不使用显示名称：

```text
ProviderId + ProfileId + SourceId + RemoteObjectId
```

`RemoteObjectId` 对核心是不可解释的字符串。Google resource key、阿里云盘 drive/file id 等细节由
Provider 自己编码、解析或重新获取。

## 4. TUI 交互

### 4.1 全局键位层级

| 键位 | 行为 |
|---|---|
| `[` / `]` | 切换上一个/下一个顶层 Tab |
| `Tab` / `Shift-Tab` | 在当前 Tab 的上、下 Panel 间切换焦点 |
| `q` / `Ctrl-C` | 打开统一退出确认框 |
| `p` | 在允许的页面打开 Provider/Source 选择器 |

全局键位只在导航模式生效。文本字段进入编辑模式后，普通字符由编辑器接收；`Esc` 先退出编辑模式。
下载 Form 不再用 `Tab` 切换字段，字段选择统一使用 `↑` / `↓`。

### 4.2 顶层 Tab

```text
Pan TUI       [ Files ]  Logs  Settings       Google / project: Ready
```

- 当前 Tab 使用高亮样式，但布局宽度保持稳定。
- Header 始终显示当前 Provider、Profile、Source 和连接状态。
- 切换 Tab 不销毁任何 Panel 的滚动位置、选择位置或筛选条件。
- 每个 Tab 单独记住当前获得焦点的 Panel。

### 4.3 Files Tab

Files Tab 固定为两个可聚焦 Panel：

```text
Pan TUI       [ Files ]  Logs  Settings       Google / project: Ready

┌ Remote Files - cp_result/models ────────────────────────┐
│ > [DIR ] checkpoints/                                   │
│   [FILE] model.bin                           1.2 GiB     │
└─────────────────────────────────────────────────────────┘
                              Tab
┌ Transfers - 3 running / 12 queued / 8 done / 1 failed ─┐
│ RUN    42% model-01.bin   420 MiB/1 GiB     18 MiB/s   │
│ RUN     8% data.tar        80 MiB/1 GiB     12 MiB/s   │
│ RUN    71% weights.bin    710 MiB/1 GiB      9 MiB/s   │
│ WAIT       tokenizer.json                               │
│ FAIL       config.bin       connection reset            │
└──────────────────────────────────────── Total 39 MiB/s ┘
```

#### Remote Files Panel

- `↑/k`、`↓/j`、`PageUp/PageDown`、`Home/End`：移动选择。
- `Enter/→`：高亮项为文件夹时进入。
- `Backspace/←`：返回父目录；Source 根目录不能继续返回。
- `r`：重新读取当前 Source 或目录，并尽量保持原选择。
- `d`：高亮项可下载时打开 Download Form。
- `p`：打开 Provider/Source 选择器。

Provider/Source 选择器使用 `←/→` 选择 Provider/Profile，`↑/↓` 选择 Source，`Enter` 激活，
`Esc` 取消。切换 Provider/Source 后恢复对应 Session 的目录栈，而不是重建全局页面栈。

#### Transfers Panel

- 同时显示 Running、Queued、Failed、Skipped 和最近完成的 Transfer。
- `↑/↓`、翻页和首尾键滚动；选择行后显示完整路径、错误或校验信息。
- 提供 All、Active、Failed 等筛选，但筛选键只在该 Panel 获得焦点时生效。
- 默认排序为 Running、Failed、Queued、Skipped、最近 Completed；同组内顺序稳定。
- Panel 标题显示 running/queued/done/failed 数量和聚合实时速度。
- 每行显示状态、单文件进度、字节数和单文件速度；未知大小文件显示已写字节而不伪造百分比。
- Job 总体进度按已处理项目数/总项目数计算；已知总字节数可作为辅助信息，不能替代项目进度。

### 4.4 Logs Tab

| Panel | 内容 |
|---|---|
| 上方 | 可滚动日志列表，支持 All / Info / Error 筛选 |
| 下方 | 所选日志的完整时间、Provider、Job、Transfer、路径和错误详情 |

并行 Transfer 的日志允许交错，但每条记录必须携带可关联的 `provider_id`、`job_id` 和
`transfer_id`。日志滚动到最新位置时继续跟随；用户向上滚动后保持所选记录。

### 4.5 Settings Tab

| Panel | 内容 |
|---|---|
| 上方 | 全局下载、网络和日志设置 |
| 下方 | 当前 Provider/Profile 的专属设置、认证状态和 Source 列表 |

- 全局 Panel 不出现 Google 或阿里云盘专属字段。
- Provider Panel 由对应 `ProviderFactory` 提供字段描述、脱敏值、校验和保存逻辑。
- 普通字段使用 `↑/↓` 选择，`Enter` 进入编辑或执行操作，`Esc` 取消编辑。
- 保存全局网络配置时重建所有受影响 Provider；保存 Provider 配置时只重建该 Provider。
- 重建失败保留旧的可用实例和用户输入，并在该 Provider 状态中显示错误。
- token、私钥和 refresh token 永不以明文出现在 TUI、日志或错误链中。

### 4.6 Download Form 与退出

Download Form 至少包含 Destination root、File workers 和只读 Target preview：

- `↑/↓` 切换字段；`←/→` 或 `-/+` 调整 workers。
- 文本字段进入编辑模式后接受光标移动和普通字符。
- `Enter` 在字段编辑时确认字段，在 Form 导航模式提交 Job。
- `Esc` 先取消字段编辑，再关闭 Form。
- `Tab` 不用于字段切换；若 Modal 未来具有两个 Panel，只能用于 Panel 焦点切换。
- workers 默认来自全局配置，第一版限制为 `1..=5`。

退出确认框在所有 Tab、Panel、Form 和下载状态下共用。daemon 模式退出 TUI 只断开订阅；embedded
模式按显式 shutdown policy 等待或中止 Job，并保留 `.part`。取消退出必须恢复原来的 Tab、Panel
焦点、Modal 和选择状态。

## 5. Provider 架构

### 5.1 注册器与运行管理器

```text
ProviderRegistry                  ProviderManager
├── GoogleProviderFactory         ├── google/default -> Ready
└── AliyunProviderFactory         └── aliyun/default -> NeedsConfig
```

- `ProviderRegistry` 只保存编译进二进制的 ProviderFactory，按唯一 `ProviderId` 注册。
- 重复 ProviderId 在启动时视为开发错误，测试必须覆盖。
- `ProviderManager` 负责读取配置、创建实例、跟踪生命周期和提供 Session。
- 核心和 TUI 不允许通过 `match provider_id` 调用 Google/阿里云盘专属代码。
- 第一版不使用动态库插件；增加 Provider 只需实现接口并在组合根注册。

### 5.2 Provider 生命周期

```text
Disabled -> NeedsConfig -> Connecting -> Ready
                              |           |
                              v           v
                         ConfigError   ConnectionError
```

错误状态属于单个 Provider/Profile：

- Google 配置错误时，阿里云盘仍能浏览和下载。
- Provider 加载在后台执行，TUI 不因认证或网络错误无法启动。
- 用户可在 Settings 中修正配置并只重试对应 Provider。
- 正在运行的 DownloadJob 持有创建时的 Provider 实例快照；热重连只影响后续浏览和新 Job。

### 5.3 统一接口边界

接口形状以对象安全、异步 trait 为约束，最终命名可在实现阶段微调：

```rust
trait ProviderFactory: Send + Sync {
    fn id(&self) -> ProviderId;
    fn display_name(&self) -> &'static str;
    fn settings_schema(&self) -> Vec<ProviderSettingSpec>;
    fn load_settings(&self, context: &ProviderContext) -> Result<ProviderSettingsSnapshot>;
    fn save_settings(&self, context: &ProviderContext, changes: &BTreeMap<String, String>)
        -> Result<()>;
    fn readiness(&self, context: &ProviderContext) -> ProviderReadiness;
    async fn create(&self, context: ProviderContext) -> Result<Arc<dyn CloudProvider>>;
    async fn login(&self, request: ProviderLoginRequest) -> Result<ProviderAuthState>;
}

trait CloudProvider: Send + Sync {
    fn identity(&self) -> ProviderIdentity;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn sources(&self) -> Result<Vec<RemoteSource>>;
    async fn list_children(&self, parent: &RemoteRef) -> Result<Vec<RemoteItem>>;
    async fn refresh_item(&self, item: &RemoteRef) -> Result<RemoteItem>;
    async fn open_download(&self, item: &RemoteItem, offset: u64)
        -> Result<RemoteDownload>;
    async fn user_info(&self) -> Result<ProviderUserInfo>;
    async fn drive_info(&self) -> Result<Vec<ProviderDriveInfo>>;
    async fn create_folder(&self, parent: &RemoteRef, name: &str) -> Result<RemoteItem>;
    async fn upload_file(&self, request: UploadRequest) -> Result<UploadResult>;
    async fn delete_item(&self, item: &RemoteRef) -> Result<()>;
    async fn task_status(&self, task: &ProviderTaskId) -> Result<ProviderTask>;
}
```

上述新增方法都是能力化操作：默认返回 `Unsupported`，Provider 只实现自身真实支持的部分。`refresh_item`
是统一的单文件/单目录元数据接口；`user_info`、`drive_info` 和 `task_status` 的结果不得携带 token、
签名下载 URL 或其他凭据。上传接口接收公共字节流，分片、上传 URL 和服务端异步任务由 Provider 隐藏，
这样 Aliyun 的 multipart 上传不会泄漏到公共下载/计划核心。

Provider 负责：

- 认证和 token 刷新；
- Provider 私有配置与 Source 解析；
- 元数据、分页、目录读取和能力映射；
- 登录、用户/云盘信息、创建目录、上传、删除和异步任务轮询等可选操作；
- 把远端下载响应规范化为 Full、Partial 或 AlreadyComplete；
- 对用户可见错误进行分类并脱敏。

公共核心负责：

- 目录导航栈和 Session；
- 递归扫描及环检测；
- 安全名称、同名消歧和本地路径；
- DownloadJob、并发队列和 Transfer 状态；
- `.part` 文件、续传决策、原子完成、进度和日志；
- TUI 状态和渲染。

递归计划不能成为 Provider 接口方法，否则路径、安全、进度和测试语义会在 Provider 之间重复。

## 6. Provider 无关数据模型

```rust
struct RemoteItem {
    reference: RemoteRef,
    name: String,
    kind: RemoteItemKind,
    size: Option<u64>,
    checksum: Option<RemoteChecksum>,
    download: DownloadCapability,
}

struct RemoteRef {
    provider_id: ProviderId,
    profile_id: ProfileId,
    source_id: SourceId,
    object_id: RemoteObjectId,
}

enum DownloadCapability {
    Supported { resume: bool },
    Unsupported { reason: String },
}
```

- 核心模型不保存 Google MIME 常量、resource key 或阿里云盘 API 响应结构。
- Provider 把目录、普通文件和不支持对象映射为统一类型。
- `RemoteObjectId` 不用作本地文件名；本地名称始终经过公共安全路径逻辑。
- Provider 返回的集合由公共层执行“目录优先、名称不区分大小写、稳定 ID 消歧”排序。

`RemoteDownload` 是规范化传输结果：

```text
Full(stream, total_size)                 -> 从 0 写入或截断 .part
Partial(start, stream, total_size)       -> 校验 start 后追加 .part
AlreadyComplete(expected_size)           -> 本地 .part 大小匹配后完成
```

HTTP 状态码、Content-Range 和 Provider 私有下载 URL 在 Provider 内转换，公共下载器不依赖
`reqwest::Response`。

## 7. 配置设计

### 7.1 文件布局

```text
~/.config/pan-tui/
├── config.toml
└── providers/
    ├── google.toml
    ├── aliyun.toml
    ├── google/
    │   └── service-account.json
    └── aliyun/
        └── token.json
```

`config.toml` 只包含公共行为：

```toml
[download]
destination = "/home/user/Downloads"
concurrency = 3
resume = true
overwrite = false

[network]
proxy = "http://127.0.0.1:7890"

[logging]
max_file_size = "10MiB"
```

Google Provider 示例：

```toml
enabled = true
credentials_file = "google/service-account.json"

[[sources]]
id = "project"
url = "https://drive.google.com/drive/folders/FOLDER_ID"
```

阿里云盘 Provider 示例：

```toml
enabled = true
client_id = "OPENAPI_CLIENT_ID"
refresh_token = "INITIAL_REFRESH_TOKEN"
drive = "default"
root_file_id = "root"

[[sources]]
id = "backup"
name = "Backup"
drive = "default"
root_file_id = "root"
```

成功刷新后的 access token 和轮换 refresh token 写入 Provider 私有的 `aliyun/token.json`，不回写
`aliyun.toml`。上述字段已由 mock server 验证；Phase 19 真实账号验收前仍不得宣称 API 行为完全稳定。

### 7.2 配置归属与合并

- 全局：默认目标、并发数、覆盖/续传、公共代理、日志策略。
- Provider：enabled、认证方式、API endpoint、Provider 私有代理覆盖、Profiles 和 Sources。
- 凭据/状态：私钥、refresh token、access token 缓存等 Provider 私有敏感数据。
- 运行时 UI 状态默认不写入配置；以后需要恢复 active tab/provider 时使用独立 state 文件。
- 网络配置合并顺序为 Provider override > global network default > direct connection。
- CLI 参数只作为本次运行覆盖，不回写配置。

Provider 配置文件和敏感状态使用 `0600`；目录使用 `0700`。序列化、错误和日志均经过脱敏。

### 7.3 旧配置迁移

当前路径：

```text
~/.config/pan-tui/service-account.json
~/.config/pan-tui/sources.json
```

迁移规则：

1. 新 Google 配置不存在但旧文件存在时，以 legacy Google Profile 只读加载并显示迁移提示。
2. 不自动移动、覆盖或删除旧文件。
3. Settings 可执行显式迁移，写入新配置前先校验，失败时保持旧配置可用。
4. `--credentials`、`--sources` 和相关环境变量在过渡期映射为 Google Profile 的运行时覆盖，并标记弃用。

## 8. 应用与 Session 状态

```rust
struct AppState {
    active_tab: AppTab,
    focused_panel: HashMap<AppTab, PanelId>,
    active_provider: ProviderIdentity,
    providers: ProviderManagerState,
    downloads: DownloadManagerState,
    logs: LogViewState,
}

struct ProviderSession {
    status: ProviderStatus,
    active_source: Option<SourceId>,
    sources: HashMap<SourceId, SourceSession>,
}

struct SourceSession {
    pages: Vec<FolderPage>,
    message: String,
}

struct FolderPage {
    items: Vec<RemoteItem>,
    selected: Option<usize>,
    offset: usize,
}
```

- 每个 Source 独立保存目录栈、每层选择与滚动位置以及最后状态/错误消息。
- 异步结果携带 `OperationId` 和完整 Provider/Source 身份；过期结果不能覆盖新 Session。
- 切换 Provider 不改变正在运行 Job 绑定的 Provider。
- Reload 只重建当前层数据，尽量按 `RemoteObjectId` 恢复选择。

## 9. 并行下载模型

### 9.1 Job 与 Transfer

```rust
struct DownloadJobState {
    id: JobId,
    provider: ProviderIdentity,
    root: RemoteRef,
    status: JobStatus,
    transfers: IndexMap<TransferId, TransferState>,
    aggregate_speed: f64,
    items_total: usize,
    items_processed: usize,
}

struct TransferState {
    id: TransferId,
    remote: RemoteRef,
    relative_path: PathBuf,
    status: TransferStatus,
    bytes_written: u64,
    total_bytes: Option<u64>,
    bytes_per_second: f64,
    last_error: Option<String>,
}
```

DownloadManager 可以同时持有多个活动 DownloadJob；每个 Job 内部的 Running Transfer 数不得超过
自己的 workers。Job 固定持有提交时的 Provider 实例，Provider 切换或重连不改变既有 Job。

### 9.2 事件协议

所有文件级事件必须携带 `job_id` 和 `transfer_id`：

```text
JobScanning
JobPlanReady
TransferQueued
TransferStarted
TransferProgress
TransferSkipped
TransferCompleted
TransferFailed
JobFinished
JobFatal
```

UI 通过 ID 更新 `IndexMap` 中对应行。禁止使用单一 `current_path/current_written/current_total` 作为并发
下载的真实状态；“最近更新文件”最多只能作为摘要，不能替代 Transfer 列表。

### 9.3 进度与速度

- 单文件百分比只在 total 已知且非零时计算。
- 单文件速度使用该 Transfer 的采样窗口，不把其他文件字节算入。
- 聚合速度是所有 Running Transfer 当前速度之和。
- Overall 按 Success、Skipped、Failed 等终态项目数推进，完成顺序不影响结果。
- 失败只累计磁盘上实际 `.part` 字节，不使用远端声明大小虚增完成量。
- 并发事件乱序不得让已完成 Transfer 回退到 Running。

### 9.4 本地文件语义

- 每个文件写入同目录 `<name>.part`，完成前不暴露最终文件。
- Partial 的起始位置必须等于本地 `.part` 大小，否则拒绝追加。
- Provider 不支持 Range 时，从 0 安全重启该文件，不能直接追加。
- 完成后 `sync_all` 并原子重命名；默认跳过已有目标，overwrite 使用安全替换。
- 拒绝写入符号链接或非普通 `.part` 路径。
- 名称中的 `/`、`\\`、NUL 和控制字符替换为 `_`；空名称、`.`、`..` 使用稳定 ID 回退。
- 同目录同名项在扩展名前追加稳定 ID 短后缀。

## 10. 首批 Provider

### 10.1 Google Drive

- 使用服务账号 JSON 和只读 Drive scope。
- Source 支持文件夹 URL、文件夹 ID 和 resource key。
- 逐页读取直接子项，并支持 shared drive 参数。
- Google 原生文件由 Provider 返回 `Unsupported` 原因，公共计划记录为 Skipped。
- 现有 `DriveClient`、认证和解析测试迁移到 `providers/google`，目标行为保持不变。

### 10.2 阿里云盘

- 认证、token 刷新和持久化全部封装在 `providers/aliyun`。
- 支持配置 Source、逐层目录浏览、分页和普通文件下载。
- 下载 URL 过期时由 Provider 刷新，核心 Transfer ID 和本地 `.part` 不变。
- Range、大小、checksum 和下载限制按实际 API 能力映射，不能伪造与 Google 相同的能力。
- 真实 API、授权方式和限流行为必须在独立验收环境验证并记录。

## 11. 日志、安全与故障语义

- 日志保存到 `~/.local/state/pan-tui/logs/YYYY-MM-DD/pan-tui_HH-mm-ss_NNN.log` 并按大小轮转；
  设置 `XDG_STATE_HOME` 时使用 `$XDG_STATE_HOME/pan-tui/logs`。
- 日志结构增加 Provider/Profile/Source/Job/Transfer 关联字段。
- 不打印私钥、access token、refresh token、完整认证响应或带敏感 query 的下载 URL。
- Provider 错误分为 Config、Auth、Permission、RateLimit、Network、NotFound、Unsupported 和 Internal。
- 用户可见错误提供可操作信息；详细错误进入脱敏日志。
- 单个 Transfer 失败后继续同 Job 其他文件；递归扫描失败属于 JobFatal。
- Provider 切换、Tab 切换和 Panel 切换不得取消后台下载。

## 12. 目标模块边界

```text
src/
├── app/                 # AppState、消息分发和异步 operation 归并
├── config/              # AppConfig、ProviderConfigStore、迁移和权限
├── provider/
│   ├── mod.rs           # 公共 trait、ID、能力和错误
│   ├── registry.rs      # ProviderRegistry
│   ├── manager.rs       # 生命周期、实例和 Session
│   ├── google/          # Google 配置、认证、API 和下载适配
│   └── aliyun/          # 阿里云盘配置、认证、API 和下载适配
├── browser/             # SourceSession、导航栈和排序
├── plan/                # Provider 无关递归计划和安全路径
├── download/            # Job、Transfer、队列、.part 和事件
├── job/                 # 多 Job runtime、快照、取消和事件广播
├── state/               # SQLite、恢复、Transfer 进度和 notification outbox
├── client.rs            # PanClient、EmbeddedPanClient 与 DaemonPanClient
├── ipc/                 # daemon 与本地版本化协议
├── daemon.rs            # DaemonService 生命周期与 TUI/CLI 共用 DaemonClient
├── notifier/            # 钉钉/飞书注册器、签名和可靠 worker
├── logging/             # 持久化日志和 TUI 日志模型
└── tui/
    ├── mod.rs           # BrowserApp 状态、表单、Source Session 和公共类型
    ├── runtime.rs       # terminal event loop、PanClient 调用、Job 订阅和日志归并
    ├── render.rs        # Tab、Panel、Modal、Transfers 和状态渲染
    └── tests.rs         # Ratatui TestBackend 与交互回归测试
```

不要求一次性物理拆成全部目录。实现按 `TODO.md` 渐进迁移，但最终依赖方向必须是：

```text
TUI / CLI -> PanClient
              ├── DaemonPanClient -> IPC -> daemon-owned ProviderRuntime/DownloadManager
              └── EmbeddedPanClient -> in-process ProviderRuntime/DownloadManager

ProviderRuntime -> Provider traits <- Google/Aliyun adapters
DownloadManager -> Plan/Download core -> Provider traits
```

TUI 不得持有 `CloudProvider`、`ProviderFactory`、`DownloadManager` 或 `JobBackend`。Tab、Panel、选择、
滚动、目录 Session 和表单草稿仍是 TUI 本地状态；Provider catalog、Source、目录、元数据、设置重建、
Job 提交/取消/快照/订阅全部通过 `PanClient`。Google 或阿里云盘模块不得反向依赖 TUI。

## 13. 测试与验收

### 13.1 自动测试

1. Fake Provider 可通过 Registry 注册并完成 Source、目录和下载流测试，核心无 provider-id 分支。
2. 重复 ProviderId、无配置、坏配置和单 Provider 连接失败均有测试。
3. Google 旧配置只读兼容和显式迁移不覆盖原文件。
4. `[`/`]` 只切 Tab，`Tab/Shift-Tab` 只切 Panel，文本编辑不误触全局键位。
5. Provider/Source 切换后恢复目录栈、选择位置和加载状态。
6. workers 为 3 时可同时出现 3 个 Running Transfer，且每行进度互不覆盖。
7. 并发事件乱序、失败、Skipped、未知大小和完成状态不可回退。
8. Provider 热重建不替换活动 Job 持有的实例。
9. Ratatui `TestBackend` 在窄终端和常规终端中不出现标题、行内容或 footer 重叠。
10. Google Provider 迁移前后的列表、递归计划、续传和路径行为一致。

### 13.2 真实使用检查

1. 仅配置 Google 时，TUI 正常启动；阿里云盘显示 NeedsConfig 或不启用。
2. Google 配置错误而阿里云盘有效时，阿里云盘仍可使用，反向同理。
3. 在两个 Provider/Source 间往返，浏览位置保持。
4. 以 workers 2 以上下载小型测试目录，Transfers Panel 同时显示多行 Running。
5. 下载期间切换 Tab、Panel 和 Provider，Job 持续运行且日志关联正确。
6. 中断后 `.part` 保留，重启后按 Provider 能力续传或安全从头开始。

真实验收使用小型、可删除的测试数据；未经明确授权不下载大型共享目录。

### 13.3 完成定义

- `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 和 release 构建通过。
- Google Drive 与阿里云盘各完成一次真实逐层浏览、小文件下载和中断恢复检查。
- 并行 Transfers UI、Provider 故障隔离和旧 Google 配置迁移通过真实 TUI 验收。
- README、示例配置和实际键位一致后，才能宣称 v3 设计已交付。

## 14. 实施顺序

Provider TUI 按 Phase 13–20 实施。长期运行能力按 Phase 21–26 实施：首次认证、脱离 TUI 的 Job
runtime、SQLite 恢复、daemon/IPC、可靠通知，最后完成 embedded 与 resident 两种模式的集成验收。

## 15. 长期运行、IPC 与通知

1. `pan-tui` 默认仍可作为一次性 TUI/CLI；可选 `daemon` 负责跨终端生命周期的 Job、恢复和通知。
   socket 存在时 TUI 优先连接 daemon；没有 socket 时才创建 embedded runtime。
2. SQLite、WAL sidecar、稳定 IPC client-id 和日志统一位于 `~/.local/state/pan-tui/`，使用私有权限；
   凭据、Notifier secret 与签名下载 URL 不进入数据库。
3. Job 请求和快照可序列化，Progress 节流批量写入；启动时 Running/Queued 原子转为 Interrupted，
   Provider 可用时使用原 Job ID 与 `.part` 继续。
4. Linux IPC 使用 `0600` Unix socket 和版本化 JSON；协议 v3 传公共 Provider catalog、Source、
   `RemoteRef` 浏览/元数据、脱敏设置快照、Job 请求、完整快照和错误分类。
   `client_id + request_id` 幂等映射与 Job 在同一 SQLite 事务写入；订阅建立时无条件发送当前完整快照，
   避免 daemon 重启后 revision 归零导致 TUI 无法恢复。
   二进制入口不直接构造 `IpcRequest`：CLI daemon 调用库级 `DaemonService`，TUI 与 CLI 控制命令共用
   `DaemonClient` 的 provider/source/list/stat/settings、status、submit、cancel 和 subscribe 实现。
5. Google Installed App 使用 Authorization Code + PKCE + loopback callback；ProviderFactory 统一提供
   `auth_status/login/revoke`，token 轮换通过 `0600` 原子 CredentialStore 持久化。
6. Notifier 是独立于 Provider 的第二套编译期注册器。钉钉和飞书 webhook/secret 只存在于 `0600` 配置，通用事件不携带
   token、签名 URL 或文件内容。
7. Job 终态和 notification outbox 在同一 SQLite 事务中提交；daemon worker 处理 Retry-After、指数
   退避和 DeadLetter，进程重启后继续待发送项。
