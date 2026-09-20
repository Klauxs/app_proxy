# Guard 分段实测：2026-09-20

## 结论

生产 Release 开启可选计时，在已安装 Codex、Claude 的空白隔离实例各运行三次。六次均完成精确关闭及带代理重启，计时记录未丢失。主要耗时是 WMI 查询返回数据，约占总时间的 61%～65%；Windows 原生终止不是当前主要瓶颈。本轮没有替换 WMI 或改变停止策略。

| 阶段（三次均值，ms） | Codex | Claude |
|---|---:|---:|
| 进程创建 → 本轮首次扫描开始 | 25.2 | 179.9 |
| 首次扫描（含第一轮 WMI） | 290.4 | 311.1 |
| 扫描完成 → 启动准备开始，含登记/调度 | 23.8 | 33.7 |
| 安装信息、数据目录、资源准备 | 48.8 | 56.0 |
| 关闭前复核，含候选枚举、WMI、重试及辅助句柄固定 | 314.2 | 539.2 |
| 停止授权、记录及派发 | 22.2 | 22.4 |
| 进入原生停止函数 → Windows 记录退出 | 5.4 | 4.8 |
| **创建 → 退出总耗时** | **730.1** | **1147.2** |
| 其中 WMI（已包含在上方，不能再次相加） | **471.6** | **699.4** |

| 样本 | 总耗时 ms | WMI 合计 ms | WMI 次数 | 关闭前重试次数 |
|---|---:|---:|---:|---:|
| Codex 1 | 783.6095 | 501.489 | 2 | 0 |
| Codex 2 | 669.7879 | 413.251 | 2 | 0 |
| Codex 3 | 736.9223 | 499.933 | 2 | 0 |
| Claude 1 | 1240.6916 | 802.840 | 3 | 1 |
| Claude 2 | 1058.3285 | 653.451 | 3 | 1 |
| Claude 3 | 1142.6832 | 642.058 | 3 | 1 |

以 Claude 1 为例：首次 WMI 273.08 ms，关闭前 WMI 261.73 ms，候选变化后重新采样的 WMI 268.03 ms，总共 802.84 ms。三次连接 WMI 合计约 37.49 ms，读取结果合计约 757.59 ms。`Next` 的超时参数是等待上限，不能据此说存在固定 250 ms 睡眠。当前记录没有逐个 `Next` 打点，不区分第一行到达与枚举结束的各自等待。

## 为什么还有 WMI

目前采用 `Microsoft-Windows-Kernel-Process`（22fb2cd6-0e7b-422b-a0c7-2fad1fd0e716）启动通知。代码只传递 PID、映像名、事件时间。本机 `Get-WinEvent -ListProvider` 返回的 ProcessStart 事件 ID 1、版本 0～4 均没有 `CommandLine`，有父 PID、创建时间、会话等字段；原始模板保存于 `.tools/guard-timing/kernel-process-start-schema.json`。

因此当前实例归属逻辑仍通过 WMI `Win32_Process` 读取 CommandLine、ParentProcessId、SessionId、CreationDate，用于 `--proxy-server`、`--user-data-dir`、主/子进程关系核验。实际用户、会话、创建时间、映像文件身份仍从原生句柄核对。WMI 用于补查，不负责监听启动，也不负责执行停止。首次扫描和关闭前复核分别取得新数据；退出竞态会完整重采样，不能复用过期名单或丢弃失败项。

WMI 不是必须长期保留的依赖。可评估原生命令行读取路径，或者包含命令行的另一类内核进程事件；微软的 [Process_V2_TypeGroup1](https://learn.microsoft.com/en-us/windows/win32/etw/process-v2-typegroup1) 文档包含 CommandLine，但它不是当前订阅的 Provider/事件模板，不能直接将字段名套入现有订阅。选择替代路径还需验证启动早期读取、权限、跨架构和 PID 重用；保留确切目标的原生句柄核验。WMI 字段说明见 [Win32_Process](https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-process)。

## 测量口径与启用方式

设置普通协调进程启动环境 `APP_PROXY_TIMING_FILE` 为目标 NDJSON 路径，再启动新协调进程。本次为 `.tools/guard-timing/timing.ndjson`；默认未设置时不记录。现有常驻进程不会自动读取后来修改的环境。日志只有阶段、关联 UUID/PID、时间和累计丢弃数，不保存命令行或应用数据。2048 条有界队列、后台写入、单进程写入预算 32 MiB；高权限进程拒绝通过此环境项写文件。写入失败或队列满不阻塞纠正，后续记录携带丢弃计数。退出前要留出后台刷新时间；文件存在不等于记录完整，本轮检查覆盖了每个目标的全链路关键阶段且丢弃数为零。

总耗时仍以确切目标句柄的 `GetProcessTimes` 创建/退出 FILETIME 计算。各 Span 用单调时钟计算持续时间，同时记录可与 FILETIME 对齐的起止时刻。分段表按不重叠的时间边界划分，嵌套 WMI 另行列出。Windows 记录退出到句柄真正变为已终止状态之间仍有清理时间，所以 `stop.wait_exit` 完成时间可能晚于表中的退出时刻；这不会被误算到前置查询。

启动到扫描不等于本进程 ETW 投递延迟：其他进程事件也会触发扫描。三个 Codex 样本在自己的启动通知到达协调进程之前就已被扫描发现；该通知实际到达时间分别为创建后 123.5351、180.6815、107.2514 ms。不能把表中 25.2 ms 解释为 ETW 平均投递时间，也不能把这些通知延迟再加到总耗时。

验证：37 项 Guard 回归和 6 项进程查询测试通过，1 个子进程 fixture 按默认 ignored；Release、Clippy all-targets `-D warnings`、fmt、diff check 通过。没有因纯打点重复整个默认测试集，上轮业务修复全量为 446 通过。管理员安装与 active ETW 实测均完成，脚本退出码 0。75 个测试应用进程、测试核心、网络辅助进程、协调进程已退出；测试保护禁用、登录入口已移除，原 Codex PID 39116 完整身份保持运行，监听任务保留为 Ready。

原始证据：`.tools/guard-timing/timing.ndjson`、`breakdown.json`、`analyze.py`、`live/results.json`、`live/run.log`、`live/final-cleanup.json`。本轮有打点开销且样本量有限，数据用于定位瓶颈，不构成延迟上限保证。
