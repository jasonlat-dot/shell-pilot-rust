# E3：子任务、工作流、MCP 与 Skills

2026-10-17。本地实现通过原生桌面 IPC 接入现有 Agent 工具轮次；验证使用回环模拟模型、MCP 和 SSH，不访问用户服务器或真实模型。

## 使用入口

客户端设置 → **工具与 Skills**：

- MCP 支持 stdio 程序、Streamable HTTP 及旧版 HTTP+SSE。通过一个 mcpServers JSON 编辑器管理，enabled / disabled 控制启停；最多 8 个。服务名称为 1–24 个字母、数字、下划线或连字符。
- stdio 使用程序和参数数组启动，隐藏 Windows 控制台；继承基础系统环境及用户显式列出的环境变量，支持 env 对象直接传值。HTTP 支持 headers 对象直接配置 Authorization 或其他自定义请求头，旧 bearerEnv 兼容；远端 Streamable HTTP 要求 HTTPS，回环测试服务允许 HTTP。headers/env 的值用 Windows DPAPI 加密保存并绑定用户、服务名及连接目标，IPC 只返回键名，JSON 编辑器用 __SHELLPILOT_SAVED__ 标记保留，删除键即清除。schema 21 新增独立凭据表，保留 schema 20 的旧 Token 数据并在 JSON 保存时迁移。
- 旧版 SSE 直接填写包含 Key 的完整 `sse-endpoint` 地址，例如 `http://appbuilder.baidu.com/v2/ai_search/mcp/sse?api_key=Bearer+实际Key`，不额外配置 Key 或做环境变量替换。URL 原样保存在当前用户的本地配置，不发给主模型。兼容远端 HTTP（明文），支持 1–300 秒请求超时，界面新建默认 300 秒；省略该字段的 JSON 使用 45 秒。
- SSE 按 2024-11-05 协议保持 GET 事件流，接收 `endpoint` 后向同源 POST 地址发送 JSON-RPC，并从 `message` 事件匹配结果。支持碎片 UTF-8、CRLF/CR/LF、多行 data 及通知；停止、超时或断线关闭连接，不自动重连或重放，不跟随重定向、不向异源 endpoint 发送请求。
- Skills 指定本机绝对目录。先发现名称/描述，再由模型调用 `read_skill` 读取 SKILL.md 和同技能目录内的参考文件；不自动执行脚本。当前支持常见字符串及折叠描述的 frontmatter，单文件最多 64 KiB，最多 128 个技能，扫描有深度和数量限制。
- 执行额度可在前端设置：主对话默认 32 轮模型请求、128 次工具调用；每个子任务默认 24 轮、64 次，一次处理默认最多 6 个子任务。可分别修改或恢复推荐值；模型轮数范围 2–256、工具次数 1–1024、子任务数 1–16。保存到当前用户的本地配置，下一次发送消息加载；旧 JSON 未包含额度时补充推荐值。
- 子 Agent 需要显式打开。`delegate_agent` 是等待结果的单层子任务，沿用父模型、当前任务目标、执行器和实时审批策略。主任务与每个子任务各自计数；父对话停止时停止子任务。不支持递归或独立后台 Agent。
- `update_plan` 保存步骤及状态，最多 12 步，最多一个进行中步骤。计划声明不代表工具已经执行。

配置修改从新轮次加载，修改还会使未消费的 MCP 审批失效。关闭子 Agent 后，既有调用签名仅用于读取历史；宿主拒绝新增委派。

桌面保存按 execution / mcp / skills 分区：外部保存只改执行额度与子任务开关，两个配置弹窗各自保存 MCP 或 Skills。后端在事务内读取当前配置，仅合并指定分区，保留 revision 校验；非 MCP 保存不改写 MCP 凭据。JSON 编辑器重新打开时显式保留每项 enabled 布尔值。本次修复按用户要求未运行测试或构建。

JSON 接受 type / transport、url / baseUrl、description、headers，以及 stdio 的 command / args / env。省略类型时按 command 或 /sse 路径推断。重复键、冲突别名、错误字段或参数类型均拒绝，不静默丢弃。编辑器提供格式化及三种停用示例，失败保留草稿；旧 Token 和配置可自动转换。完整使用示例见客户端 [agent-model-settings.md](../../ai-ssh-terminal-client/docs/agent-model-settings.md#mcp-json-配置)。

## 执行与审批

普通对话和服务器对话均可注册通用扩展；只有固定服务器目标的对话注册 SSH。

- `list_skills`、`read_skill`、`list_mcp_tools` 和计划记录是宿主限定的发现/读取/控制操作，不代表底层 MCP 或 SSH 操作已获授权。启用 stdio 服务意味着发现工具时可启动该配置中的程序。
- `call_mcp_tool` 按发现的输入 schema 校验实际参数，拒绝重复 JSON 字段、错误类型和不支持的 schema。当前为有界 JSON Schema 子集；不支持 `$ref`、组合 schema 等的工具显示为不可调用，不放宽其校验。
- `confirm` 等待具体调用的一次批准；`auto` 使用与 SSH 相同的无工具独立审查模型，不明确或失败时请求人工确认；`fullAccess` 仍复核宿主目标、配置版本、对话策略版本及轮次租约；`deny` 拒绝工具。
- 子任务调用进入父对话同一审批卡片。授权不跨服务、工具、参数、任务或调用 ID 复用。审批和执行原始日志均持久保存。
- 调用发送后丢失结果、取消、协议错误或进程中断，保守标为 unknown；不自动重试。HTTP 断开不证明远端已取消。大输出保存原始任务输出和有界预览，通过 `read_tool_output` 回查。
- 输出回查使用 `outputs[].id`，通常每页读取 4 KiB（4096 字节）；省略 `limit` 时宿主实际补成 4096，单页最多 16 KiB（16384 字节）。宿主将过大的正整数限制到 16384，保留输出 ID 和 offset；零、负数、错误类型、跨任务或未关联结果的引用仍拒绝。`eof=false` 时用 `nextOffset` 继续，不重跑命令；主任务和子任务共用归一化及结果额度。回读单独预留最多 132 KiB 的文本/十六进制 JSON 封装额度，普通工具仍沿用自己的结果上限；ToolResult 历史事件最多 192 KiB，以容纳再次转义，其他事件仍为 64 KiB。子任务耗尽预算时明确报告未完成及保留的证据。
- 通过宿主 schema 校验的 `read_tool_output` 不占工具调用额度，仍执行用户/任务归属与实时审批边界检查；其他工具和无效调用照常计数。每次向模型请求下一页仍消耗模型请求轮数。并行批次按实际调用数计工具次数，派发子任务在父任务中计一次，子任务内部单独计数。最后一个模型轮次只能生成报告；超出任一额度的批次整体不开始。
- 达到模型轮数与工具次数分别报告 `AGENT_MODEL_ROUND_LIMIT`、`AGENT_TOOL_CALL_LIMIT`，前端区分原因并指向执行额度设置；其他资源保护保留 `AGENT_TOOL_LIMIT`。配置额度不改变模型上下文容量和原始结果存储限制。
- 原始子调用带 `parentToolCallId`，界面和历史将它们归入子任务；父模型仅投影完整的委派调用/结果，避免嵌套调用破坏函数协议。原始证据仍参与整批边界和检查点保护。

### 并行与排队显示

- 同一模型批次中，相邻的独立子任务、Skills 发现/读取、MCP 目录发现及已有输出回查可并行，每批最多同时 4 个调用；子任务仍受配置的派发数量、单层及执行额度限制。模型若在后续轮次才发出调用，不会预先执行。
- 已发现且声明 `annotations.readOnlyHint=true` 的 MCP 工具允许不同服务同时调用；同一服务的现有传输仍逐条处理。该声明仅影响调度，每次实际调用仍须校验 schema、目标及独立审批，不能作为授权依据。
- SSH 共用当前终端 Shell，保持逐条执行；未标记只读或尚未发现的 MCP 调用、计划更新按顺序执行，后续调用不越过这些操作。并行子任务中的 SSH/MCP 实际执行也共用宿主门控。
- SSH 审批到执行期间，其他子任务的原始事件追加暂候，防止无关证据更新使精确 revision 审批失效。子任务模型请求及发现工作仍可推进。子调用持久 ID 按父调用隔离，模型复用局部 ID 不会串到另一子任务的输出或审批。
- 批次声明先持久化，但仅在调度和资源门控通过后推送运行事件；排队的 Skills、MCP 和命令不提前显示为运行中。刷新运行中历史时沿用持久开始记录过滤；结束后的历史保留未执行/未知结果证据。
- 任一并行调用出现结果未知或不可恢复错误，取消同组在途工作并停止后续排队调用；不自动重试，回填主模型协议仍按原调用顺序。

当前不支持 OAuth、MCP sampling/elicitation、服务主动发起工具请求或自动恢复重放。MCP 内容和 Skills 文本均是数据/任务指导，不能改变宿主审批边界。

## Codex CLI 对照

参考了官方开源实现的职责分离，按 ShellPilot 现有持久化轮次实现，没有复制 Codex 的全部协作或插件系统：

- [multi_agents.rs](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/handlers/multi_agents.rs)、[spawn.rs](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/handlers/multi_agents/spawn.rs)：子任务继承父运行环境和授权配置，限制深度，记录协作事件。
- [parallel.rs](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/parallel.rs)：由宿主声明工具能否并行，以共享/独占调度保护有副作用的执行；本项目使用有界调用组和原有逐服务 MCP 传输。
- [mcp.rs](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/handlers/mcp.rs)：工具调用固定到服务与工具，并经过宿主执行链。
- [plan.rs](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/handlers/plan.rs)：计划是独立控制工具，输出步骤状态事件。
- [skills 模块](https://github.com/openai/codex/blob/main/codex-rs/skills/src/lib.rs)、[渐进加载说明](https://github.com/openai/codex/blob/main/codex-rs/skills/src/assets/samples/skill-creator/SKILL.md)：元数据发现与按需读取参考文件。
- [MCP 传输规范](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports)：stdio JSON-RPC 分帧、HTTP JSON/SSE 与会话头。
- [旧版 HTTP+SSE 传输规范](https://modelcontextprotocol.io/specification/2024-11-05/basic/transports)：GET 事件流、endpoint 广告与独立 POST 请求。

真实模型质量、第三方服务互操作性和完整 WebView 交互仍需 E6 验收。

## 本地验证结果

- 2026-10-17 JSON 配置后补测 HTTP/SSE：协议 10 项（同批 stdio 另 1 项）、扩展执行集成 28 项、原生配置 IPC 1 项全部通过。覆盖自定义头在 HTTP 和 SSE GET/POST 的传递、JSON / 碎片多行事件回包、会话头、审批单次执行、401 / 重定向 / 畸形回包 / 错误 ID / 大响应 / 断线 / 超时 / 取消、不自动重放及凭据不进入模型消息。日志 `.codex-tmp/e3-http-sse-*.txt`；使用回环模拟服务，未连接真实百度 MCP。以下为此前各阶段记录。
- Rust 全量 381 项通过，1 项既有 POSIX 环境专用测试忽略。
- E3 扩展集成 26 项覆盖原有授权/取消/并行/历史行为、额度配置及旧配置兼容，新增 SSE URL/超时校验、精确审批后单次执行、未知结果不重放。
- 工具循环集成 11 项含最大并发数、串行屏障、协议顺序及未知结果停止排队；工具会话集成 24 项含子任务继承父服务器、双子任务 SSH 审批互斥及分页回查。主任务只有一次工具额度时，可执行一条命令及多次回查；子任务配置两次额度执行两条命令及六次回查后仍可报告。模型轮数仍独立生效。
- stdio 和碎片化 Streamable HTTP SSE、会话头、Skills 路径限制、重复参数及畸形 schema 有单元覆盖。旧版 SSE 另有 5 项协议测试，覆盖完整 URL 中的 `+`、碎片回包、取消/超时/断线/错误 ID、异源 endpoint 和重定向拒绝及连接释放。
- 客户端类型检查与 Vite 构建通过；额度设置/审批界面、历史与 IPC 合计 65 项、原生扩展 IPC 1 项通过。新增前端 SSE 切换、完整 URL 与超时保存、非法超时和切回 stdio 清理字段；原生 IPC 验证 SSE 配置读写与用户隔离。
- 最新日志为 `.codex-tmp/e3-sse-full-rust.txt`、`.codex-tmp/e3-sse-frontend.txt`、`.codex-tmp/e3-sse-native.txt`、`.codex-tmp/e3-sse-vite.txt`。此前 Tabbit 稳定启动器在沙箱内/外均无输出退出，未完成浏览器视觉验收；未切换浏览器后端。没有实际 Key，未连接真实百度 MCP。
