# ShellPilot Rust 业务库

当前版本 **v0.2.3**，与同版本桌面客户端配套；发布与升级说明见 [v0.2.3](docs/releases/v0.2.3.md)。

内置 IPC 交互说明见兄弟客户端 [docs/ipc-transport.md](../ai-ssh-terminal-client/docs/ipc-transport.md)。桌面业务仅使用 invoke/Channel，应用不提供远程后端配置；本项目仅提供 Rust 库，没有独立 HTTP 程序入口。
当前模型协议支持 Chat Completions、Anthropic Messages 和 Responses 的文字、图片与工具续聊，按协议保存推理字段、思考签名和原生输出项。续接状态与 schema 22 的持久化说明见 [MODEL_PROTOCOLS.md](docs/MODEL_PROTOCOLS.md)。


Agent 重构当前进度与新窗口续接：见 [docs/AGENT_PROGRESS.md](docs/AGENT_PROGRESS.md)。

当前已支持 SSH / SFTP、本地附件、带审批的工具与子任务、MCP / Skills、长期记忆、上下文整理及原生历史。模型层支持多模型渠道、同会话模型切换和协议原生续接状态，数据库最新为 schema 24。早期 Preview 安装包及阶段验收记录仅用于历史追溯，不代表当前发布状态。

## 开发与使用

Windows 需要 Rust stable MSVC 和 Visual Studio C++ Build Tools / Windows SDK。

本项目作为 `shellpilot_server` 库使用，`src/main.rs` 和 Cargo 的独立二进制声明已移除，`autobins = false`。不再使用 `cargo run` 启动 HTTP 服务，也不再生成新的 `shellpilot-server.exe`。

可在本项目目录单独编译或测试业务库，无需启动前端：

~~~powershell
cargo build --lib
cargo test
cargo clippy --all-targets -- -D warnings
~~~

运行桌面应用需要同时取得以下两个仓库，正式客户端使用 `master`；重现发行版时应使用对应发布提交：

| 项目 | 仓库 | 分支 | 本地目录名 |
| --- | --- | --- | --- |
| 桌面客户端（React / Tauri） | [ai-ssh-terminal-client](https://github.com/jasonlat-dot/ai-ssh-terminal-client) | `master` | `ai-ssh-terminal-client` |
| Rust 业务库（本仓库） | [shell-pilot-rust](https://github.com/jasonlat-dot/shell-pilot-rust) | `main` | `ai-ssh-terminal-server-rust` |

**按当前默认配置，两个仓库需要放在同一个父目录下，彼此平级。** 它们是两个独立的 Git 仓库，不是把 Rust 库放进客户端内部。父目录的名称和位置可以自行决定：

~~~text
你的开发目录/
├── ai-ssh-terminal-client/
│   ├── package.json
│   ├── scripts/prepare-backend-bundle.ps1
│   └── src-tauri/Cargo.toml
└── ai-ssh-terminal-server-rust/
    ├── Cargo.toml
    └── src/
~~~

注意：本仓库的远端名称是 `shell-pilot-rust`，但当前客户端默认查找的本地目录名是 `ai-ssh-terminal-server-rust`。克隆时显式指定目标目录即可，不需要修改远端仓库名称。

首次准备时，在选定的父目录执行；已经克隆好的仓库不需要重复克隆：

~~~powershell
git clone --branch main https://github.com/jasonlat-dot/shell-pilot-rust.git ai-ssh-terminal-server-rust
git clone --branch master https://github.com/jasonlat-dot/ai-ssh-terminal-client.git ai-ssh-terminal-client
cd ai-ssh-terminal-client
npm ci
npm run prepare:backend
npm run tauri dev
~~~

`npm ci` 安装客户端依赖；`npm run prepare:backend` 检查 Rust 库位置并准备说明/许可证资源，不会启动或单独编译后端；`npm run tauri dev` 启动开发界面，并由 Cargo 把 Rust 库编译进桌面程序。开发环境准备好后，后续通常只需在客户端根目录运行 `npm run tauri dev`。如果已经位于本仓库根目录，默认布局下先执行 `cd ../ai-ssh-terminal-client`。

如果希望使用其他目录名或非平级结构，需要同步调整客户端的两处路径，确保都指向同一份 Rust 业务库：

- `src-tauri/Cargo.toml` 中的 `shellpilot-server = { path = "../../ai-ssh-terminal-server-rust" }`：`path` 相对于客户端的 `src-tauri` 目录计算，指向包含本库 `Cargo.toml` 的目录。
- `scripts/prepare-backend-bundle.ps1` 中的 `$serverManifest = Join-Path $clientRoot '..\ai-ssh-terminal-server-rust\Cargo.toml'`：这里相对于客户端根目录计算，指向本库的 `Cargo.toml` 文件；该脚本也会从这个位置复制许可证，打包时会调用它。

只修改 Cargo 依赖路径，会导致资源准备/打包脚本仍按旧布局查找；只修改脚本，则 Cargo 仍找不到业务库。保留上面的默认平级结构可以省去这些修改。

Tauri 初始化 `Services::native`，前端通过 invoke/Channel 进入 Tauri Rust 桥，再直接调用本库函数。无需另外启动 Rust 服务；桌面业务不监听本地 HTTP 端口，不提供远程后端配置。

数据目录由桌面设置管理，默认 SQLite 与附件位于 `%LOCALAPPDATA%\com.jasonlat.shellpilot\backend-data`。数据与原 Java 版相互独立，尚无 MySQL 数据导入工具。

## 历史 HTTP 协议回归参考（非程序启动入口）

| 方法 | 路径 | 功能 |
|---|---|---|
| GET | /api/health | status=UP、checkedAt |
| GET | /api/v1/ssh/connection_list?userId=default | 按用户筛选连接 |
| GET | /api/v1/ssh/get_connection?connectionId=... | 查询连接 |
| POST | /api/v1/ssh/create_connection | 创建连接 |
| POST | /api/v1/ssh/update_connection | 更新连接 |
| POST | /api/v1/ssh/delete_connection?connectionId=... | 删除连接；活动终端存在时拒绝 |
| POST | /api/v1/ssh/terminal/connect | connectionId/requestId/cols/rows -> sessionId/connectionId/initialOutput |
| POST | /api/v1/ssh/terminal/cancel_connect?connectionId=...&requestId=... | 取消指定建连请求，支持取消先于 connect 到达 |
| POST | /api/v1/ssh/terminal/write | sessionId/input 原始终端输入 |
| GET | /api/v1/ssh/terminal/read?sessionId=... | 长轮询输出与断线状态 |
| GET | /api/v1/ssh/terminal/connected?sessionId=... | 窗口级状态 |
| POST | /api/v1/ssh/terminal/resize | sessionId/cols/rows |
| POST | /api/v1/ssh/terminal/close?sessionId=... | 关闭独立 shell 通道，幂等 |

下表对应库内保留的历史协议测试适配，不是当前桌面调用链或可启动的服务。桌面调用使用 IPC 和直接函数调用；数据字段沿用 camelCase，查询和响应不返回 password/privateKey。

参数错误 S0002，连接不存在 S0001，会话不存在或已关闭 S0003。SSH 错误分别为 SSH_CONNECT_CANCELLED、SSH_CONNECT_TIMEOUT、SSH_AUTH_FAILED、SSH_HOST_KEY_REJECTED、SSH_TRANSPORT_FAILED、SSH_CHANNEL_OPEN_REJECTED、SSH_BUSY。

## SSH 行为

- 使用 russh 进行原生 SSH，不调用系统 ssh.exe，也不启动 JVM。
- 密码或 OpenSSH/PEM 私钥认证；密钥算法支持 Ed25519、ECDSA nistp256/384/521 和 RSA-SHA2。禁用 RSA/SHA-1 主机签名降级。
- 一个 connectionId 使用有界的已认证传输池，默认每条传输最多 8 个窗口、每个配置最多 32 条缓存传输；每个 sessionId 是独立 PTY/shell 通道。服务器拒绝已有传输上的新通道时，记录观察到的容量并在新传输上最多重试一次，发生在 PTY/shell/startupCommand 之前。关闭或取消单个窗口不会正常关闭其他窗口。
- PTY 类型 xterm-256color；初始 120 列、24 行，尺寸允许 1..4096，支持 window-change。
- connectTimeout 默认 10 秒（允许 1..300），覆盖 DNS/TCP、SSH 密钥交换、认证和 PTY/shell 设置；同连接的配置和建连由生命周期锁串行处理。
- keepaliveInterval 默认 60 秒，0 关闭保活；连续 3 次无响应关闭传输。
- compression 默认 false；开启时优先协商 OpenSSH 延迟 zlib 压缩。startupCommand 在 shell 成功建立后写入，补一个换行。
- requestId 必须每次建连唯一，最长 128 字节。取消记录保留至少 5 分钟；完成后取消仍可关闭本次对应窗口。丢弃连接 future 同样触发清理。
- 通道确认由独立、有界的任务持有，HTTP 等待者取消后迟到的通道会被单独关闭。服务器超过 connectTimeout 仍不确认通道时，视为传输失效并关闭底层 TCP；同传输的其他窗口会收到断线状态。
- read 最长等待 25 秒，同窗口新 read 替换旧 read 返回 REPLACED；输出按 UTF-8 连续解码，缓存上限 1 MiB，丢弃最旧完整字符并设置 bufferOverflow。
- 默认连续 30 分钟没有输入、窗口调整或输出时，以 IDLE_TIMEOUT 关闭。轮询状态不延长空闲计时。
- DATA/TIMEOUT/REPLACED 时 disconnectReason 为 null；结束后保留最后输出供读取，再返回 DISCONNECTED 或 READER_ERROR。CHANNEL_DISCONNECTED/READER_ERROR 允许有限自动重连，CLIENT_CLOSED/IDLE_TIMEOUT/SESSION_NOT_FOUND 不允许。
- 已结束窗口的状态保留 5 分钟，启动新连接和状态查询时清理。最多 256 个活动/建连窗口、64 个未确认通道、4096 条取消/请求记录。
- 修改连接配置不会断开既有窗口，新窗口使用新配置和凭据；删除配置须先关闭全部窗口。
- serve 的关闭 future、TerminalManager::shutdown 和 manager 释放均清理终端与底层网络资源。

## 主机身份和凭据

strictHostKeyCheck 默认 true，必须提供 knownHosts。实际连接时解析并匹配主机、端口与公钥数据，支持逗号列表、* / ? 通配符、! 否定模式、OpenSSH |1| 哈希名称，忽略空白和注释。非 22 端口使用 [host]:port；@revoked 优先拒绝。主机证书和 @cert-authority 暂不支持，在严格模式下拒绝，不能仅凭证书内嵌公钥当作认证通过。关闭严格检查代表显式接受未知主机密钥。

SQLite 随程序构建。密码/私钥使用 Windows 用户级 DPAPI 加密为 BLOB，绑定连接 ID 和凭据类型。更新时密码/私钥空字符串或缺省保留原值，高级设置缺省保留原值。authType=2 时 password 可作为加密私钥口令；目前客户端私钥表单没有该口令输入，未加密私钥可直接使用。

数据库只能由对应 Windows 用户环境解密；复制到另一机器/账号不能直接恢复凭据。非 Windows 平台目前拒绝存储有凭据的连接，不回退明文。缺省 userId 使用 default，匹配客户端；与 Java 创建接口的历史 defaultUser 有差异。

当前由单用户桌面应用持有业务服务实例。userId 筛选不是身份认证，不支持作为多人远程服务。日志不记录请求凭据，数据库操作使用阻塞工作线程和事务。

## 嵌入与打包

本项目只输出 `shellpilot_server` 库。客户端通过 Cargo 路径依赖将它编译进 `ShellPilot.exe`，无需 JRE/JAR 或独立后端 exe。

试用版拥有独立应用标识和数据目录，业务、健康检查和附件保存使用 IPC，无本地 HTTP 监听端口。支持本地工作区自动初始化、停止、重新初始化与数据设置。SSH/SFTP 直接连接目标服务器；AI 对话和模型附件处理尚未迁移。

客户端目录中的 `npm run bundle:nsis` 可生成安装包，但修改业务代码后的开发验证只需重新运行 `npm run tauri dev`。

安装说明见兄弟客户端的 docs/INSTALL.md，详细迁移清单见 docs/MIGRATION.md。以下旧二进制与 HTTP 验证记录是历史证据，不代表当前源码仍提供独立程序。

## 验证范围

测试覆盖连接配置 CRUD/DPAPI/持久化，原生 SSH 的密码和私钥认证、Ed25519/ECDSA/RSA、PTY/shell 请求、原始读写、resize、多窗口、取消前后和 future 丢弃、握手超时后 TCP 关闭、迟到通道清理、错误主机密钥/认证、配置更新、长轮询替换、Unicode/缓存溢出、空闲回收、远端 EOF/传输断开和程序关闭，并验证全部终端 HTTP 端点的客户端协议。

SSH 集成测试使用临时回环端口、随机测试密钥和本地 russh 协议测试服务，不访问用户服务器。测试服务接受 PTY/shell 请求并回显数据，不启动真实系统 shell；仍需实际 OpenSSH/系统 shell 和目标远端服务器联调。AI Agent 接口返回 HTTP 501 / FEATURE_NOT_MIGRATED；健康 UP 仅表示 HTTP 服务可用。

## 第二阶段验证记录

2026-10-13，Windows x64 / Rust 1.99.0：
- cargo test：31 项测试通过（4 单元 + 9 基础 API/存储 + 18 SSH 集成）。
- cargo fmt -- --check、cargo clippy --all-targets -- -D warnings 通过。
- cargo build --release 通过；shellpilot-server.exe 为 5019136 字节（约 4.79 MiB）。
- Release 程序实际启动后，健康接口、terminal connected/read、取消先于 connect、连接删除及未迁移 SFTP 的 HTTP 501 行为验证通过。验证使用隔离数据目录，完成后测试进程已停止。
- 第二阶段未连接用户真实 SSH 服务器，也未执行真实操作系统 shell/PTY；当时尚未集成桌面包。

## Rust Preview 桌面集成记录

2026-10-13：客户端 test-rust-ui 分支生成 ShellPilot-Rust-Preview_0.2.0_x64-setup.exe（8134746 字节）。
Rust 服务与 Tauri 前端同进程，支持动态端口和独立 SQLite/DPAPI 数据目录，无 Java 运行依赖。
前端 65 项、原生 11 项测试通过；Release 桌面程序启动、同进程 HTTP 健康检查、连接 CRUD、退出端口释放验证通过。
未自动点击完整 GUI，未运行安装器，也未连接真实用户远端；SFTP、文件/附件和 AI Agent 仍待迁移。
## 阶段 3：SFTP
已接入现有 Tauri 文件桥：独立会话、目录操作、清单、二进制上传下载、确认、重试、取消、SSE 及容量/闲置回收。详细接口和边界见 docs/SFTP.md。
验证通过：后端 44 项测试与 Clippy、客户端原生 14 项测试（含 3 项真实 Rust 后端联调）、前端 65 项测试及生产构建。
以前第二阶段与 0.2.0 的验证记录仅反映当时范围；当前 SFTP 与独立文件/附件已迁移，下一阶段为 AI/Agent。

0.3.0 安装包已生成（8302862 字节），Release 桌面程序实际启动通过：健康 UP、内嵌 SFTP 路由、连接列表与退出端口回收正常。主程序 16544256 字节。内嵌服务退出时活动传输和 SSE 清理的第三项原生联调通过。

## 阶段 4：文件与附件

Rust Preview 0.4.0 支持本地流式上传、SHA256 元数据、签名下载/过期刷新、分页历史、删除与容量回收、可配置保留和存储限额。前端顶部“文件与附件”及设置页已同步。附件目录使用实例锁并拒绝 junction/reparse 路径，恢复只清理数据库所属对象。
默认 20 MiB/文件、1 GiB 总容量、长期保留、15 分钟链接；详细协议、设置和边界见 docs/FILES.md。
后端合计 60 项测试和 Clippy/fmt、前端 69 项回归及 1 项真实 Rust 服务联调通过。AI 对话、模型附件处理及 Agent 工具仍待阶段 5。
0.4.0 NSIS 安装包已生成（8399302 字节）；成品桌面同进程健康、附件上传/下载/删除和 SFTP 路由验证通过，停止测试进程后端口释放。实际安装与完整 GUI 由用户验证。

## Agent 重构首批内核

src/agent 已加入 SQLite 会话/任务/原始事件日志、目标与约束快照、请求幂等回执、模型请求预算和检查点版本校验。数据库自动升级到 schema 3，保留既有 SSH 与附件表。
检查点不替换原始事件，未知工具结果继续保留为不确定动作。专门测试 17 项通过；模型摘要生成、自动压缩、AI 对话和工具执行尚未接通，不对用户启用 AI。
实施与续接清单见 docs/AGENT_PROGRESS.md；每个完成项记录代码和验证证据。
## Agent 工具输出内核

新增 SQLite 原始字节分块捕获、SHA256 与按范围回查；工具结果保存有界预览和经校验的输出引用，检查点后仍可回查完整证据。取消/失败清理未发布数据，已引用证据保持不可删除。
数据库迁移到 schema 4，保留旧 ToolResult 格式；具体接口、配额与恢复边界见 docs/AGENT_OUTPUTS.md，当前进度见 docs/AGENT_PROGRESS.md。
本项仍是内部库能力，模型与前端 AI 入口尚未接通。
2026-10-13 移除桌面内置 HTTP 监听：本轮只改源码与文档，用户自行运行验证，未编译、测试或打包。随后已移除独立二进制入口，当前只输出业务库。

桌面当前版本不提供远程后端模式。SSH/SFTP 直接连接目标服务器，未来 Agent 由本地 Rust 调用模型 API。移除远程后端的此次改动按用户要求仅改源码，尚未编译测试。

2026-10-13：清除旧独立 HTTP 程序入口，删除 src/main.rs 和 Cargo [[bin]]，禁用自动二进制发现。本轮未编译、测试、打包或提交 Git。
