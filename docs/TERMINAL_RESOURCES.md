# SSH 服务器资源监控

`TerminalManager::resources(session_id)` 提供当前已认证终端的 Linux 只读采样。桌面 IPC 及兼容 HTTP 接口 `GET /api/v1/ssh/terminal/resources?sessionId=...` 共用该方法。不存在或已关闭的终端返回 `S0003`。

采集在当前终端原始 transport 的独立 exec 通道执行 `src/terminal/resources.sh`，不申请 PTY、不复用共享 Shell、不使用 Agent 输出镜像、不创建新的认证连接。关闭／取消请求释放通道租约；达到通道限额时标记暂不可用，原交互终端继续使用。采样不延长交互终端的既有空闲超时。

## 指标口径

- CPU：`/proc/stat` 聚合 CPU 的相邻差值，idle 包含 iowait；guest 已包含在 user/nice 内，不重复累计。
- 内存：`MemTotal - MemAvailable`，单位转换为 bytes；旧内核缺少 MemAvailable 时用空闲、缓存与可回收 slab 近似，并扣除共享内存。
- 网速：`/proc/net/dev` 每个非回环网卡的 bytes 差值除以远端 uptime 差值。上传是服务器 TX，下载是 RX。摘要优先 IPv4 默认路由中 metric 最低的可用网卡；无匹配时选首个非容器虚拟网卡，再回退首个可用网卡。虚拟接口可能记录与物理接口相同的流量，详情按接口分别展示，不将其相加。
- 运行时间：`/proc/uptime`；开机时间：`/proc/stat` 的 btime（Unix 秒）。
- 磁盘：`LC_ALL=C df -P -k -l`，仅可读的本地文件系统；跳过 tmpfs、devtmpfs、udev 及 /proc、/sys、/dev 下的挂载。摘要优先 `/`，详情最多 64 项。百分比采用 df 的容量口径，bytes 保留 total、used、available。

首次采样、计数回退、重启或采样间隔超过 30 秒时，速率为 null。缺失、无效字段不会变成零。完整样本必须有固定协议头及结束标记，损坏／截断响应视为失败。非 Linux 主机返回 `supported=false`。

解析器忽略探针各段之间的空行（包括只有空白字符的行），保留已解析的 uptime；否则运行时长会被清空，后续 CPU、上传及下载速率都无法计算。回归测试同时覆盖真实探针分隔格式与第二次 SSH 采样的速率结果。

## 资源与生命周期

缓存及上一份原始样本归具体终端会话所有，同会话并发请求合并到一次采集；成功／失败结果至少缓存 2 秒，不支持结果缓存 60 秒。每次采集总超时 8 秒，stdout 与 stderr 合计上限 128 KiB。失败返回旧数据、原采样时间及 `stale=true`，不输出远端错误正文。前端失败后退避 15 秒，切换终端或隐藏页面时取消请求。

无需数据库迁移、新依赖或远端常驻进程。监控的是 SSH 原始连接的主机；在交互 Shell 内再登录其他机器或进入容器，不会改变监控目标。

指标来源：[Linux /proc 文档](https://www.kernel.org/doc/html/latest/filesystems/proc.html)、[CPU 统计说明](https://www.kernel.org/doc/html/latest/admin-guide/cpu-load.html)、[网络接口统计](https://www.kernel.org/doc/html/latest/networking/statistics.html)。
