# 请求诊断日志

一次用户发送使用现有 requestId 作为 traceId；模型请求、自动审查、人工审批、工具执行、输出回传、上下文整理和结束共用该标识。显式重新发送生成新的 ID，补充/引导消息归入当前请求。历史回看不执行模型或工具。

桌面设置 → 本地工作区 → 诊断日志，可选择 debug / info / warn / error。默认 info；只记录所选级别及更严重的事件。选择后即时保存到 workspace.json 的 logLevel，并更新当前工作区的原子过滤级别；不重启或断开 SSH。工作区尚未启动时保存的级别在下次初始化时应用。

日志按操作系统本地日期分目录，文件为 UTF-8 JSON Lines：

```text
<应用本地目录>/logs/agent/2026-10-19/<traceId>.jsonl
```

设置页可打开专用日志目录；原 backend.log 继续记录工作区启动/停止。独立 Rust 服务默认在数据库目录的 logs/agent 保存请求日志。跨午夜的同一 Trace 会出现在两个日期目录，traceId 保持不变。非文件名安全的请求 ID 使用 SHA-256 文件标识，避免路径穿越。

每行包含 timestamp、level、traceId、requestId、sessionId、agentId、sequence、event 和 fields。sequence 在当前诊断实例中递增；跨进程重启以时间与文件追加顺序为准。每次模型对话调用另带 modelCallId，区分同一 Trace 内的多轮续聊与自动审查。

| 级别 | 内容 |
| --- | --- |
| debug | 消息角色和字节数、工具定义名称、预算/输出参数、模型轮次、HTTP 响应等结构与计时 |
| info | 请求开始/完成、模型调用、审批状态、工具派发与结果持久化、整理提交 |
| warn | 拒绝、取消、中断、结果未知、整理回退 |
| error | 模型 HTTP/网络/协议失败、请求失败 |

`request.started` 与 `request.completed / request.failed / request.cancelled / request.interrupted` 表示请求边界；future 被丢弃不会记为完成。`model.http_error` 在尝试读取错误正文前记录 HTTP 状态及供应商请求 ID；`model.http_error_detail` 只记录经过限定的 error.type / code 和错误分类。HTTP 400/429/5xx 不再只剩通用 MODEL_HTTP_ERROR。供应商正文若未返回/过大/超时，仍保留已获得的状态记录。

日志不保存原始用户正文、提示词、思考内容、命令参数或原始工具输出。DEBUG 也不记录 API Key、Authorization、密码、私钥、请求头和供应商原始错误正文；相关原始聊天/工具证据继续通过现有历史查询。供应商错误信息仅做有限分类（例如 reasoning_content_required、context_limit、rate_limited），不把未知自由文本写入诊断日志。

日志写入使用独立锁，不参与聊天/审批数据库事务；失败不重试模型或命令，写入失败会在宿主标准错误中报告。当前不会自动清理历史日志，也没有日志上传功能。

实现位置：`src/diagnostics.rs`、`src/agent/provider/diagnostics.rs`；聊天/工具/审批/整理入口调用统一记录器。Tokio task-local 负责异步作用域，阻塞任务通过统一 spawn_blocking 带入 Trace；独立人工审批根据工具事件与 user_event_id 定位原始请求。桌面 host 的配置、IPC 接入位于客户端 `src-tauri/src/embedded_backend.rs` / `backend_ipc.rs`。
