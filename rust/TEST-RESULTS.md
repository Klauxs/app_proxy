**2026-09-21：确认 AppData 映射，统一用户目录布局**

- 通过 `CreateFileW` 打开三个旧目录，再用 `GetFinalPathNameByHandleW` 和 `GetFileInformationByHandle` 查询实际目标。`%LOCALAPPDATA%\AppProxy`、`AppProxyResources`、`AppProxyRustResources` 均指向 `%LOCALAPPDATA%\Packages\OpenAI.Codex_2p2nqsd0c76g0\LocalCache\Local` 下的对应目录；别名和实际路径的卷号、文件 ID 分别相同。Explorer 在真实 Local 目录中看不到这些条目并非隐藏属性或刷新问题。工具进程的 `GetCurrentPackageFullName` 返回 15700（没有包身份），但文件句柄已经明确证明其文件访问发生重定向，不能只靠包身份 API 排除映射。
- 正式布局统一为 `%USERPROFILE%\AppProxy\{app,data,resources,guard}`。使用 Windows Profile Known Folder；MSIX 分身和辅助通信使用正式 data，旧 AppData 目录不导入。自定义开发 store 的包隔离行为保留。
- 实际安装的 Claude 2.2553.1.0 与 Codex 26.915.4065.0 均完成新用户目录下的包内辅助进程读写往返：包身份核验通过，包内写入可在包外读取，临时目录自动清理。未启动 Claude/Codex 应用主进程；这证明共享文件可用，不等于分身完整登录验证。
- 新增正式布局分身/控制目录回归，5 项实例数据测试通过；5 项配置并发测试通过；6 项安装事务测试通过；Clippy 全工作区全目标警告为错误通过。Windows 库首轮 194 项通过、24 项忽略，1 项原生进程/WMI 对照遭遇 PROCESS_QUERY_BUSY，单独重跑通过。本次发现的配置预检临时锁持续占用已经修复，慢查询与并发提交回归通过。
- 应用层完整回归 148 项通过、11 项忽略。Release 实际 EXE 在用户目录下临时安装、协调进程退出、升级重启及配置字节保留测试通过（11.06 秒），临时安装目录和测试进程已清理。没有部署正式 app/data 或启动真实应用。
- 新 guard 路径保留监听子目录权限；本轮没有重新安装管理员监听任务，不能视为新路径 ETW 已完成现场验证。
**2026-09-21：单文件 Setup 与同目录升级，普通权限链路通过**

新增 `AppProxy-Setup.exe`，内嵌同版前后台与 SHA-256，固定安装目录、开始菜单入口、原生进度窗口、按需监听 UAC、持久升级记录及成套文件恢复。包采用 asInvoker manifest 和静态 VC 运行库构建；既有开发版没有被替换或迁移。

- 安装事务 6 项通过：首次安装、同包重试保留文件身份、不同包拒绝接管未完成升级、两个 EXE 各替换边界与登记边界回滚、未发布 journal 恢复、部分写入恢复、陌生文件/被改动文件保留、核验期间排除第二安装器。
- 维护租约及半套程序启动阻止 2 项通过；开始菜单链接的原生临时目录创建、等价复用及冲突保留 1 项通过。
- Guard 平台相关回归 44 项通过、4 项需独立环境的测试未执行；协调进程回归 30 项通过。
- 新编译实际 EXE 在临时安装目录运行：启动协调进程 → 安装器触发退出 → 替换两份不同哈希的有效 EXE → 新协调进程恢复；配置 manifest 字节保持不变，测试协调进程全部退出。该测试没有启动真实 Claude，也没有安装管理员组件。
- `check --workspace --all-targets`、Clippy（警告视为错误）、fmt/diff 检查通过。最终静态运行库 Release 双 EXE 再次完成上述临时目录升级测试（4.25 秒）；PE 导入表确认前台、后台和 Setup 均无 VCRUNTIME/MSVCP DLL 依赖，Setup 嵌入 `asInvoker`。管理员监听 generation 换代已提供显式 ignored 实测，尚未把普通权限测试称为 UAC 升级验证。

本机 `%LOCALAPPDATA%\AppProxy` 仍包含旧脚本版数据，直接运行正式 Setup 的 `--yes --no-launch` 预检已验证：退出码 1、旧 manifest SHA-256 保持不变、程序安装目录未创建，日志明确指出数据目录冲突。没有接管旧数据，`AppProxyRust` 开发版配置和监听保持原样。详见 [安装与升级](docs/16-installation-and-upgrade.md)。

**2026-09-21：桌面快捷方式路径修复与输出精简，正式部署完成**

后续原版命名已改为应用名（`Claude.lnk`），现有入口及登记已更新；服务层 4 项回归、Clippy、Release 前后台构建通过。用户重新授权后，命名规则对应的正式监听副本已安装且与 Release 哈希一致，独立核验保护 active、ETW active、登录启动 ready。未重跑应用关闭测速。

复现并修复 Shell Link 不接受扩展路径导致的 `0x80070057`，保留原创建记录供恢复。普通输出隐藏请求 UUID，按结果、位置、原因展示；原生快捷方式 33 项、CLI 合约 3 项、显式真实桌面 fixture 创建/核验/移除通过，Clippy/fmt/Release 前后台构建通过。用户重新授权后已更新正式前后台及监听，恢复原请求创建 Claude 桌面快捷方式并完成原生核验；确认 ETW active、登录启动 ready，监听副本与 Release 后台哈希一致。未点击快捷方式重新启动 Claude。详见 [原因、修改与部署状态](docs/15-shortcut-path-and-output-2026-09-21.md)。

**2026-09-21：先关闭、后后台登记与重启准备，三轮 30～48 ms**

历史恢复、任务记录、重启资源准备和停止记录移至精确关闭后；删除重复请求查询、重复目标判断及重启前多余的一轮占用扫描。补扫描复用同一关闭路径，已有重启不再阻塞新未代理主进程，重启请求合并。首次实测暴露包激活失败，补上同一一次性 ticket 的有界重试和诊断。完整回归 474 项通过；后续补充修改的 Guard 50 项、包启动 9 项及最终 Clippy/fmt/Release 通过。最终正式 Claude 三轮关闭 46.260 / 48.400 / 30.172 ms，均成功带代理重启并验证实际 TCP 连接；额外未代理主进程在 45～52 ms 内关闭并合并重启。详情和崩溃恢复取舍见 [拆解、实现与完整实测记录](docs/14-guard-stop-first-2026-09-21.md)。

**2026-09-21：原版辅助进程导致兜底阻塞修复，实际代理连接验证通过**

修复 Claude 辅助进程显式传入默认数据目录时阻塞已确认主进程纠正的问题，未知角色与主进程身份检查保持不变。Guard 定向回归 46 项、Clippy/fmt/Release 通过；正式更新后自动纠正现场未带代理的原版。再从 Windows 应用入口启动，453.175 ms 确认关闭，2597.157 ms 创建带代理的新主进程；实际观察到网络子进程到代理端口的 10 条连接，逐条匹配 sing-box 接收端。详见 [现场、修复与验证范围](docs/13-guard-original-recovery-2026-09-21.md)。

**2026-09-21：正式订阅实例关闭延迟修复，管理员部署及三轮实测通过**

正式 Claude 原版、85 个订阅节点/4 个候选复现创建到确认退出 4584.136 ms，其中事件接收 13.956 ms、纠正登记 2399.845 ms。发现配置读取反复完整校验全部凭据；改为完整配置摘要绑定、保留不可改写凭据句柄的验证缓存。非重复回归结果 470 passed、62 ignored（首轮一项 WMI 并发竞争，Windows 包串行复跑通过），Release/Clippy/fmt 通过。9 月 21 日重试管理员授权完成新版登记，管理员副本与 Release 哈希一致；同一正式原版实例三轮 136.206 / 138.763 / 140.544 ms，均带代理重启成功，平均较修复前减少约 97%。测试进程已清理，临时计时关闭，正式 ETW 监听与共享守护登录自启动均已恢复。详见 [原因、分段和验证](docs/12-guard-subscription-validation-2026-09-20.md)。

**2026-09-20 当前参数：ETW 刷新间隔改为 20 ms**

按用户要求由 25 ms 调整为 20 ms，事件入队立即唤醒、固定时钟及 Skip 策略保持。监听相关 5 项回归及 Release 构建通过，fmt/diff check 通过，证据 .tools/guard-20ms/。本轮未更新管理员安装的监听副本，也未重跑真实应用测速或全量回归；下面的 50～117 ms 六次结果仍属于 25 ms 版本。

**2026-09-20 最新优化：事件立即唤醒与合并准备记录，六次关闭均低于 120 ms**

ETW 回调入队立即唤醒发送，固定 25 ms 刷新；事件任务复用已固定的程序/目录句柄，将两个进度状态写入合并到持久停止意图。取消、配置、精确身份、资源预留和恢复规则保持。默认回归合计 454 项通过、60 ignored，Release/Clippy/fmt/diff check 通过。真实 Codex 三次 49.799 / 95.200 / 104.073 ms（平均 83.0），Claude 84.694 / 114.441 / 117.405 ms（平均 105.5），六次均带代理重启，关闭前 WMI 全部为 0。修复测试清理工具的 PID 复用问题后确认 48 个测试应用进程退出；核心、辅助及协调服务停止，保护禁用、登录入口移除，原 Codex 保留。详见 [分段、开销与验证](docs/11-guard-wakeup-2026-09-20.md)。

**2026-09-20 最新实测：原生逐事件路径六次均在 300 毫秒内关闭**

管理员授权完成，生产 Release 对已安装 Codex/Claude 空白隔离实例各测试三次，均精确关闭并带代理重启。Codex 296.692 / 238.965 / 269.275 ms，Claude 212.738 / 249.387 / 273.367 ms；目标关闭前 WMI 全部 0 次，原生命令行读取 0.276～0.413 ms。上一轮默认回归合计 451 项通过、60 ignored，Release/Clippy/fmt/diff check 通过；本轮仅继续真实实测及文档收尾。50 个测试应用进程及核心/辅助/协调进程已退出，保护禁用、测试登录入口移除，原 Codex 保留，事件任务 Ready。六个样本不构成任意负载的延迟上限。详见 [实现、分段与验证](docs/10-guard-native-events-2026-09-20.md)。

**2026-09-20 最新分段计时：WMI 是主要耗时**

默认关闭的异步计时已加入。37 项 Guard 回归、6 项进程查询测试通过，Release/Clippy/fmt/diff check 通过；真实管理员安装 + active ETW 六次自动纠正均通过。Codex/Claude 创建到退出平均 730.1/1147.2 ms，WMI 分别 471.6/699.4 ms；Claude 三次均出现一次完整重采样。日志无丢弃，测试进程服务已清理，原 Codex 保留。详细口径与原始证据见 [分段实测](docs/08-guard-timing-2026-09-20.md)。未将先前 446 项全量回归标为本轮重新执行。

**2026-09-20 前次实测：6 次自动纠正通过，未稳定低于 1 秒**

重新发起 UAC 后安装及核验成功，生产 Release + active ETW 实测 Codex 创建到退出 **879.8283 / 772.9133 / 881.0244 ms**，Claude **1058.7017 / 1255.2256 / 981.7508 ms**。六次均完成确切目标关闭及带代理重启，脚本退出码 0，没有复现上轮退出竞态错误或残留阻塞；Claude 两次超过 1 秒，速度目标仍未完整通过。77 个测试应用进程、测试核心、辅助服务和协调进程已清理，两项保护已禁用，原 Codex 保留；监听计划任务保留为 Ready。证据 `.tools/guard-race/live/`，详见 [验收记录](ACCEPTANCE-2026-09-20.md)。本次仅补真实验证，代码回归仍为下述已完成的 446 项通过。

**2026-09-20 前次修复：退出竞态回归通过，首次真实补测因授权取消未执行**

关闭前遇到已知候选进程消失错误时，最多进行 3 次完整重新采样，总预算 5 秒；确切目标自行退出时不关闭、不重启。新的 Absent 扫描解除尚未授权停止的临时退出错误，保留历史记录和已发出停止意图的失败诊断。新增 4 项测试并扩充状态恢复断言。默认全量 **446 通过、0 失败、59 ignored**，Release/Clippy/fmt/diff check 通过，证据 `.tools/guard-race/`。Windows 管理员授权被取消，真实应用补测未启动，测试保护及协调进程已关闭。不能宣称已达到稳定 1 秒内，详见 [验收记录](ACCEPTANCE-2026-09-20.md)。

**2026-09-20 前次实测：未通过稳定低于 1 秒及整轮纠正验收**

原生查询当前包登记及清单摘要，只复用未变化的解析元数据；自有 ETW 每 100ms 校验归属并主动 FLUSH，Ready 事件合并等待缩短至 50ms。默认全量 **442 通过、0 失败、59 ignored**，额外 1 项已安装包原生/桥接一致性测试通过，Release/clippy/fmt/diff check 通过。随后真实 UAC + ETW 补验：Codex 创建到退出 784.2108 / 1135.3619 / 749.4272 ms，三次重启通过但一次超过 1 秒；Claude 首次 743.8501 ms 且重启通过，第二次关闭前因 `PROCESS_EXITED_DURING_INSPECTION` 失败，第三次因历史失败状态未就绪而没有启动。不能声明整轮通过；57 个测试应用进程已清理，原 Codex 保留。证据 `.tools/guard-subsecond/live/`，详见 [验收记录顶部](ACCEPTANCE-2026-09-20.md)。

**2026-09-20 前次优化：Guard 快速关闭**

Guard 核验目标后立即精确终止，跳过 WM_CLOSE/1.5 秒正常退出等待；关闭前主进程与辅助进程改为一次完整批量查询。新增立即停止身份/隔离测试，停止模块 4 项与 Guard 13 项通过。全量 **438 通过、0 失败、58 ignored**，Release/clippy/fmt/diff check 通过，证据 `.tools/guard-fast/`。首次 UAC 取消后，用户再次要求测速并完成授权；优化版原生创建到退出实测 Codex **2114.1952 ms**、Claude **3324.9240 ms**。Claude 带代理重启通过，Codex 关闭后因代理健康失败保持关闭，本次没有重启成功。测试保护和服务已关闭，原 Codex 保留；本次补测未修改生产代码或重复全量测试。详见 [验收记录顶部](ACCEPTANCE-2026-09-20.md)。

**2026-09-20 前次修复：真实 Codex / Claude 自动纠正通过**

修复候选进程逐个重复 WMI/祖先查询导致的 Codex 扫描超时，以及 Claude 主进程关闭后查询正在退出的辅助进程导致的 Win32 5。采用单轮批量查询和关闭前保留辅助进程句柄，身份与退出核验仍严格保留。相同 11 个候选的扫描实测 3430 ms → 265 ms。

全量默认测试 **437 通过、0 失败、58 ignored**，Release 构建、clippy `--workspace --all-targets -- -D warnings`、fmt、diff check 通过。真实 Codex 26.915.4065.0 和 Claude 2.2553.1.0 均由生产 Guard 自动关闭未代理的空白隔离实例，再以正确代理参数重启；原用户 Codex 保留。证据 `.tools/guard-fix/` 和 `live/results.json`，完整范围、测试驱动修正及收尾见 [验收记录顶部](ACCEPTANCE-2026-09-20.md)。未据此声明账号业务、全产品发行验收完成，旧压缩包没有重新打包。

**2026-09-20 修复前：真实应用 Guard 自动纠正未通过**

真实 Codex/Claude 各一个空白隔离实例补验：Codex 启用检查持续扫描超时，另行不反复查询状态的实际未代理启动在 75 秒内没有纠正回执，目标仍存活；Claude 被 Guard 确认关闭，但重启准备返回 `LAUNCH_PREPARATION_FAILED`。这是当前 Release 的真实应用阻塞，不能用下述受控 EXE 的通过结论替代。证据 `.tools/real-guard/results.json`、`codex-live-result.json`，细节见 [验收记录顶部](ACCEPTANCE-2026-09-20.md)。22 个测试应用进程已清理，原 Codex 保留；测试 Guard/内核已停用。本轮仅补验和记录，未修改生产逻辑，也未重复全量默认测试。

**2026-09-20 后续：管理员 ETW 与生产 Guard 受控 EXE 实测通过**

管理员 ETW 3 项原生测试全部通过，补齐此前 Win32 5/UAC 取消导致的缺口；证据 `.tools/acceptance-extended/elevated-results.json`。当前 Release 在独立 store 中完成真实 UAC 受保护 helper 安装、事件任务启动及 `active_etw` 连接。受控隔离 EXE 未带代理启动后，被 Guard 精确关闭并以正确代理参数重启；实际请求经 sing-box 和受控上游到达独立目标。同映像未登记进程保持存活。故意断开上游后，Guard 关闭目标并报 `GUARD_STOPPED_PROXY_UNAVAILABLE`，没有直连回退。关闭 Guard、移除普通登录入口、停止测试内核通过。

证据 `.tools/guard-e2e/correction-result.json`、`failure-result.json` 及同目录状态/清理记录，范围与测试驱动修正详见 [验收记录](ACCEPTANCE-2026-09-20.md)。本轮没有修改生产代码或重复默认全量回归；受控 EXE 的生产 Guard 验证不代表真实 Codex/Claude 全业务和真实登录触发已完成。以下记录按时间保留。

**2026-09-20 后续：取消普通启动前外部进程占用扫描**

全量串行默认回归 **437 通过、0 失败、58 ignored**（`.tools/no-startup-scan-tests.log`）。随后启动菜单列表改为只读配置，最终菜单默认回归 1 通过、5 ignored，并用 Release 交互实测四个实例选择列表只显示配置、可返回退出；最终 Release 构建、fmt、clippy `-D warnings` 和 diff 检查通过。启动菜单不再因展示运行状态触发进程扫描。

按用户决定，普通交互/快捷方式启动不再预扫描外部进程是否已运行，由应用自己处理单实例和目录占用。本工具自有请求、可信会话及资源预留仍防重；Guard 纠正仍执行原有占用/身份核验。没有合入批量 WMI 优化。回归用例改为验证外部同映像进程不阻止启动、不会被接管、同一实例后续请求复用自己的可信会话。Release 实测 Claude/Codex 各两个实例全部启动及复用成功，此前失败的 Codex B 约 4.306 秒确认。日志 `.tools/no-startup-scan-real.log`，详情见 [验收记录更新](ACCEPTANCE-2026-09-20.md)。以下保留首次验证结果。

**2026-09-20 完整验证补充：未通过全产品验收（首次结果，后续调整见上）**

Release 全量默认测试 437 通过、0 失败、58 ignored；另 26 项扩展和 7 项交互通过。真实 Codex 第二个隔离实例三次启动前检查超时，管理员 ETW 因 UAC 取消未验证；登录业务、干净机器、实际包更新及性能矩阵仍未完成。诊断发现 21 个候选串行查询约 6.5 秒，超过启动检查 5 秒预算；生产缺陷尚未修复。当前压缩包仅为验证候选，不能视为正式覆盖包。详见 [本轮验收记录](ACCEPTANCE-2026-09-20.md)，以下为历史实施记录。

**2026-09-20：共享代理内核收尾（当前批次）**

工作区 `D:\app_proxy`，分支 `codex/entry-maintenance`。实现运行中移除未被实例或下载网络引用的代理：CLI/菜单先给出具体影响，JSON 默认返回计划及退出码 5；明确确认后切换共享内核，保留其他入口，失败恢复原集合，最后入口停止内核后才提交删除配置。恢复支持配置提交两侧的中断，具体计划及 revision 变化会拒绝旧确认。Down 保留恢复集合，需先显式 core stop 后移除。

实现 `core recover-start`，创建前持久化 generation/创建者/映像/随机 Job 归属及原 Start 请求，原生创建时原子关联 Job，并仅继承只读 Job 租约。核对确认唯一原进程或确认无存活成员后，更新状态和原请求/同代未完成恢复回执；不自动启动或宣称代理健康。相同归属证据用于切换/回滚中断。旧版缺证据、原创建者未退出、错映像或多成员保留未知。IPC 3.20 对三种 core RPC 双向设版本门槛。

默认新增 7 项测试：4 项原生创建归属（创建者真实退出、句柄关闭、无创建/已退出、证据冲突和多成员保留、回执代际隔离）、2 项删除计划与提交中断恢复、1 项协议双向拒绝。最终完整串行 `cargo test --workspace --locked --no-fail-fast -- --test-threads=1`：**437 通过、0 失败、58 顶层 ignored**（`.tools/core-finish-verified-tests.log`）。ignored 包括额外平台/桌面验收和子进程夹具，不能视为全产品验收通过。

使用下载目录固定 sing-box 1.14.1 的副本，在独立临时 store、回环合成上游上另行执行 **5 项真实内核/CLI 测试，全部通过**。最终直接运行本轮编译的测试二进制：`app_proxy_app-d8692443b6257ab1.exe core_reconfigure::tests --ignored --test-threads=1`（4 项，`.tools/core-finish-real-final.log`）和 `instance_cli_contract-32f6cd528c547fe9.exe proxy_cli_recovers_start --ignored --test-threads=1`（1 项，`.tools/core-finish-cli-final.log`）。覆盖保留其他入口/删除最后入口、失败回滚、正常启动和切换阶段漏写 PID 的恢复、真实创建者进程退出后的 CLI 核对、预览与过期确认拒绝、owner 重开后历史回执，以及既有扩容、上游编辑与订阅切换回归。不是实际应用登录和公网代理隔离验收。

故障注入初版发现只关闭父进程 Job 句柄无法保留可重开的名称，隐藏控制台还会带入 conhost；已通过显式租约、KILL_ON_JOB_CLOSE 和 DETACHED_PROCESS 修正并重跑。原生核验使用文件身份，避免 Windows 长路径前缀差异导致误报。测试失败留下的 4 个临时 store 内核按明确路径与 PID 核验后清理，用户已有发行目录内核未终止。旧版本订阅门槛错误优先级已修复。一次重新链接被尚未退出的前一轮测试 EXE 占用而失败，等待其结束后完成上述最终全量回归；未清理/重置构建产物。

最终 workspace/all-targets clippy `-D warnings`（`.tools/core-finish-clippy.log`）、fmt 和 git diff --check 通过；真实 CLI help 已显示 recover-start 和 proxy remove --apply-to-running。未提交。剩余产品范围仍包括持续代理故障提示、保留数据卸载/诊断、真实 Guard/UAC/登录与账户/代理隔离、发行验收。IFEO、跨目录升级及登录任务被外部删除后的专用修复维持取消。

以下为先前批次记录。

**2026-09-20：取消 IFEO（当前范围）**

按用户最新决定，取消 IFEO。删除注册/恢复、入口解析、调试创建脱离及相应实验命令；GuardStatus 不再含 IFEO，原版与分身共用事件监听和扫描就绪标准。保留 `creation_guard` 对系统既有 Debugger 的只读拒绝；本轮没有注册、删除或修改真实应用的 HKLM IFEO 规则。

配置只保留固定空数组占位，保持原 manifest 序列化顺序和已有事务摘要。非空旧注册返回固定 `LEGACY_IFEO_CLEANUP_REQUIRED`，不回显输入、不覆盖原文件。IPC 升为 3.19，旧 major 双向握手拒绝；所有入口需使用同代二进制，未自动终止用户 coordinator。

首轮全量 427 通过、2 失败、53 ignored（`.tools/no-ifeo-tests.log`）：新增旧 major 客户端测试暴露原通用握手错误，已改为明确版本错误；快捷方式夹具直接删除复现 Win32 32，现复用已有仅针对共享占用的有界夹具重试，未改变生产删除语义。最终 `cargo test --workspace --locked --no-fail-fast -- --test-threads=1` 全量通过：**430 项通过、0 失败、53 项顶层 ignored**（`.tools/no-ifeo-final-tests.log`）。IFEO 专项及调试脱离共移除 21 项默认测试，新增 1 项旧登记拒绝且保留文件的存储测试；因此数量相对上批 450 减少 20。最终 workspace/all-targets clippy `-D warnings`、fmt 和 diff 检查通过（`.tools/no-ifeo-final-clippy.log`）。CLI help 已核验不再提供 `--debug-detach`。真实 UAC/登录触发/完整产品验收仍未执行，本批未独立 review、未提交。

以下为历史批次记录；其中 IFEO 的开发承诺和后续验收项已由本次取消决定取代。

**Rust M0 首批实现验证 — 2026-09-19**

本记录只描述已运行的代码，设计文档不等同于已实现功能。当前完成 M0 的进程创建/身份及包内 helper 验证部分，M0 尚未全部通过。

**2026-09-20：导入后继续开发——快捷方式核验与丢失恢复**

基线为 `8d03009`，当前工作区 `D:\app_proxy`、分支 `codex/entry-maintenance`。本机重新安装项目本地 Rust 1.98.1（`.tools/cargo` 和 `.tools/rustup`，未改系统 PATH），实际编译并验证。IPC 2.19 增加 ShortcutCheck、Repair/Repaired，所有快捷方式 RPC 双向要求 minor >= 19，防止旧端误解新恢复状态。`shortcut check/repair` 和中文“桌面快捷方式”菜单共用服务；核验不修复，恢复绑定原创建 ID/配置版本，独立持久请求记录先于外部发布。使用原位置、原 host、原图标；已有完整链接不换文件，修改/替换链接保留。完成回执是历史记录，重放不重新创建后来被删除的链接。新增文件身份仍由原 journal 管理，后续删除核对修复后的精确文件。

新增 4 项 journal 默认测试覆盖核验无写入、每个发布断点恢复、历史回执/二次恢复、并发替换保留、请求冲突、版本/归属/缺失资产；新增 1 项跨进程 CLI 测试覆盖核验、恢复、重放、用户修改冲突及保留实例且不启动应用。既有协议测试扩展至 2.19 和 Check。真实 Windows PTY 中在桌面创建一个自有 fixture 入口，经核验和完整链接恢复再移除，原 `native_menu_creates_and_removes_desktop_shortcut` 显式通过；最终确认该 `.lnk` 不存在。此项不是 Shell 点击启动验收。

首次默认并发测试中 6 项已有 LaunchEngine 用例因 `INSTANCE_CHECK_TIMEOUT` 失败；单项和后续串行执行通过，未放宽生产查询预算。串行全量又复现已有快捷方式变更用例的 Windows 共享冲突，其中平台用例捕获确切错误码 32；先前两次 CLI 删除未确认没有保留底层数值，不能单凭同类现象确定根因相同。未确认删除使用原请求恢复成功，连续 8 次单项复跑通过。针对已证实的文件占用，生产代码只对打开链接句柄的共享冲突进行最多约 250ms 重试；持续占用返回 `SHORTCUT_FILE_BUSY`，取得句柄后重新核验文件身份、内容和链接字段。后续全量捕获到另一种失败：已取得 DELETE 句柄后，ShortcutDelete 返回 Win32 5，检查文件不是只读。现对同一个已核验句柄上的失败删除标记进行最多约 250ms 重试（仅错误码 5/32，且文件非只读）；不改权限或属性、不重新按路径删除、不重复成功操作。夹具中的改名/清理仅对错误码 32 做最多 1 秒重试，其他错误和所有身份/内容断言保留。新增 2 项真实受控占用测试，覆盖共享句柄释放后继续、等待中改写仍拒绝删除，以及只读文件映射释放后的精确删除；既有持续占用测试断言有界失败。最终快捷方式平台专项 **32 项通过、1 项 ignored**（`.tools/shortcut-mapped-tests.log`）。

最终 `cargo test --workspace --locked --no-fail-fast -- --test-threads=1` 完整通过：**450 项通过、0 失败、53 项顶层 ignored**，日志 `.tools/maintenance-release-check.log`。最终 workspace/all-targets clippy `-D warnings`、fmt 和 diff 检查通过。前面失败日志保留用于诊断，不以单项重跑代替此次完整回归。默认并发模式未重新宣称通过。

本批只完成原位置丢失链接的维护，不包含跨发行目录升级、缺失 host/图标重建、登录入口修复、整体卸载、完整 UAC/登录链路或 IFEO 启用。IFEO 用户需求保留；命令行转发与 Chromium 挂起创建/沙箱句柄契约的限制已作官方资料与当前源码复核，见第九章，没有修改真实应用 IFEO。本批尚未独立 review、未提交。

**2026-09-20：登录任务 RPC、Guard 启用与独立就绪状态**

IPC 2.18 提供 LoginApply/Resume/Request/Status，固定派生路径，原 GuardStatus 协议保持兼容。变更持独立实际 worker 槽到 COM/记账完成，掉应答继续执行；终态先核对完整请求、绕过其他变更的忙状态。就绪查询独立单槽/3 秒预算，超时实际 worker 保留槽及 owner；核验任务定义/ACL、受保护 coordinator/监听组件、原 home 的 store/SID 与当前 home 路径，不把 Created 历史作为下次登录就绪。Guard enable 在写配置前轻量检查协议，组件就绪后自动创建；未完成请求保留原 ID 显式恢复。新增 guard login status/request/resume/remove；关闭 Guard 保留应用与登录入口。

新增 9 项默认测试：服务终态/冲突与先验拒绝、就绪证据/元数据/未完成回执、原生读取期间 journal 竞争、四操作双向 minor 门槛、忙时独立查询、超时后实际 worker 槽和 owner 保留、真实管道丢应答后的删除完成/历史重放、home 移动/替换/当前路径不匹配。超时测试用受控锁在直接 RPC handler 内阻塞工作线程，不声称真实 Task Scheduler COM 卡死验收。扩展既有真实 CLI 契约覆盖登录查询/未登记恢复、未知登录归属时仍展示缺失 listener 且成功关闭 Guard；异步操作均未启动或停止用户应用。

独立审查发现并修复终态 RPC 先争 native 槽、登录查询失败阻断 Guard 查询/关闭、UAC 后与登录创建后版本对齐、原 home 失效误报就绪四项问题，复审通过；审查方独立复跑 10 项定向验证通过。全量 **443 项通过、0 失败、53 项顶层 ignored**，clippy `-D warnings`、fmt/diff 检查通过。初次全量的新增 CLI fixture 使用非法 task 名导致校验失败，改合法 fixture 后重跑完整套件通过。没有执行真实登录触发、UAC 安装/提权事件全链或 IFEO 生产接管；这些验收及入口修复/卸载继续保留。

**2026-09-20：登录任务持久事务与显式恢复**

新增 protected state/login-task.json 保存请求和长期归属，Create/Remove 与配置/core/launch/shortcut ID 双向互斥。begin/resume 短锁接纳后返回不可克隆 Job；Job 和 Completion 持有独立非阻塞文件锁及 store owner lease，COM 执行不占配置 Mutex，直到完成记账才释放。完成先恢复普通 Pending 配置并核验 core 更新屏障，只合并精确匹配的当前集成元数据，外部操作成功而 manifest/回执失败保留原请求。完成创建重放不复活后来已删除的任务；删除须绑定原创建，可取消 Pending Create。已登记结果在 Guard 全关闭后仍可记账，缺失任务不得再创建。

新增 11 项默认回归，覆盖 intent/原生动作/manifest/终态中断、真实 Windows 文件占用造成 manifest 与回执写失败、operation 与 owner lease、无关配置合并、同版本创建/取消的身份比较、Guard 关闭后两种恢复分支、陌生元数据保留、坏 journal 拒绝、跨命名空间冲突、先恢复纯配置 Pending、已完成请求在后来 Job 与无关 Pending 阻塞期间只读重放，以及近 2 MiB/256 项历史下仍可完成和删除。独立审查指出终态重放曾先争 lease 和恢复无关配置，现提前只读核对完整请求并直接返回，回归通过。

ignored `native_login_journal_recovers_existing_registration_and_completed_removal` 显式通过：使用真实 Task Scheduler 登记后丢弃 completion，重开 store 后通过生产核验补记；真实删除后在 manifest 已提交处中断，再重开补终态；最终任务缺失。此测试只替换了提权监听组件准入，原生注册、回读与删除均真实执行；没有执行任务或用户应用。测试 UUID ec43e2f9-1e31-4800-ae2b-41612ecdc599，结束后额外只读查询确认无残留。RPC、Guard 启用/菜单接入与实际登录仍待完成。

独立最终复审通过并复跑全部 11 项默认专项。全量 workspace **434 项通过、0 失败、53 项顶层 ignored**，日志 `.tools/login-journal-final-tests.log`；clippy -D warnings、fmt/diff 通过。原生中断恢复测试由主实现方执行，审查方审阅其代码但未重复登记系统任务。

**2026-09-20：普通 coordinator 登录任务平台**

新增 guard_task::login，复用已有 Task Scheduler COM 构造和核验。登录/提权事件任务分别固定 Data 协议标记、URI/任务名、principal SID、运行级别、ACL 和触发器，不能交叉采用另一角色权限。登录任务仅当前 SID 的 LogonTrigger，LUA/InteractiveToken，固定发行版 host `serve --home ... --expected-store ...`。新建需核验已有监听组件、当前发行版来源和 coordinator 映像 pin；只创建缺失或复用完全匹配项，不覆盖冲突。Registration 可供上层持久保存；核验/空闲删除不需要原程序或数据目录仍存在，不终止任务实例或用户应用。

默认新增 5 项测试：3 项登录任务（名称/转义/参数替换拒绝、权限角色隔离、原生内存 COM 主体/触发器负例），1 项事件任务 URI/generation/角色负例，1 项真实 host 的 store 绑定。后者以文件共享锁留下真实 Pending 请求，错误 expected-store 被拒绝且 manifest/request 字节不变，正常打开才恢复 revision 2；不存在的数据目录不创建。独立审查发现校验最初位于 Store::open 自动恢复之后，已移到 Store::open_expected 持锁核验后、任何恢复前；复审和该回归独立复跑通过。

两项 ignored 原生测试显式通过：创建当前用户 UUID 登录任务、原样重复登记、不同 home 的冲突登记和删除均拒绝、原任务保留、按原记录空闲删除；以及首次读回失败任务的精确记录恢复。真实注册发现 Windows 会把自定义 URI 改写成任务路径，导致旧事件代码和新登录代码均可能误报冲突；现两种角色均构造并要求精确的任务路径 URI，其他身份约束保持。首轮 80e17af3-efdb-4aa9-a843-3042fc71bf8f 残留已通过同一 Rust 核验/删除接口恢复清理，成功重跑 7aed6e5a-5cb6-4384-82c7-4ac36b83f45b 同样已删除，随后只读查询确认两项均缺失。本批没有运行任务、切换登录或启动用户应用；这不证明实际登录触发或完整提权 Guard 链路。Guard 启用流程的持久意图/自动登记及维护入口仍待接入。

最终全量 workspace **423 项通过、0 失败、52 项顶层 ignored**，日志 `.tools/login-task-final-tests.log`；clippy -D warnings、fmt/diff 通过。独立审查定向 9 项通过，恢复顺序修复后跨进程回归再次独立通过；实际系统任务测试由主实现方显式执行，审查方未运行系统任务。

**2026-09-20：实例高级设置**

新增管理实例高级设置及 instance settings/edit，复用 ConfigRequest、完整请求摘要去重和 revision CAS。args/cwd 部分替换，env 设置/移除/恢复继承按 ASCII 大小写不敏感统一判重；禁止受管字段，原版拒绝分身路径变量。新环境值先全量校验再分存不可变秘密，manifest/intent/回执只含引用；参数和目录继续保存在 ACL 保护配置中。私密文件输入有界且校验 ACL/普通文件/重解析链，不改动用户输入，错误和摘要不回显值。IPC 2.17 对查询和编辑双向拒绝旧版本。

新增 9 项默认测试：core 2 项部分编辑/无效输入；存储 2 项持久中断恢复、秘密不泄漏/不提前写入；app 2 项输入空值/null/严格解析与摘要上限；IPC 双向版本 1 项；运行实例及旧启动计划 1 项；真实 CLI 1 项。独立审查及增量复审通过，独立复跑上述 9 项。CLI 验证目标文件已删除仍能编辑、输入保持原样、陈旧 revision 拒绝且不多写秘密。真实自有 child 在编辑后保持原身份存活、launch journal 不变；明确 revision 的待创建请求在编辑后拒绝，下次新请求读取新 secret。全量后进一步加强 cwd 夹具：初始显式子目录与应用目录不同，旧进程回执及新进程回执分别核验，定向通过。

真实 PTY 显式运行 ignored `console_advanced_settings_save_hidden_values_without_launching`：参数含空字符串及环境值都不回显，确认保存并退出；验证全局 revision 连增两次、参数保存、环境仅含 secret_ref、原值可从受保护秘密读取且无 launch attempt。首轮退出清理发现长暂停后 coordinator 已空闲退出并被后续操作重新创建；测试改为清理前重新查询并验证当前夹具 host 完整身份，不假定最初 PID 仍持锁，重跑 61.71 秒通过。仅停止自有夹具 coordinator。

首轮全量触发既有 lost-ACK 测试的查询先于接纳竞态，单独复跑通过；修正测试 helper 在原有 10 秒上限内允许暂时 None，其他回应仍拒绝，生产逻辑未改变。修复后全量 workspace **418 项通过、0 失败、50 项顶层 ignored**，日志 `.tools/instance-settings-final-tests.log`；clippy -D warnings、fmt/diff 通过。高级设置与整个产品完成情况分开，集成维护和完整 Guard/IFEO 链路仍待续。

**2026-09-20：快捷方式 coordinator、CLI 与菜单接入**

新增 shortcut create/remove/status/request/resume 命令及“管理实例 → 桌面快捷方式”菜单。RPC 不接收目标/图标/桌面路径，程序从当前发行目录、KnownFolder 和实际 EXE 资源确定。安装/资源查询在配置锁外，回锁后先校验同 ID 完整请求再检查当前 revision，避免并发已完成请求被误报 stale。移除/恢复直接使用原 journal，原应用已卸载也不触发重新解析。只读实例状态优先返回待删除请求，历史 Created 不被宣称为当前文件可用性。

IPC 2.16 双向门槛覆盖全部四种新操作；变更单槽许可由实际 blocking worker 保持至结束，额外变更立即返回 busy，查询连接仍可用。断线不取消已接受操作；CLI 丢失回应后只查询原 ID，不自动换号重试。Ctrl+C 两条等待路径均输出带原 ID 的 JSON/诊断，未查到记录不推断锁外准备已停止。生产 Remove 必须携带显示过的 expected_creation，同一锁内匹配原创建记录；即使先取消旧 pending 又在相同 revision 接受新 pending，也不能误删后者。完整终态容量投影包括该字段。

新增 10 项默认测试：4 项服务（固定字段/图标路径、无安装重放和解除、锁外编辑/版本拒绝、同请求并发、待删除状态），3 项真实 IPC（双向版本拒绝、忙时保留查询、掉应答恢复与删除），1 项平台同 revision 创建归属 CAS，2 项跨进程 CLI（用户修改保留/原 ID 查询与恢复/历史重放、缺 store 不创建及拒绝任意路径参数）。独立审查复跑全部 10 项，通过。

两项 ignored 原生验收显式通过：`native_desktop_cli_creates_exact_fixed_host_link_with_complete_icon` 用 System32/cmd.exe 只读图标资源，在实际 Desktop 创建 UUID fixture 链接；回读核验固定真实 host/参数、持久 ICO 与完整提取字节一致，CLI 解除后文件缺失且 manifest 无快捷方式。审查方独立复跑通过。`native_menu_creates_and_removes_desktop_shortcut` 真实 PTY 选择管理实例/桌面入口/确认创建，随后同菜单确认删除并退出；断言 revision 2→3→4、一个完成创建/删除记录及链接已缺失，通过。没有 Shell 点击，也没有启动 cmd 或用户应用；只终止测试自有且完整身份核验的 coordinator。提交前只读确认真实桌面没有本批 `AppProxy contract *.lnk` 残留。

全量 workspace **409 项通过、0 失败、49 项顶层 ignored**，日志 `.tools/shortcut-service-final-tests.log`；clippy -D warnings、fmt/diff 通过。独立最终复审通过，提交主题 `feat(rust): connect desktop shortcuts to coordinator and menu`。真实 Shell 点击、入口升级修复及完整产品验收仍待完成。

**2026-09-20：快捷方式持久归属与显式恢复平台**

新增 protected `state/shortcuts.json` 保存创建意图、完整计划、临时文件身份/hash、创建和解除历史结果。非启动型临时文件准备后先记身份，再使用同一 READ|DELETE 句柄执行不覆盖改名；迟到的占位文件保留。打开 store 和查询不重放外部操作；显式同 ID 恢复核对目标、临时文件和最新配置。已完成创建重放不会在解除后重建链接；用户修改/替换或损坏记录保持原状并报告冲突。身份写入前中断遗留的未知 `.tmp` 保留，不实施按文件名清理。

快捷方式意图先恢复未决纯配置操作；存在有效入口或未完成操作时，配置请求及直接 snapshot 提交都禁止删除该实例，重命名等其他编辑仍可合并。配置/core/launch/shortcut 请求编号双向冲突校验；活动归属不限七天，解除历史才按七天清理。独立审查发现终态 revision 未拒绝未来/逆序、首次接受未预留后续记录空间，现分别校验实际 manifest revision，并投影每个活动记录的最长完整终态字节预算，新增超限在 intent 前拒绝，已有记录仍可删除。

新增 15 项默认测试：3 项 staging（真实 Windows 同句柄改名、迟到占位、身份/目录冲突），11 项 journal（全部创建/解除持久断点、终态重放、未决取消、配置合并与删除保护、修改/替换保留、丢失/修改临时文件、损坏记录、双向请求冲突、数量上限清理、异常 revision、接近 8 MiB 容量），1 项既有 pending RemoveInstance 先于快捷方式接受恢复。审查方独立运行 25 项快捷方式测试及该 pending 配置测试，共 26 项通过、1 项图标实机测试保持 ignored。本批仅在自有临时目录创建链接，没有真实桌面入口、用户应用或机器集成操作。

全量 workspace **399 项通过、0 失败、47 项顶层 ignored**，日志 `.tools/shortcut-journal-final-tests.log`；clippy -D warnings、fmt/diff 通过。独立审查与修复复审通过，提交主题 `feat(rust): persist shortcut ownership and explicit recovery`。

coordinator RPC、菜单创建/清理、真实桌面点击与完整产品验收仍待完成。

**2026-09-20：快捷方式 host 隐藏启动与前台依赖流程**

host 新增 `launch <实例ID> --home <目录> --notify`，复用已有 launch_cli 的提交/查询/取消/依赖修复逻辑，仅将来源设为 Shortcut。先 describe 已有 store，不创建丢失目录。正常成功不打开控制台、不输出文本；确定需要安装/共享 core 确认时才打开前台，复用现有中文选择、具体影响、配置版本核验和失败后继续规则。未知请求不换编号重试；失败消息框保留原 launch/core 请求编号及数据目录。无 notify 只返回退出码与错误，不进入交互依赖处理。

原生前台控制台保留并恢复原标准句柄，显式绑定 CONIN$/CONOUT$ 处理重定向场景。独立审查发现仅 spawn ctrl_c future 会在第一次 poll 前留下默认终止窗口，现于首提示前同步构造 Windows CtrlC stream，注册失败拒绝继续。该时序修复同样作用于普通 CLI/menu；既有取消语义不变。

新增两项默认真实 host 契约：正常成功无 stdout/stderr，journal 来源 Shortcut、重复启动复用同一会话且只创建一次自有 child；缺 store 不创建，非法 EXE/缺内核保持单失败 attempt、返回原编号且零目标、无凭据输出。已有 3 项 launch/状态 CLI 回归同时通过。

三项 ignored 原生测试显式通过，并经独立复跑：① detached 独立控制台从重定向标准句柄切换成功，在 current-thread runtime 未 poll 信号任务前立即产生本控制台 Ctrl+C，sticky 取消与后续读拒绝通过，Drop 恢复原重定向；② 私有 CLI 槽测试程序与真实 host 保持原 peer 身份策略，缺内核时输入 2 返回，单 CORE_BINARY_MISSING attempt、零应用及零安装请求；③ 自有原生消息框回读实际控件，launch/core 两个编号可见，点击真实 Button 关闭。消息框测试最初错误假定 MB_OK 的内部按钮 ID 为 IDOK，本机实际为 IDCANCEL；修正为操作实际控件后通过。测试仅使用自身进程/独占控制台和临时目录，不操作用户应用/桌面链接，不安装内核或提权。

独立审查无剩余阻塞，共独立复跑 5 项默认与 3 项原生契约。全量 workspace **384 项通过、0 失败、47 项顶层 ignored**，日志 `.tools/shortcut-launch-final-tests.log`；clippy -D warnings、fmt/diff 通过。最后完善缺失 store 的路径诊断后对应真实 host 契约再次通过。提交主题 `feat(rust): launch shortcuts quietly with shared foreground recovery`。

本批不包括快捷方式创建事务/恢复、菜单创建入口和真实桌面点击，也未通过快捷方式进行真实网络安装或共享 core 重启。

**2026-09-20：原生快捷方式与完整图标缓存平台**

Shell Link 在 COM 内存流编码并回读，固定 host/实例/store 参数及归属标记；禁止自动链接跟踪、环境替换、runas 等额外标志。发布前要求上层 journal（尚待接入），平台使用同目录临时文件与不覆盖发布；receipt 记录实际文件身份和 SHA256。删除核对身份、原始内容和 Shell 字段后用同一 READ|DELETE 句柄标记删除，不按路径重开删文件。原有、被修改、被替换、硬链接或占用链接均保留；输出目录 root→leaf 句柄核验实际属性且禁止删除共享。审查发现预检后 junction 替换窗口，现已修复并通过故障注入；仅 READ_ATTRIBUTES 不足以固定目录，最终输出使用 FILE_GENERIC_READ。

图标从实际 EXE 首个 RT_GROUP_ICON 提取全部 RT_ICON 载荷，保留多尺寸/PNG/DIB 字节并重建 ICO offsets，数量/单张/总大小有界。LoadLibraryExW 仅资源映射，不运行应用。内容 hash 固定缓存到 protected state，原子不覆盖发布，已改内容和硬链接拒绝复用。初次真实 MSIX 提取因源目录访问权限失败；改为保留 EXE 文件禁止写/删、读取规范路径、资源映射前后复核完整文件身份。图标是展示数据，不是执行身份凭据；源目录并发替换又恢复的原子快照不在承诺范围。缓存与链接输出的目录 pin 没有放宽。

新增 11 项默认测试覆盖 COM 往返/参数、占位与用户修改保留、替换身份、额外标志、文件占用、并发发布、硬链接与无效路径/字节、目录 junction 竞态、全部图像与 offsets、大小/缺失资源、缓存重复/冲突、源文件锁与更换身份、无效 EXE 零缓存。额外显式运行真实 EXE ignored 测试：Claude 2.2553.1.0 提取 13 张/96609 字节；Codex 26.915.4065.0 的 ChatGPT.exe 提取 8 张/107770 字节；生成 ICO 由 Windows LoadImage 解码，临时 .lnk 创建/回读/删除通过。未在真实桌面创建入口，未启动/关闭用户应用，未触发 UAC。

独立最终审查无剩余阻塞，独立复跑 11 项默认专项及上述两个真实应用 EXE 验证通过。全量 workspace **382 项通过、0 失败、41 项顶层 ignored**，日志 `.tools/shortcuts-platform-final-tests.log`；clippy -D warnings、fmt/diff 通过。随后按 clippy 建议把固定分块读取改为 as_chunks，11 项专项再次通过。提交主题 `feat(rust): add owned Shell Links and complete executable icon cache`。

本批是平台能力；快捷方式事务与恢复、host 隐藏启动/通知、菜单接入及真实桌面点击仍待完成，不能据此宣称快捷方式产品流程已交付。

**2026-09-20：独立只读实例运行状态与详情**

新增 RuntimeStatus RPC（minor 15 双向准入），与 Guard 启用意图无关。先从受保护历史回执检查完整进程身份，保留会话启动时的网络；当前配置关系用可空布尔表达，无法比较不抹掉存活证据。当前安装定位变更后仍显示原会话；精确退出后继续查询外部实例归属，不把旧回执退出等同于实例未运行。外部主进程只观察，多个主进程、未知目录/身份、辅助残留均不宣称未运行。查询不创建数据、联网、接管或关闭应用，不登记会话退出；最终核对 manifest revision、会话和新 pending。安装解析与 Guard 共用实际 worker 持有的许可，3 秒截止后不会累积后台解析线程。

菜单在 Guard 未提供进程证据时补充独立状态；详情及 `instance inspect <ID> [--json]` 展示配置、运行与保护三个方面。历史网络与下次启动绑定分别显示，配置修改不会冒充已作用于当前应用；保护查询失败保留运行证据并显示固定诊断，输出前再次核对配置版本。JSON 不输出 argv、环境、秘密或依赖摘要，target_traffic_evidence 固定为 not_observed。

新增 8 项默认测试通过：Guard disabled 的真实会话与安装路径变更、旧进程退出后外部替代/消失、分身数据不创建与原版排除/多主保留、扫描后配置/启动竞态、超时后的解析槽保留、minor 15 双向拒绝、busy 状态及其他 RPC 可用、真实 CLI 启动前/运行中/绑定变化/退出查询及脱敏。扩充现有 Windows 回执测试验证只读包装在存活/退出时均不改 journal。独立复跑上述 8 项及扩充 Windows 项，共 9 项通过；独立最终复审无阻塞。测试仅操作自身受控子进程，未启动真实 Codex/Claude 或执行提权。

全量 workspace **371 项通过、0 失败、40 项顶层 ignored**，日志 `.tools/runtime-status-final-tests.log`；workspace clippy -D warnings、fmt/diff 通过。提交主题 `feat(rust): expose read-only instance runtime status and details`。快捷方式、高级设置、维护、完整 Guard/IFEO 与产品验收仍继续实现。

**2026-09-20：中文日常菜单与共用业务流程**

无参数或 `menu` 进入中文交互，接通添加/启动/管理实例、手动代理、订阅导入/刷新/选择及共享 core 状态；共用现有业务函数和一个持续有效的 Foreground 取消状态。默认原版、网络明确选择，代理健康检查通过后才创建实例。缺 sing-box 时流程内安装/返回，自动目录；保存回执与后续保护/启动分开，返回保留已保存记录。不支持隔离的模板在网络准备前拒绝分身；Codex/Claude 代理分身确认前说明默认 Guard。保存后只沿自身回执版本继续保护授权，不能以另一个客户端的新快照替代已确认版本。

新增默认测试覆盖菜单无终端时零 store 写入、默认值不吞 EOF、100KB 秘密行以 7 字节缓冲丢弃至行末且保留下一行；URL 输入现在区分空行非法与 EOF 返回。独立审查推动修复不支持分身的延迟拒绝、Clone Guard 摘要、保存后授权版本跳跃及超长秘密输入残留。独立复跑 16 项相关默认回归及两项真实 sing-box CLI 回归通过（自有路由订阅下载/等价连接保留 PID，代理更新影响/过期确认/恢复失败）。

三项 ignored 菜单契约由主代理通过真实 Windows PTY 显式运行：Environment 原版直连保存、返回不启动、改名、拒绝分身；认证代理密码输入不回显，添加实例遇缺 sing-box 选择返回后保留代理且零应用/实例/启动记录；菜单等待改名确认时另一个 CLI 修改配置，旧摘要确认被拒绝且外部结果保留。均使用独立临时 store 和不能执行的 EXE 夹具，没有操作真实应用或执行安装/提权。Ctrl+C 测试能看到前台取消提示，但 PTY 宿主也被中断，未取得完整测试断言，**不计为通过**，保留手工实机测试。审查方核对代码与报告，没有独立操作这三个 PTY。

全量 workspace **363 项通过、0 失败、40 项顶层 ignored**，日志 `.tools/menu-final-tests.log`；本批三项菜单 PTY 及两项真实 core CLI 属于 ignored 中另行显式通过的项目。workspace clippy -D warnings、fmt/diff 通过，最终独立复审无剩余阻塞。提交主题 `feat(rust): add Chinese daily menu with shared CLI workflows`。

本批仅完成日常菜单：关闭 Guard 的实例进程状态仍显示“未确认”；高级设置、快捷方式、维护入口、完整 Guard/IFEO 与产品验收继续实现。

**2026-09-20：订阅 CLI 导入、刷新与节点选择**

新增 proxy import/nodes/select/refresh。导入地址只经无回显终端或显式 stdin 读取一行，限定大小并验证；本地入口自动分配。交互节点选择支持返回/EOF，非交互要求准确名称/ID。已保存节点通过新的 IPC 2.14 分页，仅返回元数据，全局 revision 防止跨页拼接不同版本。下载/暂存取消不提交配置，丢回复仅查询或重放同一预览/stage ID；提交沿用原持久请求。刷新/选择先尝试纯配置事务，只有确定要求重配置时才建立共享内核计划；等价连接无重启。--via 先准备自有路由，必要时在当前流程提示安装或确认扩容，JSON 成功只输出最终结果。

新增 3 项默认跨进程 CLI 测试经独立复跑通过：70 节点导入/跨页列表/无下载切换/刷新保留选择与报告新增/所选消失保留旧配置/按原编号查结果；非交互缺选择及秘密地址非法时零下载零创建；4 个真实 CLI 慢下载占满会话后，第 5 个立即返回原 SUBSCRIPTION_PREVIEW_LIMIT、不产生新下载。现有订阅专项扩充保存节点分页脱敏/revision 边界及 minor 14 双向拒绝；22 项专项经独立复跑通过。

独立审查发现并推动修复两处问题：旧二选一读取将 EOF 映射为“2”，节点选择改用保留 EOF 的接口，不误选第 2 项；预览只对通信结果不明查询原 ID，确定协议/准入错误立即原码返回，不吞成超时。无回显输入的原控制台模式由 RAII 在完成或取消时恢复。

3 项 ignored 专项本轮显式运行通过：真实 sing-box 自有双 profile core 经 HTTP 路由给 CLI 下载 `.invalid` 订阅，JSON 可作为单一对象解析；等价选节点保持 PID，连接变化的刷新仅 Prepared、影响两个活动 profile、原 source/选择/PID 保留，此项独立复跑通过。两个 Windows PTY 场景由主代理操作验证：隐藏 URL 输入后 Ctrl+Z/Enter 返回且不创建 profile；缺 sing-box 的刷新流程现场显示“安装并继续/返回”，选返回后零下载且 manifest 字节不变。审查方核对 PTY 实现/记录，未独立操作这两项。原生测试初次因合成 HTTP 夹具继承非阻塞导致 CONNECT 后读取失败，修正夹具为阻塞读取后通过；没有修改生产网络策略。

这批未重复执行实际网络安装、真实服务订阅或六协议实网转发，完整菜单/应用代理/IFEO/发行验收仍待续。独立最终复审无剩余阻塞。全量 workspace **360 项通过、0 失败、36 项顶层 ignored**，日志 `.tools/subscription-cli-final-tests.log`；新增原生和两项 PTY 属于 ignored 中另行显式通过的项目。workspace clippy -D warnings、fmt/diff 通过。提交主题 `feat(rust): expose subscription import refresh and node selection`。

**2026-09-20：订阅预览协调服务与 RPC**

coordinator 新增内存预览会话：下载/解析移出配置锁，只返回分页节点元数据；最多 4 个会话，10 分钟到期，关闭会取消下载，忙槽在实际 worker 结束后才释放。预览本身不写秘密或 manifest，不启动/停止内核。确认节点后的 stage 仅发布不可变秘密和引用式 ConfigRequest；缓存精确请求与固定失败码，同 ID 重试保持原结果，提交继续走既有持久配置请求和共享 core 影响确认。coordinator 重启后预览失效，须重新下载；已提交配置仍按原请求编号查询。未提交 stage 可能留下无引用秘密文件，不自动删除用户配置。

8 项新增默认测试通过并经独立复跑：本地 HTTP 下载只读/分页/脱敏、同 ID 去重与冲突、stage 精确请求及已提交结果重放、失败具体原因重放、4 个慢下载取消及槽释放、TTL/跨 epoch 失效、刷新期间改名允许与来源 revision 变化拒绝、未运行自有 core 时不连接外部监听也不退回直连；真实管道验证 minor 13 四类操作双向准入、预览保活、分页、失败码重放及关闭。独立订阅专项共 22 项通过、3 项 ignored。

新增真实 sing-box 1.14.1 专项单独运行并经独立复跑通过：自有 core 的手动 HTTP 路由连到回环合成上游，下载 `.invalid` 地址得到两个节点，前后进程身份相同；非法 URL 拒绝后内核仍存活。下载在共享生命周期 gate 内，前后核验进程和全部入口的归属。失败保留场景为 URL 校验失败，不是网络中断验收；没有访问真实订阅、操作用户应用或执行提权操作。

CLI 导入/刷新/选择与完整菜单尚未接入；按 profile 下载目前要求自有 core 已就绪，预览服务本身不触发安装或启动。独立最终复审无剩余阻塞。全量 workspace **357 项通过、0 失败、33 项顶层 ignored**，日志 `.tools/subscription-preview-final-tests.log`；新增真实下载测试属于 ignored 中另行显式通过的一项。workspace clippy -D warnings、fmt/diff 通过。提交主题 `feat(rust): coordinate bounded subscription previews and staging`。

**2026-09-20：订阅 staging、刷新/选择事务与共享 core 恢复**

新增 Store 导入/刷新 staging API，将调用方已解析的节点转换为不可变秘密及只含引用的 ConfigRequest；不提交 manifest 或启动进程。来源 revision/URL 引用来自下载前，当前全局 revision 在 staging 时读取；registry 提交再次核验。按名字保留 ID/当前选择，所选名字消失、重名、来源陈旧或提交期间全局变化都拒绝覆盖；不变节点复用秘密。Refresh/Select 如不改变整个活动 generation 的编译字节可纯提交，连接变化仍需 PrepareSubscription 预览和已有 ApplyUpdate 明确确认。

6 项新增默认回归及独立复跑通过：导入零 manifest 变更/稳定秘密数量/成功重放/更换 URL 或密码拒绝；无关改名后刷新、节点顺序/身份/秘密复用、下载期间新选择、增删报告及精确请求重放；陈旧 source/URL、所选消失和重复节点零修改，staging 后陈旧提交拒绝；活动 generation 的未选节点更新/等价节点选择零重启及选中连接变化要求确认；Refresh/Select 精确 plan、无关字段篡改拒绝、reopen 和准备/执行回执恢复；IPC minor 12 双向拒绝发生在配置/core 接纳之前。journal 测试使用合成的当前进程身份，但不调用进程操作 API。

独立审查复现已提交导入后更换秘密内容仍被旧回执接受的问题：相同请求/name 会派生相同 ID，故仅比较引用请求摘要不够。终态 stage 重放现在只读比较全部 incoming 秘密内容，缺失或不同拒绝，不创建或修改秘密；URL 和密码两种回归均覆盖。

真实 sing-box 1.14.1 测试单独显式运行，并经独立复跑通过：两个受控 Shadowsocks 上游映射至回环 HTTP 夹具，订阅与手动 profile 共用实际 core。prepare 保持原 PID/manifest/流量；确认 Select 后旧 core 退出、订阅流量切换，手动路由保持；有效格式但选中上游不可用的 Refresh 回滚两条原路由且不改 manifest；成功 Refresh 保留选中 ID 并递增 source revision。只操作自身 core/peer Child，结束后显式停止；无真实订阅、用户凭据、用户应用或提权操作。这证明本地 Shadowsocks 链路与重配置，不代表六协议全网验收。

全量 workspace **349 项通过、0 失败、32 项顶层 ignored**，日志 `.tools/subscription-edits-final-tests.log`；新增真实切换测试属于 ignored 中另行显式通过的一项。workspace clippy -D warnings、fmt/diff 通过，独立最终复审无剩余阻塞。提交主题 `feat(rust): stage subscription edits and reuse shared core recovery`。下载协调服务、CLI 导入/刷新/选择和完整菜单仍待实现。

**2026-09-20：订阅节点秘密分存与共享内核编译接入**

manifest 新增 Subscription 来源（URL secret ID、独立 revision、SavedNode 列表）。节点只公开显示元数据和引用，完整 typed Node 通过私有 serde adapter 编码为版本化秘密文档，包含密码、UUID 与可能携带 token 的传输路径；保留现有 ACL 保护的不可变秘密文件，不宣称加密存储。Store 提交/读取验证全部引用、URL、节点类型和显示元数据绑定；未知字段、损坏或不匹配拒绝且保留原文件。共享编译器只读取选中节点并生成既有固定路由；旧手动更新拒绝覆盖订阅来源。

新增 10 项默认回归通过：六协议秘密往返与 manifest 脱敏；严格嵌套 JSON/版本/凭据/元数据/大小；混合来源共享编译、只解析所选秘密及失败传播；来源/重名/选中引用校验与手动更新拒绝；共享 URL 校验；真实 store 写入/reopen/持久请求重放及 manifest/journal/backup 脱敏；未选中节点秘密也须有效、坏提交保留及已有损坏不重置；Catalog 只显示元数据并支持六协议；所选 secret ID 的启动依赖、旧 manual 摘要兼容；真实双向 IPC minor 11 目录边界。

审查发现并修复两处问题：内部 tagged Quic unit variant 可能忽略未知字段，改为空 struct 变体并回归；先转 JSON Value 会重排旧手动认证对象字段、改变历史启动 digest，改为 untagged tuple 直接序列化，并与旧算法逐字节摘要对照。URL/来源 revision 和未选中节点变化不影响已有连接的依赖摘要；所选不可变 secret ID 改变会影响。

新增真实 sing-box 1.14.1 契约单独显式通过：从秘密文档还原六种协议，连同一个手动 HTTP profile，生产共享编译器生成的 7 入口/出口配置通过 `check -c stdin`。没有启动这七个上游连接、使用真实订阅或操作用户应用，因此不构成协议实网验收。导入/刷新、选择保持、运行中订阅变更确认和 CLI 写入口仍未接入。

全量 workspace **343 项通过、0 失败、31 项顶层 ignored**，日志 `.tools/subscription-saved-final-tests.log`；新增真实内核契约属于 ignored 中另行显式通过的一项。workspace clippy -D warnings、fmt/diff 通过。最终独立复审无阻塞，独立复跑全部 10 项新默认回归及 7 profile 真实 check 均通过；提交主题 `feat(rust): persist subscription nodes through protected secret references`。

**2026-09-20：Clash YAML 与客户端文本订阅适配**

统一解析入口识别 YAML、客户端文本、URI 和一层 Base64 包装，复用六协议 typed Node 与 outbound 编译。YAML 使用有界事件解析，支持锚点/别名/合并且限制物理深度、别名逻辑深度与展开工作量；仅提取 proxies。文本支持 Proxy/server_local 节、位置参数和键值参数、引号内的分隔符与转义。未知连接字段和无法等价转换的选项报告不可选，不将原始内容写入诊断或 manifest。

新增 10 项格式回归，合计 22 项解析专项经独立复跑通过：六协议双格式、嵌套 ALPN、四类 Base64、引号与字面百分号、YAML 合并优先级/类型/别名边界、资源上限、来源行号、重复/冲突选项与名称、未知安全约束、普通合并键和 HTTP 方法。审查推动修复证书 fingerprint 被误当 uTLS、HTTP + TLS 不等价转换、引号内合并键及默认 GET 被内核 PUT 替换；最终独立复审无阻塞问题。

3 项真实 sing-box 1.14.1 契约测试单独显式运行，并经独立复跑全部通过：既有 16 个 URI 配置 check；新增 YAML/文本六协议共 12 个配置 check；新增自有 core 和回环 TCP 上游实际观察默认 GET 与显式 POST。收包夹具在读到请求头后关闭，不构成 VMess 握手/转发成功；只启动和停止自身 Child，未接触用户应用、真实订阅或用户凭据。没有执行提权安装或 IFEO 写入。

最终全量 workspace **333 项通过、0 失败、30 项顶层 ignored**，日志 `.tools/subscription-formats-final-tests.log`。3 项内核契约属于 ignored 中另行显式验证的项目；workspace clippy -D warnings、fmt/diff 通过。格式解析完成不等于完整客户端扩展兼容，秘密分存、刷新选择保持和 CLI 仍待实现。

**2026-09-20：六协议 URI/Base64 节点与 outbound**

core 新增内存中的 Node/Protocol/Tls/Reality/Transport 类型和 URI 列表适配，覆盖 AnyTLS、VLESS、VMess、Shadowsocks、Trojan、Hysteria2；不实现 Debug/Serialize，不修改既有 manifest。原始 URI 仅一次百分号解码，Base64 Shadowsocks 凭据与 VMess JSON 保持字面值；输入/行/条目有界，重复名字拒绝，来源行号和固定错误类别不泄漏输入。未知连接字段和不支持的 TLS/transport 组合不能进入可选节点；outbound 编译重新验证并只输出允许字段。

12 项核心专项和独立复跑通过：六协议及四种 Base64 envelope、三种 SS URI、凭据/名称/path 单次解码、IPv6/IDN/端口/必需字段、重复查询键/别名/JSON 键、未知和冲突参数、TLS/Reality/多传输字段、VMess 严格整数与选项、名称地区/重名/脱敏行号、输入/行/数量限制、输出密钥规范化/协议组合、修改后重验证。独立审查推动修复 Reality 与 SS2022 输出编码不符合内核要求、SS2022 ChaCha20 错误接受 AES 专有的多密钥链、TCP HTTP 伪装被改成 HTTP/2、无 TLS 的 h2 被降为 HTTP/1 四处问题，增量复审通过。

新增真实内核契约测试单独显式通过并经独立复跑：以固定已验证 sing-box 1.14.1 执行 `check -c stdin`，16 个合成配置包含六协议、Reality 带 padding 公钥、SS2022 三种算法 URL-safe 无 padding 密钥、WebSocket/gRPC/HTTP/HTTPUpgrade/QUIC 与 AES 多密钥链。没有启动代理服务、访问上游、写用户配置或使用用户凭据。测试进程有隐藏窗口/超时/kill_on_drop。此证据只证明内核接受配置，不证明协议实际转发或真实节点连通性。

最终全量 workspace **323 项通过、0 失败、28 项顶层 ignored**，日志 `.tools/subscription-uri-final-tests.log`；新增真实内核 check 项已单独显式通过。workspace clippy -D warnings、fmt/diff 通过。Clash YAML、各客户端文本格式、持久化秘密分存、刷新/选择保持和 CLI 仍待实现；不能据本批称完整订阅功能已交付。

**2026-09-20：订阅下载传输层**

新增传输 API，明确直连或指定回环 HTTP proxy，忽略系统和环境代理；固定 UA 次序、15 秒请求/120 秒总体预算、5 次重定向并禁止 HTTPS 降级。URL 和正文只在内存中使用，错误不包含 URL、凭据、头或响应内容。原始和解压后正文分别限制 8 MiB，严格 UTF-8；支持 gzip/br/deflate（zlib）/zstd。调用方尚须核验自有 ManagedCore 入口、选择来源并按 revision 提交，不代表订阅解析或生产导入流程已接入。

独立 review 复现并推动修复三处边界：reqwest 自动解压删除重复编码头、正文网络截断被库标为 Decode、默认解码器只读首个成员并忽略尾部。现禁用自动解压并检查原始唯一编码头，网络和解码阶段分别分类；gzip/zstd 完整读取多成员/多帧，所有编码校验输入耗尽，累计限制解压大小。依赖直接使用 async-compression，没有开启 reqwest 压缩 features，因此不改变已有内核下载行为。

11 项专项和独立复跑通过：URL/入口/重定向策略；403 后 UA 回退及成功正文不解析；空/非 UTF-8/超限拒绝；gzip 解压后超限；重复/叠加/坏编码拒绝；正文截断后下一 UA 成功；四编码 HTTP 往返；垃圾尾部拒绝与 gzip/zstd 拼接/累计超限；真实 HTTP 重定向环与本地 TLS 降级拒绝；请求及总体超时；污染代理环境下子进程明确直连和明确代理。一个 ignored helper 由父测试显式调用并启用 kill_on_drop。没有访问真实订阅服务或使用用户凭据。

最终全量 workspace **311 项通过、0 失败、27 项顶层 ignored**，日志 `.tools/subscription-download-final-tests.log`；workspace clippy -D warnings、fmt/diff 通过，独立最终复审无提交阻塞问题。六协议节点解析、订阅持久化、刷新保持选择及 CLI 集成仍待实现。

**2026-09-20：自动扫描与纠正触发追加验证**

新增 7 项默认行为测试：事件风暴合并及延迟实例不饿死其他实例；未知三次短重试后退避；旧 Ready 不能覆盖新未知/待纠正/未决状态，IFEO 与新鲜度状态；受控原版/合规分身保留；受控误启动分身精确关闭且代理失败不直连，Absent 保留失败；未授权真实循环与陈旧扫描不接纳纠正；协议 minor 9 两向拒绝。失败回执并列次序及单独 origin 标签不构成纠正证据也已覆盖。全量 280 项通过、26 项 ignored，日志 `.tools/guard-scan-final-tests.log`；最终调度/输出微调后 12 项监督、2 项 CLI 定向再次通过，clippy/fmt/diff 通过。独立审查方先复跑 31 项 Guard 服务测试，再复跑最终 12 项监督测试，均通过。

正向测试覆盖真实受控 EXE 的观察→纠正适配器，未进行 UAC、计划任务注册、真实 kernel provider 或提权监听→自动循环的完整验证。所有实际停止均为测试自己创建并持有精确身份的 EXE；没有操作用户 Codex/Claude 应用或 IFEO 注册表。

**2026-09-20：普通侧监听监督追加验证**

新增 6 项监督测试通过，独立审查方另行复跑通过。覆盖缺少部署不启动/不授权、全部停用后停止监督、重复服务拒绝、旧 epoch 不覆盖新服务、超时或取消仍占用实际 native worker 名额、阻塞验证结束后超时/取消/owner 替换不再 dispatch、一次性许可与绝对截止、心跳陈旧/结束立即降级和子任务随作用域取消。全量 273 项通过、26 项 ignored，clippy/fmt/diff 通过；日志 `.tools/guard-monitor-final-tests.log`。测试没有真实运行计划任务、提升权限或操作用户应用。当前仅合并待扫描通知，自动纠正和真实提权端到端尚未验收。

**环境**

- Windows 11 专业工作站版，10.0.26200，x64，普通用户令牌。
- Rust 1.98.1（48a229cea，2026-09-01），MSVC 工具 14.36.32532。
- Rust/rustup 位于项目 `.tools`，未修改系统 PATH；发行目标仍不包含 Rust 工具链。
- Claude MSIX：`Claude_2.2553.1.0_x64__pzs8sxrjxfjjc`，主 AppId `Claude`。
- 依赖由 `Cargo.lock` 固定；没有安装、启动或复用 sing-box。

**已通过**

| 验证 | 实际结果 |
|---|---|
| Workspace 构建 | 三个 crate，CLI 与 GUI-subsystem host 均构建成功 |
| 自动测试 | 7 项通过；另 1 项需要 Claude 安装的实机测试默认忽略，本机单独执行通过 |
| 普通进程创建 | 空参数、中文、空格、引号、尾反斜杠及 Unicode 参数从真实 child 回执逐项核对一致；cwd 一致 |
| 环境 | set 大小写匹配、空字符串、unset 经真实子进程验证，父进程环境未改变；含混补丁/NUL 被拒绝 |
| 进程身份 | PID、创建时间、SID、session、映像路径及文件身份一致；伪造创建时间不能停止进程，原进程保持存活 |
| 精确清理 | 使用确认身份和持有句柄停止本轮受控 helper，确认退出；没有按名称或进程树清理 |
| 调试创建候选 | DEBUG_ONLY_THIS_PROCESS → 初始调试事件 → 脱离；创建线程退出后 child 仍存活并写出回执 |
| 请求约束 | 过期请求、错误包身份不写回执；同一请求重复执行拒绝且不覆盖第一次回执 |
| Claude 包识别 | 从当前用户包登记解析 full-trust 主程序与文件系统虚拟化属性 |
| Claude 包内 helper | Rust host 在真实 Claude 包身份下运行；包内写入 LocalState 的独立测试目录，包外成功读回匹配 nonce 的数据 |
| 临时资源 | 最后回读未发现遗留的本轮 host 进程或 `AppProxyRust-M0-*` 临时目录 |

验证期间发现 PowerShell 无法读取仍由 Rust 以写模式持有的临时脚本，已改为关闭写句柄后调用，并由 Claude 实机测试覆盖该链路。正常错误输出不包含原始 PowerShell stderr 或环境值。

**重跑命令**

在 `rust` 目录执行（有常规 Rust 环境时可直接使用 cargo）：

```powershell
.\scripts\cargo.ps1 build --workspace --locked
.\scripts\cargo.ps1 test --workspace --locked
.\scripts\cargo.ps1 -CargoArgs @('clippy','--workspace','--all-targets','--locked','--','-D','warnings')
.\scripts\cargo.ps1 -CargoArgs @('fmt','--all','--','--check')
.\scripts\cargo.ps1 -CargoArgs @('test','-p','app-proxy-app','--test','process_contract','--locked','claude_package_helper','--','--ignored')
```

**尚未验证 / 尚未实现**

- 未写 IFEO 注册表；调试创建实验不能证明真实注册匹配、防递归、Electron 同 EXE 辅助进程或父进程句柄语义兼容。
- 未启动真实 Claude 主程序，未验证原版/分身 UI、登录、实际网络请求或模型对话。包内 helper 成功只是原生包上下文能力的证据。
- 未实现 ETW listener、持久化启动状态机、代理/订阅/一键安装或用户菜单。store、命名管道与 bootstrap coordinator 的后续验证见下方追加记录。
- `SpawnSpec`/调试后端为 M0 代码；当前普通身份核验不替代 M1 的实例归属、未决结果 journal 和生产停止策略。
- 当前终止接口仅用于受控测试或失败启动的清理，尚无面向用户的“先正常关闭窗口”操作。
- M0 EnvPatch 的显式变量名只支持 ASCII，变量值和继承环境支持 Unicode；这不等于完整模板/代理环境合成已实现。

真实 IFEO 受控夹具和 ETW 验证仍待完成；应用级接管必须在相关平台实验通过后接入。实现过程中若需改变已确认的产品行为，再回到设计讨论。

**配置模型与存储追加验证**

基础模型 13 项测试通过；单所有者存储 10 项 Windows 测试通过。模型拒绝未知字段、无效引用、受管参数/环境冲突与未管理原版的 IFEO 登记。存储测试覆盖版本冲突、备份、损坏/超大文件保留、机密引用、文件占用导致的替换失败、目录及文件 ACL、继承标志、junction 拒绝，以及权限变化时写秘密前拒绝。两个功能均独立审查、修复后复审通过。

当前节点类型仅手动 HTTP/SOCKS5。存储尚未接入日常菜单；配置恢复界面、运行 journal、实例启动和代理管理仍未完成。前述 M0 记录仍只代表实验范围。

**IPC 与协调进程追加验证**

已通过 4 项真实命名管道测试、2 项帧测试，以及协调进程的 2 项协议测试和 4 项跨进程测试。跨进程场景包括：六个 CLI 同时创建全新 store 并取得同一 PID/epoch；退出后重新启动仍使用同一 store 并读取新 revision；Unicode/空格/尾部分隔符路径；坏配置原样保留；最后一个请求结束后空闲 30 秒退出；独立 PowerShell 进程连续快速建立并关闭 20 次连接后仍由原 owner 提供服务。协议测试覆盖版本/store/session/epoch 拒绝和慢客户端隔离。

后台 host 使用固定 `serve --home` 入口，通过原生 `CreateProcessW` 禁止句柄继承。原先 std::Command 默认继承捕获输出的句柄，导致 CLI 调用方等待 host 退出才收到 EOF；竞启测试捕获了该问题，修复后 CLI 在 host 存活时即可返回。Rust stable 的相关限制可查 [CommandExt::inherit_handles](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html#tymethod.inherit_handles)。

本批只实现只读 status 和生命周期。写请求去重、运行 journal、应用启动恢复、事件缓冲和实际 Guard/代理任务仍待实现；不以 bootstrap 状态表示保护生效。

**模板与实例目录追加验证**

7 项模板测试覆盖原版清除继承目录变量、Codex/Claude 空白实例参数、环境变量模板、直连/代理、IPv6、单次路径变量展开、秘密引用与环境补丁限额。6 项目录测试覆盖原版不创建目录、空白创建、改名复用、移除登记保留数据、陌生目录/错误归属拒绝、根目录句柄保留、junction 拒绝，以及使用隔离 LocalState 夹具时不同 store 的命名空间分离。两项功能均独立审查通过。

2 项跨进程测试把污染的继承代理/实例目录变量送入独立 worker，经过真实模板合成与目录准备，再由 PowerShell 回执核验 argv，原生 Rust 子进程核验环境值。PowerShell/.NET 对空变量的观察可能与原生 API 不同，empty/unset 契约以原生回执判断。两个 helper 默认列为 ignored，由父测试显式选择并执行；不添加新的发行程序。此测试没有启动真实 Codex/Claude，不证明 Electron 全组件隔离、MSIX 包内目录可访问或实际网络经过代理。

**共享 sing-box 配置追加验证**

2026-09-19：纯配置生成器通过 4 项契约测试及独立审查。每个活动 profile 生成一个回环 HTTP 入口和指定上游出口；按稳定 UUID 排序、重复引用合并，改显示名/配置 revision 不改变内核配置。选中节点的密码只在生成时解析，不提供配置对象的 Debug/Display；无 direct/selector 出口，末尾规则拒绝未匹配流量，不设置系统代理。

显式运行 `singbox_process_contract` 通过，独立 reviewer 再次运行通过。实际 sing-box 1.14.1 完成生成配置的 `check`；一个新启动进程同时向本地 HTTP Basic 和 SOCKS5 认证夹具转发 CONNECT，两端均验证收到原始目标域名。测试增加的未匹配入口被拒绝；关闭 HTTP 上游后对应入口失败，另一个 SOCKS5 上游仍成功，未发生串线。仅证明配置有效及本地 TCP 转发，不证明外网 TLS 健康、真实应用流量、六种订阅协议或生产内核生命周期。

验证程序来自[官方 v1.14.1 发布](https://github.com/SagerNet/sing-box/releases/tag/v1.14.1)，存放于忽略的 `.tools/sing-box-1.14.1-validation/`，不是产品安装或发行内容。下载 zip 的 SHA256 与发布 API 的 digest 一致：`5197f16d492d93202dc623622149a6ed040f8eca263128f91d603f2b901baa89`。解压的 EXE SHA256 为 `b838de45bd0b2e6ddbed1977e4745622f7dffab3b293807ff4c6b1b640fed909`，libcronet.dll 为 `3217c6260fbca5f16072e0b79735742f40109a63bb0ff88fd6b96dd6b54a2928`；包内 LICENSE 保留。此版本尚未通过完整协议/网络验收，不能据此声称一键安装版本已经完成验收。

开发者在仓库根目录复跑：

```powershell
$env:APP_PROXY_TEST_SING_BOX = Join-Path (Get-Location) '.tools/sing-box-1.14.1-validation/extracted/sing-box-1.14.1-windows-amd64/sing-box.exe'
& ./rust/scripts/cargo.ps1 -CargoArgs @('test','-p','app-proxy-app','--test','singbox_process_contract','--','--ignored')
```

此环境变量只用于显式集成测试，不是用户选择内核路径的产品选项。常规 workspace 测试默认忽略该外部依赖测试；发行验收须显式运行。字段依据：[HTTP 入口](https://sing-box.sagernet.org/configuration/inbound/http/)、[HTTP 出口](https://sing-box.sagernet.org/configuration/outbound/http/)、[SOCKS 出口](https://sing-box.sagernet.org/configuration/outbound/socks/)、[路由动作](https://sing-box.sagernet.org/configuration/route/rule_action/)、[上游域名解析](https://sing-box.sagernet.org/configuration/shared/dial/#domain_resolver)。

**sing-box 程序自动发现与检查（2026-09-19）**

新增 `discover sing-box`，只查找程序文件并运行 version，不创建代理服务或接入运行中进程。探测 store 完整版本目录（按数值版本降序）、绝对 PATH、Scoop 实际 EXE 和 WinGet Links；跳过 staging、相对 PATH、同一文件重复候选及带配对 `.shim` 的转发程序。Scoop 特性依据其[官方 Shim 格式](https://github.com/ScoopInstaller/Shim)，不执行额外参数或提权转发器。

4 项单测覆盖版本格式、跨位数排序/暂存目录、坏程序跳过及子进程输出/超时/错误脱敏。真实 sing-box 1.14.1 集成已扩展到生产 discover/check、坏配置拒绝与 CLI JSON，并显式运行通过；原有双出口转发验证仍通过。全量 95 项通过，5 项顶层 ignored（新增一项被父测试显式执行的 probe child）；clippy/fmt 通过，独立 review 通过。

子进程输出最多 16 KiB、单次 3 秒、异步发现等待预算 20 秒；同步文件系统调用仍受 Windows I/O 超时约束，不能宣称网络盘发现有严格总时限。每个生成配置必须再次 check；version 成功不等于配置兼容、网络健康或内核已受管理。一键安装及生产生命周期尚未完成。

**HTTPS 代理健康检查（2026-09-19）**

`proxy_health::check` 从指定的回环 HTTP 入口完成 CONNECT 和 HTTPS 请求，使用 Windows TLS 证书链及主机名验证。HTTP 客户端禁用系统/环境代理自动发现，显式代理无 NO_PROXY 旁路，不跟随重定向或自动重试。只接受调用者指定的 2xx 状态（常用 200/204），完整响应最多 1 MiB、请求总超时 10 秒。输出只有状态、耗时或固定阶段/类别；不保留原始 reqwest 错误、URL、头或正文。没有修改系统证书库、系统代理或现有应用。

6 项本地测试通过，覆盖 CONNECT/TLS 成功、普通 HTTP 伪装、未知证书、错误主机名、重定向/错误状态、带与不带长度头的超大响应、连接及正文超时、错误隐私和污染代理环境的真实子进程。测试证书只注入夹具客户端；生产函数无关闭 TLS 验证参数。

真实 sing-box 1.14.1 集成显式通过：生产配置生成、程序发现/check、自建 core、HTTP 上游夹具以及实际 TLS 请求组成完整受控链路，返回 204；停止本次 core 后请求失败。独立 reviewer 复跑本地套件及真实集成通过，clippy/fmt 通过。此证据不表示生产 core 管理、启动前门禁、运行监控、目标应用网络行为已完成。

```powershell
$env:APP_PROXY_TEST_SING_BOX = Join-Path (Get-Location) '.tools/sing-box-1.14.1-validation/extracted/sing-box-1.14.1-windows-amd64/sing-box.exe'
& ./rust/scripts/cargo.ps1 -CargoArgs @('test','-p','app-proxy-app','--lib','real_singbox_carries','--locked','--','--ignored')
```

依赖行为依据：[reqwest ClientBuilder](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html)、[显式 Proxy](https://docs.rs/reqwest/0.13.5/reqwest/struct.Proxy.html)。锁定 reqwest 0.13.5，关闭默认 features，仅启用 native-tls；rcgen/tokio-rustls 仅为测试依赖。

本批全量 workspace 回归：101 项通过、6 项顶层 ignored；两个真实 sing-box 集成已各自显式执行，另外三个 ignored 为父测试启动的受控子进程，Claude 包测试按前述环境条件另行验收。

**共享 core 初始生命周期组件（2026-09-19）**

新增受保护 generation 与 runtime journal、原生 core 进程后端及 app 层 CoreManager。配置先写不可变候选并 check，再记录 Starting、自行创建隐藏 core、持久化完整进程身份、核验每个监听端口所属 PID，最后对本次所需 profile 执行 HTTPS 检查。重开 store 后仅依据持久化完整身份恢复；存活 core 可供所含 profile 子集复用，改名不重启。记录进程已退出时重建仍保留其全部 profile，不因单实例启动丢失其他入口。

4 项新状态测试与 1 项原生双栈测试通过：配置/目录句柄禁止并发替换，坏内容摘要与 profile→inbound 映射不一致拒绝，CAS/重开/Starting 未决保留，未知字段及陌生 store 记录拒绝；IPv4/IPv6 端口确由完整身份的进程持有。端口证据读取 Windows 原生 owner 表，依据 [MIB_TCPROW_OWNER_PID](https://learn.microsoft.com/en-us/windows/win32/api/tcpmib/ns-tcpmib-mib_tcprow_owner_pid) 和 [MIB_TCP6ROW_OWNER_PID](https://learn.microsoft.com/en-us/windows/win32/api/tcpmib/ns-tcpmib-mib_tcp6row_owner_pid)。

新增真实 sing-box 管理集成显式通过：拒绝并保留外部占用 listener；两个入口共用一个自建 core；实际关闭/重开 Store 后相同 PID/创建时间/generation 重用；三次 CONNECT/TLS 请求通过；改名不重启；运行探测失败保留 core；初启失败只停止本次 core；显式 stop；探测期间配置变化拒绝旧 Ready；进程退出后单 profile 请求重建仍恢复双入口。末尾进程退出/重建用受控健康回调隔离生命周期行为，不能当作额外 TLS 成功证据。测试不启动用户应用，也没有把现有外部服务认领为自有 core。

独立 review 修复了 Stopped 单元变体忽略未知字段、探测期间配置变更竞态、header 端口映射与真实 config 不一致；修复崩溃恢复只保留本次 profile 及恢复失败后丢失集合的问题；Down 状态持久保留 generation，测试覆盖端口占用失败、重开后的双入口恢复。全量 workspace 106 项通过、7 项顶层 ignored；本批管理集成另行显式运行，clippy/fmt 通过。

```powershell
$env:APP_PROXY_TEST_SING_BOX = Join-Path (Get-Location) '.tools/sing-box-1.14.1-validation/extracted/sing-box-1.14.1-windows-amd64/sing-box.exe'
& ./rust/scripts/cargo.ps1 -CargoArgs @('test','-p','app-proxy-app','--lib','managed_core_persists','--locked','--','--ignored')
```

本批尚未接入 coordinator RPC/CLI 启动入口或 idle 策略，CoreManager 的 state 返回持久化记录，不是持续监测状态。已存在不同运行配置时返回需确认，尚未执行换代/回滚。Starting 没有持久化进程身份时返回结果未定并阻止盲目重试，完整核对与维修入口待实现。启动许可屏障、持续监测/用户故障通知、一键安装及应用启动仍未完成；不把组件测试作为完整产品或 Guard 验收。

**共享 core 控制 RPC / CLI（2026-09-20）**

coordinator 新增 core 接纳、结果查询与进程/端口观察；CLI 为 `core start/stop/status/request`。每个写请求先在受保护目录持久化 Pending，返回快速应答，随后继续执行，即使应答发送失败也不取消。不可复制的执行令牌覆盖接纳至完成，任务中断后显示 Indeterminate；旧 epoch 的 Pending 不重新执行。完成记录保存历史结果，成功 Ready 不是未来健康保证。设置中的 test_url/expected_statuses 用于生产健康检查，没有额外手动 URL 选项。

新增 11 项测试通过：规范化/required 引用、持久化去重/跨 namespace 编号冲突、时钟回拨、7 天终态清理与未决保留、取消/重开、结果替换失败、真实管道并发与丢 ACK；两个实际 CLI→host 测试验证 stop 历史结果跨重启、未知 profile 不启动、旧 Pending 保持未知。此次真实 CLI 没有启动 sing-box；成功网络转发/生命周期证据沿用上一批独立显式集成，不能据此声称应用启动已完成。

状态查询对已记录身份重新查验进程及监听端口，不能因 runtime.json 写有 Running 就宣称网络正常。协调进程有活动内核或未决请求时不空闲退出。全量 workspace 117 项通过、7 项顶层 ignored（类别同前）；clippy/fmt 和独立复审通过。未知结果核对、重配置/回滚、安装提示、启动许可和持续通知仍待实现。

**sing-box 一键安装、进度与取消（2026-09-20）**

固定[官方 1.14.1 发布](https://github.com/SagerNet/sing-box/releases/tag/v1.14.1)，下载 zip 32,841,719 字节及摘要同前；EXE、libcronet.dll、LICENSE 各自校验大小与 SHA256。LICENSE 摘要为 `bb3805862b583aee73ad6f7805ec634747a37257a637a3069857843f05ea589c`。zip 8.6.0 只启用 deflate；解包严格匹配三个成员，不使用 archive 提供的路径直接写盘，拒绝路径别名、穿越、链接、额外文件及内容不符。依据 [zip 官方 API](https://docs.rs/zip/8.6.0/zip/)。

受保护 `.staging-UUID` 内完成写入、文件 pin、version/check 与来源回执后才发布完整版本目录。未知已有目录不覆盖，完整安装核验后复用；取消仅清理本暂存的固定文件。CoreInstallation 保有 store owner 的共享句柄及目录 pin，让解包、hash、probe 脱离配置互斥锁，又不会在 Store 对象释放后丢失独占归属。3 项平台默认测试及官方 ZIP 隔离集成经独立 reviewer 显式运行通过。

下载使用显式直连、HTTPS、可信发布域重定向白名单与精确包长，连接 10 秒、停滞读取 20 秒、总计 600 秒。2 项下载/合并单测覆盖 HTTP 错误、长度不足/超额/chunked、错误脱敏、并发共享失败及后续显式重试。初始 120 秒总时限的两次真实下载超时；curl 对照 25 秒仅下载约 3 MB，随后放宽总时限。修正测试中“只污染 CLI、未污染已运行 host”的覆盖缺口后，首次 install CLI 在无效 HTTP_PROXY/HTTPS_PROXY/ALL_PROXY 与空 NO_PROXY 下创建实际下载 host，真实官方下载安装成功，用时 407.18 秒。随后验证仅安装未启动 core/app、回执跨 host 重启及二次复用。该网络测试先于后续取消/调度增量；后续代码以以下取消/管道测试及全量回归验证。

交互 `core start` 仅在确定缺少程序时提示“安装并继续 / 返回”，失败可“重试 / 返回”；JSON/非终端不隐式下载。`core install` 是显式安装请求。Pending 返回阶段与字节数，下载中状态查询实测约 0.2 秒。Ctrl+C 提交持久化取消请求；另提供 `core cancel <原安装请求ID>`。迟到的安装完成不让已中断的原客户端继续启动。

3 项取消/容量测试覆盖预取消无下载、原请求重放、不可取消其他操作、leader 取消后其他授权请求继续、等待者取消不影响 leader、32 个普通后台任务上限下仍可重放和取消。真实 Windows PTY 中下载约 1.6 MiB 后发送 Ctrl+C，原请求 `a4411c32-a211-4227-b18d-e90d37c323f5` 持久化为 Cancelled，隔离验证目录 `.tools/installer-cancel-validation-cba252ea/` 没有 bin 目录，未启动应用或 core。

独立审查发现并修复长安装 handler 占满 16 个连接槽而饿死查询/取消：后台工作使用独立计数与 RAII 完成通知，短连接可继续接受控制；完成后重新计算 idle。1 项真实管道回归在 16 个受控阻塞安装任务下验证状态/取消可达，所有任务完成后 owner 正常空闲退出。最终独立增量复审、全量 workspace 126 项测试、clippy/fmt 通过；9 项顶层 ignored，其中本批官方 ZIP 与网络下载安装已显式执行，其他类别同前。没有把 HTTP/SOCKS5/安装证据当作六协议、完整发行、应用启动或 Guard 验收。

**手动代理配置与凭据（2026-09-20）**

新增 `proxy create/list/show/update/rename/remove/request`，支持手动 HTTP/SOCKS5、自动选择回环端口、稳定 ID/端口及实例绑定。更新要求显式选择认证或无认证，密码仅接受重定向 stdin；列表不显示用户名、密码或 secret ID。新增密码使用请求 UUID 作为不可变身份，在受保护目录中先同步临时文件，再无覆盖发布；intent 仅持有秘密引用与请求摘要。原始密码不进入 manifest、配置回执、CLI stdout/stderr。失败或移除不擅自删除已有秘密。

4 项新 core 规则测试覆盖更新/改名、两类引用保护、无效地址及密码、HTTP/SOCKS5 认证边界；5 项平台测试覆盖 staging 后重试、pending 两侧恢复、活动 generation 的更新/移除拒绝与改名免重启、已有秘密冲突/损坏保留、实际文件占用触发的恢复竞态。2 项真实 CLI→host 测试验证带密码创建、摘要脱敏、改名、显式清除认证、请求查询、重启读取、移除保留秘密、两个不同入口与实例引用阻止移除。

独立审查发现：已接受但未写入 manifest 的 profile 更新可在旧配置启动后恢复，绕过运行保护。现在 CoreManager 入口先恢复配置，平台进入 Starting 时在同一 store gate 内再次恢复并检查 candidate 当前性；占用未解除则不能启动，恢复后旧 candidate 被拒绝。第二项修复将协议认证可表达性校验共用于保存和 sing-box 编译，避免保存必然无法启动的认证。

独立复审通过；全量 workspace 137 项通过、9 项顶层 ignored，clippy/fmt 通过。两项真实 sing-box TLS/生命周期集成显式通过（隔离本地夹具），没有运行真实 Codex/Claude 或写 IFEO。本批不含运行中配置切换/回滚、交互密码框、订阅或应用启动；活动 generation 的网络修改仍返回需重配置。

**共享 core 手动上游重配置（2026-09-20）**

`proxy update` 对当前运行集合中的 HTTP/SOCKS5 profile 创建受保护候选和检查过的计划，先返回受影响 profile/绑定实例，再由交互确认（默认返回）、`--apply-to-running` 或 `core apply-update <计划ID>` 执行。确认以完整旧快照及精确原进程状态为条件；改动配置或替换计划后，旧确认失效。原程序被重新核对并用于切换及回滚，不自动改用发现到的其他版本。

journal 区分 Prepared、Switching、Committing、Committed、Restoring、Restored。启动或停止的普通入口在副作用前检查 barrier；运行阶段阻止其他配置写入。新配置通过目标出口健康检查后才写提交意图并提交 manifest；提交两侧中断可重复完成，不重复换代。切换失败恢复旧配置/原 generation；恢复健康检查优先旧目标出口，最多 4 个并发逐批探测，全部旧出口都有机会；只要至少一个仍可用就保留共享 core。所有入口继续核验 PID 归属。全部失败才记录旧配置保留但 core Down；应用不被终止或改为直连。

6 项平台测试覆盖计划前后配置不变/陈旧确认、两侧提交恢复、文件占用、active barrier、未知 Starting 不误报完成、旧 generation Down 保留、坏记录保留及请求摘要绑定。1 项服务综合回归覆盖准备回执丢失、原 Apply 中断、普通 Start/Stop/重复 Apply 的确定拒绝、恢复再次中断、最终解除相关未知回执、idle 可退出，以及原终态回执实际按保留期清理后仍能完成当前恢复请求。1 项有界探测回归验证前 4 个慢失败而第 5 个健康仍可恢复，最大并发为 4。

真实官方 sing-box 1.14.1 集成在隔离 store 中验证两个入口：候选准备不切换、只改变所编辑出口、失败恢复双旧路由、普通操作无法跨过恢复屏障、commit 文件锁恢复不再次重启、未知 Starting 不重放，以及旧集合部分故障仍可提交/恢复、全部恢复失败明确 Down。转发验证使用本地 HTTP CONNECT 夹具，说明路由与生命周期；生产仍调用已有经过 TLS/证书校验测试的 HTTPS probe，不能把本夹具称作新的 TLS 验收。

真实 CLI→host 集成验证 JSON 预览后退出码 5 且原内核存活、改名后拒绝旧确认、显式执行失败后旧配置保留/内核 Down、重复恢复终态不重新创建进程、无永久未决回执。两项真实集成显式运行通过，独立 reviewer 也分别复跑通过。全量 workspace 145 项通过、11 项顶层 ignored，clippy/fmt 通过；未启动用户 Codex/Claude 或写 IFEO。

当前重配置支持已有运行 profile 的手动上游编辑。活动集合扩容/移除、无身份 Starting 的进一步进程核对、应用启动许可及持续故障监控仍待实现；`core status` 显示最近计划/阶段，`core recover-update` 只执行已有 journal 支持的核对。不会以这些组件证据宣称整套代理/应用/Guard 完成。


**共享 core 代理集合扩容（2026-09-20）**

`core start` 请求包含当前共享内核缺少的代理时，创建原集合与请求集合的并集配置。候选通过原程序 `check` 后展示旧入口中断影响、新增入口及已保存实例绑定；默认返回，显式确认或 `--apply-to-running` 才切换。已有子集复用同一个精确进程。扩容保持旧端口，保存的 manifest 和 revision 不变；新增出口检查失败时恢复旧集合，原集合内至少一条路由可用且全部入口 PID 确认才报告恢复成功。

3 项新增平台测试覆盖集合约束、预览零切换、丢失准备应答恢复、提交 intent 重开、manifest 不变、禁止夹带配置改动/删除旧入口，以及上一批 Rust schema 1 编辑 journal/Prepared 回执读取后继续扩容。新 journal schema 2 区分编辑与扩容；schema 1 仅接受编辑。并无旧 TS 数据迁移。

真实 sing-box 1.14.1 扩容集成通过：先仅启动 A，再请求 B，准备时 A 保持原进程、B 未监听；B 探测失败恢复 A；再次确认扩容后 A/B 均转发，分别请求 A 或 B 均复用同一进程。既有真实更新/回滚/中断恢复集成也通过。真实 CLI 集成覆盖编辑与扩容两路：JSON 默认只预览，配置变更使旧确认失效，扩容 `--apply-to-running` 进入应用流程，失败输出旧配置保留及代理故障，并可查询/恢复终态而不重放进程创建。

独立审查与增量复审通过；审查方另行跑通 9 项平台测试及真实 core/CLI 测试。最终全量 workspace 148 项通过，12 项顶层 ignored；上述三项真实集成显式执行通过，clippy `-D warnings`、fmt 和 diff 检查通过。全部使用隔离临时 store 与测试代理；未启动真实 Codex/Claude、未修改 IFEO。实例启动、活动集合移除、未知 Starting 核对及 Guard 仍待后续实现。


**实例启动持久状态与 core 启动许可（2026-09-20）**

新增原子 `state/launch.json`，同时保存请求别名、阶段、确认身份及实际网络绑定。多来源的新请求指向同实例未决 attempt，不再发放执行授权；编号与配置/core 请求命名空间互斥。只在成功写入 SpawnRequested 后允许平台创建；原子替换失败保留前置状态。显式取消在创建前阻止继续推进，创建后仅保留意图，不终止应用或宣称未创建。恢复旧 epoch 的前置准备可记录失败；SpawnRequested/AwaitingIdentity 保留 Indeterminate，不因时间推移自动重放。

Confirmed 保留历史成功结果，实例占用直到完整身份只读观察确认退出才释放。只读观察不请求终止权限，PID 复用证明原身份已退出，访问失败或身份内容不符保持未知。真实测试子进程验证存活、伪造身份拒绝、精确退出和再次接纳；没有启动用户应用。

core 启动许可在 ReadyToSpawn 时随 journal 发布，核对 generation/入口映射；CoreManager 在生命周期 gate 下发布并核对精确内核及端口归属。停止或应用重配置必须在任何进程终止前检查许可；确认完成释放此许可但继续保留实例占用，Indeterminate 继续持有。真实 sing-box 竞争测试将许可发布与 stop 依次排队到同一 gate，验证许可发布后 stop/重配置被拒绝且原入口仍转发；已有子集可复用；取消前置 attempt 后可以继续扩容。

7 项新增平台测试通过：别名/编号命名空间、取消边界、旧 epoch 恢复与未知不超时、原子写失败及坏记录保留、核心许可约束、真实子进程观察，以及 7 天终态清理/时钟回拨/4096 条容量后重放与取消仍可用。workspace 154 项通过后又单独运行新增容量回归，累计 155 项默认行为已验证；13 项顶层 ignored 含由父测试显式调用的新增子进程 fixture。真实 core 扩展测试另行通过（7.87 秒），审查方独立运行原 6 项状态测试与真实 core 竞争测试通过，并复审最后的容量回归。clippy `-D warnings`、fmt 和 diff 检查通过。

本批是执行引擎的持久状态基础，不提供用户 launch 命令。尚需接入跨 store 物理资源预留、安装/模板/代理完整准备、普通/MSIX 执行、明确未创建时的失败终结、helper 撤销和未知结果核对；不以本批代替完整启动及 Guard 验收。

**跨 store 物理实例预留与一次性执行许可（2026-09-20）**

新增独立用户级资源目录，使用受保护原子 claim 与 Windows 内核文件锁。物理 EXE 别名获得同一个 key，原版与不同分身目录分开；MSIX 用稳定包定位而非版本路径。原子发布 registry 避免并发初始化半成品，陌生目录、坏记录及持锁期间变化均保留并拒绝覆盖。锁可跨线程移动，owner 退出释放锁后仍保留 SpawnRequested 占用，不能因此再启动。

本地 Ready→Spawn 原子记录随机 nonce 后只发放一次执行许可，并持有 store owner lease；全局授权消费许可、核对物理绑定并写 intent，平台创建仅接收此授权。创建前错误返回完整 owner/nonce 绑定的未创建证据；创建后任何错误均未知。独立 review 发现并修复原先只按 attempt UUID 关联导致证据混用的问题，回归覆盖跨 store 相同 UUID、nonce 篡改、重复派发及本地 store 生命周期。真实子进程测试发现 Windows DOS/verbatim 路径前缀不同，确认依据物理 image/SID/session，保存系统返回的实际绝对路径供之后完整身份核对。

7 项新增测试全部通过，包括四线程初始化、原生跨进程排他、owner 死亡后未知保留、真实创建确认、存活禁止释放、精确退出后重新预留。父测试显式运行新增 ignored 子进程 fixture，均使用隔离临时目录，无用户应用或默认全局目录变更。全量 workspace 162 项通过、14 项顶层 ignored；真实 sing-box 扩容与启动许可竞争另行通过（7.81 秒）。审查方独立复跑 7 项资源及 7 项启动状态测试通过，最终复审无阻塞。clippy `-D warnings`、fmt 和 diff 检查通过。

这批提供执行前的可组合约束，尚无生产 LaunchEngine/CLI 接入；外部手动进程识别、EXE 更新后的旧进程占用、MSIX 授权/回执及未知结果核对仍需后续实现。

**完整身份绑定的原生进程查询（2026-09-20）**

ToolHelp 快照只返回进程提示，WMI COM 查询仅接受同用户/同会话完整身份；查询期间保留只读进程句柄，前后核对创建时间、映像、SID/session 和存活，并检查 WMI CreationDate 微秒精度对应关系。不会按 PID 或父 PID 单独认领实例；不会把访问失败、空命令行或查询超时视为应用不存在。

命令行通过 Windows CommandLineToArgvW 解析，保留空参数、Unicode、引号和反斜杠，不派生 Debug/Serialize，不输出 provider 描述。枚举状态遵循 [IEnumWbemClassObject::Next](https://learn.microsoft.com/en-us/windows/win32/api/wbemcli/nf-wbemcli-ienumwbemclassobject-next)；参数和时间来源为 [Win32_Process](https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-process)。新增 windows 0.62.2 COM 绑定，接口与 VARIANT 在专用线程内自动释放；不创建 PowerShell 查询进程或读取跨进程 PEB。

5 项新增测试通过：DMTF 精度/时区/无效日期、Windows 参数边界、超时和取消后实际线程仍持 slot、异常退出释放 slot、真实测试子进程参数/父 PID 回执及伪造身份/退出拒绝。外层 5 秒预算不能强制终止 COM 调用；每个进程最多一个实际未结束查询，之后返回忙而不积累查询线程。原生 helper 由父测试显式运行、精确清理，未查询或关闭用户应用的命令行。

独立审查通过并复跑全部 5 项测试；最终全量 workspace 167 项通过、15 项顶层 ignored（包含上述父调用 helper），clippy `-D warnings`、fmt 和 diff 检查通过。仅交付查询平台层，实例归属判断、外部实例占用决策和 LaunchEngine/Guard 调用仍待实现。

**单进程与物理实例归属（2026-09-20）**

新增 InstanceTarget 的只读归属判断：完整身份核对后匹配安装映像，硬链接别名视为同安装，其他独立副本不按名称认领；分身目录按物理文件身份比较。分别返回进程角色（主/辅助/未知）与实例关系（目标/其他/未知），不提供终止权限。只管理分身时无分身目录参数的原版为其他实例；辅助进程未带目录、旧包未核对、参数不可读/重复/相对等保持未知。原版看到显式数据目录不直接排除，因为它可能是应用默认目录。

开关处理依据 [Chromium Windows CommandLine](https://raw.githubusercontent.com/chromium/chromium/main/base/command_line.cc) 的前缀、大小写、空白和终止符规则，对重复值与原始命令行特殊解析保守返回未知。外部路径只接受本地绝对磁盘路径，逐级以 OPEN_REPARSE_POINT 检查并保留父句柄，不进入 UNC、设备或目录链接。目录比较放入原 WMI 线程/5 秒预算及真实未完成槽，最后仍核对原生句柄。

4 项新增测试通过：开关边界和角色、物理目录/无参数原版排除、远端/设备/相对/reparse 拒绝、真实子进程身份与分身参数归属/硬链接等价。新增 helper 由父测试运行，fixture 只证明本产品识别链路，不证明实际 Electron 多开。扩展查询测试在归属收尾阶段精确退出自己的子进程，验证不返回过时结果；没有启动或关闭用户应用。

独立审查发现 verbatim 路径重建丢失前缀会把 `user-data.` 错认成 `user-data`；修复后真实两个不同 fileID 目录的回归通过，审查方独立复跑 4 项归属及此前 5 项查询测试通过。全量 workspace 171 项通过、16 项顶层 ignored；尾点修复后针对 4 项归属测试再通过，clippy `-D warnings`、fmt/diff 通过。全局占用扫描、旧 MSIX 身份核对、实际代理参数证据和 LaunchEngine/Guard 集成仍待续。

**普通 EXE 启动 CLI（2026-09-20）**

`launch` 接入已审查的执行引擎及持久 RPC，支持原请求查询、重放与取消。缺少内核在交互流程提示安装，扩容先准备具体影响再确认；JSON 模式仅返回待操作信息。仅当前前台、明确未派发的失败允许修复依赖后新建请求继续，历史终态重放不触发修复。复审发现并修复 Ctrl+C 在准备/应用期间丢失及修复后配置复核竞态：单次前台保留取消意图，继续请求的 revision 在服务端接纳与最终派发时均核对。协议 2.8 才接受带 revision 的请求，旧编号重放不改变原有前提。

新增真实 CLI 测试覆盖一次创建、同/新编号去重、冲突拒绝、owner 重启、晚取消保留应用、精确退出及实例移除后历史查询；无效 EXE 与离线代理失败均无直连回退。新增 engine 竞争测试覆盖接纳前与派发前 revision 改变，并验证 guarded 请求不能借用其他前提的未决 attempt。旧协议测试覆盖 2.6 普通启动与 2.7 带前提启动的发送前拒绝。

真实 sing-box 1.14.1 的 CLI 扩容/更新测试另行通过，新增 launch JSON 影响预览只生成计划、不重启共享进程，重放不替换计划。Windows PTY 手工测试在缺失内核安装提示处发送 Ctrl+C，命令立即退出，journal 只有原 CORE_BINARY_MISSING 失败，没有 dispatch 或新请求；未开始下载。全量 workspace 193 项通过、18 项顶层 ignored，最后旧协议扩展回归再通过，clippy/fmt 通过。

此批只覆盖普通隔离 EXE 夹具与真实内核依赖流程；未进行真实 Codex/Claude 运行验收、MSIX 生产创建或 IFEO 注册。没有实测在成功的真实内核切换中按 Ctrl+C；该边界本批依据共享取消意图实现与独立源码复审，后续端到端验收仍需覆盖。

**MSIX 一次性包请求与撤销（2026-09-20）**

新增平台请求模块，消费已有本地 dispatch / 全局 AuthorizedSpawn，保存完整 owner/attempt/epoch/nonce、启动 binding、目标文件、helper 文件和发行者进程身份。请求和状态先在不可消费的 staging 目录完整落盘，再无覆盖地发布到 attempt 目录；发布同步返回后最终目录确实不存在才能出具未创建证据。目录、父目录和文件均校验归属/ACL/重解析；请求摘要、目录物理身份及原 journal 绑定阻止混用回执。

helper 与撤销共用独占文件锁，覆盖最终能力核对、Consuming、创建和回执。Pending 可被持久撤销；Consuming 不因超时、helper 消失或重复调用而再次执行；Created 迟到取消保留原进程。创建前核对包 family/full name、helper 文件及 session，固定并复核目标映像；创建后通过保留的原生 child handle 核对完整包身份。身份或回执保存失败均保持未知。

独立 review 发现并修复 UTC 回拨延长授权、部分发布无法核对及遗漏 child 包身份的问题。新增发行 tick 与精确发行者存活检查，单调时间包括睡眠/休眠；UTC 回跳到原有效窗口也不能延长 20 秒授权，发行者退出后晚 helper 不再创建。当前请求不序列化任意可重用执行 permit；helper 只能消费一次有效 Pending。

8 项新增测试通过：撤销后重开/重放、过期与错包、gate 排他及 Consuming 不重试、nonce/物理目录绑定、真实夹具进程一次创建与晚取消、时钟回拨/旧发行者、request/state 实际占用写失败、创建后 child 校验或回执写失败保留未知。审查方独立复跑全部 8 项通过。包上下文在这些新测试中通过私有测试调用注入，子进程为真实普通 EXE；没有启动用户 Codex/Claude，也不把这些证据称为真实 MSIX 应用验收。生产 bridge/host/LaunchEngine 接入及真实包运行验证仍待下一批。

最终全量 workspace 201 项通过、19 项顶层 ignored；clippy -D warnings、fmt 和 diff 检查通过。

**MSIX 生产执行链与恢复（2026-09-20）**

CLI/RPC 共用的 LaunchEngine 现已为 MSIX 发布一次性请求，经现有 PowerShell bridge 在指定包内启动 app-proxy-host package-child。桥接完成不当作创建证据；只接受绑定原 dispatch 的 Created / NotCreated 回执。取消、22 秒等待截止和 owner 恢复通过同一请求 gate 撤销尚未消费的能力；Consuming、目录丢失或无法核对保持未知，不重放创建、不终止应用。

真实 Claude 包的文件系统虚拟化会隐藏默认 LocalAppData 下的外部 store。实测失败后，将请求放到该包 LocalState/AppProxyRust/<store UUID>/state，helper 只校验共享命名空间的受保护归属标记，不再打开外部 store。生产 coordinator 仍将回执与原 journal 的 store/attempt/epoch/nonce/binding 完整核对。未修改应用 LocalState 的 ACL 或采用其已有数据。

新增默认测试覆盖命名空间归属与复用、旧包同名映像候选、Pending/Created/Consuming/missing 的恢复以及同 attempt UUID 跨 store 回执拒绝。全量 workspace 205 项通过、20 项顶层 ignored；审查方独立复跑相关 15 项通过，clippy -D warnings、fmt/diff 通过。真实 Claude 生产 helper 的过期请求集成另行通过（24.13 秒），确认共享 NotCreated 回执及资源释放；测试在唯一临时 store 命名空间内执行并清理。没有通过该测试启动 Claude 应用，也未写 IFEO。

上述真实包测试验证 helper 激活与回执共享，不证明实际 Codex/Claude 应用启动、分身隔离或网络代理。旧版本目前按相同映像名称保守纳入候选，未核对的候选阻止启动；精确跨版本包识别及真实应用端到端验收仍待完成。
**实际 MSIX 分身与辅助祖先归属（2026-09-20）**

环境：Windows 11 专业工作站版 10.0.26200，Rust 1.98.1；Claude 2.2553.1.0 / Claude_pzs8sxrjxfjjc / AppId Claude，隔离存储；Codex 26.915.4065.0 / OpenAI.Codex_2p2nqsd0c76g0 / AppId App，非隔离存储。全部使用单独临时 store、直连、新建空白分身，未登记原版、未写 IFEO、未复制登录数据。

Claude 两分身实际主 PID 20448/45500，分别出现窗口，LocalState 自有命名空间下两个 user-data 均由应用写入 Preferences/Local State/Network 等文件；重复启动 A 返回原 attempt/PID。关闭窗口后 Claude 留在后台，随后只清理已核对创建时间、映像和父链的本次测试进程句柄。

Codex 首次启动被 INSTANCE_PROCESS_UNKNOWN 拒绝。只读核对发现原版主进程 32152 没有分身参数，其 renderer/gpu/utility 子进程未带 user-data-dir。修复只在已识别 Auxiliary 且无自身目录时查询存活祖先；同映像/SID/session、严格更早创建时间、最大 8 层、共享 5 秒 deadline/worker slot，每层保留原生句柄并于返回前复核。自己的目录无效、不明角色、父退出/身份不符不产生排除结论，也不增加终止或 IFEO 授权。

修复后 Codex 原版保持 PID 32152/原创建时间，分身主 PID 45480/42396 同时存在各自窗口。两个独立 user-data 均写入浏览器文件，两个 app-home 均写入各自 config.toml、installation_id 和 sqlite 状态；未读取文件秘密内容。重复启动 A 返回原 attempt/PID。验收后对本次分身尝试窗口关闭，再通过已核对且保留的测试进程句柄清理残留；原版仍存活。四个 Claude/Codex attempt 最终均 session_exited=true、resource_pending=false。

新增真实父子 fixture 回归覆盖原版祖先→Other、目标祖先→Target、保持 Auxiliary 角色、父退出拒绝及复用/跨 session 身份拒绝。全量 206 项通过、20 项顶层 ignored；独立审查通过并复跑归属 5 项、查询 6 项，clippy -D warnings、fmt/diff 通过。实机检查没有登录、发送消息或验证真实代理流量，也未完成包升级、Guard/IFEO 或发布验收；不同目录写入与并存不能单独证明全部账户隔离行为。
**精确进程正常关闭平台能力（2026-09-20）**

新增 stop_exact：要求普通用户、同 SID/session 和完整进程身份，禁止停止自身；打开后核对创建时间，PID 已复用返回 AlreadyExited 且不影响新进程。保留查询句柄，枚举目标窗口并在每次关闭消息前重新核对窗口 PID；消息总预算 1 秒、单窗最多 100ms。使用 [SendMessageTimeoutW](https://learn.microsoft.com/zh-cn/windows/win32/api/winuser/nf-winuser-sendmessagetimeoutw) 的 BLOCK/ABORTIFHUNG/ERRORONEXIT，关闭请求为 best effort，只有进程句柄已 signaled 才报告退出。正常关闭等待 1.5 秒；force=false 返回 StillRunning。force=true 才申请终止权限、核对完整身份并终止该句柄，最多再等待 3 秒；不遍历或终止子进程。

3 项真实隐藏窗口夹具测试通过：正常关闭/无关窗口保留/身份伪造及 PID 复用拒绝；窗口处理 WM_CLOSE 但保持存活时不误报退出、显式 force 后结束；窗口线程挂起时有限等待后精确结束。夹具只创建自己的不可见窗口，不关闭用户应用。独立 review 通过并复跑全部 3 项通过。全量 workspace 209 项通过、21 项顶层 ignored，最终 1.5 秒等待修订后定向测试再次通过；clippy -D warnings、fmt/diff 通过。

该 API 的调用方仍必须证明实例归属和停止授权；结果仅针对一个进程，不表示辅助进程已消失。窗口句柄是瞬时对象，关闭消息只是尽力请求，不能作为可信管理授权或退出回执。实例级停机、Guard 动作/限流和用户命令仍待接入。
**Guard 代理参数证据（2026-09-20）**

InstanceTarget.inspect_proxy 在同一次有界 WMI/完整身份及目录归属观测内核对代理参数，只有目标 Main 输出参数匹配结果。错误实例、辅助、旧映像/角色或目录未知均返回 Unknown；环境型模板不提供这项证据。端点必须为非零端口的 loopback 地址，不发起 DNS 或健康探测，不读取/输出进程环境。

参数复用 Chromium 开关解释，缺失/空代理、明确不同 literal 地址或已知不同协议、重复代理及直接/PAC/auto/bypass 冲突为 Mismatched；正确单一 HTTP endpoint 为 Matching。独立 review 发现裸 IP:port 被错误拒绝，按 [Chromium 官方代理说明](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/net/docs/proxy.md#http-proxy-scheme) 修复其默认 HTTP 语义。命名 host、复杂按协议映射、回退列表、未知 scheme 和歧义 argv 保留 Unknown，不扩大为通用代理解析器。

新增一项表格回归覆盖大小写、前缀、IPv4/IPv6、终止符、缺失/冲突/歧义和裸地址等价；真实子进程观测扩展验证匹配、不同端口、非目标/辅助 Unknown，且进程持续存活。全量 workspace 210 项通过、21 项顶层 ignored；解析修复后 6 项归属测试再通过，独立审查复跑同组通过，clippy -D warnings、fmt/diff 通过。

Matching 只描述启动参数，不证明代理健康、环境变量或所有流量均受代理。调用方需要为已确认会话使用原 endpoint，不能因为新配置值或健康失败把旧会话判为误启动。当前未启用自动停止、Guard 监听、纠正或 IFEO；这些仍需后续实现和验收。

**Guard 纠正执行服务（2026-09-20）**

LaunchEngine 新增内部 Guard 纠正入口，普通请求的 origin 标签不授予关闭权限。只接受当前配置中启用 Guard 且明确归属目标的主进程，其代理参数必须确定不匹配；未管理原版、正确代理和未知身份保持存活。先捕获已核对的辅助身份，再刷新主进程证据。持久化停止 nonce 后签发一次性能力，在全局物理资源锁内保存限流历史并调用精确关闭；实际退出回执确认后才继续代理准备。跨 store 保持 5 秒冷却、60 秒最多 3 次和时钟回退拒绝。辅助仅等待，不按名或树强杀。

新增 7 项引擎和 2 项平台回归：真实夹具关闭后代理失败不直连；原版/正确代理保留；普通 Guard 标签不能停止；停止前后配置变更/取消；辅助残留不杀不重启；超时后后台线程继续持锁且不会迟到启动；实际关闭但回执写失败后恢复不重放；停止许可持久化失败和别名拒绝；跨 store 限流/时钟回退。独立审查复跑 7 项引擎测试通过，平台 3 项 Guard 相关测试此前独立复跑通过。

全量 workspace 219 项通过、21 项顶层 ignored。clippy 检出新增状态扩大 RPC enum 内存和测试锁作用域问题后，将可选 Guard 状态改为 Box（JSON 不变），收紧测试锁作用域；clippy -D warnings 通过，最终 Guard 定向回归再验证。没有关闭用户应用或写系统 IFEO；本批是内部纠正执行服务，自动监听、ETW、授权、运行故障通知及 IFEO 接入仍待实现。

**Guard 只读实例扫描（2026-09-20）**

内部 observe_guard 按已登记实例生成只读证据。Confirmed 会话使用持久化的实际 network，不因后续绑定变更关闭应用；启动或恢复未决返回 Pending，精确身份不可读保持 Blocked。外部主进程按安装/数据物理身份及参数分为 Compliant/Correction；辅助不作主目标，未管理原版排除，多个主进程保守拒绝。Correction 只是建议，真正提交仍走已实现的持久授权及重新核对流程。

扫描完成后复核 manifest revision 和是否已有新未决启动，不用旧观察覆盖新配置。异步扫描限时 5 秒；安装解析的单槽 permit 随 blocking worker 持有，超时后仍未返回时新扫描报告 busy，不能累积解析线程。只读数据接口复用既有 owner/ACL/目录 pin 检查，所有 store 和 LocalState 路径都禁止创建，缺目录/错 owner 保留现场。

6 项真实夹具扫描回归覆盖：未管理原版、合规/误启动；没有进程时不创建目录；未决启动；已确认直连会话切换代理后的历史绑定保留；多个 main 和禁用；最终配置/新请求竞争；解析线程超时后仍占单槽与恢复。2 项目录回归覆盖本地和 LocalState 未创建命名空间、缺子目录、既有目录 pin 与 owner 保留。独立审查全部复跑通过。全量 workspace 227 项通过、21 项顶层 ignored，clippy -D warnings、fmt/diff 通过。

本批没有自动启用监听或发起停止，没有代理健康检查或系统注册变更。扫描证据并不表示 Guard active；授权/组件状态、周期扫描/ETW、用户界面和 IFEO 接入仍待实现。

**Guard CLI 与授权状态（2026-09-20）**

新增 guard status/enable/disable 和 GuardStatus RPC（开发版 2.9）。状态分别提供 desired、disabled/needs_authorization/blocked、监听和 IFEO 组件、只读扫描。没有安装验证证据时不产生 active；旧 manifest 中的任务/IFEO 登记只标记 unverified。只管理分身时 IFEO 不适用。查询只读且不会触发 UAC、注册或停止应用。

启停复用持久化 BindInstance 事务和现有回执查询，不改变目标状态时不增加 revision。直接网络不允许开启 Guard，已有 IFEO 登记不能只关闭配置。组件尚未就绪时 enable 保留配置回执、返回 requires_action/5；status 查询本身成功返回 0。实例创建/克隆/绑定后也检查保护状态，默认启用 Guard 的代理实例保存成功但尚未完成授权时返回 5；若后续状态查询失败保留已提交回执并返回 6，不把已保存配置描述成未执行。保护诊断不会自动升级为纠正动作。

昂贵状态扫描在 coordinator 内最多一个；繁忙时其他查询即时返回配置/组件状态及 GUARD_SCAN_BUSY，避免占满所有 RPC 槽。新客户端连接旧 minor 8 时在发送 GuardStatus 前拒绝。GuardScan 新增 Deserialize；独立 review 发现 serde tagged unit 变体忽略额外字段，已将 Disabled/Absent 改为空结构体变体，JSON 形状不变且未知字段拒绝。

新增 2 项真实 CLI、2 项 RPC 和 1 项严格 JSON 回归；独立复跑 17 项 Guard 相关测试通过。CLI 验证只分身范围、缺授权回执、重复禁用不改 revision、直连拒绝、原版 IFEO 需求、伪造的配置登记不产生 active、未清理 IFEO 时禁止禁用；仅写临时 store 元数据，没有系统 IFEO 或 UAC 操作。全量 workspace 232 项通过、21 项顶层 ignored；最后输出文本/状态字段调整后全部 10 项 CLI 契约再次通过，clippy -D warnings、fmt/diff 通过。

组件授权部署、后台监听、ETW、定时扫描纠正与 IFEO 注册执行仍未接入。本次入口明确暴露待完成状态，不能作为这些功能已完成的证据。

**原生 ETW 进程事件平台层（2026-09-20）**

新增固定 kernel process provider 的 ProcessStart 监听，不接受任意 provider 或执行动作。会话名绑定当前用户/store/session，每次拥有独立 epoch GUID；同名冲突只查询后拒绝，不接管。停止前按原 handle 查询并核对 GUID、完整名称和实时属性。消费者保留回调状态与日志结构直到 ProcessTrace 返回，协调超时、迟到 handle 发布和 CloseTrace；不请求 UAC 或更改组权限。

TDH 读取命名 ProcessID/ImageName 属性，不把事件头 PID 或固定 payload 偏移作为生产解析依据。输出只有 PID、映像短名和事件时间，并非实例身份或停止授权。1024 项队列按 PID/短名合并，溢出/解码失败及拥有会话的丢失统计变化要求全量补扫。EVENT_TRACE_LOGFILE.EventsLost 为未使用字段，实际统计读取 EVENT_TRACE_PROPERTIES。事件批次尚未接入管道，接入时必须另外满足 1 MiB 字节预算。

4 项默认测试覆盖名称/载荷限制、队列去重/溢出/丢失/结束、真实 TDH 元数据解码及回调截断处理、会话消失后的最终批次和异主拒绝。真实空会话控制测试显式通过：StartTrace/Query、同名冲突、错误 GUID 拒绝停止、拥有的会话被另一控制句柄结束后仍可取最终批次、同名重开及清理。真实 kernel provider 子进程事件测试也显式尝试，但在 EnableProcessTrace 返回 Win32 5（访问被拒绝）；没有采集到真实事件，未触发 UAC。该失败不能由空会话控制成功抵消。

独立 review 发现并修复 drain 因 session 已消失而遮蔽最终事件/结束原因的问题；复审通过并复跑全部 4 项默认测试。全量 workspace 236 项通过、24 项顶层 ignored；clippy -D warnings、fmt/diff 通过。logman 只读检查确认无 AppProxyRust-Process- 会话残留。此批不含提权安装、事件管道、自动扫描或 IFEO，完整权限与端到端事件采集仍需后续授权组件实测。

**单向事件管道平台层（2026-09-20）**

新增独立的事件管道，提权端只能发送，普通 coordinator 只能读取，没有命令接收或任意执行 RPC。公有创建/连接接口校验本方权限，双方按持有的进程句柄核对 SID、session、logon SID、预期权限和允许的映像文件身份。固定地址与普通 RPC 分离，显式 DACL 仅允许系统/管理员创建后续服务实例，当前 logon SID 只有通用读权限，拒绝远程客户端并用 first-instance 防止接管已有名字。部署层仍须提供经验证并保留的映像身份；管道认证本身不能证明安装可信。

严格消息绑定版本/store/epoch/递增 sequence，首次连接、序号缺口、任何丢失计数变化和结束都要求补扫；重复序号、同连接切换 epoch、无效字段或载荷拒绝。取消、超时和解析失败后连接不可再用。ETW 每次最多取 128 条，最长 Unicode 名称批次小于 1 MiB；原 1024 条队列分批保留顺序，全部取完后才发结束标记，不因线程结束丢弃剩余提示。

5 项新管道测试覆盖真实只读管道、读写连接/同名新实例拒绝、普通 token 不能伪装提权端、双方身份条件、帧往返/错 store/错协议、取消后失效，以及初连/断号/计数回退/重连/结束补扫。新增 ETW 回归验证满队列结束后的全部提示和最后一批结束标记。真实管道夹具两端均为普通进程，仅测试内部私有策略；没有生产权限绕过入口。审查方另只读核对本机普通/linked token 的 logon SID 相同，但这不替代实际提权端集成。

独立审查通过并复跑 5 项管道、5 项默认 ETW 测试；全量 workspace 242 项通过、24 项顶层 ignored，clippy -D warnings、fmt/diff 通过。没有 UAC、系统注册或用户应用操作。提权部署、任务、生产 listener 循环、Guard 自动触发和 IFEO 仍待连接，不能据本批宣称保护已生效。

**Guard 受保护 helper generation 平台层（2026-09-20）**

部署路径通过系统 Program Files Known Folder 自动定位，按 SID 摘要/store/generation 分隔，不接受用户指定目录。stage 只供提权 host 调用，核对真实普通 issuer 的完整身份、同 SID/session 和持有句柄的存活状态；拒绝使用其他管理员账户凭据完成这一安装。复制对象固定为本次 host 的映像，不从普通 manifest 接受可执行来源。

审查指出仅在提权后从当前进程映像路径重新取得 fileID 不足以绑定 UAC 前的文件。已新增普通前台 InstallerSource：固定同发行目录的 host、按根到叶保留全部父目录和源文件，独立记录 fileID/大小/SHA256；stage 强制匹配这份期望后才接触机器目录。前台后续接入必须持有此对象直到提权流程结束，并通过固定 UAC 参数传递期望，不能在提权侧重读可写请求来重新决定源文件。普通进程中的源文件与父目录改名/写入拒绝测试通过；释放 pin 后，同 fileID/同大小内容变更仍被原哈希期望拒绝。

目录/文件由管理员组拥有，创建时显式使用 protected DACL：系统和管理员完全控制，普通 Users 只读/执行。回读核对 owner、全部 ACE/权限/继承、目录类型、重解析点和文件硬链接；已有陌生对象不改 ACL、不覆盖。helper 内容同步并校验大小/hash/fileID，记录最后创建并同步，之后重新打开验证整个 generation。Deployment 持有目录、helper、记录句柄；普通 coordinator 的原映像也必须另外 pin/校验。生成代际文件不等于安装激活；没有写 current 指针、任务或 IFEO，失败残留保留给后续维护。

6 项新增默认测试覆盖真实 Windows 内存安全描述符的正反例、真实普通 owner 对象拒绝且保留内容、源文件锁与身份/hash、严格记录绑定、大小上限和普通调用无机器目录写入、真实 issuer 夹具退出后的失效。审查方独立复跑全部 6 项通过。安全描述符正例在内存中验证；没有以提权令牌在 Program Files 创建 generation，因此不能把这些测试称为真实提权部署/权限继承或激活验证。前台授权、任务、后台 listener、自动 Guard 和 IFEO 仍待接入。

最终全量 workspace 248 项通过、24 项顶层 ignored；来源绑定修正后重新全量运行通过，clippy -D warnings、fmt/diff 通过。提交前工作目录未进行 UAC、Program Files 写入、任务或 IFEO 注册。

**Guard 按需监听任务平台层（2026-09-20）**

新增原生 Task Scheduler COM 模块，操作本用户/store 的固定名字，action 仅来自已核验且持有的受保护 deployment；固定 event-listen/store/generation，禁止路径中的环境变量、任务参数替换和引号。提权注册使用 CREATE 和 DONT_ADD_PRINCIPAL_ACE，不覆盖已有任务；已有精确配置可以复用。任务管理员 owner，protected DACL 限系统/管理员维护、当前用户只读和执行；普通侧不能变更提权 action。

回读校验固定路径/参数/cwd、marker/URI、SID/交互登录/最高权限、principal/action context、唯一 Exec、无 trigger、V2 兼容和按需运行条件。真实 COM 测试发现 UserId getter 将 SID 变为用户名，已通过 Windows 账户查询还原 SID 后比较。V2 限制还防止持久维护配置绕过无 trigger 判断；回归在内存定义加入 maintenance period/deadline，确认 trigger 仍为零而校验拒绝。

run 只供已核对普通 coordinator 使用，显式当前 session>0，空替换参数；scheduler instance GUID 不能代替事件管道认证或 Guard active。remove_idle 要求提权、精确任务和无运行实例，删除后核对缺失；上层仍须先静止普通 run 请求，系统没有查询/删除的原子事务。没有强制停止 API，删除注册也不删除 helper。失败回读不会盲删未验证任务，注册结果未知必须交由后续集成事务核对。

4 项新增默认测试通过：固定名字/参数/路径限制、真实 Windows 安全描述符正负、本机 Task Scheduler 服务的未注册 COM definition 往返/缺失名字只读查询、主账户/权限/action/自动 trigger/maintenance/并行及超时条件变更拒绝。独立复审通过并复跑全部 4 项。全量 workspace 252 项通过、24 项 ignored；之后收紧替换语法、context/兼容级别和 maintenance 回归，全部 4 项再次通过，clippy -D warnings、fmt/diff 通过。

本批没有在系统创建、运行或删除任何任务，也未触发 UAC。真实任务 ACL 持久化、普通侧 RunEx、提权 token/session 和 helper 激活仍未验证；前台授权、event-listen host、登录任务及自动 Guard 尚未接入，不能称监听已经生效。

**Guard 生产监听入口与崩溃恢复（2026-09-20）**

任务 action 对应的真实 host `event-listen` 入口现已接入。必须使用提升权限令牌和受保护 deployment 自身映像；普通 coordinator 的源映像经过 fileID/size/hash 验证并持续持有。认证等待共享一个 30 秒截止，坏对端不重置截止，监听器错误直接结束；认证通过后才恢复/创建 ETW。空心跳每 250ms，满批次立即继续，最终事件结束先发送再退出；断开、超时或查询错误停止 trace，普通应用不受影响。

受保护 store 目录新增固定 session journal，显式管理员 owner/普通用户只读 ACL，保留拒绝其他写入/删除的句柄直到 trace Drop 完成，跨 generation 互斥。写入且同步新 epoch 早于 StartTrace，因此崩溃发生在 StartTrace 返回 handle 前也有恢复依据。恢复先核对严格记录绑定，再只读查询固定名称，GUID 必须为旧 epoch；Windows 返回 HistoricalContext handle 后，再核对该 handle 的名称/GUID/模式/无日志文件并停止。未知和损坏记录不改写，不碰不匹配会话；原会话停止后才替换记录，部分写入最多阻止后续启动。

新增 4 项监听循环测试和 3 项 journal 测试通过，覆盖绝对认证预算、坏对端后成功、监听错误、排空队列/发送最终失败、断开不继续 drain、查询失败不发送健康心跳、空心跳、普通权限拒绝、先持久化 epoch、恢复失败保留原记录、错误 scope/格式/超大记录及真实文件写入/改名互斥。journal 正向流程使用普通临时文件，ACL 正向仍仅有既有内存描述符测试，不替代真实提权目录写入。新增真实 host 契约检查任务参数能到达入口并因普通权限退出，非法 UUID 被解析器拒绝。

显式运行 `etw::tests::native_recovery_uses_persisted_epoch_and_query_returned_handle` 通过：本机创建自有空 trace，错误 epoch 拒绝且原会话仍可查询，正确 epoch 通过查询返回 handle 停止，重复恢复缺失成功，新 epoch 的后继会话不受旧记录影响。此测试模拟 controller 丢失 handle；未强杀 helper，也未启用 kernel provider。独立审查通过，并独立复跑 4 项监听、3 项 journal 和该原生恢复测试。

真正的提权 helper↔普通 coordinator 端到端、provider 采集、前台授权和自动 Guard 尚未完成；没有 UAC、任务注册/删除或用户应用操作。

最终全量 workspace 260 项通过、25 项顶层 ignored（新增真实空 trace 恢复项已单独显式通过）；真实 host 契约也经独立复跑。clippy -D warnings、fmt/diff 通过，logman 只读检查没有本产品残留 ETW 会话。提交主题 `feat(rust): run protected guard listeners with trace recovery`。

**Guard 前台监听组件授权安装（2026-09-20）**

交互式 guard enable 发现组件缺失后可选择安装，目录自动选择；JSON、非交互、status 和后台核验没有 UAC 路径。固定 ShellExecuteEx runas 调用同发行 host，完整 issuer 身份和预先捕获的来源期望经严格有界 hex ticket 传递，不读取可写请求重新选择来源。普通 issuer/来源验证先于机器目录操作，受保护安装锁串行化同 store；不可变 listener.json 在任务注册前持久化选定 generation。结果不明时保留记录；下一次显式授权核对并复用相同部署，来源改变或外国任务冲突不覆盖。退出码不是安装证据，普通侧必须回读保护目录与真实 task。

新增 3 项 installer 测试（严格参数/大小/危险字符往返、普通权限拒绝、真实自有 child 等待及退出码）、2 项 intent 记录测试（绑定/损坏/只读保持、缺失与访问失败区分）、1 项真实 host guard-install 普通令牌拒绝契约。测试发现文件缺失既可能返回原生 Win32 错误也可能为 std::io，已统一仅把 2/3 识别为未安装，不把访问拒绝、内容损坏或已登记 generation 丢失压成缺失。

独立审查发现扫描和组件核验分别 5 秒会超过 RPC 帧 5 秒期限。现同时等待两项观察，共享 3 秒绝对截止，实际未返回的原生 worker 仍持其单槽；新增真实命名管道 RPC + 两个永不完成的观察夹具，3.06 秒内返回 metadata/超时诊断，独立复跑通过。登记核验诊断为运行未确认，不声称已证明 helper 没有运行。

前台授权从 Tokio blocking worker 改为独立线程/oneshot，sticky Ctrl+C 可结束等待并返回 unknown，不能把丢弃 future 当作取消 Windows 操作。真实 Windows PTY 验证提示显示、选择 2 和提示阶段 Ctrl+C 均立即返回；只创建分身的 IFEO 仍不适用，配置 revision 和启用意图保留。测试使用从未执行的 fixture.exe，未启动用户应用；随后关闭测试实例 Guard，让其自有 coordinator 空闲退出。未选择安装选项，因此这些 PTY 结果不证明 UAC 对话框期间取消、真实管理员目录写入、任务 ACL 持久化、提权 helper 启动或注册未知后的实机恢复。

独立审查及修复复审通过。真实提权安装、普通监听监督、登录任务、自动 Guard 和 IFEO 仍待完成，不能据本批宣称保护已激活。

最终全量 workspace 267 项通过、26 项顶层 ignored；新增等待夹具 child 由父测试显式运行。clippy -D warnings、fmt/diff 通过。测试 coordinator PID 23544 已核对空闲退出；未保留运行的测试应用或提权 helper。提交主题 `feat(rust): authorize guard listener installation from foreground`。

**IFEO 注册与恢复平台层（2026-09-20）**

新增平台注册 API，仅接受已登记、绑定代理且启用 Guard 的原版，固定受保护 host generation 与精确 EXE/fileID。实际规则使用 64 位 IFEO 视图的 UseFilter/FilterFullPath，跨 store 物理映像重复所有者拒绝；没有生产 CLI 调用这些写入，只有分身时不创建原版注册。

受保护 HKLM 产品记录用 UUID 命名单个 REG_BINARY 发布 Installing/Active/Removing/Removed；持久 intent 先于过滤项，Debugger 最后启用。解除先保存 Removing，逐项验证自有内容后禁用 Debugger、删除自身过滤项，最后恢复共享父值。Unicode 同名父项使用 Windows ordinal 大小写比较，未完成登记也参与共享引用。最后一个参与者才恢复原值，第三方改动/过滤项、未知格式或保护权限不匹配均保留并报告。全局 mutex 核验管理员 owner 与精确权限；逐级 OPEN_LINK 拒绝注册表链接跳转。系统 ACL 的 CREATOR_OWNER 是继承占位，实际 owner 仍须可信。

最初实际测试发现 Windows 注册表事务返回 6801，已移除 TxR 依赖。独立审查发现首次记录子键创建后、内容写入前的空洞，以及无记录时认领既有同名过滤项问题：分别改为单值发布和提前拒绝。记录和父过滤项数量上限均在新增前检查，512 条现存记录仍可解除，避免产品写出自身无法读取的状态。

15 项专项测试全部通过并经独立复跑：原始 UseFilter 缺失/0/1 与 mitigation 保留，重复安装，第三方冲突和物理别名，Unicode 共享父项及新一轮备份，Installing/Removing 参与者，7 个安装和 5 个解除持久断点，首次 intent 发布前中断，陌生空过滤项拒绝、丢失父项不重建，注册表原生链接拒绝，容量满后解除，严格记录/路径/字符串，以及实际 HKLM 根 ACL 只读检查。所有写入限随机自有 HKCU 子树并清理；没有 UAC、实际 HKLM IFEO 修改或用户应用操作。

全量 workspace 293 项通过、26 项顶层 ignored（.tools/ifeo-rules-final-tests.log）；随后补数量上限检查与回归，最终 15 项专项、workspace clippy -D warnings、fmt/diff 通过。真实提权 mutex/ACL、规则匹配、启动入口、Electron 辅助 continuation 与完整应用链路仍待验收，不能称 IFEO 已启用。

**IFEO 只读入口准备（2026-09-20）**

新增原始命令行 capture/verify API，使用 GetCommandLineW 保留原始 UTF-16，不从已拆分 argv 重建。固定带引号 host、ifeo-entry、规范 UUID 及 ` -- ` 边界；不搜索后续分隔符、不允许外部 home/执行器选项。目标命令后缀原样保留，复用原生 CommandLineToArgvW 解析为 OsString，未配对 surrogate 参数不会被有损替换。数据类型不实现 Debug/Serialize，错误不含原始参数。

验证读取 protected IFEO 登记，要求当前普通 medium token、非零交互 session，拒绝 restricted/UIAccess/AppContainer；核对实际 host 路径/fileID、登记归属和目标路径，继续持有受保护 deployment 文件/目录与登记目标的只读 pin。新增 open_verified 让调用方保留 Deployment；已有 verify_registered 契约保持不变。派发前复核整个登记、实际 host 和目标。当前只读准备对象不是内核 IFEO 触发证明或 spawn/continuation 授权，后续 coordinator 仍须独立校验配置和允许的激活类型。

5 项新增测试及独立复跑通过：中文/空格/空参数/引号/尾反斜杠和原始 UTF-16 保留；固定边界/UUID/缺失目标/相对路径/大小/NUL 拒绝；目标路径与真实 host 归属、fileID 正反例；当前普通 token 及合成不支持上下文；原生 GetCommandLineW 与当前 argv 对照、未知登记拒绝。测试的正向路径绑定是合成元数据，未创建真实受保护安装，不能替代正向完整 verify 或原生 IFEO 重定向实测。

全量 workspace 300 项通过、26 项顶层 ignored（.tools/ifeo-entry-final-tests.log），workspace clippy -D warnings、fmt/diff 通过。没有 UAC、HKLM 写入或用户应用操作；host 模式、认证转交、cwd/环境/STARTUPINFO/继承句柄/Job、模板激活及防递归/辅助 continuation 仍待接入验收。
# 2026-09-20：保护启用与前台摘要修复

- 前台正常保存与代理验证不再提前打印请求 UUID；JSON 和失败/结果未确认时的查询编号保留。实例摘要使用名称，分别展示进程监听和登录自启动。
- 已核验安装不再依赖后台第一条心跳才登记登录入口；前台区分旧的缺失缓存、真实组件缺失和其他故障，启用后最多等待 40 秒覆盖后台现有重试周期。等待可取消，已保存的实例和自启动登记保留。
- 回归：4 项 Guard 前台状态测试通过；实例 CLI 合约 12 项通过、4 项环境专项忽略。实际用户实例补登记后已查询到 ETW active、login ready=true；本轮未启动真实应用做关闭测试，也未注销 Windows 测试登录触发。
