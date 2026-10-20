# 请求诊断日志

一次用户发送使用现有 requestId 作为 traceId；模型请求、自动审查、人工审批、工具执行、输出回传、上下文整理和结束共用该标识。显式重新发送生成新的 ID，补充/引导消息归入当前请求。历史回看不执行模型或工具。

桌面设置 → 本地工作区 → 诊断日志，可选择 debug / info / warn / error。默认 info；只记录所选级别及更严重的事件。选择后即时保存到 workspace.json 的 logLevel，并更新当前工作区的原子过滤级别；不重启或断开 SSH。工作区尚未启动时保存的级别在下次初始化时应用。

日志按操作系统本地日期分目录，扩展名为 `.log`，内容继续使用 UTF-8 逐行 JSON，每行是独立对象（不是一个 JSON 数组）：

```text
<应用本地目录>/logs/agent/2026-10-19/<traceId>.log
```

设置页可打开专用日志目录；原 backend.log 继续记录工作区启动/停止。独立 Rust 服务默认在数据库目录的 logs/agent 保存请求日志。跨午夜的同一 Trace 会出现在两个日期目录，traceId 保持不变。非文件名安全的请求 ID 使用 SHA-256 文件标识，避免路径穿越。

每行包含 timestamp、level、traceId、requestId、sessionId、agentId、sequence、event 和 fields。sequence 在当前诊断实例中递增；跨进程重启以时间与文件追加顺序为准。每次模型对话调用另带 modelCallId，区分同一 Trace 内的多轮续聊与自动审查。

| 级别 | 内容 |
| --- | --- |
| debug | 消息角色和字节数、工具定义名称、预算/输出参数、模型轮次、HTTP 响应等结构与计时 |
| info | 用户消息与模型回复正文、请求开始/完成、模型调用、审批状态、工具派发与结果持久化、整理提交 |
| warn | 拒绝、取消、中断、结果未知、整理回退 |
| error | 模型 HTTP/网络/协议失败、请求失败 |

`request.started` 与 `request.completed / request.failed / request.cancelled / request.interrupted` 表示请求边界；future 被丢弃不会记为完成。`model.http_error` 在尝试读取错误正文前记录 HTTP 状态及供应商请求 ID；`model.http_error_detail` 只记录经过限定的 error.type / code 和错误分类。HTTP 400/429/5xx 不再只剩通用 MODEL_HTTP_ERROR。供应商正文若未返回/过大/超时，仍保留已获得的状态记录。

`conversation.message` 在接受新回合后记录一条用户输入，保存回复后记录一条助手文本。正文日志直接传入 Trace ID，不依赖异步作用域；流式增量合并后记录，历史回放不重复记录正文。`fields` 包含 role、connectionName、modelId、state、message；modelId 是实际模型名，agentId 是本地配置 ID，普通对话的 connectionName 为 null。失败或取消时保留已保存的部分回复，并明确标注 failed / cancelled / interrupted，不冒充完整回答。

示例（省略时间等字段）：

```json
{"traceId":"request-123","event":"conversation.message","fields":{"role":"user","connectionName":"我的服务器","modelId":"model-id","state":"accepted","message":"你好"}}
{"traceId":"request-123","event":"conversation.message","fields":{"role":"assistant","connectionName":"我的服务器","modelId":"model-id","state":"completed","message":"你好，有什么可以帮你？"}}
```

对话正文以明文写入本机文件，保留换行和完整显示文本，不受诊断元数据的 512 字符截断规则影响。用户在正文中输入的敏感内容也属于正文，应在分享日志前处理。程序不额外记录配置中的 API Key、Authorization、密码、私钥、请求头、系统提示词、思考内容、命令参数、原始工具输出或供应商原始错误正文。通用诊断事件继续执行脱敏；供应商错误信息仅做有限分类，不把未知自由文本写入诊断日志。

日志写入使用独立锁，不参与聊天/审批数据库事务；失败不重试模型或命令，写入失败会在宿主标准错误中报告。没有日志上传功能。

## 自动保留与清理

桌面“诊断日志 → 日志保留时间”默认 7 天，可选 1 / 3 / 7 / 14 / 30 / 90 / 365 天。配置保存在 workspace.json 的 logRetentionDays，允许 1–3650 的整数，旧配置缺少字段时默认 7。修改独立保存并立即生效，不重启或断开 SSH，也不提交其他未保存草稿。

工作区启动和日志设置保存后执行清理；工作区运行期间每小时再次检查。按目录的本地日期保留最近 N 个自然日：例如 10 月 19 日保留 7 天，保留 10 月 13 日至 19 日，删除 12 日及之前的请求日志。应用关闭期间不运行清理，下次启动补做。

只清理 logs/agent 下合法日期目录中的 `.log` / 旧 `.json` / `.jsonl` Trace 文件，旧文件不会自动改名；跳过符号链接、Windows junction 和不相关文件，不递归删除未知内容，不处理 backend.log、数据库或附件。清理失败报告到宿主标准错误，不阻止对话。

实现位置：`src/diagnostics.rs`、`src/agent/provider/diagnostics.rs`；聊天/工具/审批/整理入口调用统一记录器。Tokio task-local 负责异步作用域，阻塞任务通过统一 spawn_blocking 带入 Trace；独立人工审批根据工具事件与 user_event_id 定位原始请求。桌面 host 的配置、IPC 接入位于客户端 `src-tauri/src/embedded_backend.rs` / `backend_ipc.rs`。


供应商续接状态（reasoning_content、thinking 签名、Gemini thought_signature、Responses 密文）独立保存在 SQLite 原生上下文表，既不当作可见回复，也不写入请求诊断日志。日志保留天数只控制日期目录下的请求文件；续接状态随对话历史保留，删除对话时一起清理。


失败原因与诊断文件分开保存：schema 23 的 agent_turn_errors 以 sessionId/requestId 关联失败回合，保存经现有错误类型处理的简短原因，和最终失败状态同事务提交。历史接口返回 errorMessage，不把错误拼入 partialText，也不将供应商原始响应体或密钥作为错误详情保存。删除对话同步删除；七天诊断文件清理不删除这部分对话信息。准入前未形成持久回合的错误仅在当前页面显示，界面明确区分。

## 上游模型查询日志（2026-10-20）

“查询上游模型”不属于某条聊天：桌面 IPC 为每次查询建立独立 Trace，沿用 `logs/agent/YYYY-MM-DD/<traceId>.log` 和前台日志级别 / 保留设置，失败 Toast 带 Trace ID。设置 → 本地工作区 → 诊断日志可打开目录；同一次查询的所有分页与收尾事件使用相同 Trace。

- INFO：`request.started/completed`、`model.catalog.started/completed`、`model.catalog.http_request/http_response`，包含 GET 地址、页号、鉴权方式（bearer / x-api-key / x-goog-api-key / none）、状态码、媒体类型、结果数量及耗时。
- DEBUG：`model.catalog.response_shape` 记录 models / data 和第一条相关字段的类型，不保存内容；现有 `model.http_response` 包含安全上游请求 ID。
- WARN：取消 / Future 中断；ERROR：HTTP / 网络 / 超时 / 大小与分页失败，以及 `model.catalog.parse_failed` 的 JSON 字段路径、预期格式和实际类型。JSON 本身无效仅记录行列，不保存原文。

请求地址保留版本 / 代理前缀，pageSize / limit 可见，pageToken / after_id 值替换为 REDACTED。密钥、原始 Header 和完整响应正文不落盘。例如 `/models/0/supportedGenerationMethods` 预期 array、实际 string 时，能定位错误字段。按 2026-10-20 用户后续批准，缺失 / null / 空数组能力条目现在跳过，DEBUG 用 `model.catalog.entries_skipped` 汇总数量；nextPageToken 为 null / 空字符串时结束分页，不再记为协议错误。
