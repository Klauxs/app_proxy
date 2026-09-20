**Windows 原生适配**

Windows 平台层由 Rust 实现，现有代码仅提供平台经验；首版 MSIX 采用下文限定的 PowerShell 桥接。平台返回明确成功、明确失败或 Unknown 三态事实，不把权限不足、参数不可读或包消失压缩成不存在。

IFEO 注册、启动入口和防递归创建作为平台层独立模块，完整契约见 [第九章](D:/app_proxy/rust/docs/09-ifeo-launch-interception.md)。原始入口、CLI/Guard 和包内 helper 最终创建目标时均受该契约约束；普通 CreateProcess 成功不能证明创建的是目标而非 IFEO host。

**1. 进程创建与身份**

普通 EXE 使用 Windows 参数转义规则构造 argv，cwd/env 独立传递；不经过 cmd.exe，不接受 .cmd/.bat 作为普通应用。平台统一封装一个生产 spawn 接口；无 IFEO 时的普通创建可选 std::process::Command + Windows 扩展，需要精确句柄或 IFEO 调试创建时封装 CreateProcessW。M0 同时验证普通与防递归模式后确定组合，不维护两套业务启动流程。[Rust Windows 进程接口](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html)。

子进程句柄不继承不相关的文件/管道；关闭 launcher 不自动 kill 应用。控制台与 GUI subsystem、CREATE_NO_WINDOW 等标志区分 helper 和用户目标，不能用隐藏窗口选项误隐藏目标界面。spawn 成功只表示创建，随后必须查询身份。

ProcessIdentity 包含 PID、GetProcessTimes 创建时间、完整映像路径/文件身份、用户 SID 和 session。查询只申请必要权限；终止时另外申请终止权限并持有对应句柄再次核验，避免 PID 复用窗口。[Windows 进程权限](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights)。

进程枚举可用 Windows 原生枚举，命令行首版通过 WMI COM 查询 Win32_Process，保持当前数据来源而移除 PowerShell 子进程。WMI 查询在独立 COM 线程上限时执行，返回前复核创建时间；API 超时不等同底层查询已经停止。不能为了移除 WMI 改用未验收的跨进程 PEB 读取。图标/进程 API 的 FFI 封装建立 RAII 和有界缓冲检查。

当前平台查询使用 ToolHelp 快照给出 PID/父 PID/名称提示，不把这些提示作为实例归属。按完整身份读取命令行时仅接受同用户、同会话，保留不含终止权限的原生句柄，WMI 前后都复核其存活与完整身份，并核对 `CreationDate` 的微秒精度；父 PID 仍不是已验证祖先。WMI 和 COM 对象始终留在独立线程，外层 5 秒预算到期返回未知，每个进程最多一个实际未结束查询；调用者取消也不会提前释放该名额。命令行使用 Windows 参数解析，不输出原文；空值表示不可读，不能当作缺少代理参数。该查询层尚需由实例分类和 LaunchEngine/Guard 使用。

单进程归属判断使用精确安装映像和分身 `user-data` 目录文件身份，名称、PID 或父 PID 不构成管理授权。Chromium 参数按 Windows 开关前缀/大小写与终止符识别，重复、空数据目录或 `single-argument` 等不能可靠解释的输入返回未知。主进程、辅助进程、角色未知与实例关系分别表示；辅助进程没有目录参数时不凭父 PID 推断归属。只登记分身的目标不会认领无分身参数的原版；原版目标遇显式数据目录仍需核对，因为该目录可能就是默认目录。

已识别且没有自身目录参数的辅助进程可通过存活祖先补充只读归属：逐层核对同映像文件身份、SID/session 和严格更早的创建时间；最多 8 层，共享原 5 秒查询预算。每层持有原生句柄，递归返回后再次验证存活与完整身份。无法读取、父退出、PID 复用、角色不明或自身目录无效时不据此排除。继承归属不会把辅助进程提升为主进程，也不授予 Guard 或 IFEO 的执行权限。

从外部命令行读取的目录只进行有界本地比较：拒绝 UNC/设备/相对路径和远端磁盘，从根向叶使用不跟随 reparse 的句柄并保留父目录 pin，遇链接或不可读保持未知。保留 verbatim DOS 路径语义，不能把尾点目录归一为其他目录。该比较与 WMI 共用同一个查询预算和进程句柄，比较后进程退出或身份变化就拒绝结果。此接口不证明全系统没有其他实例，不验证应用所有网络流量，也不授权自动终止。

停止步骤为验证身份 → 获取并保留进程句柄 → 对属于该 PID 的窗口尝试正常关闭 → 等待 → 再验证 → 通过该句柄终止。窗口消息发送本身必须限时。禁止按进程名杀、直接 /T 杀整棵未验证进程树、为方便开启全局调试权限。

**2. MSIX 解析与稳定定位**

保存 family_name + app_id，每次启动重新查当前用户包登记和清单里的实际 Executable。Codex 的文件名可以是 ChatGPT.exe；品牌识别属于模板，不属于执行器。若匹配多个登记，返回歧义错误并要求选择安装，不能取第一个。

检查 EntryPoint 为 Windows.FullTrustApplication，读取具有正确 namespace 的文件虚拟化字段。AppContainer 不作为兼容承诺。包 EXE 原目录被用作 cwd 时跟随更新，用户显式 cwd 则保持。计划生成与执行之间包版本改变时重新解析/拒绝旧计划。

平台安装解析返回持有只读文件句柄的短期结果，由句柄取得规范化路径及卷/文件身份，用于识别大小写路径和硬链接。准备阶段拒绝文件写入/删除共享，最终创建前仍复核 locator 和包版本；确认创建后释放该安装句柄，不阻碍应用生命周期内的更新。此解析只证明当前文件/安装身份，不证明目标程序可启动、多开或代理已生效。[GetFinalPathNameByHandleW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew)。

首版用受控 PowerShell 5.1 脚本执行 Appx 解析/激活，参数通过结构化输入或参数绑定传入，绝不把用户数据拼进 `-Command`。PowerShell 可执行文件来自 SystemRoot 固定路径，-NoProfile、-NonInteractive；输出只有版本化 JSON DTO，限制大小和超时。后续原生包查询通过相同 contract tests 后替换这个适配器，不改变业务接口。

**3. 包上下文激活与回执**

当前已验证路径是 `Invoke-CommandInDesktopPackage -PreventBreakaway`。微软将该命令定位为调试工具，保证范围主要是包身份及虚拟化资源访问；不能把它描述成通用正式应用激活保证。[官方说明](https://learn.microsoft.com/en-us/powershell/module/appx/invoke-commandindesktoppackage?view=windowsserver2025-ps)。

Rust 首版保留这一路径，但将包内 Node helper 替换为同发行版本的 `app-proxy-host.exe package-child`。package-child 仅用于普通权限、指定包身份、指定 attempt，不接受客户端直接提供任意管理员执行动作。是否能在目标包上下文加载该原生 EXE 是 M0 必须实机证明的前置条件。

外部 coordinator 在共享根创建唯一 request，内容包含 protocol_version、attempt_id、nonce、expected_package、resolved_exe、cwd、args、EnvPatch、过期时间及配置摘要。文件独占创建，权限限制，路径和祖先拒绝重解析；回执包含 attempt/nonce、PID、创建时间、包身份及结果类别，原子写入同一可信目录。

request 消费采用独占 claim 文件/系统锁，helper 全程持有，确保一个 attempt 只有一个消费者。读取 request 后校验归属/版本/截止时间/包身份，再写 consuming 状态，构造环境并创建应用。回执路径由 request 路径派生，不接受请求指定任意输出位置。

迟到处理不能只依赖“进程启动前检查了一次 expires_at”。helper 在 spawn 临界区前再次验证执行能力，并与 coordinator 的 cancel/reconcile 使用同一 attempt gate：撤销方确认 helper 未进入 spawn，或等待 consuming/receipt，再决定是否允许新 attempt。已进入系统创建调用且结果未知时维持 Indeterminate。helper 崩溃在创建与写回执之间时，必须检查实例进程，无法确认则人工处理，不能按 TTL 自动放行。

初始 TTL 为 20 秒、外部等待 22 秒；到期只撤销尚未消费的能力。明确收到成功/失败且消除迟到风险后才能删除 request、claim 和 receipt。含 env/args 的文件不进诊断包，最终清理失败可记录路径 ID，不能打印内容。

平台请求实现补充：完整 request/state 先写入 helper 不接受的临时目录，再无覆盖地原子发布到 attempt 目录；最终目录不存在时，已结束的同步发布可以返回明确未创建证据。最终目录存在但无法读取时保留未知。请求同时绑定发行者完整身份、发行时系统 tick 和 UTC 截止时间；helper 必须核对发行者仍存活，并检查跨进程 tick 差值，防止 UTC 回拨或重启后复用旧能力。Windows tick 包括睡眠与休眠时间，见 [Windows Time](https://learn.microsoft.com/en-us/windows/win32/sysinfo/windows-time)。创建后的完整包版本通过原生 child handle 查询，校验失败或写回执失败保留 consuming，不终止应用也不再次消费。

此请求模块已有夹具验证；生产 `package-child` 入口、包激活桥接、LaunchEngine 接入和真实目标应用仍需后续验收，不能用平台请求测试代替 MSIX 完整启动证据。

**4. 存储虚拟化**

新实例先分配稳定 StorageLocation：普通目录或关闭写虚拟化的包采用 store；其他包采用 `%LOCALAPPDATA%\Packages\<family>\LocalState\AppProxyRust\<namespace>`。namespace 由 store_id 稳定派生，首次使用创建本产品专属归属标记。独立任务、pipe、ETW session 和 cache 同样使用 AppProxyRust 命名空间，不扫描/导入旧工具目录。

一旦已有分身目录，不因更新后清单属性变化自动换位置。包身份不变时继续定位同一数据位置，并验证包内可访问性；不满足则停在 storage 阶段。将来显式变更数据位置必须要求实例退出。原版启动不设置独立目录。

通过已打开文件句柄解析物理路径，验证包外 Explorer/普通用户进程是否能访问；不能以包内 Test-Path 成功证明快捷方式图标在包外可读。所有文件副作用都检查归属、路径穿越、reparse point 和同卷替换条件。路径预检查不被宣称能彻底防住同用户并发替换，关键写入结合句柄和最终路径复核。

**5. ETW listener 与权限分离**

普通 coordinator 负责判断和启动；提权 listener 只消费 kernel process provider 并发送事件。它不接受任意文件写入、执行程序、终止进程或变更目标环境的 RPC。提供的数据为 protocol version、epoch、sequence、PID、映像短名、事件时间；创建时间/用户/session/参数由普通侧重新查询。

使用当前用户/store/session 绑定的固定 ETW session 名，创建前检查同名 session 的 GUID/属性，不抢占别人的 session。StartTrace/EnableTrace/OpenTrace/ProcessTrace/CloseTrace 及 TDH 解码封装在专用线程；丢事件、回调异常、缓冲区超限都转为 degraded 并触发补漏扫描。低流量刷新策略在 Rust 实现后实测，不声称换语言自动降低 ETW 的系统刷新延迟。[StartTraceW](https://learn.microsoft.com/en-us/windows/win32/api/evntrace/nf-evntrace-starttracew)。

安装 helper 至管理员拥有且普通用户不可写的 Program Files 子目录，任务 action 指向固定受保护文件；普通用户仅有读取/执行及任务读取/运行权限。更新和移除必须前台 UAC，不由后台循环发起。首版保持“同一用户具管理员成员身份后提升”的现有安装限制；标准账户使用其他管理员凭据的场景需另行设计，不能认为已支持。

安装时固化 user SID、store ID、helper hash 和协议版本；提权程序不从用户可写 manifest 加载可执行路径或任意插件。受保护复制内容与 hash 校验在提权侧完成，避免验证后替换源文件的竞态。首版不开放从任意 URL 自动更新提权 helper。

部署平台采用不可变 generation：固定位置为系统 Known Folder Program Files 下的 AppProxyRust/Guard/用户摘要/store/generation，不接受手动安装目录。普通前台先固定同发行目录的 host 文件及所有父目录，记录 fileID、大小和 SHA256，并持续持有句柄直到提权安装结束；这些预期值通过固定 UAC 参数传递，不在提权侧从可写请求重新计算。提权侧只能复制自身映像路径对应且匹配该预期的文件，并验证仍存活的普通请求进程属于同一 SID/session。

每代 helper 与记录由管理员拥有，显式受保护 DACL 仅授予系统/管理员完全控制，普通 Users 只读/执行；不修补或覆盖陌生目录的 ACL。新 generation 和文件独占创建，内容同步后回读大小/hash/fileID/记录绑定，并保留验证句柄。目录、重解析点及文件硬链接异常拒绝。此阶段不切换任务或 IFEO；失败残留不作为有效安装，也不自动递归清除。任务注册事务必须在后续另行核验，不能用 generation 存在替代 Guard active。

前台监听安装固定调用同发行目录的 host `guard-install --ticket <hex>`。ticket 为有界严格 JSON 的小写十六进制编码，仅含版本/store/普通 issuer 完整身份/UAC 前来源期望，不含写入位置或任意执行命令。ShellExecuteEx 使用固定 runas、NOASYNC、NOCLOSEPROCESS 及隐藏 helper 窗口；操作系统安全提示仍显示。只在交互式 Guard 启用流程中用户选择安装后调用，不从查询或后台循环发起。[ShellExecuteEx 标志契约](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-shellexecuteinfow)。

提权侧在验证 issuer 与来源后持有受保护 store 的独占安装锁。`listener.json` 记录选定 generation，必须同步并回读后才注册任务；后续显式授权复用该 generation，精确来源不同、损坏记录及陌生任务均报错，不另选 action 覆盖。注册失败或回执丢失保留意图；普通侧仅以保护目录/内容与实际任务回读确认安装，不以 helper 退出码代替证据。此固定记录不表示运行就绪。

UAC 等待放在独立前台线程；Ctrl+C 可以结束等待并让普通 issuer 退出，尚未开始的迟到 stage/register 会因 issuer 失效被拒绝；已经提交的注册仍可能完成，故结果为未知而非已回滚。helper 启动后的进程等待预算为 60 秒，超时只保留证据和来源 pin，不终止 helper、不自动再执行。此预算不包括用户在 Windows 授权界面的思考时间。实际 UAC 取消和提权安装端到端必须实机验证，普通令牌测试不能替代。

普通 coordinator 的监听监督随服务启动，在有启用 Guard 的实例时核验不可变部署、当前普通映像和实际任务，再按当前 session 请求运行并认证事件连接。后台不发起 UAC；准备和连接各 5 秒，失败 30 秒后重试，只有一个实际未返回的 native worker。准备的异步等待持有一次性撤销门；在系统运行调用前同时复核服务 epoch、撤销状态与绝对截止，已进入调用的不声称撤销。停用全部 Guard 或服务退出后释放连接与 pins，终止自身读取任务。

收到已认证批次才报告 ETW 连接，结束批次立即降级，5 秒无新批次视为心跳过期。读取独占任务不会为定时器取消一半的帧。初连、事件提示和丢失合并为一个待扫描通知；连接正常每 30 秒补扫，已验证组件但断线时每 2 秒补扫。断线本身不关闭应用，误启动纠正仍按同一归属/参数政策执行。

自动消费者只排列已登记且启用 Guard 的实例，每实例最多一个排队位置、一个全局扫描任务，延迟实例轮转到队尾。未知结果最多三次短间隔核对，之后每 30 秒重查，事件风暴不取消退避。服务 owner、授权 Arc 或 manifest revision 变化时取消旧扫描并清队列；结果接纳时再次持有授权门检查，随后同步提交持久纠正请求。实际关闭与启动仍由 LaunchEngine 复核配置、身份、预留与持久限流。已接纳请求按原请求规则收尾，停止扫描不谎称撤销既有系统副作用。

状态同时要求有效监听授权和当前 revision 的后台扫描证据；40 秒未更新的证据降级。本次只读查询发现未知、待纠正或未决启动时优先降低状态，不能用旧 Ready 覆盖。进程关闭后仍保留最近一次实际纠正的失败诊断，秒级时间戳并列时保守保留失败，不凭 origin 标签认定纠正。真实 UAC→任务→ETW→自动扫描完整链路尚待实机验收；普通测试覆盖调度、状态、未授权循环与受控进程观察/纠正适配。

**6. 命名管道协议边界**

受保护 host 的 `event-listen --store UUID --generation UUID` 入口先核验提升权限、deployment 和自身映像，持有普通 coordinator 映像的身份/hash pin 后创建事件管道。在统一 30 秒截止前等待普通 coordinator 认证；陌生连接不会延长截止。只有认证通过才恢复/开启 ETW。每 250ms 发送提示或空心跳，满批次继续排空，结束标记在最后一批发送。管道断开/写入超时/查询错误时退出并停止自有 trace，用户应用不受影响。此入口没有接收控制命令、启动应用或终止应用的能力。

ETW 所属记录固定保存在受保护 store 目录的 `events-<session>.json`，跨 helper generation 共享。写入句柄拒绝其他写入和删除，并保留至 trace 停止后。新 epoch 必须在 StartTrace 前同步到磁盘；崩溃后只在持有相同独占句柄、记录绑定 SID/store/session 且旧 epoch 与查询返回 GUID 一致时恢复。查询返回的 WNODE_HEADER.HistoricalContext 是会话 handle；再用该 handle 核对名称/GUID/实时模式/无日志文件，之后才停止。未知会话、损坏记录和身份冲突均保留并报错，不按名称直接删除。[Windows WNODE_HEADER 契约](https://learn.microsoft.com/en-us/windows/win32/etw/wnode-header)。旧 trace 停止后才写下一 epoch，部分写入不能丢失一个仍存活 trace 的所有权记录。

普通 coordinator 管道和提权事件管道分开。两者都设置显式 DACL、拒绝远程客户端、限定用户/logon SID 和 session；客户端验证 server PID、令牌和映像身份，服务端验证连接方，nonce 不是身份校验的替代品。

不能使用默认 named pipe DACL，也不能无意授予普通客户端创建同名 pipe 实例的权限。监听管道采用 first-instance 检测防占位冒充；发生冲突返回错误，禁止连接未经验证的端点。[微软管道权限说明](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)。

同用户/同权限恶意进程不构成本工具能提供的隔离边界；重点是避免把普通请求转换成额外管理员能力。高权限进程只提供固定事件能力，校验失败时 fail closed。

**7. 快捷方式与任务**

通过 Shell Link COM 创建 `.lnk`，目标是安装中的固定 `app-proxy-host.exe`，参数为 launch + instance ID + store locator。名字采用可读实例名加短 ID；用户改名可更新显示，但数据 ID 和 target 不变。图标从实际 EXE 完整提取到持久 cache，按内容 hash 命名，必要时调用 SHChangeNotify。

原生平台已实现内存编码/回读、同目录原子不覆盖发布，以及按文件身份、内容 hash 和目标参数核验后使用同一句柄删除。发布/删除期间固定输出目录链，拒绝重解析点、硬链接和额外启动标志；不调用 Resolve 或运行链接。[Shell Link 接口](https://learn.microsoft.com/en-us/windows/win32/shell/links)。

Store 集成记录先保存意图，再准备不可直接启动的 `.app-proxy-stage-*.tmp`，持久化其 fileID/hash 后才通过同一已核验句柄改名为 `.lnk`，禁止覆盖迟到的占位文件。[FILE_RENAME_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info)。打开 store 和查询只读状态不恢复外部操作；显式重试同一请求时核对临时文件/目标和 manifest，保留用户修改、身份替换和未知结果。文件已发布而 manifest 尚未提交时合并最新配置，已完成请求仅重放历史回执。临时文件创建后、身份落盘前中断可能留下未知 `.tmp`，不按名字认领或清理。

有效入口的归属记录持续保留，解除后的历史请求保留七天；新增前按所有活动记录的完整终态预留 8 MiB 内的空间，记录上限不阻止已有入口清理。接受意图前先恢复配置请求，同一实例尚有快捷方式或未决操作时拒绝删除实例。

coordinator RPC、shortcut CLI 和管理实例菜单已接入。客户端仅提交实例、动作、请求编号和版本条件；host 固定为当前发行目录中的 app-proxy-host.exe，Desktop 由当前用户 KnownFolder 解析，资源查询/图标提取在配置锁外完成。新请求回锁后先重放，再复核版本、缓存图标并执行持久事务。单个实际 worker 持有工作槽直至结束，断线不撤销已接受工作，查询不占该槽。实例查询优先展示待删除请求；删除携带已显示的 create ID，在同一锁内核对，避免同版本下先取消旧创建又接受新创建时误删。真实桌面 CLI 和菜单创建/回读/删除通过，真实 Shell 点击仍待验证。

host 的 `launch --notify` 已复用 CLI/menu 启动流程，来源为 Shortcut。正常成功不打开控制台；确定需要前台依赖操作后才打开中文窗口，展示影响/安装与返回选择。控制台标准句柄显式绑定 CONIN$/CONOUT$，离开时恢复原句柄；分配后、首个提示前同步注册 Ctrl+C，因为 [AllocConsole 会重置控制处理器](https://learn.microsoft.com/en-us/windows/console/setconsolectrlhandler)。失败和未知结果显示系统消息框，含已有请求编号及数据目录；不通过关闭窗口推断已接受操作已撤销。无 notify 不弹窗、不执行交互安装。先只读验证 store 归属，丢失/损坏数据目录不会被静默重建。

图标读取使用 [LoadLibraryExW 资源映射](https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-loadlibraryexw)，不执行目标代码。保留首个图标组的全部尺寸和原始图像载荷，ICO 存到受保护 state 的 `icon-<sha256>.ico`，已有内容不一致则保留并报冲突。源 EXE 禁止写/删，映射前后检查规范路径的文件身份；不要求 WindowsApps 祖先目录列举权限。不承诺源目录被并发替换又恢复时的原子快照，图标仅用于展示，不能作为启动/删除授权证据。输出目录仍完整固定。

保存本工具创建的链接、task 名及预期 target/args。删除前核验仍属于本工具，用户已修改目标则保留并报告冲突。原有应用快捷方式不覆盖。

coordinator 登录任务在 Guard desired enabled 且授权已完成时安装，以普通权限启动 serve；事件任务按需启动提权 listener，无需第二个独立登录触发器。任务验证涵盖 owner SID、RunLevel、action 路径、参数和协议版本，不仅看名称。

登录任务平台现已实现，尚未接入 Guard 启用的持久事务。Prepared 从已核验监听组件和同发行版 coordinator 映像派生固定路径；调用方须在注册前持久保存 Registration。固定 action 为 `serve --home <原数据目录> --expected-store <UUID>`，Windows 参数转义保留空格和尾反斜线，拒绝 `%`/`$(` 替换形式。host 在 store 锁内、恢复任何未完成配置前核对 UUID；目录缺失不创建，目录改绑另一 store 时不恢复其配置。注册不自动执行任务，只创建缺失项或复用完整相同项；现存冲突保留。读取和空闲解除只依赖原归属记录，不要求原 host、store 或提权监听器仍存在，不调用 Stop。

登录任务限定当前用户的唯一启用 LogonTrigger、无延迟/重复/起止时间窗口，使用 LUA 和 InteractiveToken；任务 owner 为当前 SID，当前 SID/SYSTEM/Administrators 可维护。事件任务仍由 Administrators 拥有，普通用户仅读取/运行；两个角色的 ACL 和定义不能互换。注册依据 [Task Scheduler 权限上下文](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks) 和 [LogonTrigger 属性](https://learn.microsoft.com/en-us/windows/win32/taskschd/logontrigger)，并以真实 Windows 注册/回读作为兼容性证据。Task Scheduler 实际会将 URI 写为任务路径，两种任务均精确要求该 URI；协议角色由 Data 标记、SID/store/generation 由主身份、固定名称和完整 action 共同核对，不依赖自定义 URI 被保留。

事件任务平台使用固定 SID 摘要/store 名称，只创建缺失任务；已有精确匹配的注册复用，任何不同配置报冲突，不自动覆盖。action 从已持有的受保护 deployment 派生，只有固定 event-listen 与 store/generation UUID；路径拒绝环境或任务参数替换语法。普通用户仅有读取和运行权限，管理员与系统可维护，注册时禁止自动添加 principal 的写权限。回读同时核对任务 ACL、归属标记、真实账户 SID、V2 兼容级别、principal/action context、唯一 action、无触发器和运行条件。UserId getter 可能返回账户名，必须解析为 SID 比较。

普通 coordinator 只能以当前正数 session 请求 RunEx，不提供替换参数；返回的 scheduler instance GUID 不证明 helper 已运行，仍需认证事件管道。删除前必须先由集成事务停止本工具的普通 run 请求，再核对同一任务无运行实例；查询与删除不构成 Windows 提供的原子锁。此模块不强制终止实例、不自动删除 helper，也不将注册成功报告为保护 active。

**8. 分发与升级**

首版继续支持固定目录便携安装，不宣称移动目录后已有入口自动修复。release 包包括 CLI、host 和版本化 MSIX 桥接，不携带 sing-box。运行时一键安装的内核独立记录来源、版本和许可证。新版本替换整个发行目录中的应用文件需先停止 coordinator/listener 或采用 side-by-side 安装；不在运行中覆盖同名 EXE。

提供 `integration repair`：核验新工具位置、修复本工具快捷方式及普通任务；提权 listener 需要更新时提示前台授权。数据根和实例 ID 保持。只有 coordinator 管理任务已停止且无未决 attempt 才允许版本切换；用户应用可否继续运行取决于是否能继承观测，不在首版升级流程中假定热接管。
