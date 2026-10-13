# ShellPilot Rust Server

Agent 重构当前进度与新窗口续接：见 [docs/AGENT_PROGRESS.md](docs/AGENT_PROGRESS.md)。

Java 服务端的分阶段 Rust 重构，目标是随 Tauri 桌面客户端打包，移除 JRE 和独立 MySQL 服务依赖。当前已实现连接配置管理、原生 SSH 终端、SFTP 文件管理/传输及本地文件/附件存储。AI Agent 尚未迁移。客户端 test-rust-ui 分支现已将本库直接嵌入 Tauri，并生成独立的 Rust Preview 0.4.0 Windows 安装包。

## 开发与运行

Windows 需要 Rust stable MSVC 和 Visual Studio C++ Build Tools / Windows SDK。

~~~powershell
cargo test
cargo clippy --all-targets -- -D warnings
cargo run
~~~

从兄弟目录 ai-ssh-terminal-client 的 test-rust-ui 分支也可运行：

~~~powershell
npm run dev:rust-server
npm run build:rust-server
~~~

默认监听 http://127.0.0.1:8889，避免与 Java 8888 端口冲突。客户端中新增自定义后端地址 http://127.0.0.1:8889，可使用 SSH 连接管理和终端。根路径 / 不是网页；健康检查在 /api/health。

数据保存在 %LOCALAPPDATA%\ShellPilot\rust-server\shellpilot.sqlite3。覆盖配置：

~~~powershell
$env:SHELLPILOT_PORT = '8890'
$env:SHELLPILOT_DATA_DIR = 'E:\project\ShellPilot\rust-data'
cargo run
~~~

两个后端数据相互独立，尚无 MySQL 数据导入工具。

## 已迁移接口

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

沿用客户端 camelCase 字段和 {code, info, data} 响应，成功码 SUCCESS_0000。错误同时使用 HTTP 4xx/5xx；现有客户端支持这些错误响应。查询和响应不返回 password/privateKey。

参数错误 S0002，连接不存在 S0001，会话不存在或已关闭 S0003。SSH 错误分别为 SSH_CONNECT_CANCELLED、SSH_CONNECT_TIMEOUT、SSH_AUTH_FAILED、SSH_HOST_KEY_REJECTED、SSH_TRANSPORT_FAILED、SSH_BUSY。

## SSH 行为

- 使用 russh 进行原生 SSH，不调用系统 ssh.exe，也不启动 JVM。
- 密码或 OpenSSH/PEM 私钥认证；密钥算法支持 Ed25519、ECDSA nistp256/384/521 和 RSA-SHA2。禁用 RSA/SHA-1 主机签名降级。
- 一个 connectionId 复用一条已认证传输，每个 sessionId 是独立 PTY/shell 通道。关闭或取消单个窗口不会正常关闭其他窗口。
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

仅允许回环监听和已列出的 Tauri/Vite 来源。userId 筛选不是身份认证，当前面向单用户桌面应用，不支持作为多人远程服务。日志不记录请求凭据，数据库操作使用阻塞工作线程和事务。

## 嵌入与打包

同一 crate 输出 shellpilot-server.exe 和 shellpilot_server 库。公开 serve(listener, Arc<Store>, shutdown) 入口支持宿主绑定动态端口 0、发布实际端口和管理退出。TerminalManager 同样可以由 Tauri 直接持有。

~~~powershell
cargo build --release
~~~

输出 target/release/shellpilot-server.exe，可独立运行。
客户端 test-rust-ui 分支的 npm run bundle:nsis 则将本库编进 shellpilot-rust-preview.exe，安装包无需 JRE/JAR 或独立后端 exe。
试用版拥有独立应用标识和数据目录，默认绑定 127.0.0.1:0，支持自动启动、停止、重启和设置；内置模式已启用 SFTP 文件管理及独立附件管理；AI 对话和聊天模型附件处理尚未迁移。
安装说明在兄弟客户端的 docs/RUST_PREVIEW_INSTALL.md，详细迁移清单见 docs/MIGRATION.md。

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