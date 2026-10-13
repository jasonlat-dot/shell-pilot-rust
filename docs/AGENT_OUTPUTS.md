# A5：Agent 完整工具输出存储与范围回查

本项实现上下文内核的输出存储，不执行 SSH 命令，也未接通模型、chat_stream 或前端 AI 入口。
代码位于 src/agent/output.rs、output_writer.rs、output_model.rs，迁移为 migrations/004_agent_outputs.sql，schema 4。

## 数据流与边界

先由 Repository::append 记录 ToolCall，随后 begin_output 为该调用的 stdout 或 stderr 建立捕获。
OutputWriter::write 接收分块字节；finish 完成 SHA256 与有界预览后才发布。Drop、cancel、输入错误和超限会清理未发布的行与数据块，保留原工具调用的不确定状态。
append_captured_result 把 stdout/stderr 的预览与 OutputReference 加入 ToolResult，并在同一事务中关联输出和事件。它不推断执行成功，调用方必须提供真实 Outcome/exitCode。
重复捕获请求完成后返回 Existing 只读回执；重复事件返回 inserted=false。调用方不得据此重新执行命令，实际副作用去重仍属 D3。

选择 SQLite 分块代替额外文件目录：输出、会话、事件和检查点在同一数据库，备份/恢复时无需协调两个存储源，也不引入新的文件路径或符号链接入口。
写入时仅持有本地 SQL 操作所需的锁，不在读取输入流时持有数据库锁。内核接口是同步的，异步宿主需要阻塞工作线程/有界桥接；网络工具应使用现有超时与取消循环调用 write，不能在 async 线程直接调用阻塞的 copy_from。

## 默认限制

| 项目 | 默认值 |
|---|---|
| 单路完整输出 | 最多 16 MiB |
| 存储数据块 | 64 KiB，最后一块可更短 |
| 模型预览 | 约 4 KiB 加截断标记，保留头尾 |
| 单次范围回查 | 1..65536 字节 |
| 每会话总容量 | 128 MiB |
| 全库总容量 | 512 MiB |
| 全库输出数量 | 10000 |
| 同时捕获 | 4 路 |
| 捕获无写入进展 | 120 秒后过期 |

OutputLimits 可由宿主配置，但有最大值与一致性校验。全库限额最大 1 GiB，单路限额最大 16 MiB。
捕获开始前按单路上限预留，完成后按真实大小计费；剩余空间不足一整路上限时，不能再开始新捕获，即便预期输出较小。
容量统计是逻辑字节；删除未引用输出后 SQLite 可复用页，不保证数据库文件立即缩小。
已关联事件的完整输出不允许 discard，防止检查点或历史引用失效；存储满时拒绝新捕获，不覆盖历史证据。后续历史归档/保留策略需要单独实现。

## 范围回查

Repository::output 返回 ID、channel、size、sha256、taskId、callId、有界预览、previewTruncated/previewLossy 和关联事件 ID。
read_output_range(sessionId,userId,outputId,offset,limit) 返回原始 bytes、offset、nextOffset、eof 和 size：
- 按 session/user 过滤，输出必须已完成；写入中的半成品不可读。
- offset 以原始字节计，允许等于总长度，此时为空且 eof=true；越界或 limit 非法返回错误。
- 读取跨块范围仍有界，并核对数据块长度与 SHA256；缺块或内容校验失败不返回部分成功。
- 字节偏移可切入 UTF-8 字符，调用方需拼接或增量解码。原始二进制完整保留；预览采用有损 UTF-8 时明确标记 previewLossy。
- 查询不会执行命令，也不改变任务的绑定服务器。

## 引用与恢复

ToolResult.outputs 是带默认空列表的可选字段，兼容 schema 3 旧事件；每项包含 id、channel、size、sha256。
关联时核对输出所属会话/调用、完成状态和全部元数据，拒绝伪造、重复及跨会话引用。旧任务的迟到输出和结果继续归属原 taskId，不随当前页签或新任务切换目标。
检查点不删除事件/完整输出，压缩后仍可按输出 ID 回查证据。

过期恢复只删除 writing 且已超时的捕获，保留其他实例仍活跃的捕获以及所有完整输出。
新捕获前会执行过期恢复，宿主也可调用 recover_outputs；本项没有独立后台清理循环。过期后原 writer 的 token/状态检查会失败，不能发布或删除替代捕获。
取消与输出读取失败不代表远端命令没有产生副作用；D3 仍需实现真实执行器的状态核实与恢复策略。

## 验证

tests/agent_outputs.rs 的 17 项测试覆盖：
- 大 Unicode 输出的有限缓冲读入、SHA256、跨块与 UTF-8 字节范围回查；
- 空输出、二进制预览、有界事件与 stdout/stderr 关联；
- 会话/用户/调用归属、伪造引用拒绝与幂等回执；
- 取消、Drop、输入失败、超限清理、并发配额和容量回收；
- 过期恢复、陈旧 writer、其他连接活跃捕获、SQLite 重启；
- 缺块/内容损坏拒绝、检查点后证据保护和旧事件兼容。

A1–A4 原有 17 项测试继续通过。全量与桌面回归结果见 AGENT_PROGRESS.md。

本批最终验证：后端全量 94/94、桌面桥 3/3、Clippy -D warnings 与 fmt 均通过；未打包。
