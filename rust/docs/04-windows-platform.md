**Windows 原生适配**

Windows 平台层由 Rust 实现，现有代码仅提供平台经验；首版 MSIX 采用下文限定的 PowerShell 桥接。平台返回明确成功、明确失败或 Unknown 三态事实，不把权限不足、参数不可读或包消失压缩成不存在。

IFEO 注册、启动入口和防递归创建作为平台层独立模块，完整契约见 [第九章](D:/app_proxy/rust/docs/09-ifeo-launch-interception.md)。原始入口、CLI/Guard 和包内 helper 最终创建目标时均受该契约约束；普通 CreateProcess 成功不能证明创建的是目标而非 IFEO host。

**1. 进程创建与身份**

普通 EXE 使用 Windows 参数转义规则构造 argv，cwd/env 独立传递；不经过 cmd.exe，不接受 .cmd/.bat 作为普通应用。平台统一封装一个生产 spawn 接口；无 IFEO 时的普通创建可选 std::process::Command + Windows 扩展，需要精确句柄或 IFEO 调试创建时封装 CreateProcessW。M0 同时验证普通与防递归模式后确定组合，不维护两套业务启动流程。[Rust Windows 进程接口](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html)。

子进程句柄不继承不相关的文件/管道；关闭 launcher 不自动 kill 应用。控制台与 GUI subsystem、CREATE_NO_WINDOW 等标志区分 helper 和用户目标，不能用隐藏窗口选项误隐藏目标界面。spawn 成功只表示创建，随后必须查询身份。

ProcessIdentity 包含 PID、GetProcessTimes 创建时间、完整映像路径/文件身份、用户 SID 和 session。查询只申请必要权限；终止时另外申请终止权限并持有对应句柄再次核验，避免 PID 复用窗口。[Windows 进程权限](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights)。

进程枚举可用 Windows 原生枚举，命令行首版通过 WMI COM 查询 Win32_Process，保持当前数据来源而移除 PowerShell 子进程。WMI 查询在独立 COM 线程上限时执行，返回前复核创建时间；API 超时不等同底层查询已经停止。不能为了移除 WMI 改用未验收的跨进程 PEB 读取。图标/进程 API 的 FFI 封装建立 RAII 和有界缓冲检查。

停止步骤为验证身份 → 获取并保留进程句柄 → 对属于该 PID 的窗口尝试正常关闭 → 等待 → 再验证 → 通过该句柄终止。窗口消息发送本身必须限时。禁止按进程名杀、直接 /T 杀整棵未验证进程树、为方便开启全局调试权限。

**2. MSIX 解析与稳定定位**

保存 family_name + app_id，每次启动重新查当前用户包登记和清单里的实际 Executable。Codex 的文件名可以是 ChatGPT.exe；品牌识别属于模板，不属于执行器。若匹配多个登记，返回歧义错误并要求选择安装，不能取第一个。

检查 EntryPoint 为 Windows.FullTrustApplication，读取具有正确 namespace 的文件虚拟化字段。AppContainer 不作为兼容承诺。包 EXE 原目录被用作 cwd 时跟随更新，用户显式 cwd 则保持。计划生成与执行之间包版本改变时重新解析/拒绝旧计划。

首版用受控 PowerShell 5.1 脚本执行 Appx 解析/激活，参数通过结构化输入或参数绑定传入，绝不把用户数据拼进 `-Command`。PowerShell 可执行文件来自 SystemRoot 固定路径，-NoProfile、-NonInteractive；输出只有版本化 JSON DTO，限制大小和超时。后续原生包查询通过相同 contract tests 后替换这个适配器，不改变业务接口。

**3. 包上下文激活与回执**

当前已验证路径是 `Invoke-CommandInDesktopPackage -PreventBreakaway`。微软将该命令定位为调试工具，保证范围主要是包身份及虚拟化资源访问；不能把它描述成通用正式应用激活保证。[官方说明](https://learn.microsoft.com/en-us/powershell/module/appx/invoke-commandindesktoppackage?view=windowsserver2025-ps)。

Rust 首版保留这一路径，但将包内 Node helper 替换为同发行版本的 `app-proxy-host.exe package-child`。package-child 仅用于普通权限、指定包身份、指定 attempt，不接受客户端直接提供任意管理员执行动作。是否能在目标包上下文加载该原生 EXE 是 M0 必须实机证明的前置条件。

外部 coordinator 在共享根创建唯一 request，内容包含 protocol_version、attempt_id、nonce、expected_package、resolved_exe、cwd、args、EnvPatch、过期时间及配置摘要。文件独占创建，权限限制，路径和祖先拒绝重解析；回执包含 attempt/nonce、PID、创建时间、包身份及结果类别，原子写入同一可信目录。

request 消费采用独占 claim 文件/系统锁，helper 全程持有，确保一个 attempt 只有一个消费者。读取 request 后校验归属/版本/截止时间/包身份，再写 consuming 状态，构造环境并创建应用。回执路径由 request 路径派生，不接受请求指定任意输出位置。

迟到处理不能只依赖“进程启动前检查了一次 expires_at”。helper 在 spawn 临界区前再次验证执行能力，并与 coordinator 的 cancel/reconcile 使用同一 attempt gate：撤销方确认 helper 未进入 spawn，或等待 consuming/receipt，再决定是否允许新 attempt。已进入系统创建调用且结果未知时维持 Indeterminate。helper 崩溃在创建与写回执之间时，必须检查实例进程，无法确认则人工处理，不能按 TTL 自动放行。

初始 TTL 为 20 秒、外部等待 22 秒；到期只撤销尚未消费的能力。明确收到成功/失败且消除迟到风险后才能删除 request、claim 和 receipt。含 env/args 的文件不进诊断包，最终清理失败可记录路径 ID，不能打印内容。

**4. 存储虚拟化**

新实例先分配稳定 StorageLocation：普通目录或关闭写虚拟化的包采用 store；其他包采用 `%LOCALAPPDATA%\Packages\<family>\LocalState\AppProxyRust\<namespace>`。namespace 由 store_id 稳定派生，首次使用创建本产品专属归属标记。独立任务、pipe、ETW session 和 cache 同样使用 AppProxyRust 命名空间，不扫描/导入旧工具目录。

一旦已有分身目录，不因更新后清单属性变化自动换位置。包身份不变时继续定位同一数据位置，并验证包内可访问性；不满足则停在 storage 阶段。将来显式变更数据位置必须要求实例退出。原版启动不设置独立目录。

通过已打开文件句柄解析物理路径，验证包外 Explorer/普通用户进程是否能访问；不能以包内 Test-Path 成功证明快捷方式图标在包外可读。所有文件副作用都检查归属、路径穿越、reparse point 和同卷替换条件。路径预检查不被宣称能彻底防住同用户并发替换，关键写入结合句柄和最终路径复核。

**5. ETW listener 与权限分离**

普通 coordinator 负责判断和启动；提权 listener 只消费 kernel process provider 并发送事件。它不接受任意文件写入、执行程序、终止进程或变更目标环境的 RPC。提供的数据为 protocol version、epoch、sequence、PID、映像短名、事件时间；创建时间/用户/session/参数由普通侧重新查询。

使用当前用户/store/session 绑定的固定 ETW session 名，创建前检查同名 session 的 GUID/属性，不抢占别人的 session。StartTrace/EnableTrace/OpenTrace/ProcessTrace/CloseTrace 及 TDH 解码封装在专用线程；丢事件、回调异常、缓冲区超限都转为 degraded 并触发补漏扫描。低流量刷新策略在 Rust 实现后实测，不声称换语言自动降低 ETW 的系统刷新延迟。[StartTraceW](https://learn.microsoft.com/en-us/windows/win32/api/evntrace/nf-evntrace-starttracew)。

安装 helper 至管理员拥有且普通用户不可写的 Program Files 子目录，任务 action 指向固定受保护文件；普通用户仅有读取/执行及任务读取/运行权限。更新和移除必须前台 UAC，不由后台循环发起。首版保持“同一用户具管理员成员身份后提升”的现有安装限制；标准账户使用其他管理员凭据的场景需另行设计，不能认为已支持。

安装时固化 user SID、store ID、helper hash 和协议版本；提权程序不从用户可写 manifest 加载可执行路径或任意插件。受保护复制内容与 hash 校验在提权侧完成，避免验证后替换源文件的竞态。首版不开放从任意 URL 自动更新提权 helper。

**6. 命名管道协议边界**

普通 coordinator 管道和提权事件管道分开。两者都设置显式 DACL、拒绝远程客户端、限定用户/logon SID 和 session；客户端验证 server PID、令牌和映像身份，服务端验证连接方，nonce 不是身份校验的替代品。

不能使用默认 named pipe DACL，也不能无意授予普通客户端创建同名 pipe 实例的权限。监听管道采用 first-instance 检测防占位冒充；发生冲突返回错误，禁止连接未经验证的端点。[微软管道权限说明](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)。

同用户/同权限恶意进程不构成本工具能提供的隔离边界；重点是避免把普通请求转换成额外管理员能力。高权限进程只提供固定事件能力，校验失败时 fail closed。

**7. 快捷方式与任务**

通过 Shell Link COM 创建 `.lnk`，目标是安装中的固定 `app-proxy-host.exe`，参数为 launch + instance ID + store locator。名字采用可读实例名加短 ID；用户改名可更新显示，但数据 ID 和 target 不变。图标从实际 EXE 完整提取到持久 cache，按内容 hash 命名，必要时调用 SHChangeNotify。

保存本工具创建的链接、task 名及预期 target/args。删除前核验仍属于本工具，用户已修改目标则保留并报告冲突。原有应用快捷方式不覆盖。

coordinator 登录任务在 Guard desired enabled 且授权已完成时安装，以普通权限启动 serve；事件任务按需启动提权 listener，无需第二个独立登录触发器。任务验证涵盖 owner SID、RunLevel、action 路径、参数和协议版本，不仅看名称。

**8. 分发与升级**

首版继续支持固定目录便携安装，不宣称移动目录后已有入口自动修复。release 包包括 CLI、host 和版本化 MSIX 桥接，不携带 sing-box。运行时一键安装的内核独立记录来源、版本和许可证。新版本替换整个发行目录中的应用文件需先停止 coordinator/listener 或采用 side-by-side 安装；不在运行中覆盖同名 EXE。

提供 `integration repair`：核验新工具位置、修复本工具快捷方式及普通任务；提权 listener 需要更新时提示前台授权。数据根和实例 ID 保持。只有 coordinator 管理任务已停止且无未决 attempt 才允许版本切换；用户应用可否继续运行取决于是否能继承观测，不在首版升级流程中假定热接管。
