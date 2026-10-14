# Rust SSH Agent 实施清单与续接入口

更新日期：2026-10-14。状态：阶段 5 实施中，模型设置、文本对话、持久项目工作区及 C1–C4/C4R 整理/宿主上下文路径已启用。
用户要求：参考 Codex CLI 优化上下文和工具决策；先列清单，每完成一项更新标记，供换窗口后续接。当前不打包。需要修改前端的阶段同步修改 test-rust-ui。
源码参考与设计依据：[CODEX_AGENT_REFERENCE.md](CODEX_AGENT_REFERENCE.md)。本文件是进度唯一入口，完成标记必须有代码与验证证据，不以“已设计”替代“已实现”。

Rust 重构交接、已完成工作与运行方式见 [WORK_HANDOFF_2026-10-14.md](WORK_HANDOFF_2026-10-14.md)。

## 工作目录与约束

- Rust（当前本机）：E:/project/ShellPilot/ai-ssh-terminal-server-rust，main，HEAD 0c27b34（包含 c4bd68d）
- 前端（当前本机）：E:/project/ShellPilot/ai-ssh-terminal-client，test-rust-ui，HEAD 980ecb6（包含 b7a9735）
- Java 参考（当前本机）：E:/project/ShellPilot/ai-ssh-terminal-server，保留原实现
- Codex 参考：E:/project/ai-ssh/artifacts/codex-reference，提交 f82e36592278a0a782fb0d0fc97e5fa10e832ac2
- 安装包：E:/project/ShellPilot/releases，最新 0.4.0；页签切换修复在源码，尚未打进安装包。
- PowerShell；Git 位于 C:/Program Files/Git/cmd/git.exe，当前 Cargo 位于 C:/Users/kingdee/.cargo/bin/cargo.exe（1.99.0）。
- 当前 Node 24.19.0：C:/Users/kingdee/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin/node.exe。旧电脑路径仅为历史记录。
- 保留各仓库已有未提交变更；不连接用户远端、不执行实际变更命令来做测试。
- 里程碑、测试结果和续接状态统一维护本文件，不再额外创建逐阶段验证文档；commit note 直接发给用户，不保存到文档。
- SSH 任务绑定目标，切换 UI 页签不能改变在途任务目标。权限来自可信应用状态，模型摘要不产生权限。
- 当前主要验证是临时目录/回环协议测试；真实模型、完整 GUI 和用户远端验证单独记录。
- 后续前端变更需要同时做好功能和样式，优先复用客户端 main 分支的设计系统、Select/Ui 控件与主题参数；不能只接通功能后保留粗糙表单。相关界面需检查明暗主题、窄窗口和实际交互。

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
- [x] B1 模型适配器与配置：供应商能力、模型窗口、API 地址/密钥安全保存、基础参数校验。OpenAI 兼容 Chat Completions 非流式适配与回环测试完成；前端配置入口待 B4。
- [x] B2 对接 agent 配置列表、create_session、chat_stream 和 stop_chat；保留现有前端事件协议。基础文本流式对话与本地 IPC/Channel 测试完成，UI 入口仍待 B4。
- [x] B3 流式取消/超时/错误、同会话并发隔离、历史与恢复，不将 EOF 当成功。持久轮次、请求幂等、停止/完成事务排序及租约恢复已实现并通过回环测试；可见历史面板留给 B4。
- [x] B4 前端模型设置和 Rust AI 入口；已在 B1–B3 后端测试基础上启用文本对话、模型切换及原生历史，配置桥和前端回归通过。
- [x] B5 真实回环模型协议联调，前端解析与停止/恢复回归。Rust 55、原生 IPC 15、前端 48 项及构建/静态检查通过；用户本轮免除浏览器连接与截图前置要求，真实 WebView 视觉验收留待 E6，详见本文件 B5 完成记录。

### C. 检查点压缩
- [x] C1 普通模型语义摘要适配器与摘要结构校验，不开放副作用工具。只返回经校验的摘要草稿；13 项专门测试、相关 Agent 97 项及原生 IPC 15 项通过，自动触发/低水位/提交仍待 C2。
- [x] C2 自动压缩状态机：当前文字模型请求前自动检查、较低水位、有限重试和检查点提交；工具批次预算边界已提供并验证，真实工具循环接入留给 D1/D4。失败保留日志/旧检查点；Agent 112、桌面 IPC 17、前端 51 项通过。
- [x] C3 支持的 Responses 供应商远端压缩能力分支；完整保存/复用返回窗口，用供应商输入计数验证预算，绑定原模型/接口/凭据，前端明确配置能力。专测 17、相关 Agent 130、桌面 IPC 19、前端 58 项通过。
- [x] C4 压缩后重新注入实时目标、环境、可信授权与最新用户请求。宿主共享真实终端身份/生命周期，当前文字权限不从摘要产生；Agent 136、终端 22、桌面 IPC 20、前端 63 项及构建/静态检查通过，详细记录见本文件 C4。
- [x] C4R 持久项目/作用域与重启恢复：schema 9/10 保留原日志/检查点/窗口，同身份重连接续、离线文字讨论、不同项目隔离。最近两次窗口 UI 已按用户要求撤销，项目记录保留在原 Agent“历史”入口；Rust 173、桌面 IPC 22、前端 74 项及构建/静态检查通过。
- [ ] C5 前端上下文占用与真实压缩事件，失败不显示成功。
- [ ] C6 跨压缩约束、插入新消息、模型缩窗、大附件与中断恢复测试。

### D. 工具决策与执行
- [ ] D1 主模型工具循环与严格参数校验；辅助意图标签可选，不作为授权或强制路由。
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

A0–A6、B1–B5、C1–C4 已完成各自基础阶段。C2 普通语义摘要与 C3 Responses 远端整理路径保持；C4 在整理后从当前宿主/SQLite 重建目标、约束、实时环境及文字权限，原始日志保留。旧摘要或原生窗口不能授予命令权限；远端 os/cwd 没有探测时保持未知。

本机客户端 test-rust-ui/980ecb6、Rust main/0c27b34，均包含既定基线。C3 和 Node 启动修复由用户提交；本轮 C4/C4R 代码与说明仍未提交，保留已有 src-tauri/icons/icon.icns 用户修改。Cargo 相对依赖 ../../ai-ssh-terminal-server-rust 正确；schema 10，Cargo/npm 依赖及锁文件不变。稳定配置 ID、名称、DPAPI 和原始历史保持。

最新验证：Rust 14 单元 + 137 Agent 集成 + 22 终端集成 = 173/173，其中 Agent 146（单元 9 + 集成 137）。桌面 IPC 22/22；前端相关 74/74（撤销两次窗口 UI 后的历史入口回归）；TypeScript/Vite、业务库严格 Clippy 通过。新增项目专测 4、桌面项目重启/新 PTY 实际续聊 1、前端项目恢复 5/IPC 1/侧边栏交互 4。最后项目迁移专测从实际 schema 9 升到 10，再次回归 4/4 并严格 Clippy 通过。全部使用临时数据库、随机回环模型/本地模拟 SSH，不使用真实模型 Key 或用户服务器。前端构建仅有既有 chunk 大小提示；未构建安装包。

C4R 持久项目/作用域与历史恢复逻辑保留；用户随后撤销最近两次窗口 UI。当前为原左侧服务器列表、中央终端、右侧 Agent 和原设置导航；项目记录从 Agent 既有“历史”弹层进入，不保留 Codex 项目侧栏、最左图标栏或主区视图切换。历史按同一 sessionId/原 agentId 恢复，再连接/核对身份；新聊天才创建独立上下文，不合并不同服务器历史。

下一项 C5：前端上下文占用与真实整理事件，失败不显示成功；C6 综合回归仍待办。SSH 执行和审批待 D，聊天附件与记忆待 E。本轮不提交、不推送、不打包。用户之前已免除浏览器连接前置；本次 Tabbit diagnose 未返回可用实例（退出 1），使用组件/流程回归和主题/窄窗口样式源代码检查，未完成真实 WebView 视觉验收。请用户重启开发版验证侧边栏和实际服务器历史。

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
