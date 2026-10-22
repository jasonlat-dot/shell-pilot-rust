# 终端侧栏目录读取

`TerminalManager::directory_list(session_id, user_id, path)` 向桌面 `GET ssh/terminal/directory` IPC 返回目录元数据。空字符串解析为登录账号的 SFTP 家目录，非空要求绝对路径。返回 `path`、`entries` 和 `truncated`；条目只含名称、目录/符号链接标记及大小，没有文件正文。

请求核对原终端的用户身份与连接状态，复用该终端创建时的 SSH transport。普通身份使用 SFTP `realpath`、`opendir`、`readdir` 和句柄关闭；已接入的 sudo root Shell 使用下述会话通道。不创建新的认证连接、不向 PTY 注入读取命令、不重复 sudo。不访问二次 SSH 的其他目标。

目录通道占用原 transport 的通道额度及宿主 opening 额度，打开期限 3 秒、目录请求总期限 5 秒；单包最多 256 KiB，目录最多 4096 个有效条目及 1 MiB 名称。省略 `.`、`..`、控制字符及带路径分隔符的异常名称，超限时返回明确的 `truncated=true`。错误不转为空目录；请求取消、断线或工作区关闭时释放读取通道，不关闭交互终端。

当前目录由客户端会话级 Bash 钩子上报，具体接入与界面说明见 [客户端终端文件浏览](../../ai-ssh-terminal-client/docs/terminal-files.md)。客户端在路径变化或提示符返回时刷新，可见时每次读取完成 5 秒后更新；隐藏和切换目录时取消旧请求，迟到结果不会跨服务器或路径覆盖。没有有效报告时只展示登录目录并提示，不通过额外 exec 猜测当前 Shell 的 cwd。

本功能不增加数据库迁移、供应商参数或模型请求。需要配套重编译桌面客户端及 Rust 库。

## 复用终端 root 认证

客户端 Bash 集成将原登录 UID 随无其他参数的 `sudo -i` / `sudo --login` 传入目标 Bash。认证成功后由这个 root Shell 启动只读元数据 worker，使用 root 拥有、普通账号不可改写的临时目录 `/tmp/shellpilot-files.<nonce>.<random>`，请求 FIFO 和回复文件只授权原登录 UID。后台的 SSH exec 仅以原登录账号转发固定请求和读取元数据，不运行 sudo、不存储或重放密码。worker 只进行 canonical cwd 与 GNU find 元数据读取，输入不是 Shell 代码；响应限量，领取后回收，未领取 15 秒回收，最多 32 份，父 Shell 退出自动清理。

`directory_access::Reports` 独立解析 PTY 中当前会话私有 nonce 绑定的 `F;uid;loginUid;bridge` 报告。bridge 必须匹配该会话 nonce 和严格随机路径格式，前端不能选择另一终端的通道。提权状态与 revision 绑定，读取完成后再次复核，退出 root 时丢弃迟到结果；断线、取消和超时释放原 SSH channel。通道未就绪明确报错，不重新索取认证、不退回普通身份冒充成功。普通 SSH root 登录继续直接使用其 SFTP 身份。

需要 Linux Bash、GNU find/timeout 和命名管道，不修改服务器配置、账号或 sudoers，不安装 Python/agent。

## 文件管理默认当前终端身份与目录

两个文件管理入口统一绑定逻辑终端标签，打开时传递该终端 sessionId 和当前目录。`TerminalManager::file_access` 核对连接归属/用户、捕获 transport 与身份 revision，返回不可由 JSON 伪造的 `TerminalFileAccess`，交给 `sftp::Manager::open_terminal`。普通身份复用原 transport 的 SFTP；root 身份经固定 `files_collect.sh` 请求已认证 root Shell 的 worker，使用单次随机 ID 的入/出 FIFO，启动固定路径的 OpenSSH `sftp-server`；不重复认证、不读取保存的密钥、不存储 sudo 密码。

文件流最多 4 条，未连接流 10 秒回收，结束时关闭进程和 FIFO。worker 与原 root Shell 同生命周期；重新接入替换旧 worker。`TerminalFileAccess` invalidation 监控在退出 root/切换身份/终端关闭时撤销文件会话与传输，打开新操作前也同步复核。文件会话关闭不 shutdown 复用的 transport。原有独立 SFTP 认证方式兼容，仍保留其自身的关闭行为。

默认目录是初始显示位置，不是新的访问根范围；已知目录时打开 `/` 范围后定位到该目录。传输中的旧窗口不会因再次打开入口被强制重连。同服务器多终端分别持有文件窗口，目录和认证状态不互相继承。支持实际读写传输，因此仅在用户打开文件管理时创建完整 SFTP 通道；侧栏目录轮询仍只读元数据。

权限切换撤销旧文件会话时返回 `SFTP_IDENTITY_CHANGED`（409），目录操作与关闭事件均保留该原因；桌面适配层继续向迟到请求传递此状态。前端静默清理旧身份，不弹“连接、会话、任务或文件不存在”。实际连接中断、文件不存在与权限错误仍使用原有错误处理。
