# Java -> Rust 迁移清单

参考基线：Java server 0ed9681；client b3be20f，实验分支 test-rust-ui。
Rust 新项目与两个原项目并列，未替换 Java 源码。

**当前状态（2026-10-14）**：A0–A6、B1–B5、C1–C6（含持久项目 C4R）已完成当前文字阶段，AI 面板与真实整理状态已开放，数据库 schema 11。C6 补齐缩窗/新约束/中断恢复及附件拒绝边界，修复待回复用户补充遗漏和中断状态误标；下一项 D1。真实媒体处理/预算验证仍归 E1，工具执行/审批仍待 D。本文早期阶段、HTTP 与打包记录属于历史；当前架构、完成标记和验收边界统一以 [AGENT_PROGRESS.md](AGENT_PROGRESS.md) 为准。

## 架构

React/xterm -> Tauri invoke/Channel -> 同进程 Rust 业务库 -> SQLite / russh / 后续模型 HTTP。
桌面运行不提供本地 HTTP 监听、远程后端配置或独立 server 二进制。库内历史 HTTP 适配仅作为回归参考，不是当前部署或前端调用方式。

## 状态

| 阶段 | 状态 | 范围 |
|---|---|---|
| 1 | 已实现并验证 | 健康检查；连接配置 CRUD；SQLite；DPAPI；lib/bin |
| 2 | 已实现；31 项测试、Clippy、release 构建及 HTTP 验证通过 | 原生 SSH、密码/私钥、known_hosts、PTY、读写/resize、多窗口、取消、断线/长轮询状态、资源清理 |
| 3 | 已实现并联调验证 | 独立 SFTP 会话、目录操作、流式上传下载、任务/确认/取消/重试、SSE、容量和闲置回收；已恢复前端文件管理 |
| 4 | 已实现并联调验证 | 本地附件流式上传、元数据/历史、签名链接/刷新、大小/类型/容量限制、删除/保留策略、设置；前端已同步 |
| 5 | 实施中：A/B 与当前文字 C1–C6 已完成，AI 文字面板已开放 | 上下文内核、模型设置、流式文本/停止/恢复、自动整理、Responses、持久项目、占用/整理状态与连续边界恢复；下一项 D1，附件/工具/记忆继续推进，见 AGENT_PROGRESS.md |
| 6 | 试用包已生成，成品启动验证通过；用户安装和完整 GUI/远端验证待完成 | Tauri 内嵌库、自动启动/停止/重启与设置、动态端口、独立应用/数据、移除 JRE/JAR；暂禁用未迁移入口 |

## 阶段 2 实现和边界

src/terminal 中实现 HostVerifier、TerminalManager、有界输出状态和 HTTP 路由。每个连接的配置与建连按锁串行处理，传输复用，终端通道独立。取消的 socket、PTY、迟到 channel 和已发布窗口分别由资源 guard 与请求记录清理。窗口关闭后保留原因，结束后不允许 CLIENT_CLOSED、IDLE_TIMEOUT 或 SESSION_NOT_FOUND 自动重连。

测试覆盖纯 Rust 本地 SSH 协议，包括密码、Ed25519/ECDSA nistp256/384/521/RSA 私钥和服务器密钥。测试服务不启动系统 shell，不能替代真实 sshd + 操作系统 PTY 验证；本轮没有连接用户真实远端，也没有实际 Tauri UI 联调。

known_hosts 支持直接密钥、host:port、hash、wildcard/negation 和 revoked；证书/CA 在严格模式下拒绝。RSA 只使用 SHA-2。连接 HTTP 错误使用语义码与相应状态，符合客户端 ApiRequestError 处理。

默认 long-poll 25 秒、缓存 1 MiB、空闲回收 30 分钟，库层 TerminalSettings 可配置。连接仍按桌面单用户访问设计。

## 阶段 3：SFTP

已通过 12 项真实协议集成测试和 3 项客户端原生桥到实际 Rust 后端的端到端测试。
后端合计 44 项、原生合计 14 项、前端 65 项回归通过；前端 test-rust-ui 已恢复文件管理并处理过期会话。
详见 SFTP.md。SSE 使用完整快照恢复，无事件重放；目前无暂停或断点续传接口。

## 阶段 4：文件与附件

已实现本地附件存储及前端独立入口，详见 FILES.md。后端 60 项测试（含 16 项附件）、前端 69 项回归与 1 项真实 Rust 服务联调通过。0.4.0 使用同进程后端，旧配置自动采用附件默认值；已存文件历史与刷新链接可跨重启恢复。

## 下一步：AI 与 Agent

对照 Java Agent 配置、session、chat_stream/stop_chat，迁移模型请求、流式错误与取消、历史和附件读取，再处理工具执行、命令确认与记忆。前端与后端每阶段同步更新。
上下文压缩、工具决策与目标绑定的候选设计及源码证据见 CODEX_AGENT_REFERENCE.md；该文档是设计参考，尚未实现为 Rust Agent 功能。
附件存储完成不等于 AI/多模态已完成；open_attachment 提供受校验的本地读取入口，尚未调用模型或自动执行命令。
仍需真实 OpenSSH/系统 shell、用户远端及完整 GUI 联调；当前协议测试不替代该验证。

## 数据迁移与打包

MySQL 尚不自动读取；一次性迁移需处理 Java 凭据加密格式、高级配置、聊天记录和附件，导入前备份并设计冲突策略。

客户端 test-rust-ui 的 src-tauri/src/embedded_backend.rs 已适配 bundled_backend_* 命令和内嵌库生命周期，前端已使用 Rust 服务设置页和动态地址。原 Java 启动器源码保留作参考，试用版编译不使用它。

当前源码依赖兄弟目录的 shellpilot_server crate，由 Tauri 直接初始化 Services::native，无 HTTP 监听端口；安装资源仅包含说明与后端许可证。src/main.rs 与独立二进制声明已移除。旧试用安装包的动态端口验证属于历史记录，当前源码尚未重新打包。正式替代全部 Java 功能仍需完成阶段 5。

2026-10-13：A6 内核组合测试与任务目标绑定接口已补强，40 项针对 Agent 的测试和前端 TypeScript 检查通过。真实 SSH 执行去重的联验仍依赖 D3；下一步为 B1 模型适配与配置，保持 AI 入口关闭。

2026-10-14：B1/B2 已完成模型配置与基础文本对话 IPC/Channel。本轮 Rust 43 项、原生 IPC 7 项、前端 27 项与 TypeScript 检查通过；下一步 B3 流式竞态/错误矩阵与轮次恢复。B4 模型设置页面和 AI 面板入口仍未开放，完整记录见 AGENT_PROGRESS.md 和 AGENT_CHAT.md。

2026-10-14：B3 已完成 schema 6 轮次、请求幂等、停止/完成事务排序与历史恢复；针对性 Rust 56 项、原生 IPC 9 项、前端 32 项通过。下一项 B4 模型设置及可见基础文本 AI 面板，详细边界见 AGENT_TURNS.md。
