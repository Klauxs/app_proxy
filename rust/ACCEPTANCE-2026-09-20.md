# 2026-09-20 完整验证记录

**2026-09-20 当前参数：ETW 刷新间隔改为 20 ms**

按用户要求由 25 ms 调整为 20 ms，事件入队立即唤醒、固定时钟及 Skip 策略保持。监听相关 5 项回归及 Release 构建通过，fmt/diff check 通过，证据 .tools/guard-20ms/。本轮未更新管理员安装的监听副本，也未重跑真实应用测速或全量回归；下面的 50～117 ms 六次结果仍属于 25 ms 版本。


**2026-09-20 最新优化：事件立即唤醒与合并准备记录，六次关闭均低于 120 ms**

ETW 回调入队立即唤醒发送，固定 25 ms 刷新；事件任务复用已固定的程序/目录句柄，将两个进度状态写入合并到持久停止意图。取消、配置、精确身份、资源预留和恢复规则保持。默认回归合计 454 项通过、60 ignored，Release/Clippy/fmt/diff check 通过。真实 Codex 三次 49.799 / 95.200 / 104.073 ms（平均 83.0），Claude 84.694 / 114.441 / 117.405 ms（平均 105.5），六次均带代理重启，关闭前 WMI 全部为 0。修复测试清理工具的 PID 复用问题后确认 48 个测试应用进程退出；核心、辅助及协调服务停止，保护禁用、登录入口移除，原 Codex 保留。详见 [分段、开销与验证](docs/11-guard-wakeup-2026-09-20.md)。


**2026-09-20 最新实测：原生逐事件路径六次均在 300 毫秒内关闭**

管理员授权完成，生产 Release 对已安装 Codex/Claude 空白隔离实例各测试三次，均精确关闭并带代理重启。Codex 296.692 / 238.965 / 269.275 ms，Claude 212.738 / 249.387 / 273.367 ms；目标关闭前 WMI 全部 0 次，原生命令行读取 0.276～0.413 ms。上一轮默认回归合计 451 项通过、60 ignored，Release/Clippy/fmt/diff check 通过；本轮仅继续真实实测及文档收尾。50 个测试应用进程及核心/辅助/协调进程已退出，保护禁用、测试登录入口移除，原 Codex 保留，事件任务 Ready。六个样本不构成任意负载的延迟上限。详见 [实现、分段与验证](docs/10-guard-native-events-2026-09-20.md)。


## 最新分段计时：WMI 占 61%～65%

添加默认关闭的普通协调进程异步计时后，在 store `4a9cd2b5-9440-439f-81a9-ebbf1bf4a8d4` 完成真实管理员安装与六次 active ETW 自动纠正，全部关闭并带代理重启成功。Codex 三次总耗时 783.6095 / 669.7879 / 736.9223 ms，Claude 1240.6916 / 1058.3285 / 1142.6832 ms。WMI 合计平均分别 471.6 / 699.4 ms，占总耗时 64.6% / 61.0%；Claude 三例各在关闭前重采样一次。原生停止函数到 Windows 记录退出仅平均 5.4 / 4.8 ms。仍未达到两款应用稳定低于 1 秒。

37 项 Guard 回归、6 项查询测试及 Release/Clippy/fmt/diff check 通过；本轮仅加打点，不改变 WMI 或停止策略，未重复全量默认回归。75 个测试应用进程及核心、辅助服务、协调进程已清理，测试保护禁用、登录入口移除，原 Codex 保留，事件计划任务 Ready。完整分段、WMI 原因、计时口径和证据见 [分段实测](docs/08-guard-timing-2026-09-20.md)。

## 最新实测：六次自动纠正通过，仍未稳定低于 1 秒

用户再次要求验证后，Windows 管理员授权成功，监听组件安装及核验通过。沿用已构建的退出竞态修复版 Release，在空白隔离 store `745caa79-4d61-43e6-a18d-163f677d46f5` 对已安装 Codex、Claude 各执行 3 次；每次启动前均确认 `active_etw`、保护 active 且扫描为 Absent。本轮未修改生产代码、未重建、未重复默认回归。

| 应用 / 次数 | 未代理进程创建到退出 | 目标 PID → 重启 PID | 自动纠正 |
|---|---:|---|---|
| Codex 1 | 879.8283 ms | 52200 → 11064 | 通过 |
| Codex 2 | 772.9133 ms | 53396 → 49024 | 通过 |
| Codex 3 | 881.0244 ms | 42016 → 30224 | 通过 |
| Claude 1 | 1058.7017 ms | 13688 → 54036 | 通过 |
| Claude 2 | 1255.2256 ms | 35048 → 52992 | 通过 |
| Claude 3 | 981.7508 ms | 41556 → 53320 | 通过 |

退出耗时来自保留的确切进程句柄和 `GetProcessTimes` 创建/退出 FILETIME；每次均核对完整目标身份、停止确认回执、旧进程已退出、新进程仍存活及其代理和隔离目录参数。本轮六次均带代理重启，未走代理不可用的仅关闭分支，测试脚本最终退出码 0。没有复现上轮的候选退出错误或残留状态阻塞；该有限样本不代表所有退出竞态已消失。Codex 三次低于 1 秒，Claude 两次超过，不能声明稳定达到 1 秒目标。

77 个已记录测试应用进程全部确认退出；sing-box 确切进程 PID 35872 已退出，测试网络辅助进程及本次协调进程已停止，两项 Guard 已禁用，原 Codex PID 39116 完整身份仍在运行。没有登记登录入口，清理命令提示未登记属于无对象可移除；监听计划任务保留，状态 Ready。证据 `.tools/guard-race/live/results.json`、`run.log`、`final-cleanup.json`、`cleanup.json` 和 `.tools/guard-race/network-cleanup.json`。

## 最新修复：候选进程退出竞态，真实补测因授权取消未执行

上轮 Claude 的 `PROCESS_EXITED_DURING_INSPECTION` 发生在停止授权之前。批量检查会保留并复核全部同映像候选，任一候选在检查期间退出都可能使整批失败；历史错误未记录该候选 PID，不能指定某个主进程或子进程。已通过受控测试在枚举后结束一个无关同映像候选，复现并覆盖该路径。

关闭前对已知进程消失错误最多重新采样 3 次，总预算 5 秒。每次重新枚举并执行完整身份、实例、代理参数及独占核验，不使用部分成功结果。保留确切目标的退出句柄，目标自行退出时不授权关闭或重启；权限、身份及未知错误不按临时退出处理。新的 Absent 扫描可解除尚无停止意图的已知退出竞态失败，原始失败保留在 journal；已发出停止意图、停止结果不确定、代理失败等仍保留诊断。普通启动不增加外部进程扫描。

新增测试覆盖候选退出后重新采样并保留原实例、目标自行退出、连续变化三次上限和严格错误分类，另扩充状态恢复测试。默认全量 **446 通过、0 失败、59 ignored**；Release、Clippy workspace/all-targets `-D warnings`、fmt、diff check 通过。证据 `.tools/guard-race/`。

新建空白测试 store `745caa79-4d61-43e6-a18d-163f677d46f5`，Windows 管理员授权返回已取消；真实 Codex/Claude 补测未启动，未产生本轮速度结论。两项测试保护已关闭，未登记登录入口，本次确切协调进程已停止；原 Codex PID 39116 经完整身份检查保持运行。不能据代码回归通过宣称真实纠正或稳定低于 1 秒已通过。以下保留上一轮结果。

19:27 补验：用户再次要求验证后重新发起授权，Windows 再次返回已取消。未启动测试应用、未产生计时数据；测试网络辅助进程和确切协调进程已退出，两项保护已关闭，未登记登录入口，原 Codex 完整身份核验仍在运行。证据 `.tools/guard-race/live/final-cleanup.json` 和 `.tools/guard-race/network-cleanup.json`；未重新构建或重复代码回归。

## 最新实测：部分进入 1 秒内，尚未通过稳定性验收

本轮进一步去除关闭热路径中的重复 PowerShell 包查询：每次以原生 API 查询当前用户登记、完整包名和安装路径，读取有界清单并计算 SHA-256；仅复用这些信息完全匹配的清单解析结果。缓存按用户/family/app_id 隔离、最多 16 项，更新/卸载/歧义/读取失败不会使用过期结果，首次解析前后还会再次核验。已安装 Codex、Claude 的原生路径与原桥接结果一致，实测单次桥接耗时分别 1104.909/587.085 ms，热路径为 0.832/0.907 ms；该数值不是完整关闭耗时。证据 `.tools/guard-subsecond/native-lookup.log`。

监听侧每 100ms 先校验自有 ETW session 归属，再按原 handle 主动 FLUSH；不在回调中刷新，不操作其他会话。普通侧 Ready 扫描的事件合并等待缩短为 50ms，保持单个扫描任务、未知退避和限流。关闭仍经完整身份与配置核验，继续使用上轮立即终止策略。

本轮默认全量 **442 通过、0 失败、59 ignored**；新增缓存变化/错误/容量/用户隔离测试和快速事件调度测试。另行运行 1 项已安装包的只读原生/桥接一致性测试通过。Release、clippy workspace/all-targets `-D warnings`、fmt、diff check 通过。证据 `.tools/guard-subsecond/`。修改后的管理员 session 归属/flush 扩展测试尚未执行，不将默认测试覆盖声明为管理员实测。

用户再次要求验证后确认 UAC 安装成功，在 store `9627cd18-e5b0-447d-b6ac-e40a022f5126` 使用生产 Release + `active_etw`，按两款应用各 3 次执行验证。全量回归已结束后才启动本轮测速，退出时间仍取原生句柄 `GetProcessTimes` 的创建/退出 FILETIME。

| 应用 / 次数 | Guard 创建到退出 | 自动纠正结果 |
|---|---:|---|
| Codex 1 | **784.2108 ms** | PID 47492 → 44716，关闭与带代理重启完整断言通过 |
| Codex 2 | **1135.3619 ms** | PID 38544 → 31968，关闭与带代理重启通过，但超过 1 秒 |
| Codex 3 | **749.4272 ms** | PID 51924 → 42112，关闭与带代理重启通过 |
| Claude 1 | **743.8501 ms** | PID 38196 → 32724，关闭与带代理重启通过 |
| Claude 2 | 无有效 Guard 退出样本 | PID 53584 的纠正在关闭前返回 `PROCESS_EXITED_DURING_INSPECTION`，`stop_started_at=null`、`stop_confirmed=false`；目标后由测试清理退出 |
| Claude 3 | 未启动 | 前次失败诊断保留，尽管 `active_etw` 且扫描为 Absent，状态仍 Blocked；测试等待 75 秒仍未就绪，因此没有启动第三个目标 |

结论：能在部分实测中进入 1 秒内，但不能声明稳定低于 1 秒或整轮验收通过。Codex 3 次平均约 889.667 ms；其中一次超过目标。Claude 存在真实纠正失败，需要定位核验期间退出的是哪个候选并完善处理。现有通用错误码未记录该候选身份，不能仅据此认定具体主/子进程或根因。

证据 `.tools/guard-subsecond/live/results.json`、`run.log`；失败的 Claude 原生退出文件另存 `claude-round2-cleanup-exit.json`，其中约 7.9 秒属于清理路径，不计入 Guard 耗时。脚本总断言失败属实，未用成功样本覆盖失败。本次只补验和更新记录，没有修改生产代码或重新运行全量测试。

收尾：57 个记录完整身份的测试应用进程均已退出，原 Codex PID 39116 保留；测试 Guard 关闭、普通登录任务移除，测试内核和网络服务停止。受保护事件任务保留安装、处于 Ready。临时驱动源码仍归档 `.tools/guard-subsecond/guard_latency.rs`；旧压缩包未更新。

## 最新优化：缩短未代理进程的存活时间

按用户要求优先快速关闭，Guard 的停止策略改为完整身份核验和持久停止授权后立即精确终止，不再发送窗口关闭消息或等待 1.5 秒正常退出。普通显式停止保留原策略。关闭前原有“单独查主进程→查候选→再次查主进程”合并为一次带代理参数核验的候选批量查询；主进程缺失、归属或代理参数不匹配仍拒绝关闭，所有候选原生句柄仍被持有并前后核验，辅助进程句柄仍在停止前捕获。

新增原生测试覆盖立即策略不发送 WM_CLOSE、拒绝伪造用户/会话/映像与复用 PID、保留无关进程、退出确认和重复停止。停止模块 4 项通过，Guard 13 项回归通过；默认全量 **438 通过、0 失败、58 ignored**，Release 构建、workspace/all-targets clippy、fmt、diff check 通过。证据 `.tools/guard-fast/`。

本轮临时驱动在应用创建后持有原生进程句柄，等待退出后通过 `GetProcessTimes` 读取内核记录的创建/退出 FILETIME，计量不包含重启或命令行核验。优化前 Release 实测：Codex PID 35368 为 **5864.1039 ms**，Claude PID 29588 为 **7399.6591 ms**。Codex 带代理重启通过；Claude 已确认退出，但之后测试上游健康失败，返回 `GUARD_STOPPED_PROXY_UNAVAILABLE`，不把该次记为重启成功。启动测试网络服务后的首次健康检查也失败，后续恢复才进行基线测试。证据 `baseline/*-exit-timing.json`、`baseline/results.json`、`baseline/run.log`。

首次新版监听组件 UAC 授权取消后停止了实测；用户再次要求“测一下”后重新授权安装成功，确认 `active` / `active_etw`，使用同一优化版 Release 完成本轮退出计时：

| 应用 | 优化前创建到退出 | 优化后创建到退出 | 本轮结果 |
|---|---:|---:|---|
| Codex | 5864.1039 ms | **2114.1952 ms** | 未代理 PID 50744 已精确关闭；后续代理健康检查失败，返回 `GUARD_STOPPED_PROXY_UNAVAILABLE`，没有重启或直连回退 |
| Claude | 7399.6591 ms | **3324.9240 ms** | 未代理 PID 10424 已精确关闭，带代理重启为 PID 29872；代理参数、数据目录、新进程存活和原 Codex 保留全部核验通过 |

证据 `.tools/guard-fast/live/*-exit-timing.json`、`results.json`、`run.log`。计时是原生主进程创建到退出，不包含新进程创建或测试断言时间。各应用前后各一次实测，且基线期间有默认回归运行，不作为严格同负载基准、稳定上限或 p95。`results.json` 的 `pass` 表示关闭及对应策略断言通过；Codex 的 `proxy_unavailable=true` 和失败回执明确说明本次没有重启成功。Claude 启动至完整纠正断言的 7.8 秒是另一指标。

收尾：本轮 23 个记录完整身份的测试应用进程均已退出；测试 sing-box PID 38188 与 Python 服务 PID 54064 均停止，原 Codex PID 39116 保持运行。测试 Guard 关闭，普通登录任务移除；新受保护事件任务保留安装、处于 Ready。临时 Cargo example 仍归档在 `.tools/guard-fast/guard_latency.rs`，未放回源码 targets。本次仅补测和更新记录，没有修改生产代码或重复全量测试；旧压缩包未更新。

## 最新修复：真实 Codex / Claude 的 Guard 自动纠正通过

本轮修复了前次真实应用补验暴露的两个生产问题，并重新编译 Release CLI/host：

- **Codex 扫描超时**：同一轮已核实身份的 Chromium 候选改为批量 WMI 查询，祖先进程行只在该轮复用。仍持有并前后核对原生进程句柄、创建时间、用户和会话；查询错误及超时不会当作进程不存在。相同 11 个候选的单轮归属扫描从 3430 ms 降至 265 ms；这是一次前后对比，不是启动总耗时或 p95。证据 `.tools/guard-fix/scan-before.log`、`scan-after.log`。
- **Claude 关闭后重启准备失败**：真实复现确认，主进程关闭后，正在退出的辅助进程可能在 `QueryFullProcessImageNameW` 返回 Win32 5。改为关闭主进程前核验并保留辅助进程句柄，关闭后只等待这些确切句柄的退出信号；仍拒绝尚未退出的辅助进程。证据 `claude-stop-before.log`、`claude-stop-after.log`。

使用已安装的 Codex 26.915.4065.0、Claude 2.2553.1.0 和新的空白隔离 store `30acab22-cd15-4cd7-989c-0d43cf61e943` 实测。通过真实 UAC 安装本轮受保护监听组件，确认 `active` / `active_etw` 后，以真实 EXE、独立用户目录和 `--no-proxy-server` 启动；生产 Guard 自动关闭并带正确代理参数重启。两款应用都通过：原目标完整身份匹配、关闭确认、新 PID 存活、实际命令行代理地址和数据目录正确、没有残留 `--no-proxy-server`、原用户 Codex PID 39116 保留。完整回执见 `.tools/guard-fix/live/results.json`，原始过程见同目录日志。

最终完整断言：Codex PID 53980 → 700，11.620 秒；Claude PID 52236 → 1864，10.503 秒。时间包含启动、Guard 处理及命令行核验，不是纯 ETW 延迟。收尾按完整身份确认本轮各次试验累计记录的 79 个应用进程均退出；原 Codex PID 39116 仍在运行。测试 Guard 均关闭，普通登录任务删除，测试 sing-box PID 49776 与 Python 网络服务 PID 37884 均退出。受保护事件任务保留安装、处于 Ready。清理证据 `live/cleanup.json`、`network-cleanup.json`、`login-remove.json`、`core-stop.json`；临时 Cargo example 已归档至 `.tools/guard-fix/real_guard_acceptance.rs` 并移出源码 targets。

默认全量测试 **437 通过、0 失败、58 ignored**；扩展已有原生测试覆盖批量与单个查询归属一致、伪造身份拒绝、保留句柄观察进程退出和父进程退出后的辅助进程。Release 构建、workspace/all-targets clippy `-D warnings`、fmt、diff check 通过。证据 `.tools/guard-fix/full-tests.log`、`test-counts.json`、`release-build.log`、`clippy.log`、`fmt.log`。普通启动仍不做外部进程占用扫描。

测试驱动也修正了新 store 尚无启动 journal、清理时进程已在退出、读取 journal 遇到短时 Windows 共享冲突等问题。共享冲突曾使驱动漏记已经成功重启的 Codex PID 30544，随后一次重复启动被现存测试会话接收，没有新纠正回执；已从持久 Guard 回执恢复确切身份并清理，补充启动前必须为 Absent 的断言，以及异常收尾时从 journal 收集本测试实例已确认进程的步骤。各次失败/成功日志均保留；这些驱动失败不冒充生产通过结果，最终以完整断言和清理后的回执为准。

范围：这是实际应用的自动纠正链路验证，使用空白数据和受控测试上游，未登录账号，也不代表真实账号业务流量、干净机器、包更新或全产品发行验收全部完成。旧压缩包未重新打包；最新程序位于 `rust/target/release/`。以下失败记录保留为修复前历史。

## 修复前补验：真实 Codex / Claude 的 Guard 自动纠正未通过

在已安装的 Codex 26.915.4065.0 和 Claude 2.2553.1.0 上分别创建空白隔离实例，复用前述测试 store 与已授权 Rust 监听组件。通过 `Invoke-CommandInDesktopPackage` 启动临时测试驱动，再用真实 EXE、独立用户数据目录和 `--no-proxy-server` 制造未代理启动；自动纠正完全交给当前 Release 生产 Guard。没有登录测试账号或使用原实例数据，也未修改生产代码。

| 应用 | 实测结果 | 证据 |
|---|---|---|
| Codex | 初始启用检查持续 `GUARD_SCAN_TIMEOUT` / `GUARD_SCAN_BUSY`，未确认保护就绪。随后另做真实未代理启动，期间不反复调用状态扫描，只观察持久回执：PID 49016 运行 75.109 秒后仍存活，没有该实例的新 Guard 纠正回执；最后状态仍为 `GUARD_SCAN_TIMEOUT` | `.tools/real-guard/results.json`、`codex-live-result.json`、`codex-live.log` |
| Claude | 初始检查曾短暂 `GUARD_RESOLUTION_BUSY`，随后达到 `active` / `active_etw`。未代理主进程 PID 24212 被 Guard 识别并确认关闭，但重启准备失败，终态为 `LAUNCH_PREPARATION_FAILED`，`binding=null`；没有带代理重启成功的回执 | `.tools/real-guard/claude-before.json`、`results.json`，请求 `aeb38df3-8d8c-409e-905f-58c278a3f1a2` |

结论：**受控 EXE 的成功不能推广为真实 Codex/Claude 自动纠正可用。** 当前真实应用链路存在两个阻塞：Codex 扫描超时，Claude 已关闭后重启准备失败。Claude 的通用错误码没有保留底层失败原因，本次没有据此臆断具体 Win32 错误或根因；需要继续定位、修复并重跑真实应用验收。Codex 第二轮实际失败发生在不反复查询状态的观察方式下，不能仅归因于测试状态轮询。

测试上游仅允许健康检查和受控目标，不用于账号/业务验收。首次健康检查失败，随后同一上游复查通过才开始应用测试。临时准备驱动最初未按 Claude 的包虚拟化选择 LocalState，触发 `PACKAGE_STORAGE_MODE_CHANGED`；已修正测试数据位置并成功准备后才执行上述应用测试，该准备错误不计产品缺陷。

收尾核验：22 个记录完整身份的测试应用主/子进程均已退出，原 Codex PID 39116 身份与存活核验通过；测试 Guard 均关闭，登录任务已移除，测试 sing-box 与 Python 上游已停止。受保护事件任务恢复 Ready、未运行。临时 Cargo example 已移出源码 targets，源码与全部回执留在 `.tools/real-guard/`；`git diff --check` 通过。没有修改或打包生产二进制。

## 后续实测：管理员 ETW 与 Guard 自动纠正

2026-09-20 使用当前 Release CLI/host，在独立测试 store 中完成生产 Guard 的 UAC 授权安装、受保护事件任务启动、普通 coordinator 接收事件、自动纠正和关闭。目标是受控 EXE 夹具，使用 Codex 参数模板及真实 sing-box 1.14.1；没有操作用户正在使用的 Codex 或旧代理内核。这项通过不等于真实 Codex/Claude 的全部业务验收通过。

| 项目 | 结果与证据 |
|---|---|
| 管理员 ETW 原生事件、会话冲突/归属及外部停止、持久 epoch 恢复 | **3/3 通过**；`.tools/acceptance-extended/elevated-results.json` 与 `*-elevated.log`，补齐此前受权限阻塞的三项 |
| UAC 安装与生产监听 | 受保护 helper/事件任务安装成功，状态到达 `active` / `active_etw`，登录入口核验通过；`.tools/guard-e2e/guard-before.json` |
| 自动关闭并以代理重启 | 手动启动未带代理的隔离夹具 PID 42432，Guard 核对后关闭它并启动 PID 52940；实际代理参数和数据目录正确，约 5.56 秒取得完整流量回执；`.tools/guard-e2e/correction-result.json` |
| 真实流量经过代理 | 新夹具 → sing-box `127.0.0.1:61893` → 回环 HTTP 上游 → 独立目标服务，收到 `guard-e2e-origin-ok`；回执及 `upstream.ndjson` 同时留证 |
| 不误关同映像未登记进程 | 两种场景都保留未登记原版夹具 PID 53168，以完整进程身份核验；`run.log`、`failure-result.json` |
| 上游故障不回退直连 | 故意拒绝测试上游连接；Guard 确认关闭目标，返回 `GUARD_STOPPED_PROXY_UNAVAILABLE`，未生成替代进程；`.tools/guard-e2e/failure-result.json` |
| 关闭及移除普通登录入口 | `guard disable`、`guard login remove` 和 `core stop` 均成功；同目录 `disable.json`、`login-remove.json`、`core-stop.json` |

首次授权后的短时状态仍显示 `GUARD_LISTENER_MISSING`，监听监督下一轮重试后到达 Active；未将安装命令的瞬间返回当作已就绪。第一次流量断言失败源于测试上游只允许外部 HTTPS CONNECT，而 sing-box 对本地 HTTP 目标也使用 CONNECT；补全精确目标白名单后通过。故障脚本最初期待底层健康错误码，实际产品返回 Guard 专用错误码；修正测试预期后再次执行通过。这些仅调整临时测试驱动，没有修改生产逻辑。

故障重试场景约 30 秒才完成，包含扫描退避和最终无替代进程观察；上述时间均为单次实测，不是 ETW 事件延迟或性能 p95。生产 ETW 已连接与原生事件测试通过是独立证据，不将自动纠正总耗时冒充纯 ETW 延迟。

旧脚本的 `AppProxy-Events-*`、`AppProxy-Guard-*` 任务原本均为 Ready，无旧监听/Guard 进程。本轮保留旧 sing-box PID 27312 和原 Codex PID 39116；不必为隔离验证退出它们。

收尾：8 个有完整身份记录的测试夹具/内核进程均已退出，Python 测试网络服务也已停止；普通登录任务已删除，临时 Cargo example 已移回 `.tools/guard-e2e/guard_acceptance.rs`，`git diff --check` 通过。管理员事件任务已停止并处于 Ready。删除该任务的最后一次 UAC 返回“操作已被用户取消”，没有重试或声称已删除：固定测试任务 `AppProxyRust-Event-cfcc7e999dc3c172-587f20f4-590c-49e5-b747-0a168653ef3a` 和对应 Program Files 受保护测试安装目录保留，未运行。证据 `cleanup-processes.json`、`cleanup-tasks.json`、`original-processes-preserved.json`、`original-tasks-preserved.json`。测试数据和日志保留供复核。

**后续调整：按用户决定，已移除普通启动前判断外部进程是否运行的两处扫描。** 启动实例选择列表也只读配置，管理列表/详情才读取运行状态。应用自行处理单实例转交和数据目录占用；本工具请求去重、可信会话复用、代理准备、启动结果核对及 Guard 纠正检查保留。没有延长扫描超时，也没有合入中途尝试的批量 WMI 优化。管理详情的只读查询和 Guard 扫描仍独立存在。

调整后的全量默认回归 437 通过、0 失败、58 ignored（`.tools/no-startup-scan-tests.log`），涵盖外部同映像进程存在时普通启动成功且不认领旧进程、自有会话复用、Guard 纠正边界。随后菜单列表调整经最终菜单默认回归、Release PTY 配置列表显示/返回验证；最终 Release、fmt、clippy 和 diff 检查通过。全量未把 ignored 项计为通过。

调整后的 Release 实机验证：Claude A/B、Codex A/B 四个直连隔离实例全部确认启动，详情读取和重复请求复用均通过。Codex A 约 3.410 秒、B 约 4.306 秒；B 的重复启动复用约 0.719 秒。这是单次实测，不是冷/热性能矩阵。证据 `.tools/no-startup-scan/apps.json`、`.tools/no-startup-scan-real.log`。本轮 35 个已核对归属的测试进程（含 4 个主进程）已退出，原有 Codex 和 sing-box 完整身份与存活核验通过；证据 `.tools/no-startup-scan/cleanup.log`。新的 CLI/host 位于 `rust/target/release`，下文旧验证 zip 未重新打包。

以下保留首次验证结果。**全产品验收仍未完成，旧压缩包不应当作正式覆盖包。** 首次验证时真实 Codex 第二个隔离实例连续三次启动前检查超时，该普通启动路径已按上述调整取消；管理员 ETW、真实业务及干净系统等剩余条件不因此视为通过。

本轮以 `D:\app_proxy` 的 `codex/entry-maintenance` 工作区为准，包含此前未提交修改。IFEO、跨目录升级、外部删除登录任务后的修复，按用户决定不在范围内。历史记录里的通过结论不替代本轮实测。

## 环境和执行范围

- Windows 11 专业版，10.0.26200，普通用户令牌。
- Rust 1.98.1；Cargo.lock 固定依赖；Windows x64。
- Claude 2.2553.1.0，`Claude_pzs8sxrjxfjjc!Claude`。
- Codex 26.915.4065.0，`OpenAI.Codex_2p2nqsd0c76g0!App`。
- 真实内核使用固定 sing-box 1.14.1；网络专项使用回环合成上游和受控 TLS 服务；另执行官方安装包真实下载。
- 实机应用使用新建测试 store：`%LOCALAPPDATA%\AppProxyRust-Acceptance-db571b05-bdc0-4e48-8d72-dd95bbb97372`，创建空白隔离实例，不导入用户账号。

## 构建、自动测试和交互验证

| 项目 | 本轮结果 | 本地证据 |
|---|---|---|
| Release workspace 构建 | 通过，约 1 分 08 秒 | `.tools/acceptance-release-build.log` |
| Release 默认全量测试，串行 | **437 通过、0 失败、58 ignored** | `.tools/acceptance-release-tests.log` |
| 29 项明确选择的扩展测试 | **26 通过、3 因权限受阻** | `.tools/acceptance-extended/results.json`、同目录逐项日志 |
| 交互 PTY 契约 | **7 项通过；1 项取消测试未完成** | 下表及 `.tools/acceptance/interactive-results.json` |
| 最终菜单默认回归 | 1 通过、5 ignored；交互项另行执行 | `.tools/acceptance-menu-final.log` |
| fmt / clippy（all-targets，warnings 为错误） | 通过 | `.tools/acceptance-fmt.log`、`.tools/acceptance-clippy.log` |
| `git diff --check` | 通过 | 本轮命令输出 |

扩展项通过范围包括：真实共享内核扩容/重配置/删除/回滚/Starting 核对、两条认证上游隔离、CONNECT 和 TLS、订阅六协议配置检查与实际 HTTP 方法、订阅预览和 CLI、官方 sing-box 下载/安装/取消清理、Claude 包内 helper 回执、原生通知和前台控制台、桌面链接/图标、普通登录任务注册及 journal 恢复。这些证据不等于目标应用全部业务流量通过代理。

3 个 ETW 测试均在 `StartProcessTrace` 返回 Win32 5。曾发起只执行这三项测试的 UAC 提权，Windows 返回“操作已被用户取消”，没有管理员测试结果。未把普通权限失败算作产品实现失败，也未把已准备脚本算作通过。

| 交互测试 | 结果 |
|---|---|
| 参数和环境变量隐藏输入、保存但不启动 | 通过 |
| 添加原版、保存返回、改名、不支持分身时拒绝 | 通过 |
| 手动认证代理已保存，缺内核时返回、不创建实例 | 通过 |
| 菜单旧确认遇到外部改名，拒绝覆盖新配置 | 通过 |
| 菜单创建并移除桌面快捷方式 | 通过 |
| 订阅 URL 隐藏输入，节点选择 EOF 保留配置 | 通过 |
| 订阅刷新缺内核时返回，不下载、不修改配置 | 通过 |
| 隐藏密码处 Ctrl+C，验证退出码和未保存状态 | **未完成**：子菜单显示取消，但 PTY 连同父测试中断，没有完整断言结果 |

交互过程中发现测试夹具持有最初 coordinator PID；停留超过 30 秒后 coordinator 可能退出并重新引导，导致清理旧 PID 后 `Store::open` 误报 `STORE_ALREADY_OWNED`。本轮仅修改 `menu_contract.rs` 的四处清理，在打开 store 前重新核对并关闭当前夹具 owner。原版用例重新运行约 50.76 秒、缺内核用例约 85.76 秒，均通过。Release 全量测试先于这项仅影响测试夹具的修正；最终菜单回归和交互重跑覆盖该修正，生产二进制未因它变化。

过期确认用例第一次人工驱动使用了错误 CLI 参数 `--name`，未成功制造并发修改，随后断言失败；使用正确位置参数、确认外部修改成功后重跑通过。这是测试驱动错误，不计产品缺陷。

## 实机应用、快捷方式和打包

| 场景 | 本轮结果 |
|---|---|
| Claude A / B 两个空白直连实例 | 两者启动确认、重复启动复用成功；独立目录均有约 316 个文件写入 |
| Codex A 空白直连实例 | 启动确认、重复启动复用成功；独立目录有实际写入；原有 Codex 同时存活 |
| Codex B 空白直连实例 | **失败**：三次请求均为 `INSTANCE_CHECK_TIMEOUT`，无已确认目标进程 |
| Claude A 桌面链接 Shell 激活 | 成功；请求 `9549822f-2e55-4790-a715-738556aad38c` 关联原确认回执 `af05095f-3759-4f9d-83a7-a8fcd433fc63`，复用原进程；链接随后移除 |
| 中文及空格目录解压运行 | 成功；CLI/host SHA-256 一致；首次 store 初始化及普通权限身份检查通过 |
| Windows 进程探针 | args/cwd、env set/unset、父环境不变、创建线程退出后存活、伪身份拒绝、精确停止六项通过 |

应用与路径证据：`.tools/acceptance/apps.json`、`codex-b-retry.json`、`data-directories.json`、`shell-activation.json`、`package.json`。目录写入和进程身份是本轮证据，不代表账号、完整隐私隔离或业务功能验收。

验证候选包：`release/rust-validation-20260920-144240.zip`，6,706,061 字节。包含 CLI、host、文件哈希 manifest 和验证状态说明，不含 sing-box 或用户数据。`dumpbin /DEPENDENTS` 确认依赖 **VCRUNTIME140.dll** 和 Windows UCRT/系统库；未在没有开发环境的干净机器验证，不能宣称完全免依赖便携发布。

### 新发现的发布阻塞缺陷

Codex 原版与第一个隔离实例运行时，第二实例连续三次失败，约 7.3 秒后返回 `INSTANCE_CHECK_TIMEOUT`；`instance inspect` 同时可返回 `INSTANCE_OBSERVATION_TIMEOUT`。

只读诊断使用生产的安装解析、候选发现和逐进程归属查询 API：安装解析约 621 ms，发现 21 个候选约 43 ms，含逐个 WMI/祖先查询的扫描合计 6,475 ms。源码启动前扫描总预算 5 秒，状态观察预算 3 秒。证据指向多进程下串行查询及重复祖先查询超出总预算。诊断使用原版目标分类，未记录失败生产请求内部逐段耗时，因此这些数字是独立诊断测量，不冒充生产 trace。

相关代码：`crates/app-proxy-app/src/launch_engine.rs` 的 `check_occupancy`，`launch_engine/observation.rs` 的 `observe_instance`，`crates/app-proxy-windows/src/process_query.rs` 与 `instance_process.rs`。诊断源码和输出保存在 `.tools/acceptance/scan-diagnostic.rs`、`scan-diagnostic.log`；临时 Cargo example 已移除。

**首次验证时未修改生产扫描实现。** 后续用户明确取消普通启动前的“是否已运行”判断，现已移除该启动路径的两次扫描并完成上述实机复测。独立只读状态查询和 Guard 的扫描实现未优化；不能把它们的超时当作“无进程”。

## 尚未满足的验收条件

| 项目 | 当前状态及原因 |
|---|---|
| 管理员 ETW、生产 Guard 授权→事件→误启动纠正 | 后续已完成：原生三项与生产链路受控 EXE 自动纠正/故障场景通过，见本文顶部；真实目标应用业务仍单独验收 |
| 真实注销再登录触发 | 未执行；任务注册成功不等于登录触发成功 |
| Codex / Claude 登录、对话、Code / Cowork、真实代理线路 | 无本轮测试账号及线路输入，未验收 |
| 实际应用两代理出口隔离和运行时断网通知 | 内核受控上游专项通过；目标应用业务场景未验收 |
| MSIX 实际版本更新 | 未执行；没有通过升级/降级已安装应用制造该场景 |
| 干净机器首次安装和系统兼容矩阵 | 当前仅一台已有开发环境的 Windows 11；未执行 |
| 冷/热各 30 次及 core 有/无组合、10 分钟 Guard 空闲资源 | 未完成；多实例启动和管理员 Guard 后续已补验，仍不能从少量启动计算验收 p95 |
| 整体卸载、脱敏诊断包导出 | 当前 CLI 尚无对应入口，属于功能缺口，不是仅缺测试 |
| 持续运行故障的产品通知 | 核心状态记录有测试；持续故障通知产品闭环尚未完成 |

## 清理与保留

本轮创建且已核对归属的应用进程共 28 个（含 3 个主进程），均已退出；关闭前后核对原有 Codex PID 39116、原代理 PID 27312 身份与存活，未结束它们。测试 coordinator 随后自行退出。证据 `.tools/acceptance/cleanup.log`；辅助诊断源码仅保存在 `.tools`，没有遗留到生产 targets。

测试创建的桌面链接已移除，任务专项自身完成注册/移除和冲突保留校验。系统已有的 `AppProxy-Events-*` / `AppProxy-Guard-*` 任务保持原样。没有再次发起 UAC。

验证压缩包、日志和独立测试数据保留，应用目录统计约 759 MB；未删除用户原应用数据，未执行注销、重启或包更新。没有提交 Git 或覆盖旧下载版程序。
