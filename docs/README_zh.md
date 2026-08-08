# net-pan

[English](../README.md) | 简体中文

`net-pan` 是一个 Provider 无关的终端网盘下载工具，可通过 TUI 浏览云盘并将文件或目录下载到
本地。项目同时提供脚本化 CLI、持久化任务、可选 daemon、断点续传和下载完成通知。

## Provider

| Provider | 状态 |
| --- | --- |
| Google Drive | 浏览、元数据、下载、续传、Installed App OAuth、服务账号 |
| 阿里云盘 | OAuth 刷新、云盘信息、浏览、元数据、下载、续传 |

阿里云盘目前已通过可控 mock 测试，真实账号验收仍在进行。Google Docs、Sheets 和 Slides
暂未实现导出格式，因此会记录为 Skipped。

## 安装

使用下面的命令安装最新 Linux 或 macOS 版本。安装器会校验 SHA-256：

```sh
curl -fsSL https://github.com/rainysteven1/net-pan/releases/latest/download/install.sh | sh
```

默认安装到 `~/.local/bin/net-pan`。可自定义安装目录：

```sh
curl -fsSL https://github.com/rainysteven1/net-pan/releases/latest/download/install.sh \
  | NET_PAN_INSTALL_DIR="$HOME/bin" sh
```

当前发布平台：

- Linux x86_64
- macOS x86_64
- macOS arm64

压缩包、SHA-256 校验文件和 Homebrew formula 可在
[Releases](https://github.com/rainysteven1/net-pan/releases) 页面下载。

## 快速开始

直接启动 TUI：

```sh
net-pan
```

首次启动会创建 `~/.net-pan`，并为内置 Provider 创建未启用的草稿账号。在 `Accounts` 中创建或
激活账号，在 `Settings` 中配置账号，在 `Files` 中浏览 Source。按 `g` 打开全局下载目录和代理设置。

全局下载目录默认为 `~/Downloads`，必须是已经存在的绝对路径，且不能是符号链接。默认跳过已经
完整存在的本地文件；Provider 支持时，程序会通过 HTTP Range 续传 `.part` 文件。

## Google 配置

每个账号拥有独立且稳定的 Profile 目录：

```text
~/.net-pan/providers/google/<PROFILE_ID>/
```

使用 Installed App OAuth：

1. 启用 Google Drive API，并创建 Desktop OAuth 客户端。
2. 将客户端 JSON 放到 Profile 目录的 `oauth-client.json`。
3. 在 `sources.json` 中添加一个或多个 Google Drive 文件夹 ID 或链接。
4. 在 TUI 中完成账号配置，然后执行：

```sh
net-pan auth login google <PROFILE_ID>
net-pan auth status google --profile <PROFILE_ID>
```

也可以使用服务账号。JSON 密钥文件权限应设为 `0600`，并需要将 Source 文件夹共享给服务账号
邮箱，然后在 Provider Settings 中选择服务账号模式。凭据、refresh token 和签名下载 URL 不会
写入日志。

`sources.json` 示例：

```json
{
  "sources": [
    "https://drive.google.com/drive/folders/FOLDER_ID",
    "ANOTHER_FOLDER_ID"
  ]
}
```

## TUI 按键

| 按键 | 操作 |
| --- | --- |
| `Up` / `Down` | 在当前列表中移动 |
| `Enter` / `Right` | 打开选中的页面或目录 |
| `Backspace` | 返回上一级 breadcrumb |
| `Left` | 聚焦左侧 Navigation |
| `[` / `]` | 切换 Provider 类型 |
| `d` | 下载选中的文件或目录 |
| `o` | 打开 Source 选择器 |
| `r` | 刷新当前 Source 或目录 |
| `l` | 打开运行日志 |
| `g` | 打开全局设置 |
| `Space` | 暂停或恢复当前任务 |
| `x` | 停止当前任务 |
| `q` / `Ctrl-C` | 确认退出 |

Logs 页面使用 `a/i/e` 筛选日志级别，使用 `f` 筛选 target，使用 `Tab` 在列表和详情间切换。
Provider、Source、daemon、settings 和 download 事件统一记录在按时间排序的 Session 日志中。

## Daemon 与 CLI

启动长期运行的 daemon：

```sh
net-pan daemon
```

存在可用 daemon 时，TUI 会连接 daemon；否则使用内嵌 runtime。daemon 管理的任务不会因为退出
TUI 而停止。

常用脚本命令：

```sh
net-pan status
net-pan providers
net-pan sources google --profile <PROFILE_ID>
net-pan ls google <SOURCE_ID> <OBJECT_ID> --profile <PROFILE_ID>
net-pan stat google <SOURCE_ID> <OBJECT_ID> --profile <PROFILE_ID>
net-pan download google <SOURCE_ID> <OBJECT_ID> /absolute/destination \
  --profile <PROFILE_ID> --workers 2
net-pan job <JOB_ID>
net-pan watch
net-pan pause <JOB_ID>
net-pan resume <JOB_ID>
net-pan cancel <JOB_ID>
```

Provider、Source 和 object ID 都是不透明标识，应从前一个 JSON 响应获取，不要解析 Provider
专属 URL。用户级 systemd service 示例位于 `contrib/net-pan.service`。

## 通知

daemon 从 `~/.net-pan/notifiers.toml` 加载钉钉和飞书 webhook Notifier。任务结束事件会先持久化到
SQLite outbox，再进行有限次数重试和幂等投递。该文件应设置为 `0600`。

## 本地数据

所有运行数据都位于 `~/.net-pan`：

```text
settings.toml
providers/<provider>/<profile>/config.toml
state/state.db
state/net-pan.sock
logs/YYYY-MM-DD/net-pan_HH-mm-ss_NNN.log
notifiers.toml
```

Provider 目录权限为 `0700`；配置、token、数据库、socket 和日志文件使用私有权限。程序不会读取
系统 `HTTP_PROXY`、`HTTPS_PROXY` 和 `NO_PROXY`，请在 TUI 中配置全局代理或 Provider 代理。

## 从源码构建

```sh
git clone https://github.com/rainysteven1/net-pan.git
cd net-pan
cargo build --release
./target/release/net-pan
```

运行完整检查：

```sh
./scripts/check.sh --full
```

发布和打包说明见 [releasing.md](releasing.md)。

## License

项目使用 MIT License，详见 [LICENSE](../LICENSE) 和
[THIRD_PARTY_NOTICES](../THIRD_PARTY_NOTICES)。
