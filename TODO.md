# 分阶段 TODO

本文由 `DESIGN.md` 转换而来。执行规则：

- `[ ]` 表示未完成；`[~]` 表示已有实现但尚未通过本 Phase 验收；`[x]` 表示已实现并验证。
- 当前 Phase 的 Gate 未通过时，不进入下一 Phase。
- 行为设计发生变化时，先更新 `DESIGN.md`，再更新本清单。
- Phase 0–12 是 v2 Google Drive 专用实现的已验证历史基线；Phase 13–20 实施 v3 Provider TUI 设计。
- 未完成 Phase 的目标行为不得提前写入 README，README 始终描述当前可运行版本。

## Phase 0：冻结设计

- [x] P0.1 明确逐层文件管理器，而不是一次显示完整树。
- [x] P0.2 明确键位、面包屑、当前目录下载和确认流程。
- [x] P0.3 明确 Drive API、认证、分页、递归扫描和下载语义。
- [x] P0.4 明确安全边界、错误处理、测试矩阵和完成定义。
- [x] P0.5 明确语言规则：设计/计划文稿中文，README 和代码英文。

Gate：`DESIGN.md` 和本清单存在，设计状态标记为已冻结。**已通过。**

## Phase 1：Rust 基础、认证和单层 Drive API

- [x] P1.1 初始化 Cargo 2024 项目并锁定依赖。
- [x] P1.2 实现 Drive 数据模型及文件夹/Google 原生文件判断。
- [x] P1.3 实现代理感知的服务账号 JWT token 流程和提前刷新。
- [x] P1.4 实现分享链接/裸 ID 解析与根目录元数据读取。
- [x] P1.5 实现直接子项分页读取、目录优先排序和共享盘参数。
- [x] P1.6 增加数据模型、解析、响应和排序测试。
- [x] P1.7 运行 Gate：格式化、编译、相关测试和真实 `--list`。

Gate：Rust 程序能使用现有服务账号只列出 `cp_result` 第一层，不递归、不下载。**已通过。**

## Phase 2：TUI 逐层浏览

- [x] P2.1 实现页面栈、选中索引和父页面缓存。
- [x] P2.2 实现 `Enter/→` 进入目录，`Backspace/←` 返回。
- [x] P2.3 实现方向键、翻页、Home/End 和根目录边界。
- [x] P2.4 渲染当前层类型、大小、选中详情和面包屑。
- [x] P2.5 实现 `d` 确认框和 `y/n/Esc`；确认前禁止下载。
- [x] P2.6 使用 `TestBackend` 验证当前层、导航和弹窗。
- [x] P2.7 运行 Gate：自动测试以及真实 PTY 的进入/返回/退出。

Gate：用户可以进入和离开真实 Drive 子目录；界面不展开完整树，退出后没有产生本地下载文件。**已通过。**

## Phase 3：递归计划与本地下载

- [x] P3.1 仅在确认后递归扫描当前目录，并处理分页。
- [x] P3.2 实现安全名称、层级保持和 Drive 同名项目消歧。
- [x] P3.3 实现普通文件流式下载、`.part`、Range 续传和大小校验。
- [x] P3.4 默认跳过已有目标，并通过 `--overwrite` 支持原子替换。
- [x] P3.5 跳过 Google 原生/受限项目，单文件失败后继续。
- [x] P3.6 发送扫描、文件、总体进度和最终汇总事件。
- [x] P3.7 在 TUI 中消费事件并渲染 Gauge、路径和计数。
- [x] P3.8 测试计划、安全路径、进度归并和下载边界。
- [x] P3.9 运行 Gate：全部自动检查和小型模拟下载路径。

Gate：用户按 `d` 并确认后，当前目录按原层级写入目标；中断文件可续传，失败保持可见，程序不虚报成功。**已通过。**

## Phase 4：迁移清理与交付

- [x] P4.1 删除 Python 源码/测试、`pyproject.toml`、`uv.lock` 和 Python 缓存。
- [x] P4.2 更新 `.gitignore`，加入 Rust 构建产物并保留凭据保护。
- [x] P4.3 完善英文 README：安装、键位、凭据、运行、限制和恢复。
- [x] P4.4 运行 `cargo fmt --check`。
- [x] P4.5 运行 `cargo clippy --all-targets -- -D warnings`。
- [x] P4.6 运行 `cargo test` 和 release 构建。
- [x] P4.7 使用真实账号和 TUI 导航根目录及至少一个子目录。
- [x] P4.8 验证目标本地目录可写，并报告未执行的真实下载检查。
- [x] P4.9 扫描最终 Rust 代码和 README，确认全部为英文。

Gate：Rust TUI 是唯一实现；文档与行为一致；全部自动检查通过；真实只读导航通过；代码和 README 为英文。**已通过。**

## Phase 5：无链接启动与保存链接层

- [x] P5.1 删除链接/ID 位置参数，程序直接进入 TUI。
- [x] P5.2 实现 `sources.json`、环境变量/参数解析和链接去重。
- [x] P5.3 对保存链接读取真实元数据并验证文件夹权限。
- [x] P5.4 为 TUI 增加不伪造 Drive ID 的 `Shared links` 虚拟页面。
- [x] P5.5 在虚拟层显示链接数量，支持进入共享文件夹并禁止直接下载虚拟层。
- [x] P5.6 让 `--list` 无链接列出保存入口及数量。
- [x] P5.7 更新英文 README 和自动测试。
- [x] P5.8 运行格式化、Clippy、测试和 release 构建。
- [x] P5.9 使用真实服务账号验证保存入口和多层 TUI 导航，不执行真实下载。

Gate：直接运行程序即可看到 `Shared links` 数量并逐层进入保存的共享文件夹；虚拟层不能误触发下载。**已通过。**

## Phase 6：收紧虚拟层键位

- [x] P6.1 虚拟层仅允许 `Enter` 进入选中的保存链接。
- [x] P6.2 虚拟层忽略 `d` 和 `→`，且不显示下载入口提示。
- [x] P6.3 更新英文 README、自动测试并通过完整 Gate。

Gate：`Shared links` 层只能按 `Enter` 进入；`d` 和 `→` 不改变状态，真实目录键位保持不变。**已通过。**

## Phase 7：按层级拆分键位上下文

- [x] P7.1 将选择移动和 `r` 重新加载抽成两个浏览层共用的有效操作。
- [x] P7.2 为 `Shared links` 和真实 Drive 目录建立独立事件分支。
- [x] P7.3 从虚拟层删除返回、右移和下载键位及相关提示。
- [x] P7.4 实现虚拟层重新读取来源配置、目录层重新读取当前子项，并保持选中项。
- [x] P7.5 将英文 README 改为按层级展示键位。
- [x] P7.6 运行自动检查与真实 PTY 层级键位验收。

Gate：每个层级只注册并显示自身有效键位；两个浏览层都支持符合各自数据源的 `r` 重新加载；虚拟层不存在返回、右移和下载操作。**已通过。**

## Phase 8：下载高亮项

- [x] P8.1 将下载目标从当前页面目录改为当前高亮项。
- [x] P8.2 文件夹建立递归计划，普通文件建立单文件计划；Google 原生或禁止下载项隐藏 `d`。
- [x] P8.3 按高亮类型动态注册并显示 `Enter/→` 与 `d`，空目录删除无效键位。
- [x] P8.4 在确认框、计划状态和总体进度中明确高亮项范围及实际目标路径。
- [x] P8.5 更新英文 README、计划测试和 TUI 自动测试。
- [x] P8.6 运行完整自动检查与真实 PTY 确认框验收，不开始真实下载。

Gate：在 `cp_result` 高亮 `CUHK_LOWER` 或普通文件后按 `d`，确认框目标必须与高亮项一致；
`Overall` 只表示该高亮项，且不包含同层其他项目。**已通过。**

## Phase 9：虚拟层直接下载共享文件夹

- [x] P9.1 在 `Shared links` 高亮保存入口时注册并显示 `d`。
- [x] P9.2 将高亮入口的真实 Drive 文件夹元数据绑定为下载目标，不要求先按 `Enter`。
- [x] P9.3 保持 `Enter` 进入浏览、`r` 重载和虚拟层无返回/右移操作不变。
- [x] P9.4 更新英文 README 和 TUI 自动测试。
- [x] P9.5 运行完整自动检查与真实 PTY 确认框验收，不开始真实下载。

Gate：启动后在 `Shared links` 高亮 `cp_result` 并按 `d`，确认框必须显示目标
`Shared links / cp_result` 和默认本地目标下的 `cp_result`；取消后不得扫描或下载。**已通过。**

## Phase 10：准确进度、完整路径与下载日志

- [x] P10.1 总体百分比改为已处理项目数/总项目数，所有终态均推进计数。
- [x] P10.2 当前文件显示字节数与实时平均速度，不显示百分比；总体只显示项目数与百分比，不显示速度。
- [x] P10.3 失败只累计 `.part` 实际字节，修复 9.8 GiB 失败文件造成的虚假进度跳变。
- [x] P10.4 下载路径保留保存链接根目录到高亮项的全部安全路径组件。
- [x] P10.5 实现 `~/.local/state/pan-tui/logs/YYYY-MM-DD` 下的 Loguru 风格日志和 10 MiB 大小轮转。
- [x] P10.6 成功和 Skipped 记录 `INFO`，失败记录 `ERROR`，并保留完整路径与原因。
- [x] P10.7 使用 `Tab` 在文件树和日志卡片间切换，日志卡片支持滚动和 `a/i/e` 筛选。
- [x] P10.8 日志卡片每次进入时定位最新日志，并只显示自身有效键位。
- [x] P10.9 更新英文 README，增加进度、路径、日志布局和日志视图键位。
- [x] P10.10 增加进度、失败、路径、轮转和 TUI 测试，运行完整自动检查与真实使用验收。

Gate：失败大文件只把实际 `.part` 字节写入汇总和日志；任一终态都按一个项目推进 Overall；
从 `cp_result/CUHK_LOWER` 下载高亮项时保留该祖先路径；日志文件和 Tab 日志卡片均能看到对应 INFO/ERROR。**已通过。**

## Phase 11：下载 Form 与内部文件并发队列

- [x] P11.1 将确认框改为 Form，提供可编辑 Destination root 和只读完整目标预览。
- [x] P11.2 增加 File workers 字段，默认 1，限制在 1–5，并建立独立 Form 键位上下文。
- [x] P11.3 把单个递归计划中的可下载文件放入有界并发队列，不并发多个高亮下载任务。
- [x] P11.4 并发 Progress 必须携带文件身份，使 Current file 展示最近更新文件及其速度。
- [x] P11.5 Overall 继续按所有文件 Success/Skipped/Failed 终态计数，不受完成顺序影响。
- [x] P11.6 日志位于最新时继续跟随；向上滚动后切走再返回必须恢复同一条日志。
- [x] P11.7 `q` 与 `Ctrl-C` 在所有视图复用退出确认框，取消后恢复原状态。
- [x] P11.8 更新英文 README、Form/队列/日志位置/退出测试并运行完整门禁和真实 TUI 验收。

Gate：Form 可以把目标根改为绝对路径并选择 1–5 workers；内部文件同时运行数不超过设置值；
日志 Tab 往返遵守跟随或恢复语义，且不启动多个高亮下载任务。`q` 与 `Ctrl-C` 复用确认框，
取消后保留底层状态。30 个测试、Clippy、release 构建和真实 TUI 验收全部通过。**已通过。**

## Phase 12：TUI 应用代理设置

- [x] P12.1 增加 `config.toml` 网络配置，空 proxy 表示直连。
- [x] P12.2 OAuth 与 Drive 客户端显式忽略系统代理变量，并只使用应用 proxy。
- [x] P12.3 浏览器与日志卡片按 `s` 打开可编辑设置页。
- [x] P12.4 保存后热重建客户端并后台重新加载共享入口。
- [x] P12.5 初始入口加载改为后台任务，网络失败时仍可修正设置。
- [x] P12.6 更新测试、README，并运行完整自动检查与真实 TUI 验收。

Gate：无配置时不读取 `HTTP_PROXY/HTTPS_PROXY/NO_PROXY`；设置页保存代理后当前会话重新连接，
配置持久化到 `~/.config/pan-tui/config.toml`。自动检查和真实 TUI 验收全部通过后关闭。**已通过。**

## Phase 13：v3 设计冻结与回归基线

- [x] P13.1 统一 Tab、Panel、Provider、Profile、Source、DownloadJob 和 Transfer 术语。
- [x] P13.2 冻结 `[` / `]` 切换顶层 Tab、`Tab/Shift-Tab` 只切换上/下 Panel 的键位语义。
- [x] P13.3 将 Files 页冻结为 Remote Files 与多行 Transfers 两个 Panel，删除单一 Current file 目标设计。
- [x] P13.4 冻结 Provider 注册器、故障隔离、Session 保留和公共下载核心边界。
- [x] P13.5 冻结全局配置、Provider 私有配置、凭据/状态和旧 Google 配置迁移规则。
- [x] P13.6 运行当前 Phase 12 的格式化、Clippy、33 个测试和 release 构建，记录重构前基线。
- [x] P13.7 检查 README 仍只描述当前实现；v3 目标章节不再保留冲突键位或单文件并发设计。

Gate：v3 目标设计只有一份，当前 Google Drive 行为基线全部通过，且本 Phase 不改变 Rust 运行行为。**已通过。**

## Phase 14：Provider 无关模型与注册器

- [x] P14.1 增加 `ProviderId`、`ProfileId`、`SourceId`、`RemoteObjectId` 和组合 `RemoteRef`。
- [x] P14.2 将 `DriveItem` 抽象为 Provider 无关 `RemoteItem`、`RemoteItemKind` 和 `DownloadCapability`。
- [x] P14.3 定义对象安全的 `ProviderFactory`、`CloudProvider`、能力和脱敏错误接口。
- [x] P14.4 实现编译期 `ProviderRegistry`，拒绝重复 ID，且核心不使用 provider-id `match` 分派。
- [x] P14.5 实现 `ProviderManager` 生命周期：Disabled、NeedsConfig、Connecting、Ready 和独立错误状态。
- [x] P14.6 使用 Fake Provider 验证注册、Source、目录读取、下载流规范化和单 Provider 故障隔离。
- [x] P14.7 运行格式化、Clippy、37 个单元测试和 release 构建。

Gate：Fake Provider 不修改 TUI/下载核心即可注册并完成浏览与下载接口测试；重复 ID 和单 Provider
失败具有确定错误，现有 Google 行为尚未迁移但基线测试继续通过。**已通过。**

## Phase 15：分层配置与旧配置迁移

- [x] P15.1 将 `config.toml` 扩展为仅包含下载、公共网络和日志设置的 `AppConfig`。
- [x] P15.2 实现 `providers/<id>.toml`、Provider 私有目录和 `ProviderConfigStore`。
- [x] P15.3 ProviderFactory 自行加载、校验、脱敏和保存专属配置，核心不解析专属字段。
- [x] P15.4 实现 Provider proxy override > 全局 proxy > 直连的合并规则。
- [x] P15.5 对配置目录、配置文件、凭据和 token 状态执行 `0700/0600` 权限约束。
- [x] P15.6 新 Google 配置缺失时只读加载旧 `service-account.json` 与 `sources.json`，不自动改写。
- [x] P15.7 实现幂等显式迁移、原文件保留和已有新配置保护；旧 CLI/env 保留待 Google 组合根接入。
- [x] P15.8 测试配置 round-trip、脱敏、权限、坏 Provider 配置隔离和 legacy 迁移；44 个测试通过。

Gate：全局文件中没有 Google/阿里云盘专属字段；两个 Provider 的配置和敏感状态互不覆盖；旧
Google 配置在不修改磁盘的情况下仍可加载。**已通过。**

## Phase 16：Google Drive 迁移到 Provider

- [x] P16.1 将现有认证、链接解析、API 和响应模型迁入 `provider/google` 边界。
- [x] P16.2 把 Google 文件夹、普通文件、原生文件和权限映射为统一 `RemoteItem` 能力。
- [x] P16.3 将 Google Full/Partial/416 语义规范化为 `RemoteDownload`，公共下载器不再依赖 Drive 响应。
- [x] P16.4 让递归计划只依赖 `CloudProvider`，继续复用安全路径、同名消歧和环检测。
- [x] P16.5 启动和 `--list` 通过 Registry/ProviderManager 获取 Google，不直接构造 `DriveClient`。
- [x] P16.6 迁移并扩充现有 Google 单元测试，执行迁移前后行为对照。
- [x] P16.7 使用现有服务账号真实验证新 Provider 配置、Source、逐层浏览、1096-byte 下载和 512-byte `.part` 续传路径。

Gate：Google Drive 的列表、导航、下载、日志和安全语义与 Phase 12 等价；核心模块不再导入
`DriveClient`、Google MIME 常量或 Google API 响应类型。

## Phase 17：Tab、Panel 与 Provider Session TUI

- [x] P17.1 将 `ContentView` 拆为 `AppTab` 与每个 Tab 独立的 `PanelFocus`。
- [x] P17.2 渲染固定宽度 `Files / Logs / Settings` Tab 条和当前 Provider、Profile、Source 状态。
- [x] P17.3 实现 `[` / `]` 循环切 Tab，`Tab/Shift-Tab` 只循环当前 Tab 的上/下 Panel。
- [x] P17.4 Files 上方渲染 Remote Files，下方建立 Transfers 容器；切 Tab 不丢失焦点和滚动位置。
- [x] P17.5 完成 Provider/Profile/Source 选择器；每个 Source 独立保存目录栈、选择、滚动位置和错误状态。
- [x] P17.6 Settings 上方编辑 AppConfig，下方通过 Provider schema 脱敏编辑并仅重建当前 Provider/Profile。
- [x] P17.7 下载 Form 改用 `↑/↓` 切字段，文本编辑模式不误触 `[` / `]` 全局键位。
- [x] P17.8 增加键位优先级、Session 保留、Modal 恢复和 `60x20`/标准终端尺寸的 TestBackend 测试。

Gate：`[` / `]` 不改变 Panel 焦点，`Tab` 不改变顶层 Tab；Google Source 往返后恢复原目录与选择；
Settings 不再通过独立 `s` 页面绕过 Tab 模型。

## Phase 18：并行 Transfer 状态与可观测性

- [x] P18.1 增加稳定 `JobId`、`TransferId`、`DownloadJobState` 和 `TransferState`。
- [x] P18.2 计划完成后为每个文件创建 Transfer；Queued/Running 数量受 workers 有界队列约束。
- [x] P18.3 所有文件事件携带 job/transfer/provider 身份，并按 ID 更新状态 Map。
- [x] P18.4 删除单值 `current_path/current_written/current_total` 作为并发真实状态的设计与实现。
- [x] P18.5 Transfers Panel 逐行渲染 Running、Failed、Queued、Skipped 和最近 Completed。
- [x] P18.6 显示单 Transfer 进度/速度、聚合速度、Job 项目进度和 All/Active/Failed 筛选。
- [x] P18.7 日志增加 Provider/Job/Transfer 关联字段，选中日志下方显示完整详情。
- [x] P18.8 测试 workers 上限、事件乱序、终态不可回退、未知大小、失败 `.part` 字节和稳定排序。
- [x] P18.9 在 PTY 中用 workers=2 的模拟目录验证两行 Running、两行 Queued 和最终聚合一致。

Gate：并发数为 N 时最多 N 行 Running，且每行路径、字节、速度和终态互不覆盖；聚合统计与最终
汇总一致，切换 Tab/Panel 不影响下载。

## Phase 19：阿里云盘 Provider

- [ ] P19.1 在可控测试账号上确认可用认证流程、token 刷新、API 范围、限流和下载 URL 生命周期。
- [x] P19.2 实现阿里云盘独立配置、凭据/状态持久化、脱敏和 Settings schema。
- [x] P19.3 按当前范围实现 OAuth 刷新、用户/云盘信息、文件列表/元数据和下载 URL；创建、上传、移动、复制、删除及异步任务明确返回 `Unsupported`。
- [x] P19.4 实现 Sources、目录元数据、分页、目录优先排序和能力映射。
- [x] P19.5 实现普通文件 Full/Partial 下载；URL 过期刷新不得改变 Transfer 身份或 `.part` 路径。
- [x] P19.6 按实际 API 能力实现或明确禁用 Range、checksum 和受限对象下载。
- [x] P19.7 增加 mock API 的认证、分页、错误、刷新 URL、续传和脱敏测试；当前 69 个测试通过。
- [ ] P19.8 使用小型真实目录完成逐层浏览、普通文件下载、中断和恢复验收。

Gate：阿里云盘 Provider 通过统一接口完成 Source、浏览和下载；核心与 TUI 没有新增 Aliyun ID
分支；不支持能力在 UI 中明确显示而不是伪装成功。

## Phase 20：双 Provider 集成与交付

- [ ] P20.1 同时配置 Google 与阿里云盘，验证任一 Provider 配置/认证失败不阻止另一个。
- [x] P20.2 PTY 下载期间切换 Provider、Source、Tab 和 Panel，验证 Job 继续使用原 Provider 实例并完成。
- [x] P20.3 保存全局网络配置时重建已就绪 Provider 并保留失败前实例；Provider 专属配置只重建当前实例，失败保留旧实例和编辑输入。
- [ ] P20.4 验证旧 Google 配置显式迁移、回滚和弃用提示，不删除用户文件。
- [x] P20.5 更新英文 README、示例配置、键位表、Provider 状态、迁移指南和已知限制。
- [x] P20.6 运行 `cargo fmt --all -- --check`、Clippy、69 个测试、examples 和 release 构建。
- [ ] P20.7 分别完成 Google 与阿里云盘真实浏览、小文件下载和中断恢复。
- [x] P20.8 复核日志与错误输出，确认没有凭据、token 或敏感下载 URL；签名 URL 传输错误也经过固定消息脱敏。

Gate：同一 `pan-tui` 二进制可以可靠切换 Google Drive 与阿里云盘 Source，逐层浏览并下载到
本地；Files 下方准确展示并行 Transfer；配置、故障和 Session 相互隔离；README 与实际行为一致。

## Phase 21：首次认证与账号引导

- [x] P21.1 增加 `pan-tui auth login <provider>`、`auth status` 和 `auth revoke` 命令，认证流程不依赖 TUI 生命周期。
- [x] P21.2 建立 Provider-owned `CredentialStore`；refresh/access token 使用 `0600` 原子持久化，OAuth state/PKCE verifier 仅驻留登录 Session 内存，client JSON 保持为 Provider 私有配置文件。
- [x] P21.3 Google 增加 Installed App OAuth 2.0 Authorization Code + PKCE + loopback callback；现有 Service Account 继续作为高级/自动化 Profile。
- [~] P21.4 Google OAuth 已自动刷新并处理 refresh token 缺失、撤销式 refresh 拒绝、端口占用和五分钟回调超时；scope 不足分类和对应测试待补。
- [ ] P21.5 Aliyun 原生首次登录只在用户已有获批企业应用的 `client_id/client_secret` 时启用，并通过官方授权流程获取首个 refresh token。
- [ ] P21.6 个人 Aliyun 不调用 AList 公共 token broker；新增独立 AList/OpenList Bridge Provider 设计与最小连接验证。
- [ ] P21.7 Settings 显示 `SignedOut / Authorizing / Ready / ReauthRequired / EnterpriseAppRequired`，单 Provider 登录失败不阻塞其他 Provider。
- [~] P21.8 mock OAuth server 已覆盖 state/PKCE、loopback 回调、token 轮换和 `0600`；撤销、超时及更多失败响应脱敏测试待补。
- [ ] P21.9 使用现有 Google 账号完成首次 OAuth、Source 列表、小文件下载和重启后自动刷新验收；企业 Aliyun 保留凭据 Gate。

Gate：普通 Google 用户无需创建 Service Account 即可完成首次登录；认证状态可恢复、可撤销且不泄漏
凭据；Aliyun 对企业原生模式和个人 Bridge 模式做明确区分，不伪装个人官方 OAuth 可用。

## Phase 22：脱离 TUI 的 DownloadJob Runtime

- [x] P22.1 定义可序列化 `DownloadJobRequest`、`DownloadJobSnapshot`、`TransferSnapshot` 和稳定 Job/Transfer ID。
- [x] P22.2 将 Job ID 分配、计划、worker task、取消和事件广播移入 `DownloadManager`，TUI 只提交命令并订阅事件/快照。
- [x] P22.3 支持同时存在多个 Job；每个 Job 固定持有提交时的 Provider/Profile/Source 身份与 Provider 实例。
- [x] P22.4 使用有界 worker 事件通道、非阻塞广播和快照合并，订阅者断开不停止下载。
- [~] P22.5 已实现 Cancel、Interrupted 和 Provider 实例固定语义；Pause 与完整 Shutdown policy 尚未实现。
- [~] P22.6 TUI 不再持有/abort 单独 JoinHandle；daemon 断开语义已具备，embedded 退出策略仍需显式化。
- [~] P22.7 已覆盖多 Job、订阅重连、取消、恢复和 worker 上限；事件 lag 与 shutdown 竞争测试待补。

Gate：关闭或重建 TUI view 不改变 Job；所有下载行为可在无 Ratatui 的测试进程中提交、观察、取消和完成。

## Phase 23：SQLite Job/Transfer 持久化与恢复

- [x] P23.1 使用 `rusqlite` bundled 与 WAL 建立版本化 schema/migration，数据库默认位于 `~/.local/state/pan-tui/state.db`。
- [x] P23.2 持久化 Job 请求、状态、Provider 身份、目标、选项、汇总和时间戳；敏感凭据及下载 URL 不进入数据库。
- [x] P23.3 持久化 Transfer 路径、大小、已写字节、终态和错误，并以 500ms 窗口批量提交 Progress。
- [x] P23.4 启动恢复将遗留 Running/Queued 转为 Interrupted，并在 Provider 可用时使用原 Job ID 与 `.part` 续传语义重提。
- [~] P23.5 schema 过新和写入失败可诊断且不改动完成文件；损坏库只读诊断模式与磁盘满验收待补。
- [~] P23.6 已覆盖恢复、重复终态、WAL 重开、v1->v2 migration 和多 Job 查询；强杀进程与损坏库测试待补。

Gate：进程被强制终止后重启，Job/Transfer 历史仍可查询，未完成文件从可信 `.part` 状态恢复且不重复完成通知。

## Phase 24：Daemon 与本地 IPC

- [x] P24.1 同一二进制提供 `daemon`、`submit`、`status`、`cancel` 和 `tui`；TUI 通过统一 PanClient 连接 daemon 或 embedded runtime。
- [x] P24.2 Linux 使用 `0600` Unix domain socket 和版本化 JSON message protocol；协议层不暴露 trait object 或 Provider 专属结构。
- [x] P24.3 IPC v3 已实现 Provider 浏览与 snapshot event subscription；`client_id + request_id + request_json` 持久化幂等映射与 Job 同事务写入，冲突请求被拒绝。
- [x] P24.4 TUI 优先连接现有 daemon；无 socket 时使用 embedded foreground runtime，存在但不可用的 socket 不会触发并发打开 SQLite。
- [~] P24.5 已提供 `contrib/pan-tui.service`、Ctrl-C graceful shutdown、单实例 socket 和 stale socket 恢复；正式安装流程待验收。
- [x] P24.6 daemon 启动从 SQLite 恢复可用 Provider 的 Interrupted 队列，客户端生命周期不持有 worker task。
- [~] P24.7 已覆盖真实 Unix socket、`0600`、协议版本不匹配、幂等重提、订阅及 daemon 重启后的 PanClient 重连；并发客户端、强制 crash 和 PTY 重连待补。

Gate：TUI 退出后下载继续；重新打开 TUI 可观察同一 Job；CLI 与 TUI 通过同一 IPC 契约提交和控制任务。

## Phase 25：钉钉、飞书 Notifier 与可靠 Outbox

- [~] P25.1 已建立独立 `NotifierFactory`/`Notifier` 注册器和 schema 描述；Settings 通用编辑面板尚未接入。
- [x] P25.2 定义脱敏 `NotificationEvent`，只包含 JobCompleted、JobPartiallyFailed、JobFailed 和 ReauthRequired 公共字段。
- [x] P25.3 实现钉钉 webhook、timestamp/HMAC 签名、429/Retry-After 和脱敏响应错误分类。
- [x] P25.4 实现飞书 webhook、签名、限流和响应错误分类。
- [x] P25.5 SQLite v2 增加 notification outbox、attempt、next_attempt_at 和幂等键；Job 终态与 outbox 同事务提交。
- [x] P25.6 daemon 独立 worker 执行指数退避与确定性抖动，永久/耗尽失败进入 DeadLetter；sent 项不会再次投递。
- [ ] P25.7 Settings 脱敏编辑 webhook/secret，支持测试通知；日志不得输出 webhook URL、签名或消息中的本地敏感路径。
- [~] P25.8 已覆盖签名、钉钉 429/Retry-After、幂等终态和 outbox worker；飞书 mock、5xx、超时、重启与 DeadLetter 测试待补。

Gate：下载完成与通知入队原子一致；daemon 在通知发送前崩溃后仍会补发；同一终态不会产生重复成功通知。

## Phase 26：常驻模式集成与交付

- [x] P26.1 默认保持 TUI/CLI 一次性体验；后台下载、恢复和通知通过可选 daemon 启用，不强制所有用户常驻。
- [ ] P26.2 完成 Google OAuth、Service Account、Aliyun Enterprise、AList Bridge 的 Profile/Source 切换与故障隔离。
- [ ] P26.3 PTY 提交 workers >= 2 Job 后退出 TUI，确认 daemon 继续下载，重开 TUI 后状态与 SQLite 一致。
- [ ] P26.4 使用真实钉钉和飞书测试群分别验证成功、部分失败和认证失效通知，不提交 webhook secret。
- [~] P26.5 已更新 OAuth、daemon 优先连接、IPC v3、统一 state/log 目录、systemd 和通知配置；SQLite 备份、完整迁移和故障排查仍待补。
- [~] P26.6 fmt、Clippy、94 个测试、release、examples、隔离 HOME daemon smoke、真实 Google Source 列表和凭据输出复核已通过；Installed App 真实首次登录及本轮真实下载/续传仍待凭据与验收窗口。
- [x] P26.7 将 4000+ 行单文件 TUI 拆为 `mod.rs` 状态层、`runtime.rs` 运行循环、`render.rs` 渲染层和 `tests.rs` 回归测试，保持 `tui::run_browser` 等公共入口不变。
- [x] P26.8 提供库级 `DaemonService`、`DaemonClient` 和 `PanClient`；`pan-tui daemon` 仅组装依赖，TUI 与脚本 CLI 共用同一 IPC 客户端实现。

Gate：`pan-tui` 既可作为无需后台服务的一次性下载器，也可作为可恢复、可通知的长期下载服务；两种
模式共享同一 Provider、Job、持久化和错误语义。

## Phase 27：统一 PanClient、Provider IPC 与脚本 CLI

- [x] P27.1 定义 Provider-neutral catalog/settings DTO 和对象安全 `PanClient`，覆盖 Provider、Source、目录、元数据、设置、Job、取消、快照和订阅。
- [x] P27.2 实现 `ProviderRuntime`，集中管理 catalog、实例查找、单 Provider 保存/重建、失败保留旧实例和全局网络重连。
- [x] P27.3 将本地协议升级到 IPC v3，daemon 独占 resident 模式的 ProviderRuntime，并增加 Providers/Sources/ListChildren/RefreshItem/SaveSettings/ReconfigureNetwork。
- [x] P27.4 实现 `DaemonPanClient` 与 `EmbeddedPanClient`；embedded 恢复和 daemon 恢复使用相同 Provider 身份匹配规则。
- [x] P27.5 TUI 移除 `CloudProvider`、`ProviderFactory`、`DownloadManager` 和 `JobBackend` 依赖；只保留 Tab、Panel、Source Session、选择、滚动和表单草稿。
- [x] P27.6 增加 JSON CLI：`providers`、`sources`、`ls`、`stat`、`download`、`job`、`watch`；脚本与 TUI 使用同一 daemon 协议。
- [x] P27.7 测试 embedded/daemon 行为一致、Provider 重建失败保留旧实例、IPC catalog/source/stat、Job 幂等/订阅，以及下载/CLI 安全边界；94 个测试和全目标 Clippy 通过。
- [~] P27.8 已在真实 daemon 上完成 Google `providers/sources/ls/stat` 和 PTY TUI 浏览；CLI 小文件下载及 daemon 重启后 PTY 重连验收待补。

Gate：daemon 模式下 TUI 不创建第二套 Provider；CLI 与 TUI 对同一 Provider/Source/RemoteRef 和 Job
观察到一致结果；embedded 模式只替换 PanClient 实现，不改变上层交互和错误语义。
