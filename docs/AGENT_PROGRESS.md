# Rust SSH Agent 实施清单与续接入口

更新日期：2026-10-14。状态：阶段 5 实施中，模型设置、基础文本 AI 面板和原生历史已启用。
用户要求：参考 Codex CLI 优化上下文和工具决策；先列清单，每完成一项更新标记，供换窗口后续接。当前不打包。需要修改前端的阶段同步修改 test-rust-ui。
源码参考与设计依据：[CODEX_AGENT_REFERENCE.md](CODEX_AGENT_REFERENCE.md)。本文件是进度唯一入口，完成标记必须有代码与验证证据，不以“已设计”替代“已实现”。

Rust 重构交接、已完成工作与运行方式见 [WORK_HANDOFF_2026-10-14.md](WORK_HANDOFF_2026-10-14.md)。

## 工作目录与约束

- Rust（当前本机）：E:/project/ShellPilot/ai-ssh-terminal-server-rust，main，HEAD 01ad303（包含 c4bd68d）
- 前端（当前本机）：E:/project/ShellPilot/ai-ssh-terminal-client，test-rust-ui，HEAD b71ed34（包含 b7a9735）
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
- [ ] C2 自动压缩状态机：每次模型请求前/工具批次后检查、较低水位、有限重试、失败保留日志/旧检查点。
- [ ] C3 支持的 Responses 供应商远端压缩能力分支；完整保留其输出窗口，不跨供应商复用不透明对象。
- [ ] C4 压缩后重新注入实时目标、环境、可信授权与最新用户请求。
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

A0–A6、B1–B5、C1 已完成各自基础阶段。C1 提供普通模型的严格 JSON 语义摘要草稿，目标/约束/未知结果由程序保留；生成不修改原日志或检查点，不执行任何工具。最新验证：C1 专门 13 项、相关 Agent 合计 97 项、原生桌面 IPC 15 项通过，Clippy -D warnings 与修改 Rust 文件格式检查通过。

本机客户端 test-rust-ui/b71ed34、Rust main/01ad303；包含既定基线。B5 已由用户提交，本轮开始两个工作区干净，C1 修改尚未提交。Cargo 相对依赖正确；未改前端接口/样式、依赖锁或 schema 7。本轮只有 Rust 代码与已有进度/说明文件改动。

下一项 C2：在模型请求/工具批次边界接入自动压缩状态机，检查较低水位、有限重试和失败回退，成功后通过 A4 版本校验提交检查点。C1 尚未接入当前聊天路径，前端不显示“已压缩”；C3 远端压缩、C4 重注入、C5 UI 仍待办，SSH 执行和审批待 D，聊天附件和记忆待 E。用户的真实 WebView 视觉验收仍保留到 E6。本轮不提交、不推送、不打包。
详细验证结果和下一步会在每个完成项后记录。

### B4：2026-10-14 完成

- 前端：`ModelSettings.tsx`、`api/models.ts` 提供模型配置创建/编辑/删除、Key 保留/替换/清除、窗口/输出/超时和可选参数；保存立即生效，无需 `.env`。`RustBackendSettingsDialog.tsx` 增加模型分类，`App.tsx` 启用基础文本 AI 面板、配置选择及文本引用。
- 原生：`backend_ipc.rs` 新增按用户和配置 revision 隔离的模型 CRUD；复用 B1 ModelRepository，DPAPI/数据库放在 spawn_blocking；错误码和状态明确，模型密钥不回传。产品无 HTTP 监听端口。
- 历史：使用 B3 数据库会话/轮次，支持会话及消息分页，拒绝不前进、超长或版本改变的分页；完整、失败、取消、中断、仍生成分别显示。活动历史自动刷新且可按 requestId 停止；继续旧会话使用原目标，终端页签切换不能修改目标。
- 边界：面板明确为文本对话，聊天附件入口禁用，纯文本粘贴不转上传附件；不开放工具执行、审批或自动压缩。生成期间限制模型修改和切换；删除配置不删除历史。
- 验证：客户端 `node --import tsx --test tests/agent-history.test.ts tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts`，37/37；`cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1`，11/11；TypeScript 与 Vite 生产构建通过。Clippy 无新增告警，保留既有 sort_by_key/zombie_processes 告警；Rust 修改文件格式化通过。
- 环境：普通沙箱 Tauri 宏编译因 Windows 路径访问失败，改在正常 Windows 权限下离线验证后通过。未使用真实模型 Key、未连接用户服务器、未做完整 GUI 验收或安装包。
- 文档：[客户端模型设置教程](../../ai-ssh-terminal-client/docs/agent-model-settings.md)，同步 AGENT_MODELS.md 与客户端 agent-ipc.md。
- 下一项 B5。较早记录里的“B4 待办 / AI 面板禁用”属于当时阶段，以顶部清单和本段为准。保留 A6 手动勾选，不提交 Git。

### B4 样式回修：2026-10-14

- 用户确认基础对话正常，同时要求改进 UI 并参考 main 样式。对照发现既有设置/聊天 CSS 保留，但 B4 新表单和模型栏没有复用其控件与布局。
- 模型配置改为列表＋编辑区，字段按连接/认证/预算分组，底部固定保存与删除确认；复用 main 的 settings 参数、Select 和 Toggle，补齐加载、空列表、选中、错误和保存状态。
- AI 面板采用紧凑模型选择栏和设置图标，恢复操作独立显示；设置导航补充图标与说明，工作区字段等宽，统一按钮、间距和主题。
- 浏览器隔离预览使用内存模拟配置，不接触真实密钥/数据库：检查 1280/860/740 宽窗口、亮暗主题、320 像素聊天面板、保存无认证配置、自定义下拉框及高级参数。740 宽窗口下模型编辑区无横向溢出，保存栏可见。临时预览文件与进程在完成后清理。
- 前端 37 项相关回归、TypeScript 和生产构建通过。Rust 业务未改，本轮不重复原生测试；没有完整真实桌面/供应商联验，B5 仍待办。未打包、未提交。

- 用户截图后续修订：内容区最大 1280px，2240px 大屏实测编辑区约 1052px，留出右侧空白；内部足够宽时分为模型渠道/认证和接口/生成两栏。地址整行，短字段按用途两两并排；添加 AccentIcon、BASE URL/MODEL/敏感/参数等 Badge，以及协议、模型列表和推理快捷选项的蓝色选中高亮。修正旧响应式 CSS 覆盖整行地址的规则。
- 隔离浏览器预览补验 2240×1100、1280×900 和 740×720，亮暗主题、窄屏无横向溢出；推理 medium 快捷选项与下拉框同步，模拟保存成功。37 项相关测试、TypeScript 和生产构建通过。真实模型调用仍由后续 B5 验收；没有新增不支持的模型协议或附件能力。

- 名称与标题后续修订：用户要求配置名称可编辑，删除额外“模型设置”和“编辑配置”两个标题区域。显示名称与内部 ID 分离，schema 7 新增 agent_model_names；旧配置回退原 ID，新建配置使用独立 UUID。改名在同一 revision/事务中保存，不改变密钥熵绑定、历史归属或活动会话 ID；列表和 AI 面板使用显示名称。
- 验证：模型 20 项、上下文 17 项、对话 15 项，共 52 项 Rust；原生 IPC 12 项；前端 37 项；TypeScript、生产构建、Rust Clippy -D warnings 通过。覆盖 schema 6 升级、旧名回退、改名持久化、密钥继续解密、版本冲突与会话关联。隔离浏览器验证名称编辑后列表更新、两个冗余标题不存在；未访问真实供应商或用户服务器。保持 B5 待办，不打包、不提交。

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
