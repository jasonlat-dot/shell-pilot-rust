# Rust SSH Agent 实施清单与续接入口

更新日期：2026-10-15。状态：阶段 5 实施中，模型设置、文本对话、持久项目工作区及 C1–C6 已完成当前文字阶段；D1 已完成 Chat Completions 原生工具循环内核，尚未接入桌面聊天。下一项 D2：SSH 工具目标绑定与实时复核。
用户要求：参考 Codex CLI 优化上下文和工具决策；先列清单，每完成一项更新标记，供换窗口后续接。当前不打包。需要修改前端的阶段同步修改 test-rust-ui。
源码参考与设计依据：[CODEX_AGENT_REFERENCE.md](CODEX_AGENT_REFERENCE.md)。本文件是进度唯一入口，完成标记必须有代码与验证证据，不以“已设计”替代“已实现”。

Rust 重构交接、已完成工作与运行方式见 [WORK_HANDOFF_2026-10-14.md](WORK_HANDOFF_2026-10-14.md)。

## 工作区与约束

- 客户端分支 test-rust-ui，最新已核对提交 9585898（聊天输入区、上下文展示与终端交互）；Rust 业务库分支 main，最新提交 08960fd（C6 补充消息与中断状态修复）。本轮 D1 开始前两个工作区均干净；本轮修改未提交，远端推送状态未核对。
- 项目、参考源码和工具位置由本机环境决定；跨电脑续接时按实际位置核对 Cargo 依赖，不沿用旧电脑的绝对路径。
- 当前源码未重新打包；已有 0.4.0 安装包不能代表最新功能。
- 保留各仓库已有未提交变更；不连接用户远端、不执行实际变更命令来做测试。
- 里程碑、测试结果和续接状态统一维护本文件，不再额外创建逐阶段验证文档；commit note 直接发给用户，不保存到文档。
- SSH 任务绑定目标，切换 UI 页签不能改变在途任务目标。权限来自可信应用状态，模型摘要不产生权限。
- 当前主要验证是临时数据库/回环协议测试；真实模型、完整 GUI 和用户远端验证单独记录。
- 后续前端变更需要同时做好功能和样式，优先复用客户端 main 分支的设计系统、Select/Ui 控件与主题参数；相关界面需检查明暗主题、窄窗口和实际交互。
- 最新 UI 要求覆盖此前撤销布局时的约束：参考用户新截图，左侧上下分区放置会话历史和 SSH 连接，中央 Agent 对话，右侧终端；本地工作区、模型设置、外观和关于与“终端”同级放在左侧主导航，取消设置页内重复分类栏。只调整页面，沿用现有业务逻辑。

## 实施清单

### A. 可续接基础与上下文内核
- [x] A0 建立持久进度文件、源码参考和新窗口续接入口。
- [x] A1 Rust Agent 会话/任务模型：目标绑定、版本、约束，跨会话隔离。
- [x] A2 SQLite 原始事件日志：顺序追加、幂等请求 ID、工具调用/结果关联、取消与未知结果、重启恢复。
- [x] A3 请求预算器：模型窗口、输出/系统/工具/媒体/安全预留、UTF-8 估算、完整工具组选择和硬上限。
- [x] A4 检查点持久化：结构化目标/约束/证据/待办，版本快照与比较提交，压缩中用户变更/取消不能被覆盖。
- [x] A5 工具完整输出存储与按范围回查；只将有界结果投入模型窗口。
- [x] A6 内核测试：多会话、错误主机、任务继续、重复副作用、超限、过期检查点、重启与保留原始证据。内核组合回归已通过，用户已手动确认勾选；真实 SSH 执行副作用去重的最终联验仍归 D3。

### B. 模型配置与基本对话
- [x] B1 模型适配器与配置：供应商能力、模型窗口、API 地址/密钥安全保存、基础参数校验。OpenAI 兼容 Chat Completions 非流式适配与回环测试完成；前端配置入口已由 B4 接入。
- [x] B2 对接 agent 配置列表、create_session、chat_stream 和 stop_chat；保留现有前端事件协议。基础文本流式对话与本地 IPC/Channel 测试完成，UI 入口已由 B4 接入。
- [x] B3 流式取消/超时/错误、同会话并发隔离、历史与恢复，不将 EOF 当成功。持久轮次、请求幂等、停止/完成事务排序及租约恢复已实现并通过回环测试；可见历史面板已由 B4 接入。
- [x] B4 前端模型设置和 Rust AI 入口；已在 B1–B3 后端测试基础上启用文本对话、模型切换及原生历史，配置桥和前端回归通过。
- [x] B5 真实回环模型协议联调，前端解析与停止/恢复回归。Rust 55、原生 IPC 15、前端 48 项及构建/静态检查通过；用户本轮免除浏览器连接与截图前置要求，真实 WebView 视觉验收留待 E6，详见本文件 B5 完成记录。

### C. 检查点压缩
- [x] C1 普通模型语义摘要适配器与摘要结构校验，不开放副作用工具。本阶段返回经校验的摘要草稿；13 项专门测试、相关 Agent 97 项及原生 IPC 15 项通过，自动触发/低水位/提交已由 C2 接入。
- [x] C2 自动压缩状态机：当前文字模型请求前自动检查、较低水位、有限重试和检查点提交；工具批次预算边界已提供并验证，真实工具循环接入留给 D1/D4。失败保留日志/旧检查点；Agent 112、桌面 IPC 17、前端 51 项通过。
- [x] C3 支持的 Responses 供应商远端压缩能力分支；完整保存/复用返回窗口，用供应商输入计数验证预算，绑定原模型/接口/凭据，前端明确配置能力。专测 17、相关 Agent 130、桌面 IPC 19、前端 58 项通过。
- [x] C4 压缩后重新注入实时目标、环境、可信授权与最新用户请求。宿主共享真实终端身份/生命周期，当前文字权限不从摘要产生；Agent 136、终端 22、桌面 IPC 20、前端 63 项及构建/静态检查通过，详细记录见本文件 C4。
- [x] C4R 持久项目/作用域与重启恢复：schema 9/10 保留原日志/检查点/窗口，同身份重连接续、离线文字讨论、不同项目隔离。Rust 173、桌面 IPC 22、前端 74 项及构建/静态检查通过；随后按新参考图调整为左侧会话/SSH、中央对话、右侧终端及统一主导航，见末尾 UI 记录。
- [x] C5 前端上下文占用与真实压缩事件：最近输入快照、计数来源、实际检查点次数及进行中/成功/失败/取消/中断状态，schema 11 持久恢复；Rust 定向 8、原生 Channel 1、前端 21 项及 TypeScript/Vite/严格 Clippy 通过。
- [x] C6 当前文字阶段的跨整理约束、新消息插入、模型缩窗、超大输入/附件拒绝边界与中断恢复。补齐 4 个连续场景，修复未回复用户补充遗漏与中断误标失败；自动整理 25、状态 4 项及严格 Clippy 通过。实际图片/PDF 输入与媒体计数尚未接入，验证归 E1，不能把拒绝测试当作媒体能力已完成。

### D. 工具决策与执行
- [x] D1 主模型工具循环与严格参数校验：Chat Completions 非流式原生内核、宿主工具白名单、闭合 schema 子集、本地参数校验、有界循环与取消/未知结果保护；9 项定向测试及严格 Clippy 通过。尚未接入桌面聊天、SSH 或持久执行恢复；Responses 工具协议仍未开放。辅助意图标签不作为授权或强制路由。
- [ ] D2 SSH 工具执行绑定 connectionId/terminalSessionId/taskId；实际目标状态复核。
- [ ] D3 命令策略和明确执行确认；持久授权范围，取消/结果未知/恢复时不盲目重执行。
- [ ] D4 工具批次、输出回查与错误分类；执行失败不自动判为意图偏差。
- [ ] D5 前端执行确认/绑定服务器/工具结果 UI 和真实本地 SSH 协议回归。

### E. 附件、记忆与高级 Agent
- [ ] E1 图片/PDF/文本附件到模型的转换、能力/预算/所有权检查。
- [ ] E2 长期记忆与短期任务上下文分离，来源/更新时间/适用目标。
- [ ] E3 子 Agent 与工作流、MCP、Skills，继承目标和授权边界。
- [ ] E4 旧数据导入设计与迁移验证（连接、聊天、附件、记忆）。
- [ ] E5 同模型固定任务 A/B 评测：完成率、约束保留、目标正确率、token/延迟/额外调用、重复执行。
- [ ] E6 全部前后端回归、真实模型/GUI/远端验收；用户要求时再生成安装包。

## 当前进度与下一项

A0–A6、B1–B5、C1–C6（含 C4R）已完成当前文字阶段，D1 工具循环内核已完成。C2 普通语义摘要与 C3 Responses 远端整理路径保持；C4 从宿主/SQLite 重建可信上下文，C5 展示真实输入/整理状态，C6 保留连续边界与恢复修复。D1 不改变当前桌面文本对话；旧摘要或原生窗口不能授予命令权限。

当前已核对客户端 test-rust-ui/9585898、Rust main/08960fd，C6 和此前 UI 已由用户提交。本轮 D1 开始前工作区干净，未提交、推送或打包本轮修改。schema 仍为 11；本轮仅修改 Rust 代码、定向测试和重构文档，不修改前端或 IPC。

本轮 D1 运行 `cargo test --test agent_tool_loop`，9/9 通过；`cargo clippy --test agent_tool_loop --lib -- -D warnings` 通过，新增/修改 Rust 文件格式检查通过。使用临时 SQLite、随机回环模型与模拟工具，不使用真实模型 Key 或用户服务器。没有运行全项目、前端或 Tauri 测试；C6 的 29 项属于上轮验证。

C4R 持久项目/作用域与历史恢复逻辑保留。用户随后提供新参考图并明确要求只修改页面：当前调整为左侧会话/SSH 上下分区、中央 Agent、右侧终端，设置分类与“终端”同级合并到左侧主导航，不保留设置页右侧重复栏；这项新要求覆盖之前 UI 撤销时的布局约束。历史仍按同一 sessionId/原 agentId 恢复，再连接/核对身份；新聊天才创建独立上下文，不合并不同服务器历史。

下一项 D2：原生 SSH 工具绑定 connectionId/terminalSessionId/taskId 并复核实际目标；执行审批与持久恢复归 D3，工具批次整理、完整输出/回查及产品集成继续由 D4/D5 接通。D1 目前仅支持 Chat Completions 非流式内核；Responses 的工具调用与工具流增量尚未实现，不能把现有文字能力当作已支持。聊天附件与真实媒体预算、记忆待 E。本轮没有新增可从前端验收的工具能力，仍可正常使用现有文字对话。

## 验证与完成记录

- A0：2026-10-13，创建本文件；已有源码研究报告保留明确提交和实现边界。
- 既有基线：后端 60 项测试、前端 69 项＋1 项真实附件联调、原生 15 项通过。页签修复另有 25 项相关回归和前端生产构建通过。

## 新窗口如何继续

先阅读本文件，再阅读 CODEX_AGENT_REFERENCE.md 与 MIGRATION.md；核对实际源码、git status 和最近验证记录。
只推进尚未勾选的第一组必要工作，不重复已通过的验证、不将旧进度当作新功能已经启用。
对一项实现并通过适当测试后，更新标记、代码位置、命令/结果与下一项。
若有用户新指令，以新指令更新本文件的范围与约束。

### A1–A4：2026-10-13 完成

- 代码：src/agent/model.rs、repository.rs、budget.rs、mod.rs；migrations/003_agent_context.sql，Store 初始化加载 schema 3。
- A1：持久会话、不可变任务目标快照、约束、乐观版本。目标字段形状已校验，真实终端状态复核仍待 D2。
- A2：事务追加事件、请求 ID 幂等回执（inserted=false 不得再次执行）、调用/结果唯一关联；迟到结果保留原 taskId。单事件限 64 KiB、同时未返回的工具最多 64 个。超大输出文件化仍待 A5。原始事件不被检查点替换。
- A3：上下文/输出/安全预留、固定提示/工具/必需请求/媒体成本计入预算；UTF-8 启发式估算包含工具参数；完整工具组选择，超限需压缩，不直接拿裁剪结果冒充完整模型上下文。精确媒体成本和模型 usage 校准待 B1。
- A4：检查点必须保存相同目标、用户约束、未返回调用与显式 unknown/cancelled 结果；证据引用限定本会话；新消息、取消、目标变更或另一检查点提交后，旧版本提交被拒绝。这里只保存/校验检查点，模型摘要生成和自动压缩仍待 C1–C4。
- 验证：cargo test --test agent_context，17/17 通过；cargo clippy --all-targets -- -D warnings 通过。包括真实 SQLite 关闭重开与两个线程同时写入。
- 前端：本批没有新 HTTP/聊天能力，继续禁用 Rust AI 入口；不把内核完成显示为 AI 已可用。
- A6 尚未勾选：已有 17 项内核回归，但完整输出回查、恢复时实际工具副作用去重仍需后续集成测试。

- 首批全量回归：cargo test，77/77 通过；cargo fmt --check 通过。既有 SSH、SFTP、附件与基础 API 全部通过；未连接用户实际远端。

- 桌面集成：cargo test --manifest-path src-tauri/Cargo.toml --lib rust_preview_tests -- --test-threads=1，3/3 通过（其余 12 项未在本批重复运行）。
- 历史阶段记录：首批 A0–A4 完成时的游标为 A5；当前 A5 也已完成，最新游标见本文件末尾。

### A5：2026-10-13 完成

采用 SQLite 64 KiB 分块存储原始字节，避免新增磁盘目录和文件锁恢复机制。事件保存有界预览及输出 ID/长度/SHA256 引用，输出和既有会话库一起备份。模型层与 HTTP 尚未接通，不开放用户可见 AI 入口。
验证完成：大输出分块、跨块/Unicode 字节回查、会话/调用归属、幂等捕获、取消/失败/过期恢复、配额、事件引用与检查点后回查。
- A5 当前证据：src/agent/output.rs、output_writer.rs、output_model.rs 和 migrations/004_agent_outputs.sql 已实现；tests/agent_outputs.rs 17 项及原有 agent_context 17 项通过，Clippy -D warnings 通过。最终后端全量 94 项、桌面桥 3 项回归均通过，A5 已完成。
- A5 边界与接口说明：AGENT_OUTPUTS.md。保留原始二进制，范围读取校验块摘要；模型只使用预览/引用。未新增 HTTP 或用户界面入口，实际模型/执行器未接通。
### A5 最终验证与续接

- 实现：OutputWriter 有界流式捕获；SQLite 固定数据块及逐块/整路 SHA256；每次回查最多 64 KiB；约 4 KiB 的头尾预览及原始字节引用。空输出和无效 UTF-8 均可保留原始字节。
- 一致性：输出引用与结果事件事务关联；核对 session/user/callId/元数据；迟到结果保留原任务；引用后的输出不可丢弃，检查点不删除证据。旧 ToolResult 默认 outputs=[]。
- 资源边界：单路 16 MiB、会话 128 MiB、全库 512 MiB、最多 10000 项/4 路捕获、120 秒无写入过期；完整边界见 AGENT_OUTPUTS.md。
- 测试：cargo test，94/94 通过（新增 agent_outputs 17，原有 agent_context 17，以及 SSH/SFTP/附件/API 等 60 项）。
- 静态检查：cargo clippy --all-targets -- -D warnings、cargo fmt --check 通过。
- 桌面集成：cargo test --manifest-path src-tauri/Cargo.toml --lib rust_preview_tests -- --test-threads=1，3/3 通过；其余 12 项未重复运行。
- 数据库：新增 004_agent_outputs.sql，schema 4；既有事件和连接/附件数据保持可读。配额为逻辑字节，SQLite 文件不保证即时缩小。
- 本批没有模型/HTTP/GUI 回查入口，前端保持 Rust AI 禁用；仍未打包、未调用用户远端或真实模型。
- A6 尚未完成：需要内核综合场景验证；实际 SSH 副作用去重和恢复核实仍依赖 D3 执行器，不能仅凭回执测试宣称已完成。
- 最新续接游标：A6，然后 B1。A0–A5 已完成；没有正在后台执行的测试或重构。继续维护本清单，每个完成项附代码与验证证据。

### 2026-10-13：内置 IPC 传输改造

用户要求内置业务使用 invoke/Channel，消除 WebView HTTP/1.1 长轮询连接槽瓶颈。本项是传输适配，不推进 Agent A6/B1，不启用 Rust AI，不生成安装包。

- 后端：src/services.rs 提供共享 Store/TerminalManager/SFTP/Files 生命周期，HTTP 与 IPC 使用相同实例；终端新增最多 64 KiB 的 UTF-8 输出读取，附件新增原生 AsyncRead 流式入口并共用存储校验/提交。
- 客户端：src-tauri/src/backend_ipc.rs、src/api/builtin.ts；SSH 配置和操作、健康与附件元数据走 invoke，终端输出走带逐帧确认的 Channel。Rust 文件桥直接使用内置 SFTP 服务；附件原生选择/保存，内容不经过前端。
- 远程兼容：HTTP 协议保留；src/api/nativeFetch.ts 转发原生 HTTP 及有界流式响应，WebView 不再承载远程终端长轮询/Agent 流。浏览器和远程 multipart 保留 fetch。
- 验证：客户端原生 21/21，包括 8 个真实回环 SSH 窗口、输出确认/Unicode、无 HTTP 的 SFTP 直连；前端新增传输测试 7/7；附件原生测试 2/2。最终后端全量 96/96、fmt 检查通过；后端 Clippy -D warnings 在新增流式入口后已通过。
- 已知已有问题：前端 mjs 回归 177/178，正式版 bundle 名称断言与 Rust Preview 配置不一致。HEAD 中已有相同配置和断言，本批未修改名称或此测试。
- 文档：兄弟客户端 docs/ipc-transport.md。旧电脑工具/安装包路径只作为历史记录；本机项目路径已更新。
- 实际 GUI 对话框和用户真实远端未验证。续接仍从 Agent A6，然后 B1。

- 最终前端 TypeScript/业务回归 83/83，生产构建通过；原生参数整理后 Clippy 无新增警告，仍有原项目的 3 项警告（旧 TLS refresh、连接测试嵌套 if、聊天历史排序）。未更改这些无关代码。


### 2026-10-13：第 10 个 SSH 窗口附近失败的修复

用户试用报告第 10 个左右的新窗口返回 SSH_TRANSPORT_FAILED。原实现每 connectionId 仅一条已认证传输，与 OpenSSH 默认每连接 10 个会话的限额吻合；实际用户服务器配置尚未核实。

- src/terminal/mod.rs 改为有界连接池；默认每传输 8 个窗口、每配置 32 条缓存传输，保留全局 256 个窗口上限。通道名额涵盖未确认和迟到通道，关闭最后一个窗口后回收传输。
- 对已有传输的新通道被 AdministrativelyProhibited/ResourceShortage 拒绝，记下已观察容量并在新传输上最多重试一次。重试发生在 PTY/shell/startupCommand 之前；新传输仍拒绝时返回 SSH_CHANNEL_OPEN_REJECTED，不无限重连。
- 更新配置保留旧窗口；关闭/取消/传输失效只影响对应传输上的窗口。服务退出清理缓存传输与窗口资源。
- 新增 24 窗口/服务器限额 10、限额 2 与启动命令只执行一次、连接池容量/故障隔离、限额 0 无无限重试回归。
- 用户要求本次只改代码、自行测试：不再运行后续测试或启动程序。该指令到达前终端 22 项已通过；收尾后的版本未重新运行全量测试。GUI/用户服务器仍由用户验证；未提交/打包。


### 2026-10-13：桌面整体运行，不再监听本地 HTTP 端口

- embedded_backend.rs 不再绑定 TcpListener 或调用 serve_services，只初始化同进程 Services::native，并在退出/重新初始化时清理运行时与业务资源。
- 健康检查、SSH/SFTP、附件操作全部通过 IPC；每次初始化生成独立身份，旧实例请求不能误路由到新实例。附件为原生文件引用，HTTP 签名链接仅用于独立 server。
- 本地设置移除端口和下载链接有效期，继续沿用数据目录与旧配置；远程 HTTP/HTTPS 档案不变。前端打包为静态资源，仅开发时 Vite 使用 1420。
- 按用户要求仅修改代码、现有测试适配和文档，未编译/构建/运行测试/启动 GUI/生成安装包/提交 Git。此前通过记录不能用于证明本轮改动已通过。
- 不推进 A6/B1，不启用尚未迁移的 Rust AI 功能。


### 2026-10-13：移除桌面远程后端模式

用户明确要求桌面是一个整体，不提供外部 ShellPilot 后端。客户端已移除远程地址/证书/连接档案及原生 HTTP 转发，SSH 终端输出统一 Channel，SFTP 桥直接调用应用内 Manager，附件只使用原生文件句柄。旧远程活动档案不影响启动，旧 HTTP URL 不能路由应用请求。

与目标服务器的 SSH/SFTP 网络通信保留；Agent 的模型请求继续属于后续本地 Rust Agent 迁移。独立 HTTP 测试适配不是桌面部署方式。不推进 A6/B1，不启用 AI。源码、测试适配和文档已更新；遵照用户要求未编译/测试/启动/打包/提交。


### 2026-10-13：移除旧独立 HTTP 启动入口

用户要求应用作为整体，无独立后端部署。删除 src/main.rs，移除 Cargo [[bin]] 并设置 autobins=false；本项目只作为 shellpilot_server 业务库供 Tauri 引用。cargo build --lib/cargo test 可独立编译或测试库，无需运行前端。库内历史 HTTP 协议测试适配保留，但不提供独立可执行程序。README 和客户端说明已更新；按要求未编译/测试/启动/打包/提交。

### A6 内核组合验证与任务绑定补强：2026-10-13

用户最新要求：回到 Agent 主线，本轮做简单测试。此前“只改代码、不运行测试”的限制不再适用于本轮；仍不打包、不提交、不连接用户服务器或真实模型。

- 代码：src/agent/model.rs 新增 ToolBinding，绑定由应用可信快照构造，不从模型参数推导执行权限。src/agent/repository.rs 新增 append_tool_call 与 tool_call_binding。
- 准入：在同一 SQLite 事务内核对当前活动 taskId/connectionId/terminalSessionId。模型参数中出现路由字段时也必须匹配绑定；错误任务、主机或终端均原子拒绝，不追加事件或改变版本。
- 恢复：通过原始 TaskStarted 事件读取旧调用目标，不使用当前 UI 页签或新任务目标。重放请求返回 inserted=false 的原始回执，目标绑定不匹配则拒绝。该回执和历史绑定均不是执行授权；D2/D3 还需检查实时 SSH 状态与可信用户授权。
- tests/agent_kernel.rs 新增 6 个组合场景：错误任务/主机/终端与模型路由参数、并发重复请求和重启回执、切换目标后的迟到完整输出与检查点恢复、取消/新约束/过期检查点、跨会话与用户输出隔离、小模型窗口完整工具组与必需输入超限。
- 验证命令：cargo test --manifest-path E:/project/AIShellTerminal/ai-ssh-terminal-server-rust/Cargo.toml --offline --test agent_kernel --test agent_context --test agent_outputs。结果：6+17+17，共 40/40 通过。
- 前端：在客户端目录执行 node node_modules/typescript/bin/tsc --noEmit，退出码 0。未修改前端业务或开放 AI 功能。
- 格式化：rustfmt --edition 2021 --config skip_children=true src/agent/mod.rs src/agent/model.rs src/agent/repository.rs tests/agent_kernel.rs 成功。
- 环境与修正：首次沙箱运行的两项临时数据库重开测试因 Windows 路径权限失败，正常用户权限重跑通过。新组合测试中的跨会话输出拒绝断言修正为既有 NotFound 语义，未改变输出存储的错误约定。
- 边界：重复执行计数是模拟派发，未执行真实 SSH 命令，不能宣称 D3 已完成。未跑全量 Rust/Tauri 测试、完整 GUI、真实模型或打包。随后用户已手动确认 A6 内核阶段勾选；D3 接通后回补真实执行器联验。

### B1：2026-10-13 完成

- 用户要求：继续 B1，做简单测试，完成后像 A1–A5 一样回填勾选；保留用户手动勾选的 A6。仍不打包、不提交、不使用真实密钥/模型或用户 SSH 服务器。
- 代码：src/agent/provider/{config,repository,protocol,transport,mod}.rs；migrations/005_agent_models.sql；Store 初始化加载 schema 5；tests/agent_models.rs。客户端 src-tauri/Cargo.lock 仅同步内嵌库新增的 reqwest 依赖。
- 配置：供应商协议、显式能力、模型 ID/上下文窗口、输出与安全预留、参数支持、URL/代理前缀、超时与响应大小校验。按用户隔离、每用户最多 32 项、乐观版本保存/删除。不根据模型名称推断能力和窗口。
- 密钥：Windows DPAPI 加密，绑定 Windows 账户与用户/模型配置 ID；公开数据只有是否配置/就绪标志。keep/set/clear 分开，携带旧密钥更改 API 基础 URL 必须明确替换或清除；运行配置不序列化，Debug/错误不泄露密钥。非 Windows 不降级为明文存储。
- 协议：OpenAI 兼容 Chat Completions 文本与 function 工具的非流式请求；完整历史工具组与 A3 预算校验；超限拒绝，返回压缩提示和响应 usage。关闭重定向与自动重试、响应逐块限额、整体超时与取消；HTTP 错误正文不透传，截断/过滤/无效响应不判完整成功。没有工具执行或模型授权。
- 测试命令（Rust 项目目录）：cargo test --offline --test agent_models --test agent_context --test agent_kernel --test agent_outputs。首轮 17+17+6+17 共 57/57 通过；补充无 Content-Length 分块限额/读取超时并调整 IPv6 回环代理判断后，cargo test --offline --test agent_models 最终 18/18 通过；既有 40 项无后续业务改动。
- 静态检查：cargo clippy --offline --lib --test agent_models -- -D warnings 通过；对新增/修改模块和测试执行 rustfmt，并做针对性格式检查。
- 桌面兼容（客户端目录）：cargo test --manifest-path src-tauri/Cargo.toml --offline --lib rust_preview_tests -- --test-threads=1，5/5 通过。验证同进程库和既有本地 SFTP 桥，不连接用户服务器；未运行其余 9 项。
- 环境：首次离线编译缺少新增依赖 want，联网下载依赖后测试均离线执行；测试在正常 Windows 用户权限下运行，避免已有沙箱临时路径 canonicalize 权限问题。测试服务仅回环监听，产品库不新增监听端口。
- 说明：AGENT_MODELS.md 记录配置字段、密钥动作、调用入口、协议边界与测试命令。模型能力与窗口尚未通过真实供应商探测；token 估算仍为启发式，usage 持续校准/媒体预算留给后续阶段。
- 续接：B1 已勾选，下一项 B2；B3 流式取消/结束与恢复、B4 模型设置和 AI 入口继续保持待办。未做真实模型/完整 GUI/全量 Rust 回归或安装包。较早的 A6 未勾选文字为当时记录，以顶部清单及当前进度为准。

### B2：2026-10-14 完成

- 用户要求：接通 B2，做简单测试并给 commit note；继续回填完成标记，不替用户提交 Git。A6/B1 已有勾选保持不变。
- Rust：src/agent/chat.rs 提供按用户/配置创建会话、就绪模型列表、活动生成和停止；src/agent/provider/stream.rs 提供字节级 SSE 文本适配；Services 管理同进程 Agent 生命周期，不新增产品监听端口。
- 桌面：backend_ipc.rs 对接 GET query_ai_agent_config_list、POST create_session/stop_chat，以及 builtin_agent_subscribe/builtin_agent_ack。目标来自存活 SSH 终端与 Store 所属用户，不能由模型或前端额外 routing 参数指定。
- 前端：src/api/agent.ts 改用 invoke/Channel，沿用 text/done/error 事件与 parseStreamEvent，逐帧确认、取消只针对对应 streamId；保留 App.tsx 的 Rust AI 面板禁用逻辑，模型设置仍待 B4。
- 持久化：复用 schema 5 的 A2 原始日志与模型配置。用户消息先入库，完整 assistant 入库后才发 done；异常写取消记录，不把部分文本当完整成功。同会话只允许一个活动生成；旧目标与用户约束重新注入模型请求。
- 边界：文本与回复 16000 UTF-8 字节、回复 JSON 编码 60000 字节、SSE 行/帧 64 KiB，网络响应上限与整体超时沿用 B1。历史达到 200 条或已有检查点明确拒绝，不静默截断。附件/工具对话未接入，不执行 SSH 命令或产生模型授权。
- 流式验证：finish_reason=stop 和 [DONE] 缺一不可；截断/过滤/错误 JSON/意外工具调用失败。停止、调用方取消、工作区关闭清理当前生成；信号回执不保证供应商计算已停止。
- 简单 Rust 测试：cargo test --offline --test agent_chat --test agent_models --test agent_context，8+18+17，共 43/43 通过。包含 Unicode/CRLF 网络分片、历史续聊、EOF、停止、同会话互斥、用户/目标隔离、附件拒绝、预算/JSON 大小和非成功响应。最终补强取消后的任务约束保留后，agent_chat 8 项再次通过；另外 35 项代码未变。
- 原生测试（客户端目录）：cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1，7/7 通过，其中新增模型控制、Channel 输出确认与 stop_chat 回环测试 3 项；没有运行其余 10 项。
- 前端测试（客户端目录）：node --import tsx --test tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts，27/27 通过；node node_modules/typescript/bin/tsc --noEmit 通过。新增 Agent IPC 6 项覆盖控制形状、Unicode/空白、done/error、取消和接收器错误。
- 静态检查：cargo clippy --offline --lib --test agent_chat -- -D warnings 通过；修改 Rust 文件 rustfmt/针对性格式检查通过。桌面 cargo clippy --manifest-path src-tauri/Cargo.toml --offline --lib --tests 通过，既有 chat_history 排序及旧 backend_process 测试进程警告保留，无新 Agent 警告。
- 环境与修正：一次静态检查的自动授权超时，普通沙箱重试因父目录配置访问失败；正常权限重试成功。Clippy 指出测试锁跨 await，已改为明确作用域后通过。没有改用户凭据或连接真实供应商/远端。
- 文档：AGENT_CHAT.md 记录原生接口、事件、参数、日志和边界；客户端 docs/agent-ipc.md 指向该文档。下一项 B3，保持 B3/B4/B5 未勾选；未做完整 GUI、真实模型、全量测试或安装包。

### B3：2026-10-14 完成

- 用户要求：接通 B3；网络中断后继续同一任务，保留已有修改。未运行被自动授权超时拒绝的首次检查；恢复后重试编译成功。客户端原有 icon.icns 修改保留，未参与本轮操作。
- 数据：006_agent_turns.sql、src/agent/turns.rs；Store 加载 schema 6，保留旧事件/输出/模型表。请求准入与用户事件同事务；终态、完整回复、usage 与原始事件同事务提交，单会话 running 唯一索引保护跨 Manager 并发。
- 会话：src/agent/chat.rs 增加 requestId、持久收据、原生历史/会话/事件分页；完成重放不需要原模型配置或密钥，不调用模型。失败/取消/中断重放返回记录，不自动重新生成。新请求可明确继续，同目标和用户约束保持绑定。
- 停止：requestId 精确匹配，准备阶段先复核用户/Agent 再信号取消；已准入轮次的停止标记与完成提交由 SQLite 排序。停止先到不提交成功，完成先到不反写取消；旧停止请求不影响下一轮。跨 Manager 通过心跳观察取消标记。
- 恢复：首个/有界增量保存部分文本；future Drop 安排 interrupted 写入，心跳每秒续租、30 秒过期。下一次 receipt/history/admission 回收过期轮次，不抢占新鲜租约、不盲目重试、不接受旧 owner 的迟到成功。上下文新版本不被旧回复覆盖。
- 流：保留明确 stop+[DONE] 才成功，新增状态化首帧前后超时、HTTP/协议/usage/容量错误。失败部分文本可回查，模型历史只纳入完整轮次；旧 B2 原始记录保留，可通过 events 读取。检查点重建和自动压缩仍待 C。
- 客户端：Agent API 新增 sessions/history/events；App 为每次生成固定 requestId，停止携带同一 ID。Channel 检查请求关联/序号，去重、缺帧拒绝并屏蔽结束后的回调；原生订阅 Drop 清理自己的状态。AI 面板未开放，B4 再接模型设置和可见原生历史。
- 测试：cargo test --offline --lib agent::，6/6；cargo test --offline --test agent_chat 最终 15/15；原有 agent_models 18、agent_context 17 通过（schema 断言更新为 6），共 56 项。核心包括停止/完成顺序、幂等重放、错误主机/用户/Agent、跨会话/Manager、租约过期及真实 SQLite 关闭重开、future Drop、错误/超时和成功后的接收失败。
- 客户端验证：cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1，9/9；node --import tsx --test tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts，最终 32/32；TypeScript --noEmit 通过。补充结构化 native done 的 JSON 原文重读回归，避免旧包装解析改变模型文本。
- 静态检查：Rust clippy --offline --lib --test agent_chat -- -D warnings 通过；桌面 clippy --manifest-path src-tauri/Cargo.toml --offline --lib --tests 通过，仅报告既有聊天排序和旧进程测试警告；修改的 Rust 文件格式检查通过。
- 边界：租约测试人为设为过期以模拟崩溃时间，再关闭重开数据库；硬崩溃可能丢失尚未持久的少量增量。没有真实模型/GUI/用户远端/打包或全量回归。原有 A6/B1/B2 勾选保留，B3 勾选，下一项 B4。
- 文档：AGENT_TURNS.md 记录请求身份、终态排序、历史接口、恢复宽限和验证命令；AGENT_CHAT.md / 客户端 agent-ipc.md 更新续接指向。

### B5：2026-10-14 完成

- 按本机 ShellPilot 两个仓库读取交接并核对分支/基线/工作区；库的 .idea/misc.xml 保留，Cargo path 无需修改。
- 新增 Rust 协议测试 3 项、原生 IPC 组合测试 3 项、前端 IPC 3 项、界面组件 3 项；总验证 Rust 55、原生 15、前端 48 项全部通过。
- 前端 App.tsx 固定恢复中的轮次 ID 并防止重复停止，5 秒确认超时后重读权威历史；AgentPanel.tsx/ModelSettings.css 同步补等待、禁用及现有图标，保留 B4 模型设置布局。
- TypeScript/Vite 构建、Rust Clippy -D warnings、桌面 Clippy 与修改 Rust 文件格式检查通过；桌面只有两项既有告警，大 chunk 提示保留。
- 实际浏览器验收工具 Tabbit diagnose 返回 69；用户明确要求不处理该项、由代理判断继续，因此 B5 按自动回归勾选。没有真实桌面截图、明暗/窄屏视觉验收、真实模型矩阵或用户服务器操作，验证边界与后续实际检查保留在本文件 B5 / E6 记录中。
- B1–B4 没有重写；schema 7、稳定配置 ID、名称、DPAPI 密钥和历史关联未改变。未提交、未推送、未打包。下一项 C1。

#### B5 验证命令与结果

| 范围 | 命令（对应项目目录执行） | 结果 |
| --- | --- | --- |
| Rust 模型/对话/上下文 | cargo test --test agent_chat --test agent_models --test agent_context | 18 + 20 + 17 = 55 通过 |
| 原生桌面 IPC | cargo test --manifest-path src-tauri/Cargo.toml --lib backend_ipc::tests -- --test-threads=1 | 15 通过，其他 10 项未运行 |
| 前端解析/历史/传输/附件兼容/组件操作 | node --import tsx --test tests/agent-ipc.test.ts tests/agent-history.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts tests/chat-text-ui.test.mjs | 15 + 4 + 5 + 16 + 8 = 48 通过 |
| TypeScript | node node_modules/typescript/bin/tsc --noEmit | 通过 |
| 前端生产资源构建 | node node_modules/vite/bin/vite.js build | 通过，既有大 chunk 提示保留 |
| Rust 静态检查 | cargo clippy --offline --lib --test agent_chat --test agent_models -- -D warnings | 通过 |
| 桌面静态检查 | cargo clippy --manifest-path src-tauri/Cargo.toml --offline --lib --tests | 通过；仅既有 chat_history 排序及旧 backend_process zombie_processes 警告 |
| 修改 Rust 文件 | rustfmt --edition 2021 --check 对应测试文件 | 通过 |
| 改动检查 | git diff --check（允许 CRLF） | 通过 |

Node 使用 C:/Users/kingdee/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin/node.exe；Cargo 使用 C:/Users/kingdee/.cargo/bin/cargo.exe。后续按本机 PATH 或实际安装路径执行。没有 npm prebuild 图标生成、Git 提交/推送或安装包构建。

#### B5 覆盖与验收边界

- Rust tests/agent_chat.rs：多行 SSE/注释/CRLF/UTF-8 分片、JSON/代码原文与 usage；10 种 choice/index/role、工具/旧 function、拒绝、截断/过滤、非法 JSON、过早 DONE 异常；行/帧/响应 3 种超限。保留已交付文本，不误报成功或自动重试。
- 客户端 src-tauri/src/backend_ipc_tests.rs：回环模拟模型供应商经过产品 agent_stream/dispatch/ACK 和真实 SQLite，覆盖未 ACK 时停止、旧请求/ACK 隔离、显式续聊、future Drop、中断/失败记录、关闭重开及缓存重读。
- tests/agent-ipc.test.ts / tests/chat-text-ui.test.mjs：取消隔离、完成/停止竞态、迟到回调、恢复原请求不自动发送或覆盖草稿、旧会话回执不清空新草稿。修正元素夹具遍历 undefined 时重入根节点的问题。
- 原生发送端为测试接收器，前端使用 IPC/hooks/元素夹具；没有完整真实 WebView 截图。测试端口仅模拟模型供应商，不是产品 HTTP 后端。
- 用户要求本轮不处理浏览器连接，因此 B5 以自动回归完成；E6 补验：途中停止及原请求恢复、历史生成中的等待/禁用、停止与完成竞态、切换会话不跳回旧会话、关闭重开后原文/状态、明暗主题和 320px AI 面板。
- 未验证真实供应商兼容矩阵或真实 SSH 副作用；没有实现 C/D/E。schema 7 与 DPAPI 不变，用户 .idea/misc.xml 保留。

### C1：2026-10-14 完成

- 核对基线：客户端 test-rust-ui/b71ed34，Rust main/01ad303；两者包含交接基线且开始工作区干净。src-tauri 的 ../../ai-ssh-terminal-server-rust 正确解析本机路径，不需要修改。
- 代码：src/agent/compact.rs，mod.rs 公开 compact 模块；tests/agent_compact.rs。没有新增逐阶段文档、数据库迁移、依赖或前端接口。
- 入口：compact::Summarizer::new(store).summarize(sessionId, userId, cancel)。按会话所属用户读取其模型配置，使用现有普通 Chat Completions 非流式适配器。一次调用只发一个请求、不自动重试、没有可执行工具定义；模型返回工具、拒绝、截断或协议错误均不能变成有效摘要。
- 来源：版本快照、可信任务与目标、旧检查点、之后的原始事件和失败/取消/中断文字轮次的部分文本。所有模型输入中的日志/旧摘要/命令文本均标作不可信证据，不能产生执行授权。生成中的文字轮次返回 Busy，不争抢 B3 租约或恢复任务。
- 范围：最多 200 条新增事件/失败轮次、序列化来源最多 512 KiB。完整请求连同提示、输出与安全预留经过 B1/A3 预算检查，超限明确拒绝，不默默丢消息。没有新增证据时返回 EmptySource；更大历史的分段和自动重试由 C2 后续处理。
- 输出：只接受完整 JSON 对象，且必须仅有 summary、nextSteps、evidenceEventIds。拒绝缺字段、未知/重复字段、代码围栏或嵌入 JSON 修复；summary 为非空、最多 16000 UTF-8 字节，nextSteps 最多 32 项且每项非空/最多 4000 字节；证据 ID 必须非空、唯一、在本次提供的来源范围内，最多 200 项。
- 应用字段：CheckpointContent 的 task（含目标/约束/状态）、pendingToolCallIds、uncertainToolCallIds 完全从快照构造，模型不能编辑。旧检查点证据和本次未决工具/失败轮次的关键引用由程序合并保留，累计超过 200 项或整个检查点超过 64 KiB 时拒绝，不截掉回查线索。
- 返回：Draft 包含 snapshot、content、usage 和实际使用的模型 ID。生成、无效输出、取消、超时和失败不写事件或检查点。输入采集前后及返回前核对快照；新消息、停止或目标变更使旧草稿失效。调用方以后提交仍必须使用 Repository::save_checkpoint 的 A4 CAS，不能用生成时的校验替代最终事务检查。
- 测试：真实随机回环模型服务验证语义输出协议与 usage、17 种坏摘要、越权字段、跨用户/会话/未来证据、拒绝/工具/截断、取消/超时/HTTP 错误、输入容量/模型窗口、活动轮次互斥、失败部分文本、旧检查点引用、未知工具保留、生成后版本冲突，以及 SQLite 关闭重开后的检查点/原事件/原始输出范围回查。13 项专门测试通过；测试显式保存检查点仅验证 A4 兼容，不是 C1 生产自动保存。
- 凭据：使用临时测试 DPAPI Key 验证认证仅进入原生 Authorization 请求头，未加入摘要输入、Draft 或原日志；没有使用用户密钥或实际供应商/SSH 服务器。用户原文和日志自行携带的敏感信息并未实现通用自动脱敏。
- 边界：结构/出处与可信字段可以校验，模型自由文本的全部语义正确性不能由这些规则保证；真实模型摘要质量与收益比较留到后续 C/E5。C1 不保证摘要更短、不执行自动压缩、不提交检查点、不替换模型聊天窗口；当前文字对话行为和模型设置保持原状。尚未提供 UI/IPC 摘要入口，因此本轮不改前端业务和样式，也不重复前端 JS/视觉验收。
- README 顶部同步当前文字对话能力，并明确其早期迁移/安装包段落属于历史记录，当前能力以本清单为准。

#### C1 验证命令与结果

在 Rust 库目录：
- cargo test --offline --lib agent::：6/6，通过 B3 轮次/恢复内核。
- cargo test --offline --test agent_compact --test agent_context --test agent_kernel --test agent_outputs --test agent_models --test agent_chat：13+17+6+17+20+18 = 91/91；连同上述 6 项，相关 Agent 共 97/97。
- cargo clippy --offline --lib --test agent_compact -- -D warnings：通过，无告警。
- rustfmt --edition 2021 --config skip_children=true --check src/agent/mod.rs src/agent/compact.rs tests/agent_compact.rs：通过。

在客户端目录：
- cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1：15/15，通过同进程库嵌入与模型/停止/历史/Channel 的既有协议；其余 10 项未重复运行。
- 前端工作区未改，Cargo 依赖锁未改。未启动真实 GUI、用户服务器或安装包。

完成标记 C1 已回填，下一项 C2。本轮未提交、推送或打包，commit note 只在聊天中提供。

### C2：2026-10-14 完成

- 基线：客户端 test-rust-ui/b71ed34、Rust main/a142a46；本轮开始工作区干净，Cargo 的 ../../ai-ssh-terminal-server-rust 依赖按实际目录正确解析。B5 与 C1 是用户已提交版本，C2 本轮不提交。
- 代码：Rust src/agent/compact/auto.rs、compact.rs、chat.rs、provider/protocol.rs、provider/repository.rs、repository.rs；tests/agent_auto_compact.rs。前端 ModelSettings.tsx、RustBackendSettingsDialog.tsx、api/chatErrors.ts、tests/model-compaction-ui.test.mjs；原生 src-tauri/src/backend_ipc_tests.rs。
- 触发：每次基础文本请求入轮次前估算完整序列化消息（含实时任务、旧检查点、近期历史、最新输入），达到 compactPercent × 可用输入容量，或原始增量达到 200 条等消息/请求体压力时尝试整理。可用输入 = 窗口 − 输出 − 安全预留；默认触发 75%。估算仍是 UTF-8 启发式，没有冒充精确 tokenizer。
- 低水位：候选摘要替换较早的完整对话前缀，目标降至触发水位的 80%（默认 75% → 60%，设置 60% → 48%）。必须同时满足完整普通请求的窗口、256 消息、2 MiB 请求体硬上限，并确实减少估算成本或被强制整理的消息数，才提交。最新用户输入不被裁剪；实时目标/约束从可信状态加入 system，摘要作为不可信历史数据。
- 范围与重试：增量读取最多 256 条，单次摘要来源最多 200 条/512 KiB；完整对话/工具组边界不被拆分。有界只读预检寻找能进入当前模型窗口的前缀，尽可能保留最近两轮完整对话。最多两次摘要调用，第二次要求更短摘要（4096 → 2048 UTF-8 字节）；只对无效摘要或缩减不足进行这次重试，网络/认证/超时错误不自动重试。旧检查点可再次整理，原始日志不删除。
- 失败：只在完整原始上下文本身可发送且快照未变时回退原上下文，不声称整理成功；否则明确报错，不发普通请求、不创建文字轮次。无效/未够低水位摘要、来源超限、取消、目标或新消息变化均不覆盖旧检查点。最新必需输入自身超窗时，在调用摘要之前拒绝。
- 提交：Repository::save_checkpoint_guarded 支持较早前缀，但以完整最新 Snapshot 做 A4 CAS，前缀必须为同会话事件、不得后退或拆开工具调用/结果组，引用不得超过前缀。事务写入前和最终提交前检查取消，已观察到取消则回滚 checkpoint/revision；若有效提交先于停止胜出，保留有效检查点但不继续普通模型请求。成功后重读并按新 revision 入轮次。
- 配置与停止：在途请求冻结用户所属模型配置和原生密钥，摘要期间设置修改/删除不能改路由；后续请求使用最新配置。准备阶段仍注册准确 requestId，可在首个文字帧/轮次入库前停止；停止、丢弃订阅、重读旧完成回执均不重新摘要或重发模型。
- 工具边界：check_after_tool_batch 校验完整真实调用/结果组并返回预算/触发信息，显式 unknown 仍为未知；不制造成功结果、不执行工具。当前文本请求继续拒绝工具历史，真实工具循环在 D1/D4 接入，不能把本项当成 SSH 执行已完成。
- 前端：高级参数增加 1–99% 自动整理比例，复用 ModelField、主题参数、标签与蓝色高亮；保存保留配置 ID、密钥及历史关联，锁定时仍禁用。新增容量/整理失败指导，工作区能力说明同步。C5 的占用/真实整理状态标记尚未实现；此轮没有新增 CSS 或截图，真实明暗主题/窄屏视觉验收仍留待 E6。

#### C2 验证命令与结果

在 Rust 库目录：
- cargo test --offline --lib agent::：8/8（原轮次内核 6 + C2 取消事务/前缀和并行工具组 2）。
- cargo test --offline --test agent_auto_compact --test agent_context --test agent_kernel --test agent_outputs --test agent_models --test agent_chat --test agent_compact：13+17+6+17+20+18+13 = 104/104；连同单元 8 项，共 112/112。
- C2 集成 13 项覆盖：普通低预算无摘要、触发和硬超窗缩减、保持近期轮次/最新原文/可信约束/全量历史、无效摘要两次上限、HTTP 错误无重试且仅完整可用原文回退、缩减不足不提交、准备停止/调用方取消、新消息/目标竞态、最新必需输入超窗、SQLite 重开/请求回执重放、工具批次完整性、在途模型配置冻结、失败保留已有检查点及其后的全部证据。
- cargo clippy --offline --lib --test agent_auto_compact --test agent_compact -- -D warnings：通过。
- 修改的 Rust 文件 rustfmt --edition 2021 --config skip_children=true --check：通过。

在客户端目录：
- cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1：17/17。新增 2 项通过真实产品 agent_stream/dispatch/ACK 和 SQLite 验证自动压缩后 Channel 与可见历史兼容、首帧前停止摘要；其余 10 项未重复运行。
- node --import tsx --test tests/agent-history.test.ts tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts tests/chat-text-ui.test.mjs tests/model-compaction-ui.test.mjs：51/51。新增 3 项验证百分比读取/保存/身份与密钥保持、锁定交互及错误提示。
- TypeScript noEmit 和 Vite 生产构建：通过。保留既有大 chunk 提示。
- cargo clippy --manifest-path src-tauri/Cargo.toml --offline --lib --tests：通过，无新增告警，既有 sort_by_key/zombie_processes 告警保留；修改原生文件 rustfmt 检查通过。
- 协议测试使用随机回环供应商与临时数据，不使用用户 Key、不访问真实供应商或 SSH 服务器。原生测试接收器/前端元素夹具不等于真实 WebView 视觉验收。未启动用户现有安装包验证、未打包。
- schema 7、Cargo/npm 依赖锁均未改。完成标记 C2 已回填；最新续接游标 C3。commit note 只在聊天中提供。

### 2026-10-14：修复 Node 18 开发启动故障

- 用户运行 npm run tauri dev 时，系统 Node 18.12.0 无法导入 Rolldown 使用的 node:util.styleText，BeforeDevCommand 退出。已安装 Vite 8.3.0 / Rolldown 1.2.8 的 package.json 要求 Node ^20.19.0 或 >=22.12.0，本机已有 Node 24.19.0。
- 客户端 scripts/node-runtime.mjs 在导入工具前选择兼容 Node，优先显式 SHELLPILOT_NODE，否则检查当前 Node、npm runtime、PATH 和已有 Codex runtime。只调整该工具及子进程的 PATH；不用下载、不修改系统 Node、不硬编码用户绝对路径。缺失/错误的显式路径给出明确提示。
- package.json 的 dev、tauri、build、preview 使用该启动入口，原来的 npm run tauri dev 命令保持可用；工具参数和 npm 调用身份继续传递。README 同步运行版本与可选路径配置。没有新增 npm 依赖，不改用户已有锁文件/Cargo 配置/图标变更。
- 验证：tests/node-runtime.test.mjs 5/5，覆盖版本边界、从旧 Node/PATH 选取新版本、显式路径错误、子进程环境隔离和缺失运行时错误。额外使用系统 Node 18.12.0 且移除 PATH 中 Node 24 的环境，Vite --version / Tauri --version 均成功自动切换至 24.19.0；TypeScript noEmit、Vite 生产构建通过。
- 实际启动：从旧 Node 18 的 npm-cli 执行 npm run dev，自动切换 Node 24，Vite 在 1420 就绪且 GET / 返回有效前端页面；不再出现 styleText 错误。只终止本轮测试创建的进程树，未保留开发端口。未做完整桌面 GUI/供应商调用，此次不重复 Agent 业务测试。
- 当前 C2 已完成、下一项仍为 C3。本次未提交、推送或打包，commit note 只在聊天中给用户。

### C3：2026-10-14 完成

- 基线：客户端 test-rust-ui/3585c82、Rust main/0b0b47a，C2 已由用户提交；Node 18 启动修复和用户锁文件/Cargo 配置/图标/暂存状态保留。本轮不提交、不推送、不打包。
- 代码：src/agent/provider/responses.rs、config.rs、protocol.rs、transport.rs、stream.rs、mod.rs；src/agent/compact/auto/responses.rs、auto.rs、responses_store.rs、repository.rs、turns.rs、chat.rs、store.rs；migrations/008_agent_responses.sql、tests/agent_responses.rs。既有模型/上下文/对话/压缩夹具仅补默认字段和 schema 8 断言。
- 协议：新增 openAiResponses 与默认 false 的 remoteCompaction，拒绝 Chat Completions 启用远端能力。普通 Responses 请求使用完整 input、可信 instructions、store=false、truncation=disabled、max_output_tokens 和需要时的 reasoning.effort；不用 previous_response_id 或隐式 context_management。当前仅文字/原生 reasoning 项，不启用工具执行或附件。
- 流与输出：语义 SSE 只把 output_text.delta 送到前端；确认 response ID、递增 sequence_number、output/item/content 索引、完整 output_item.done 与最终 response.completed/output/已交付文本一致。EOF、缺失终态/项目、拒绝、tool、failed/incomplete、错序或文本不一致不能成为成功。保留完整 message（含 phase/元数据）与 reasoning 原生输出，下一轮原样进入 input；失败只有文字部分证据，无完整窗口。
- 精确预算：Responses 使用同地址和路径前缀下的 input_tokens 计数完整输入及 instructions；密文字节长度与 compact 的本次 usage 不当作下次窗口 token 数。普通请求和最终发送点都检查可用容量，限制 2 MiB 请求/窗口、512 输入项目；不允许供应商默默截断。计数接口缺失明确报 MODEL_RESPONSES_UNSUPPORTED，不偷偷切协议。
- 远端整理：触发条件仍为 compactPercent 或 200 条增量压力；先确认最新必需输入可用，历史来源仍能进入模型窗口，再发一个 standalone compact 请求，不自动重试。完整返回数组整体保存/重放，包括保留消息、reasoning、encrypted compaction 和附加元数据；不取单个加密项或剪掉返回消息。追加最新用户原文、重新加入可信 instructions，再计数，必须降到触发水位的 80% 且成本下降才提交。Responses 开关关闭时仅按硬容量文字对话；C2 普通语义摘要路径保持。
- 保存与失败：schema 8 新增原生检查点窗口和 assistant 事件窗口表，分别在 A4 CAS 检查点事务、B3 已完成文字轮次事务中写入。取消在最终提交前被观察到则连同窗口/检查点回滚；旧检查点、原始事件、失败部分文本和回查线索保留。网络/无效输出/不足低水位时，仅完整且仍可发送、快照未变的原窗口可回退；不能发送则拒绝，不创建轮次或伪造成功。来源超窗不先剪历史来调用 compact。
- 隔离：窗口绑定规范化的供应商协议、完整模型接口地址、模型 ID 与原生凭据指纹，按会话/用户读取；跨接口/模型/密钥复用在网络前拒绝，需原配置或新建会话。改名/预算不改变绑定，在途请求冻结配置；活动期间删配置仍按原地址完成，不改路由。窗口及凭据绑定不通过 IPC、可见历史、Completion 序列化或 Debug 回显。ChatMessage 的原生窗口变体不能由 IPC JSON 构造。
- 资源：单窗口最多 2 MiB/512 项；累计原生读最多 8 MiB/256 项；越界整体拒绝。HTTP 使用既有取消、总时限、正文大小边界和禁止重定向/自动重试的客户端；供应商错误正文不返回用户。schema 7 旧配置/名称/DPAPI/文字历史保持可读，旧配置省略 remoteCompaction 时默认关闭。
- 前端：ModelSettings.tsx / api/models.ts 新增协议选择、Responses 固定输出参数、默认关闭的远端能力开关与状态一致的百分比说明；chatErrors.ts 给出能力缺失和窗口绑定变化的指导。复用 Select、Toggle、字段、标签/主题参数；少量 CSS 保持蓝色选择高亮、段落换行和窄栏开关排列，没有新增冗余标题。原生 backend_ipc_tests.rs 补同进程 Responses → Channel/ACK → SQLite 的成功与首帧前停止场景。
- 边界：此项是供应商支持时的文字远端整理，不开放工具/附件、不把不透明窗口作为授权。完整实时环境/授权注入仍是 C4，真实整理/占用 UI 是 C5，跨阶段综合及真实模型质量验证仍待 C6/E5/E6。未连接浏览器做截图，真实明暗主题/窄屏 WebView 视觉验收仍留待 E6。
- 官方依据与详细协议在已有 AGENT_MODELS.md；用户操作在客户端 docs/agent-model-settings.md。里程碑和测试结果只记录本清单，无新增逐阶段验证文档；commit note 只在聊天中提供。

#### C3 验证命令与结果

Rust 库：
- cargo test --offline --lib --test agent_responses --test agent_auto_compact --test agent_compact --test agent_chat --test agent_context --test agent_models --test agent_kernel --test agent_outputs：14 单元 + 17+13+13+18+17+20+6+17 集成 = 135/135。Agent 部分为 9 单元 + 121 集成 = 130/130。
- C3 17 项集成：Responses 流/非流与原生输出重放、完整 canonical 窗口/最新输入/可信约束/原始历史、密文长度与实际预算区别、能力关闭、7 种坏 compact/缩减不足、完整可用原窗口回退、两个取消入口、新消息竞态、接口/模型/协议/密钥隔离、SQLite 重开/删配置缓存回执、10 种错误 SSE 保留失败证据、计数接口缺失、旧配置默认值/IPC 无法伪造窗口、失败保留旧检查点及原始尾部、来源/必需输入超窗、在途配置冻结、schema 7 升级。
- 原子窗口事务单元 1 项验证取消发生在窗口插入后时全部回滚，及完整窗口/用户隔离；原有 C1/C2/模型/内核/输出/对话回归保持通过。
- 最终流分段校验后 cargo test --offline --lib --test agent_responses --test agent_auto_compact：14+17+13 = 44/44；最终发送点硬预算检查另复验 agent_responses。
- cargo clippy --offline --lib --test agent_responses --test agent_auto_compact --test agent_models --test agent_chat --test agent_compact -- -D warnings：通过；最终修改另做 --lib --test agent_responses 严格检查通过。修改 Rust 文件 rustfmt --check 通过。

客户端：
- cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1：19/19；最终分段校验后 --lib backend_ipc::tests::c3_ 的 2/2 再次通过，不额外累计为新测试。其余 10 项未重复运行。
- node --import tsx --test tests/agent-history.test.ts tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts tests/chat-text-ui.test.mjs tests/model-compaction-ui.test.mjs tests/node-runtime.test.mjs：58/58；最终提示修订后模型 UI 的 5/5 再次通过。覆盖保存协议/能力/百分比、稳定配置/名称/密钥、切回协议清除远端能力及锁定交互。
- TypeScript noEmit 与最终 Vite 生产构建通过，保留既有大 chunk 提示。桌面 cargo clippy --offline --lib --tests 通过，无新增告警，sort_by_key/zombie_processes 既有告警保留；修改原生文件格式检查通过。
- Windows 多个 Rust 链接与用户开发构建同时发生，首次综合构建较慢，出现一次增量缓存 finalize 的访问提示但测试最终全部通过；后续编译并行度限制为 2。没有停止用户开发程序。
- 回归模型都是随机回环模拟供应商和临时 SQLite，未使用用户 Key 或真实模型服务、不执行 SSH 变更；前端 hooks/元素夹具和原生测试接收器不等于完整 WebView 视觉验收。

完成标记 C3 已回填，最新续接游标 C4。本轮未提交、推送、打包。

### C4：2026-10-14 完成

- 基线与范围：Rust main/0c27b34、客户端 test-rust-ui/980ecb6，均包含既定基线，工作开始时干净。实现 C4，不重写 B1–B4、不跳至 C5/D；未提交、推送或打包。
- 参考与适配：实际读取本机固定 Codex 提交 f82e36592278a0a782fb0d0fc97e5fa10e832ac2 的 compact.rs/build_compaction_replacement_history、session/mod.rs/build_initial_context_with_world_state、context/world_state/compact_permissions.rs。采用宿主重建当前状态、权限和最新用户请求的原则；不搬 CLI 工作目录、沙箱允许前缀、插件世界状态或自动 SSH 探测。
- Rust 代码：src/agent/live.rs、chat.rs、compact/auto.rs、compact/auto/responses.rs、agent/mod.rs、services.rs；src/terminal/mod.rs、output.rs。Services 的 Agent 与终端共享 TerminalManager，模型准备从阻塞线程读取 SQLite/宿主；不会新增产品端口或独立 server。
- 身份与有效性：PTY 打开时只捕获实际连接的 connectionId/owner/host/port/username；连接配置修改不改变已有终端身份。准备、整理完成、检查点提交前、入轮次和发普通模型前复核原 terminalSessionId/connectionId/当前用户。共享生命周期 token 中止整理/计数/文字等待，检查点事务在提交前直接检查原终端取消；关闭原终端后不发后续普通请求、不生成成功回执，原日志/旧检查点保留。已经有效提交的检查点保留，关闭发生在完成提交之后的回执仍可只读重放。
- 上下文：两条整理路径都重新注入当前目标、约束与活动 taskId。停止后新任务目标使用最新提问，保留约束但不将旧 taskId 当成活动任务；最新用户原文仍为最后一条 user，不被摘要或远端保留消息替代。任务/消息更改仍由 A4 完整快照 CAS 拒绝旧结果；Responses 提交后重新读取/构造，核对完整 body 与已计数请求一致，不剪规范窗口。
- 环境与权限：区分 desktopOs 和远端 os/cwd（未探测为 null/未知）；不解析终端输出来确认事实，不用本机 Windows 或 startupCommand 冒充远端环境。不向模型发送 SSH 密码、私钥、启动脚本或 known_hosts。原生权限为 textOnly、sshExecute/localExecute=false、tools/approvals 为空；来源不是 IPC、历史摘要、root 身份或模型的授权描述。当前没有命令审批数据库/执行器，D3 才接入真实原生授权来源。无终端宿主的库级调用明确 verified=false，产品始终装配真实终端宿主。
- 前端：src/state/chatTarget.ts、App.tsx、AgentPanel.tsx、api/chatErrors.ts、src-tauri/src/backend_ipc.rs。聊天标题名称与状态灯一起跟随保存的 connectionId+terminalSessionId，不随当前页签切换；纯文本历史标记未绑定，关闭/重连新 ID 显示原终端已断开。AGENT_TARGET_UNAVAILABLE/AGENT_TARGET_MISMATCH 提示连接目标后新建会话。复用既有标题、状态灯、主题与宽度规则，完整名称通过 title 提示；没有新增表单/CSS/冗余标题。组件元素及状态回归通过，实际明暗主题/窄面板 WebView 视觉验收仍待用户/E6。
- 文档：已有 AGENT_MODELS.md、客户端 docs/agent-model-settings.md、docs/agent-ipc.md 同步当前边界和可测试步骤；交接文档只更新续接摘要。里程碑和验证只维护本文件，不新增阶段 MD、commit note 文件。
- C4 六项新业务回归：普通/远端整理后正确注入实际主机、任务和约束；修改连接配置不冒充新主机；两种历史均保留伪造授权文本但不能改变当前权限；最新 Unicode/换行/两侧空白原样；两种整理期间断开不改完整 Snapshot、不发文字请求；重连 ID 不能改绑旧聊天；停止后旧检查点保留、新目标/约束/无授权重建；错误主机或用户在模型调用前拒绝。

#### C4 验证命令与结果

Rust 库：
- cargo test --offline --lib --test agent_auto_compact --test agent_responses --test agent_chat --test agent_context --test agent_compact --test agent_models --test agent_outputs --test agent_kernel --test terminal：14 + 17+19+18+17+13+20+17+6 + 22 = 163/163。Agent 部分 136/136；C4 新增 6 项，其余回归保持通过。
- cargo clippy --offline --lib --test agent_auto_compact --test agent_responses --test agent_chat --test terminal -- -D warnings：通过。测试锁改为词法作用域明确释放，避免跨 await 告警；最终对 C4 六项复验。
- 所有修改 Rust 文件 rustfmt --edition 2021 --config skip_children=true --check、Git diff --check（Windows cr-at-eol）：通过。

客户端：
- cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1：20/20；新增 1 项在两种协议上通过真实产品 agent_stream/Channel/ACK 验证整理中断开、历史/检查点保留、重连拒绝和已完成回执重放。其余 10 个库测试未重复运行。
- node --import tsx --test tests/agent-history.test.ts tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts tests/chat-text-ui.test.mjs tests/model-compaction-ui.test.mjs tests/node-runtime.test.mjs tests/chat-target.test.ts：63/63。新增 4 项绑定目标状态和 1 项标题元素/状态灯/完整名称提示回归。
- TypeScript noEmit 和 Vite 生产构建：通过，保留既有大 chunk 提示。桌面 cargo clippy --offline --lib --tests 通过，无新增告警；原生修改文件 rustfmt 与补丁空白检查通过。
- 初次编译较慢，桌面改为 1 个编译作业，Rust 2 个作业；不终止用户开发程序。原生新测试最初误用 B5 短文本回执预期，修正为本轮模拟模型原文后全部通过，不影响生产代码。
- 验证只使用随机回环模拟模型/SSH 和临时 SQLite；不使用真实供应商 Key，不执行用户服务器变更。没有实际供应商语义质量验证、完整 WebView 截图或安装包构建。

完成标记 C4 已回填，最新续接游标 C5。本轮未提交、推送、打包。
## C4R 完成记录：持久项目与重启恢复（2026-10-14）

用户发现早期作用域入口仍依赖当次普通根会话/终端：重启后点击“针对当前服务器”可能进入新聊天，侧边栏不能直接找到原服务器历史。最终方案改为默认普通 Agent 对话、服务器项目及项目内持久聊天，替换临时范围选择 UX。

- schema 9 增加普通/server scope：服务器身份从真实 SSH 握手获取 host/port/username/hostKey；普通对话不随页签绑定。文字生成/整理与 SSH 执行 lease 分离，关闭终端可离线解释已有证据。原始日志、检查点与完整 Responses 窗口按原 sessionId 隔离；整理后状态变化重新投影/计数，不能复用失效预算。
- schema 10 增加 agent_projects/agent_project_chats。项目检查连接所有权、保存创建时地址/端口/用户快照、首次实际 SSH 密钥；之后重新绑定需身份一致。创建聊天与项目归属同事务，失败不留下半个会话。既有已验证 scope 按服务器身份补项目归属，未验证旧 task 归档，不改原任务/消息/检查点，不从修改后的配置推断旧密钥。
- GET projects/project_chats 支持跨模型侧边栏与每组 50 条分页；POST create_project/project_terminal/project_chat 分别负责保存项目、核对当前终端和显式创建聊天。历史点击从 SQLite 读原 sessionId/agentId，连接失败保留原文；绑定操作不创建聊天或调用模型。项目内新聊天/换模型创建独立上下文，旧聊天留在项目。
- AgentWorkspace 使用现有 Icon/Select/Modal 与主题变量，项目与普通历史在左，主区“对话 / 终端”切换；终端仍挂载，文件/SSH 页签与手动命令入口保留。管理服务器和模型设置可从侧边栏打开。复用现有连接，重开时重连原逻辑终端页签，避免反复新增终端；自动重连后原生重新核对身份。终端选区仅可加入来源项目已验证终端的聊天，普通/其他项目拒绝自动混入。
- Rust 项目 4 项专测覆盖重开不新增聊天、原日志保留、不同项目/模型隔离、分页/所有权、地址/账号/密钥变化拒绝、实际 schema 9→10 及重复打开不重复迁移；基础库/Agent/终端合计 173 项通过。
- 桌面 IPC 22 项通过，新增用随机 SSH/模型夹具完成：原项目聊天生成 → 关闭工作区 → 重开 → 同 ID 读原文 → 新 PTY 核对 → 显式下一轮继续。恢复与绑定阶段模型请求计数不增长；续聊请求含原约束和新 PTY，原会话已有两轮，未生成替代会话。
- 前端相关 74 项通过，新流程测试原会话/原模型先恢复、断线保留、普通不连接、复用连接、修改资料先阻止连接、身份失败不绑定；UI 测试项目/普通分组、历史点击、新聊天、保存服务器选择、视图切换与错误反馈。TypeScript/Vite 通过，Rust 严格 Clippy 通过。桌面 Clippy 沿用已有告警；真实供应商、真实服务器与 WebView 视觉需用户验收，未提交/推送/打包。

续接请从 C5 开始，保持本项目与 scope 层；不要重新恢复“针对当前服务器”入口，不把终端页签选择当成聊天归属。真正 SSH 工具执行仍要在 D 阶段绑定当次 PTY/taskId 与原生授权；已验证项目/文字环境不等于执行许可。下一窗口先核对实际目录、分支、工作区与本文件，保留用户修改。


### C4R 界面追加：最左侧导航（2026-10-14，随后已撤销）

- [x] 按用户 Codex 截图将模型配置、外观、SSH 连接、本地设置/关于移动到 56px 最左侧图标栏；项目与普通历史位于第二列，主内容位于第三列。入口有名称提示、键盘焦点和当前项高亮，使用现有 Icon/主题变量，窄窗口项目列缩为 210px。
- [x] 模型与外观入口直接选择设置页的受控分类，移除桌面设置页内的重复分类菜单；SSH 连接管理展示在主内容区，点击终端入口返回终端。普通/项目历史保持挂载，切换配置页不新建聊天、不修改项目或服务器绑定。独立启动设置页保留原导航。
- [x] 前端项目工作区交互 5/5，相关前端整体 75/75；TypeScript/Vite 与 diff 空白检查通过。新增一项验证模型/外观/SSH/设置切换保持项目/会话 ID，不触发新建/恢复聊天。真实 WebView 明暗主题/窄窗口视觉仍由用户确认，本次沿用先前免除浏览器前置的约定。Rust 业务/IPC/数据库未修改，不重复编译 Rust；C5 仍是下一项。不提交、推送或打包。

代码位置：客户端 src/components/AgentWorkspace.tsx、AgentWorkspace.css、RustBackendSettingsDialog.tsx、src/App.tsx；复用 tests/project-workspace-ui.test.mjs。


### 用户要求撤销最近两次 UI（2026-10-14）

- [x] 撤销 Agent 主区/项目侧栏布局和最左图标导航，恢复原左侧服务器列表、中央终端、右侧 Agent、PanelDivider/命令区和设置分类。RustBackendSettingsDialog 的仅本次 UI 改动恢复到原版本。
- [x] 删除本轮自行创建的 AgentWorkspace.tsx/AgentWorkspace.css 与已撤销布局测试，项目/普通聊天归组入口保留在 Agent 原“历史”弹层（ProjectChatHistory.tsx）；新建项目、原 ID/原模型历史恢复、自动连接/同身份重连仍使用已验证的底层逻辑。没有回退 Rust 库、IPC、schema 或数据库，不覆盖用户 icon.icns 修改。
- [x] 项目历史组件 4/4，前端相关整体 74/74；TypeScript/Vite 与补丁空白检查通过。仅前端 UI 与进度/使用说明调整，不重复 Rust 编译/协议回归。不提交、推送、打包。
- 续接保持此原窗口布局，不重新启用已撤销的项目侧栏/图标栏；C5 仍为下一项。历史 C4R/最左导航章节中的截图布局属于已撤销版本，不能作为当前 UI 的要求。

代码位置：客户端 src/App.tsx、src/components/AgentPanel.tsx、ProjectChatHistory.tsx、RustBackendSettingsDialog.tsx、src/reference.css；测试改为 tests/project-history-ui.test.mjs，projectConversation.ts 与相关功能回归保留。

### 按新参考图调整桌面布局（2026-10-14）

- [x] 用户明确要求只改 UI；该新要求覆盖此前“保持原窗口布局”的限制。左侧工作区同时显示 Agent 项目/普通会话和 SSH 连接，中央为对话，右侧为终端。保留原主导航，没有增加新的业务或项目系统。
- [x] 将已有 ProjectChatHistory 移到常驻侧栏，沿用原新建/恢复回调、sessionId/agentId、锁定和分页；SSH 组件沿用原连接/复用/编辑/删除逻辑。设置分类改为右侧图标与名称栏，原设置内容与保存行为不变。
- [x] 对话、终端及连接组件保持挂载；支持侧栏、SSH 分区、终端面板折叠以及对话/终端宽度调整。选择连接时展开终端。常用命令入口保留，在终端下方展开；窄窗口改为对话在上、终端在下。
- [x] 111 项相关前端回归通过，TypeScript 与 Vite 构建通过，保留已有 chunk 大小提示。新增外置历史在生成期间可折叠且保留草稿的回归；更新布局断言。广播测试原有旧断言在 HEAD 也不匹配 C4R 的作用域目标，已改为同时验证现有 Rust 固定作用域目标与旧单终端分支，未修改广播或对话发送业务代码。
- [x] 使用现有真实 React 组件、隔离模拟项目/模型/终端数据完成浏览器浅色、深色、1760px/900px 布局、设置右栏、分区折叠、草稿保留和设置返回检查；900px 页面无横向溢出。模拟预览不调用真实 SSH/模型，不等同于原生 WebView 全链路验收。
- 未修改 Rust 业务、原生 IPC、API、数据库 schema 或依赖。本轮不重复 Rust 测试、不提交、不推送、不打包。C5 仍是下一项，未因 UI 调整勾选。

#### 用户澄清：分类与“终端”放在同一主导航

- 用户按截图箭头明确要求将设置页内的分类合并到“终端”所在的左侧主导航。新增本地工作区、模型设置、外观和关于的同级入口，移除嵌入设置页的重复分类栏；高亮直接跟随当前分类，AI 面板的模型设置按钮仍打开对应页面。
- 设置页面沿用既有内容和操作，仅增加分类的受控 UI 参数；独立启动设置页仍保留自身导航。修正“关于”区域受旧通用 flex 样式影响的横向排列。
- 用户明确“UI 修改不用测试”：此次导航修正不运行测试、构建或浏览器验收。上文 111 项及浏览器结果属于此前三栏布局版本，不宣称覆盖本次修正。不提交、不推送、不打包，C5 保持待办。

### C5：2026-10-14 完成

本轮用户要求继续参考文档与 Codex CLI，实现后做少量必要测试，并说明能否在前端测试。基线为客户端 test-rust-ui/99fde44、Rust main/b02037e，开始时均干净；保持最新三栏布局与统一主导航，不推进 C6/D/E，不提交、推送或打包。

#### 参考与实现

- 沿用 CODEX_AGENT_REFERENCE.md 固定的 Codex 提交 [f82e36592278a0a782fb0d0fc97e5fa10e832ac2](https://github.com/openai/codex/tree/f82e36592278a0a782fb0d0fc97e5fa10e832ac2)。实际核对 `codex-rs/core/src/session/context_window.rs` 的当前窗口/可用输入语义、`codex-rs/protocol/src/protocol.rs` 的 TokenCount/ContextCompacted 事件，以及 `codex-rs/core/src/compact.rs` 的替换并重算后完成、失败/中断分支。另核对 [OpenAI compaction 文档](https://developers.openai.com/api/docs/guides/compaction) 的完整窗口处理；没有将 Codex 内部代码或不透明项直接套用为应用授权。
- Rust：新增 `src/agent/context_status.rs`，普通/Responses Controller 在测量、实际开始、失败/回退和 guarded checkpoint 提交处报告真实状态；`chat.rs` 把状态接到现有 Channel，准备后重测最终完整输入。Responses 复用已有供应商计数，不新增计数请求，不用密文字节估算占用。
- `migrations/011_agent_context_status.sql` 与 Store 升至 schema 11。独立表保存最后一次状态、输入预算快照及请求所有者，不修改原事件/轮次/检查点/窗口，不增加会话 revision。读取只核对用户、模型和会话，不解密凭据、不调用模型或新建聊天。
- `phase` 为 idle/running/succeeded/failed/cancelled/interrupted。普通摘要和远端整理分别使用 summary/remote；只保存固定错误码，不保存供应商错误正文、提示、API Key 或完整供应商窗口。整理次数每次从同会话真实检查点数量读取，成功必须验证对应 checkpointId 已存在。
- 提交 worker 在实际检查点成功后、等待 UI ACK 前持久记录成功，避免订阅断开/停止发生在提交后时将已成功结果降为取消；迟到的 Started/Failed/Finished 不覆盖已提交成功。每次请求拥有独立观察者令牌，旧请求不能覆盖新请求状态。准备中定期续 30 秒租约，异常退出遗留的 running 在过期读取时投影为 interrupted；不自动重试模型或执行操作。
- 客户端原生 `backend_ipc.rs` 增加 GET `agent/context_status`；流增加 `{event:"context",status}`，沿用原 sequence/ACK、requestId 和取消校验。前端 `api/agentContext.ts` 校验状态结构/容量关系，`api/agent.ts` 检查关联后交付；非法帧按协议失败处理，不误判为回复完成。
- `App.tsx` 按当前模型/会话读取快照，异步切换使用 AbortSignal/会话版本隔离，流式更新仍受当前请求限制。历史 running 仅在必要时短间隔刷新到终态；新发送清除旧请求快照，读取失败可手动重试，不重新发送用户消息。
- `AgentContextStatus.tsx/.css` 在 AI 面板模型说明下增加紧凑状态条，复用 Icon 与主题变量，允许换行；点击展开容量、预留、触发比例和时间。显示最近输入百分比、估算/供应商计数、已整理次数及真实阶段。未测量不冒充 0%，不增加两层标题、手动整理按钮或执行权限。

#### 语义与边界

- “最近输入”是最近一次准备发送的完整输入快照，分母为模型窗口减输出/安全预留；不是累计计费 usage，不包含未发送草稿或回复后的新增内容。输入确实超过容量时明确提示超出。Chat Completions 仍为启发式估算，Responses 为供应商计数。
- 新请求开始重置本次 phase，但总次数按真实检查点保留；旧历史没有状态表记录时显示未知占用和实际次数。配置更改后不会伪造重算旧快照，新请求才按新配置测量。
- 失败保留旧日志/检查点；原窗口确实可发送才显示回退。未提交的取消、中断、过期观察者或摘要缩减不足不增加次数。有效检查点先于停止提交时保留该真实成功，即使普通回复未继续。
- 状态不是新的上下文来源或授权；当前仍只进行文字对话，不执行 SSH/本地工具。没有调用用户服务器、真实模型或生成安装包；完整 WebView 视觉及真实模型摘要质量仍需用户验收。

#### 必要验证（未跑全量）

| 范围 | 命令或过滤项 | 结果 |
| --- | --- | --- |
| Rust 状态存储 | `cargo test --offline --lib agent::context_status::tests` | 4/4；schema 10 升级/重开、无凭据只读、租约中断、旧请求隔离、未提交不报成功、提交先于 ACK/取消 |
| 普通摘要成功 | `cargo test --offline --test agent_auto_compact threshold_compacts_only_complete_prefix` | 1/1；真实 running/succeeded、检查点次数及估算来源 |
| 普通摘要失败回退 | `cargo test --offline --test agent_auto_compact network_summary_error` | 1/1；失败不报成功、无额外摘要重试、次数不增加 |
| 准备阶段取消 | `cargo test --offline --test agent_auto_compact cancellation_during_summary` | 1/1；调用方/原生停止均不提交或继续普通请求 |
| Responses | `cargo test --offline --test agent_responses remote_compaction_preserves_the_entire` | 1/1；真实保存、供应商计数及完整窗口保持 |
| 原生真实 Channel | 客户端 `cargo test --manifest-path src-tauri/Cargo.toml --offline --lib c5_context_status_uses_real_channel_events_and_owner_scoped_persisted_measurements -- --test-threads=1` | 1/1；逐帧 ACK、成功持久读取、越权拒绝、模型摘要仅一次；最终提交/取消保护修改后再次通过 |
| 前端 | `node --import tsx --test tests/agent-context-status.test.mjs tests/agent-ipc.test.ts` | 21/21；快照校验、真实状态渲染、关联与 ACK、异常隔离 |
| 静态检查 | Rust `cargo clippy --offline --lib -- -D warnings`；客户端 TypeScript `--noEmit`、Vite `build`；修改文件 rustfmt 与两仓库 `git diff --check` | 通过；Vite 保留既有大 chunk 提示 |

以上 Rust 共 8 项，不把历史全量结果计入本轮。原生测试接收器适配新增的 context 帧，继续逐帧确认；未将未知帧忽略行为加到生产解析。测试只使用临时数据库和回环模型。

#### 可以在前端测试

1. 在客户端开发工作区重新运行 `npm run tauri dev`，等待 Rust 重编译；仅刷新前端或启动旧安装包不能验证本轮。
2. 选择现有模型，新建普通或项目聊天，发送短消息。模型区下方应从“未测量”变为“最近输入 …%”，点击可见详细预算与来源。不要把未发送草稿算入已测量值。
3. 可暂将高级参数触发比例设为 60%；若真实模型窗口至少 8192，可用 8192 窗口、1024 输出、512 安全预留。以短任务/约束开始，再多轮加入较长文本，达到触发条件时观察整理中/保存成功与次数增加，原始历史仍完整。触发取决于完整请求容量，不保证固定第几条发生；单条输入最多 16000 UTF-8 字节。
4. 整理等待时停止、切换历史/重启后重新查看。未提交不应增加次数，提交先胜出则保留真实成功；异常退出 running 租约过期后显示中断。不同聊天不串状态。真实失败时显示失败或回退，不能显示成功；无需特意破坏现有密钥来测试。
5. Responses 需要供应商支持 input_tokens，远端整理还需明确开启并支持 compact。应显示“供应商计数”，不展示密文。测试后可把比例恢复默认 75%。

客户端教程和交接文档已同步。C5 勾选完成；下一项 C6，再进入 D 阶段工具决策与审批。

### C6 用户实测追加：重复摘要丢失业务事实（2026-10-14）

- 用户在同一聊天补充公开报告编号，早期可回答，经过多次整理后回答“不确定”。只读检查测试数据库确认：编号原文仍在，第三、第四次检查点摘要含编号，第五次摘要遗漏编号；当前投影已越过原文。这是重复有损摘要的事实遗漏，不能把检查点提交成功当作语义保留成功。未修改用户数据库、替换摘要或重新调用用户模型。
- 原测试的“验收口令”可能与摘要不复述凭据的规则冲突，因此改用公开业务编号复验；公开编号的遗漏已排除仅由口令命名产生的解释。
- 新增 `RetainedUserFact { eventId, quote }`，检查点保存 `retainedUserFacts`。旧检查点 JSON 缺字段时默认空数组，schema 仍为 11，无新表/迁移。新的普通摘要 JSON 必须返回该字段，可为 []；提示优先提取用户明确要求保留的公开业务标识、日期、路径和决策，不能提取认证凭据或授予执行权限。
- 程序在新草稿中合并上次已收录事实，即使模型后续在 prose 或新事实中完全省略，也不会丢失。保留不同事件的更正，回答时按事件出处与明确更正理解，不把旧记录当实时环境。事实属于不可信历史数据，与 task/target/permissions 分离。
- 新 `src/agent/facts.rs` 校验引用必须为本会话、检查点范围内的真实 user_message，quote 必须是该原文的非空逐字片段；拒绝助手猜测、其他会话、未来事件、伪造或越权字段。草稿在阻塞工作线程校验，最后仍做快照/CAS/取消检查。Repository 事务再次验证，不能通过绕过摘要器删除既有事实或遗漏其证据引用。
- 总数最多 32 条，每条最多 512 UTF-8 字节；保留事实与 prose 一起计入完整输入容量、低水位和请求限制。超出上限拒绝候选/沿用原安全回退，不截掉旧事实。仍由模型选择初次收录内容，不保证任意细节都被自动挑中，也不是无限长期记忆；认证秘密不外泄仍依赖既有原生密钥隔离与模型提示，不声称已有通用敏感数据分类器。
- `compact/auto.rs` 把事实显式放进真正的下一轮模型请求，较低水位预检也计入已有事实；Responses 分支保留已有字段但不解析不透明窗口。没有新增普通对话端口、工具执行、前端界面或外部请求。原生测试夹具同步新摘要格式，C4 停止测试先确认 C5 状态帧，再在正文阶段停止，保持“已整理后停止”的原场景。

定向验证（不跑全项目）：

- `cargo test --offline --test agent_compact`：16/16，新增 3 项覆盖首次有出处事实、五次模型主动省略后的持久保留、SQLite 重开、拒绝事务删除、伪造/跨会话/助手/未来引用、缺字段、旧 JSON 读取与 32 条配额。
- `cargo test --offline --test agent_auto_compact`：21/21，新增 1 项跨两次真实自动整理，确认编号原文已不在 user 消息中，但事实仍随 checkpoint 实际送给模型；既有预算、取消、版本竞态、宿主身份和权限测试保持。
- 客户端 `cargo test --manifest-path src-tauri/Cargo.toml --offline --lib c5_context_status_uses_real_channel_events_and_owner_scoped_persisted_measurements -- --test-threads=1`：1/1，修订摘要格式后真实 Channel/ACK 与持久状态仍兼容。
- `cargo check --offline --tests`、`cargo clippy --offline --lib --test agent_compact --test agent_auto_compact -- -D warnings`、修改文件 rustfmt 和两仓库 `git diff --check` 通过。未改前端业务/UI，不重复 TypeScript、Vite 或浏览器测试。普通摘要 16 + 自动整理 21 + 原生 1 共 38 项；真实供应商修复后语义质量仍待用户复验。

前端复验：重新启动 `npm run tauri dev` 让 Rust 编译。在原聊天重新补充一次公开编号（或新聊天中在第二轮加入新的公开业务事实），再经过多轮其他话题和至少两次整理后询问。既有已遗漏的旧摘要不会被此升级自动回填，原始历史仍保留，不需要清空数据库。程序保证已收录事实后续不被摘要器静默删除，真实模型是否初次选择/正确使用这些事实需继续验收。

本项是用户反馈修复及 C6 部分证据，不把完整 C6 勾选完成；其他跨整理/模型缩窗/附件边界与中断综合回归仍待办。未提交、推送或打包。

### C6：2026-10-14 当前文字阶段完成

本轮以 Rust `30a2733`、客户端 `f8e73b7` 为干净基线，按用户要求继续文档主线并做少量必要测试。上面的“C6 部分证据”属于当时记录；本节完成后续连续场景，顶部清单和下一项已更新。

#### 参考与修复

- 实际重新阅读固定 Codex 提交 `f82e36592278a0a782fb0d0fc97e5fa10e832ac2` 的 `codex-rs/core/src/session/context_window.rs` 与 `codex-rs/core/src/compact.rs`：按当前模型窗口判断硬上限、从真实用户消息重建替换历史、重新注入宿主上下文，替换并重算后才报告成功；取消/中断不能等同完成。本轮使用已保存的源码副本，没有运行 Codex 或复制其远端压缩协议。
- `src/agent/compact/auto.rs`：原投影把用户事件暂存至下一条完整 assistant 才加入请求，循环结束时遗漏仍待回复的用户补充。整理中插入消息会使旧草稿失效，但后续继续请求可能漏掉该补充。现在把有效尾部 user 事件放在本次最新请求之前；仍跳过失败/中断轮次的输入，并由明确取消/任务切换边界清理暂存，不把未完成回复伪造成完整回复。不新增前端插队或并发发送入口。
- `src/agent/context_status.rs`：`AGENT_INTERRUPTED` 原来被通用错误分支标为 failed，现在明确映射 interrupted。用户停止仍是 cancelled，真实失败仍是 failed，实际提交成功不被晚到的中断降级；租约过期逻辑保持。
- 没有新 schema、端口、接口、依赖、前端布局或工具权限变更。模型窗口由用户明确配置，缩窗不能保证任何输入都可压入；不能安全压缩时继续明确拒绝并保留原记录。

#### 新增的 4 个连续场景

`tests/agent_auto_compact.rs` 中以 `c6_` 开头：

1. **重复整理后缩窗**：带可信任务约束和用户原文出处事实，从 12288 窗口提交首个检查点；增加其他话题后切到 8192 再整理。检查新请求的实际输入预算、最新用户消息、约束、事实、真实次数及原始日志；测试模型后续摘要刻意遗漏事实。
2. **已有检查点时插入新约束**：让第二次摘要等待，追加未回复的用户补充；旧草稿因 revision 改变被拒绝，不追加失败轮次或普通模型请求。显式继续后，新补充仍在未被压缩的尾部，送入模型且早于本次最新请求；旧的未准入请求不被偷偷重放。
3. **中断、完全重开与继续**：第二次摘要等待时 Drop/abort future，确认 interrupted、次数未增加、无普通请求、旧检查点/原文不变；释放原 Store/Manager，独立打开同一临时 SQLite 验证状态。旧完成请求在原回环端点关闭后仍能本地重放；更换临时测试端点后，使用原 sessionId 和新 requestId 继续，成功提交下一检查点且只发送一次普通请求。
4. **附件/超大文本拒绝边界**：已有检查点时单个/256 个未支持附件引用、超过 16000 UTF-8 字节的文本、合法但超出模型预算的输入都不能修改原日志/检查点或触发无用模型请求；随后恢复容量仍可正常对话。这里只测试附件拒绝，未读媒体文件，实际图片/PDF 转换和精确媒体计数仍归 E1。

#### 验证结果与边界

- `cargo test --offline --test agent_auto_compact -- --test-threads=1`：25/25，通过新增 4 个场景与受投影变更影响的既有自动整理回归。
- `cargo test --offline --lib agent::context_status::tests -- --test-threads=1`：4/4，验证已提交成功、旧请求隔离、状态恢复与未提交不报成功。本次共 29 项，不把早期事实回归或 C5 结果累加成本轮运行数。
- `cargo clippy --offline --lib --test agent_auto_compact -- -D warnings`、3 个修改 Rust 文件 rustfmt/格式检查及两个仓库 `git diff --check` 通过。
- 首次沙箱运行被 Windows DPAPI/本机回环限制，改用正常用户权限；正常权限首次运行复现中断误标，并暴露测试夹具误把新任务当原任务补充及过紧低水位的预期，已按真实语义修正夹具。修复后目标回归全通过，不降低预算/版本/取消保护以迎合测试。
- 未运行全项目、前端、Tauri 或真实 GUI 回归，没有调用真实模型或用户服务器，没有打包、提交、推送。用户上一轮截图显示三次整理后正确回答公开编号/时间/天数，属于用户手动反馈，不能替代本轮真实供应商验收。

#### 前端可以验证的部分

重新运行 `npm run tauri dev` 等待 Rust 重编译。在真实模型支持的范围内，用 16384 窗口、2048 输出、512 安全预留、60% 触发做一次长对话整理，再保存 8192 窗口并返回原聊天继续；检查新请求详情采用 8192、可以整理则真实增加次数，不能安全降到容量则明确报错并保留原历史。用公开报告编号、备份时间和天数测试事实，不使用凭据。

整理等待时点停止，再发送含新约束的“继续”；退出并重开同一历史，检查原文/已提交次数仍在且可继续。停止前检查点已经提交时仍保留真实成功；尚未提交不应增加次数。异常中断才显示中断，不要求正常用户停止也显示中断。UI 生成期间锁定发送，所以并发插入竞态由自动化测试覆盖，不需要用户绕过界面。

C6 在当前文字能力范围内勾选完成；下一项 **D1 主模型工具循环与严格参数校验**。D2/D3 再接实际目标和执行审批，E1 再验证真实媒体，不提前宣称 SSH 命令执行或附件对话已启用。

### D1：2026-10-15 主模型工具循环内核完成

以 Rust `08960fd`、客户端 `9585898` 为干净基线，本轮仅推进第一项未完成里程碑，不改变桌面业务入口。此前 C6 末尾的“下一项 D1”是历史记录，当前游标为 D2。

#### 参考与实现

- 重新读取固定 Codex 提交 `f82e36592278a0a782fb0d0fc97e5fa10e832ac2` 的 [ToolRouter](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/tools/router.rs#L257) 及 [turn loop](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/session/turn.rs)：主模型产生调用，宿主路由执行，结果反馈后继续模型回合；完整批次后复核上下文边界。没有添加每轮必经的业务意图分类器。
- 核对 [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling) 的多步工具流程和严格对象约束。本轮对 OpenAI 兼容服务使用本地确定性校验，不依赖供应商是否支持 `strict:true`，也没有给所有兼容服务强加这个供应商参数。
- `src/agent/tool_loop.rs`：新增 `ToolRegistry`、`ToolExecutor`、`ValidatedCall`、`ToolLoop`。注册表在回合内不可变；调用既有 `ModelClient.complete` 完成“主模型 → 工具批次 → 结果 → 主模型”，收到无工具的完整 assistant 回复才结束。处理器只由原生宿主提供，模型传入的字符串不能选择任意执行器或产生权限。
- `src/agent/tool_loop/schema.rs`：schema 和参数使用同一闭合子集校验。支持 object/array/string/integer/number/boolean/null、enum、数值/长度/数量边界及 description；所有对象字段必填且禁止未知字段，嵌套规则相同；拒绝未知关键字、引用、联合类型和重复 JSON 字段。不做字符串转数字，integer 限有符号 64 位。注册时限制 schema 大小/深度/节点数，调用参数上限 64 KiB；不宣称实现完整 JSON Schema。
- 批次先整体校验协议、调用 ID、参数和预算，再派发合法调用。错误参数只返回 `invalid_arguments`，供模型修正，不执行工具或把参数值拼入错误诊断。模型不认识的工具、跨回合重用 ID 或不完整协议明确失败。
- 默认最多 8 次模型请求、32 个请求调用，每项工具 30 秒、结果 8 KiB；配置有硬范围。结果超限仅回传 `output_too_large`，不把超大原文塞入模型；当前尚未接完整输出存储，不宣称可回查。
- 工具定义、调用、结果和协议开销都计入每次请求及批次边界；需要整理时返回 `NeedsCompaction` 并保留完整调用/结果，不裁剪、不继续发送，也不在本轮内自动压缩。D4 再接 C2 的持久上下文整理和工具输出链路。
- 调用结果在等待执行前先标为 `result_unknown`，尚未开始的调用明确 `not_executed`。取消、超时或宿主返回未知结果即停止剩余批次，不自动重试。成功/失败/拒绝分别反馈；同一 loop 对象一旦开始就不能再次 run，包括被丢弃的 future，避免同对象重放。子取消令牌在 future 丢弃时也会取消。
- `ModelClient.config()` 只读返回该客户端实际采用的归一化配置，避免循环预算和请求配置不一致。无新增依赖、schema、端口、IPC 或前端改动。

#### 验证与范围

- `cargo test --test agent_tool_loop`：9/9。使用真实回环 HTTP 模型响应、临时 SQLite 和模拟工具，验证多回合/多工具、严格参数与模型修正、非法 schema、跨轮重复 ID、回合/调用额度、工具定义/结果预算、取消/超时/未知、失败/超大输出、关闭工具能力和 Responses 拒绝。
- `cargo clippy --test agent_tool_loop --lib -- -D warnings` 和修改 Rust 文件的 rustfmt 检查、`git diff --check` 通过。未运行全量、前端/Tauri、真实模型或用户 SSH 测试，不提交、推送或打包。
- **本轮前端没有新增可测试入口**：桌面仍使用既有文本流。D1 是 Chat Completions 非流式工具循环内核，尚未连接持久轮次/执行日志、C2 自动整理、SSH 执行器或审批；Responses 工具调用与工具流增量未开放。不能从此勾选推断 Agent 已能操作服务器。
- **下一项 D2**：实现绑定任务与目标的 SSH 处理器，执行前核对真实连接/终端状态；D3 完成可信审批与持久执行恢复后再开放副作用能力，D4/D5 对接完整输出、模型/上下文链路及前端展示。
