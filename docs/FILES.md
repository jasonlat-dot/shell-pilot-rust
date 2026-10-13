# 阶段 4：本地文件与附件

Rust Preview 0.4.0 将附件存储直接嵌入 Tauri 主程序。前端顶部“文件与附件”无需 SSH 会话即可上传，查看已存文件、刷新下载链接和明确确认删除。上传记录只在当前应用内存中；“已存文件”来自持久化数据库，重启后可重新加载。文件存储完成不代表已接入 AI 对话，Agent/聊天是阶段 5。

## 接口

统一使用 {code, info, data} envelope，成功码 SUCCESS_0000，camelCase 元数据与原客户端上传协议兼容。

| 方法 | 路径 | 内容 |
|---|---|---|
| POST | /api/v1/files | multipart/form-data，严格一个 file 字段，不接受 userId 或其他字段 |
| GET | /api/v1/files?offset=0 | 每页最多 20 项，items、offset、hasMore；每次签发新链接 |
| GET | /api/v1/files/{fileId} | 已存文件元数据和新下载链接 |
| DELETE | /api/v1/files/{fileId} | 删除数据库所属附件与本地对象，释放容量 |
| GET | /api/v1/files/storage | usedBytes、reservedBytes、maxStorageBytes、fileCount、maxFiles、maxFileBytes、retentionDays、downloadLinkSeconds、enabled |
| GET | /api/v1/files/local | 校验完整签名查询参数后下载原始二进制 |

上传/元数据包括 fileId、fileName、contentType、size、sha256、status=UPLOADED、downloadUrl、urlExpiresAt。
链接签署 storageId、objectKey、fileName、expires，HMAC-SHA256；客户端原样使用整个 URL。
签名密钥每次启动随机生成、不写入配置或数据库。服务重启后旧链接无效，从已存文件或元数据接口获取新链接。
已过期返回 HTTP 410 / FILE_DOWNLOAD_LINK_EXPIRED，非法参数/签名返回 400 / FILE_DOWNLOAD_LINK_INVALID，缺失附件返回 404 / FILE_DOWNLOAD_NOT_FOUND。
链接以实际回环监听地址生成，不采用客户端 Host/Forwarded 头。

## 默认限制与生命周期

- 单文件非空且最多 20 MiB，整个 multipart 最多 21 MiB。
- 扩展名 png/jpg/jpeg/webp/pdf/txt/log/csv/json/yaml/yml/md/docx/xlsx，大小写不敏感；保留净化后的原文件名，磁盘名使用随机 UUID。扩展名验证不代表内容识别；contentType 是规范化后的上传声明。
- 存储限额 1 GiB，最多 10000 项，后端最多 4 路上传、8 路下载；前端队列最多 2 路。
- 上传前按单文件上限预留容量，完成后计实际大小；已有已存文件、待清理对象和活动上传都计入限额。因此容量余量小于 20 MiB 时，小文件也会拒绝新上传。
- 上传和下载使用分块读写、有界缓冲，不将整份文件加载到内存。上传流 120 秒无新数据会失败。
- 临时文件先写入、flush/sync 后以硬链接原子发布，不覆盖既有对象，随后更新 SQLite。附件目录所在文件系统需支持硬链接（本次验证为 NTFS）。
- HTTP 等待者消失后由独立任务完成或清理；中断/超限不会暴露半成品。完整上传但响应丢失时，文件可能已经保存，手动重试可能产生新文件，前端不会自动重试。
- 默认保留天数 0，长期保留。设置正数后按创建时间清理，包括此前已存附件；退出前请自行保留需要的文件。
- 启动时恢复未完成上传/删除，随后每 60 秒清理，单批最多 100 项；仅操作数据库所属对象及可核对的临时别名，保留不相关文件。清理失败记录仍占容量并在下次重试。
- 附件根目录和读取对象拒绝 junction/reparse/symlink。根目录持有 OS 文件锁，同一数据目录只能有一个附件服务实例；第二实例返回 FILE_STORAGE_BUSY，不能误清理第一实例的上传。

默认附件目录为数据库父目录下 attachments。桌面默认：
%LOCALAPPDATA%\com.jasonlat.shellpilot.rust-preview\backend-data\attachments。
SQLite schema 版本 2 自动创建 file_assets 表，保留现有连接配置。切换目录不会搬迁数据；备份需同时保留 SQLite 与附件目录。

下载以 application/octet-stream、Content-Disposition: attachment、UTF-8 文件名、nosniff 和 no-store 返回。下载链接是有效期内的持有者凭据，应保持完整且避免分享。该服务仍面向本机单用户，userId 不是身份认证。未集成 MinIO、云对象存储或自动导入 Java 附件。

## 配置

桌面“客户端设置”的 Rust 服务页支持启用附件、限额 20..102400 MiB、保留 0..3650 天、链接 1..10080 分钟。保存后需重启内置服务，旧链接和当前 SSH/SFTP 会话随之失效。
rust-backend.json 使用 fileStorageEnabled、maxStorageMiB、fileRetentionDays、downloadLinkMinutes；旧版配置缺字段时兼容默认值。

独立 server 环境变量：

| 变量 | 默认 |
|---|---|
| SHELLPILOT_FILES_ENABLED | true |
| SHELLPILOT_FILE_MAX_STORAGE_MIB | 1024 |
| SHELLPILOT_FILE_RETENTION_DAYS | 0 |
| SHELLPILOT_FILE_LINK_SECONDS | 900 |

## 验证

16 项附件后端测试覆盖 multipart 分块/Unicode/SHA256、请求限制、容量/数量、过期/篡改、下载头、重启恢复/链接重签、保留策略、分页/删除、并发/停服清理、Windows junction 和双实例锁。
既有 SSH/SFTP/API 回归一起运行，后端合计 60 项；cargo fmt 与 Clippy -D warnings 通过。
客户端 69 项前端回归及 1 项真实 Rust 文件服务联调通过。真实联调使用临时目录验证上传、下载、链接过期刷新、3 个文件经队列并发、历史和删除容量回收。
客户端 15 项原生测试通过，须串行运行：cargo test --manifest-path src-tauri/Cargo.toml --lib -- --test-threads=1。GUI 文件选择、系统浏览器下载和实际安装仍由用户试装验证。

后续 AI 阶段可用 Manager::open_attachment(fileId) 读取受校验的本地对象；本阶段未宣称迁移 chat_stream、模型调用或 Agent 工具。
