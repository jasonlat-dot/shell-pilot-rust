# Rust SFTP 文件管理（阶段 3）

Rust Preview 0.3.0 使用 russh-sftp 3.0.1，兼容既有客户端 HTTP 和 Tauri 文件桥协议。
每个文件窗口建立独立 SSH 连接，复用已保存的连接参数、DPAPI 凭据和严格主机身份校验；每次元数据操作或文件传输独占 SFTP 子通道。
终端与 SFTP 互不依赖。当前固定桌面用户 default；打开会话时拒绝其他 userId 的连接。

## 接口

前缀 /api/v1/sftp。JSON 成功响应沿用 SUCCESS_0000 与 {code, info, data}，错误使用 SFTP_* 和对应 HTTP 状态。

| 方法 | 路径 | 功能 |
|---|---|---|
| POST | /sessions | connectionId、可选 rootPath -> 会话 |
| GET / DELETE | /sessions/{id} | 查询 / 关闭会话，关闭时取消任务和断开独立 SSH 连接 |
| GET / DELETE | /sessions/{id}/entries?path=... | 列目录 / 删除普通文件或空目录 |
| POST | /sessions/{id}/directories | path，新建目录 |
| POST | /sessions/{id}/files | path，新建空文件，不覆盖 |
| GET | /sessions/{id}/transfers | 任务完整快照 |
| GET | /sessions/{id}/events | progress / session-closed SSE |
| POST | /transfers | 创建上传清单或扫描生成下载清单 |
| GET | /transfers/{id} | 查询单个任务 |
| POST | /transfers/{id}/cancel | 取消整个任务，取消后需新建任务 |
| PUT | /transfers/{id}/items/{itemId}/content | application/octet-stream，上传条目 |
| GET | /transfers/{id}/items/{itemId}/content | 二进制下载流 |
| POST | /transfers/{id}/items/{itemId}/confirm | saved，确认客户端落盘 |

条目状态：PENDING、RUNNING、SENT、COMPLETED、SKIPPED、FAILED、CANCELLED。
目录和文件都计入任务完成数。下载字节发出后为 SENT，必须等客户端保存完成后 confirm；
空文件仍执行下载和确认，空目录直接创建本地目录后确认。
saved:false 将未完成条目标记 FAILED，可重新下载；目录重新创建成功后可再次确认。已完成确认幂等。
文件 FAILED 可重新请求 content；RUNNING、已完成或 SENT 的重复 content 请求返回冲突。

## 文件和路径规则

- 远程采用 POSIX 绝对路径；会话根目录留空使用账号默认目录。路径逐级 lstat 检查并校验 realpath，不跟随符号链接。
- 拒绝越级片段、反斜杠、通配符和控制字符。符号链接/特殊文件可列出，但不允许传输、跟随或删除。
- 上传补齐缺失父目录，目录准备失败时直接返回错误，不发布无法重试的目录条目。目录下载包含顶层名称、嵌套文件、零字节文件及空目录。
- 不递归删除，不删除会话根目录；新建空文件使用独占 OPEN，不覆盖同名条目。
- 上传先以独占 OPEN 创建目标同目录的随机 .sftp-*.part，分块写入、检查实际字节数及远端大小，关闭文件后提交。
- FAIL/SKIP 保留同名文件；提交时再次检查冲突。支持 hardlink 扩展时以不覆盖的硬链接提交，否则使用协议标准要求拒绝现有目标的 SFTP v3 RENAME。
- 显式 REPLACE 仅在服务器支持 posix-rename@openssh.com 时可用，检查目标类型/大小/修改时间后原子替换；不使用删除旧文件再写入的方式。当前客户端 UI 提供 FAIL 和 SKIP。
- 取消发生在 OPEN 回复到达前也保留临时路径清理责任；普通错误和取消尽力关闭句柄、删除本次临时文件。网络或服务器故障可能留下临时文件，日志记录路径。
- HTTP 文件流断线使条目失败。上传内容发送完整后响应丢失，提交结果可能不确定；先查询任务和远端文件，再决定重试，不能盲目重复提交。
- 取消不能撤销已经提交的文件；准备阶段创建的目录不会因失败自动递归删除。
- 路径检查无法代替远端账号权限和 SFTP chroot，远端同时重命名路径仍存在文件系统竞态。动态变化文件无一致性快照。

## 容量和生命周期

默认 50 个会话、每会话 100 个保留任务、每任务 10000 个条目、全局 100000 个跟踪条目。
路径最大 4096 字节、64 层、单个名称 255 字节；文件最大 64 GiB、任务累计最大 256 GiB。
桌面用户全局 4 个文件传输，每会话 2 个；元数据操作每会话一个。超出并发返回 429，不无限排队。
后台清理器每秒检查，无活动会话 30 分钟回收；任务 30 分钟无更新且无 RUNNING 条目时移除，包括放弃的等待/失败任务。

建连遵循 connection.connectTimeout，目录操作和扫描限 30 秒；文件流按连续 120 秒无进展判断，不限制文件传输总时长。
读写块最大 32 KiB，SFTP 响应包最大 256 KiB；目录分批读取并及时检查容量。
下载生产端通过最多两个数据块的有界队列向 HTTP 发送，慢客户端、取消或丢弃响应都会释放传输槽和通道。

SSE 首次发送快照，后续每 300ms 检查变化，15 秒心跳；一个会话只保留一个订阅，重连替换旧订阅。
事件是完整进度快照，无需重放事件序号。断开 SSE 不取消文件任务；查询和 SSE 不刷新会话空闲时间。
关闭、断网、闲置回收和宿主退出会终止旧会话。客户端收到 session-closed 后不自动建连，需要用户重新打开。
客户端已修正过期会话返回 404 的分类，并确保关闭过期窗口仍能清理本地记录。

## 验证

Windows x64，2026-10-13：

- 新增路径/清单单元测试及 12 项真实 SSH/SFTP 协议集成测试，后端合计 44 项测试通过；Clippy -D warnings 与 fmt 检查通过。
- 覆盖密码/私钥和主机密钥、终端独立性、目录/权限/越界/链接、嵌套和空条目、FAIL/SKIP/REPLACE、字节校验及重试、延迟 OPEN 回复时取消、下载响应丢弃/停滞、容量/空闲/任务回收和 SSE 重连/关闭。
- 客户端新增 3 项原生端到端测试：实际 Tauri Rust 文件桥 -> 实际 Axum 后端 -> 临时 SFTP 服务 -> 临时本地文件。验证 Unicode/嵌套/空文件与目录上传下载、同名保护、取消和过期会话；原生合计 14 项测试通过。
- 前端生产构建与 65 项前端回归测试通过，文件管理入口已恢复。
- 测试 SFTP 服务实现真实协议并操作临时 Windows 目录，未访问用户 SSH 服务器；实际 OpenSSH、Unix 权限和完整 GUI 点击仍由后续联调验证。

客户端原生全套使用 cargo test --lib -- --test-threads=1 通过。旧手写 HTTP mock 与新增协议测试同时并行时曾出现偶发本机网络错误；真实文件传输在单个测试内仍并发执行。
