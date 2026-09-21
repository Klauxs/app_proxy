**App Proxy Rust 版设计**

状态：基础平台、配置/存储、认证管道、协调进程、实例配置、共享 sing-box 管理与启动 CLI 已实现。中文日常菜单已接入实例创建/管理/启动、手动代理、订阅、保护授权、桌面快捷方式及实例高级设置。启动流程支持缺失内核安装、共享代理扩容确认、查询与取消；MSIX 已接入包内 helper 和持久回执恢复。Codex/Claude 直连双分身已实测；账户与代理隔离、Guard 完整链路、入口维护及完整验收仍待完成，不能作为正式启动器使用。详见 [验证记录](TEST-RESULTS.md) 和 [功能进度](IMPLEMENTATION.md)。

**构建与验证**

需要 Windows x64、Rust 1.98.1 和 Visual Studio C++ Build Tools。`Cargo.lock` 已固定依赖。常规环境在本目录执行：

```powershell
cargo build --workspace --locked
cargo test --workspace --locked -- --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
.\target\debug\app-proxy.exe discover claude
.\target\debug\app-proxy.exe discover sing-box
.\target\debug\app-proxy.exe probe process
.\target\debug\app-proxy.exe probe package claude
.\target\debug\app-proxy.exe status --json
```

本机 Rust 安装在项目 `.tools` 内，未修改系统 PATH；可使用 `./scripts/cargo.ps1 build --workspace --locked`。传递 Cargo 的 `-p` 或 `--` 等参数时用数组，避免 PowerShell 参数绑定冲突，例如 `./scripts/cargo.ps1 -CargoArgs @('clippy','--workspace','--all-targets','--locked','--','-D','warnings')`。

Windows 原生测试默认建议按上面的 `--test-threads=1` 运行：进程观察刻意限制实际在途 native 查询，并发运行多个独立启动测试可能耗尽查询预算。测试内部的并发/竞争场景仍照常执行。需要安装应用、提权或明确桌面操作的 ignored 测试须按各项说明单独验收。

`status` 首次运行在 `%USERPROFILE%\AppProxy\data` 创建独立 Rust 数据目录，之后连接或启动同一 store 的普通权限协调进程。可用 `--home <绝对路径>` 指定开发测试目录；已有非空未知目录或坏配置不会被重置。当前返回基础状态和实体数量，`phase` 为 `bootstrap`，不代表代理或 Guard 已运行。没有资源和已开启 Guard 的配置时，协调进程在最后一个请求结束后空闲 30 秒退出。配置编辑和应用启动均有持久请求记录，支持去重与查询。IPC 协议为 major 4，没有 minor：CLI、host 与 coordinator 必须成套使用，任何协议变化都提高 major，版本不一致在握手时明确拒绝。

普通启动不扫描外部进程判断“是否已运行”，由应用处理自身的单实例和目录占用。启动实例选择列表只读取配置；本工具只复用自己的可信会话并防止未决请求重复执行。外部旧进程不会被接管，向它转交启动请求也不代表新的代理参数已经生效。Guard 执行纠正时仍核验目标身份和占用；管理列表/详情中的进程观察属于独立只读查询。

无参数运行 `app-proxy.exe` 或执行 `app-proxy.exe menu` 打开中文菜单，需要交互终端；输入被重定向时会提示使用 CLI，且不创建数据目录。实例默认原版，网络须明确选择。菜单创建时不再询问名称：原版使用“Claude 原版”等默认名，添加或复制分身自动使用“Claude 分身 1、2…”并避开已有同名；仍可在管理实例中改名。选择代理后先验证健康，缺 sing-box 时现场提示安装或返回，安装路径自动选择；返回保留已保存的代理。实例保存后再处理保护和启动，返回不会撤销已保存实例。更改配置使用摘要版本核验，其他客户端修改后要求重新选择。Codex/Claude 代理分身默认开启 Guard，不继承被复制实例的关闭状态；只登记分身时不接管原版。实例列表在 Guard 无进程证据时补充独立只读查询，关闭 Guard 的实例也可显示运行状态。`instance inspect <实例ID> [--json]` 展示进程身份、会话启动时的网络与当前配置关系，以及 Guard/监听 状态。查询不接管或关闭外部进程，不创建数据；身份不明、繁忙或超时保留“未确认”。进程存活不表示代理可用或应用实际流量已验证。

实例配置命令已可使用（仅保存配置，不启动应用或启用保护）：

```powershell
.\target\debug\app-proxy.exe instance create --preset claude --data isolated --direct --name "Claude 分身"
.\target\debug\app-proxy.exe instance list --json
.\target\debug\app-proxy.exe instance clone <实例ID> --name "新分身"
.\target\debug\app-proxy.exe instance rename <实例ID> "新名称"
.\target\debug\app-proxy.exe instance bind <实例ID> --direct
.\target\debug\app-proxy.exe instance settings <实例ID> --json
.\target\debug\app-proxy.exe instance edit <实例ID> --file <受保护的绝对路径.json> --revision <版本> --json
.\target\debug\app-proxy.exe instance remove <实例ID>
.\target\debug\app-proxy.exe instance request <请求ID> --json
```

创建默认原版，必须选择 `--direct` 或 `--proxy <已登记代理ID>`；普通 EXE 使用 `--exe <绝对路径> --adapter codex|claude|chromium|environment`，只有已支持的 Codex/Claude 模板允许分身。应用位置和分身存储自动解析，不复制登录数据。移除只删除登记，保留数据；已有系统集成时先要求清理。每次写入前会输出请求编号，响应中断后查询原编号，不自动重新创建。首次登记应用和创建实例是两个请求，实例创建失败可能保留应用记录。列表为摘要，显示名最多 256 字符，不含参数、环境值和代理凭据；不是完整配置导出。Guard 授权仍待实现。

“管理实例 → 高级设置”可替换启动参数、修改工作目录，以及设置、移除或恢复环境变量继承；参数和变量值不回显，保存只影响下次启动。CLI 的 `instance settings` 返回摘要和编辑所需的全局 revision；`instance edit` 从最大 128 KiB 的受保护普通 JSON 文件读取变更，拒绝不符合当前用户 ACL 或包含重解析路径的文件，不改动输入文件权限或内容。文件可放在本工具已创建的受保护 `state` 目录下。省略字段或 `null` 保持原值，`args: []` 清空参数。例如：

```json
{"args":["--example","two words"],"cwd":{"kind":"application"},"env":{"set":[{"name":"EXAMPLE_TOKEN","value":"new value"}],"unset":["OLD_TOKEN"],"inherit":["RESTORED_VARIABLE"]}}
```

`cwd` 也可使用 `{"kind":"explicit","path":"${app_dir}\\work"}`。只有参数和目录展开已支持的路径变量，环境值按原文保存。变量名称按 ASCII 大小写不敏感匹配；`set` 空值与 `unset` 不同，`inherit` 仅移除用户覆盖，恢复启动时基础环境的继承，不重新读取系统环境。受管代理和实例目录变量不能覆盖。新环境值单独存入 ACL 保护的明文秘密文件，参数及工作目录仍存于受保护配置；摘要和回执不包含这些值。

实例启动入口（普通 EXE / 已支持的 MSIX）：

```powershell
.\target\debug\app-proxy.exe launch <实例ID>
.\target\debug\app-proxy.exe launch <实例ID> --request-id <请求ID> --json
.\target\debug\app-proxy.exe launch inspect <请求ID> --json
.\target\debug\app-proxy.exe launch cancel <请求ID> --json
```

启动会先准备绑定代理，失败不改直连。缺少 sing-box 时在原流程提示安装；新增代理需要重启共享内核时先列出具体影响，确认后才执行。JSON/非交互模式通过 `requires_action` 和退出码 5 返回待处理事项，不自动安装或应用重启计划。只有本次前台请求已明确失败且未创建应用，依赖修复成功后才继续启动；期间配置改变会拒绝继续。

相同请求编号查询历史结果，不重复安装或创建应用；结果不明时保留编号，使用 `launch inspect` 查询。Ctrl+C 请求取消尚未创建的应用，并阻止当前流程的后续动作；已创建的应用会保留。已接受的内核切换仍需按其原请求编号查询结果。MSIX 通过一次性包内 helper 创建并保存回执；隔离存储包的请求位于自有 LocalState 命名空间。桥接退出不代表应用已创建，消费中或回执缺失保持未知。当前已验证普通 EXE 夹具、真实 Claude 过期 helper 回执，以及 Claude/Codex 各两个直连分身并存和独立目录写入；Codex 原版共存及重复启动复用通过。登录与代理隔离仍待验收；Guard 完整提权链路仍待验证，启动成功不代表保护生效。

快捷方式目标入口 `app-proxy-host.exe launch <实例ID> --home <数据目录> --notify` 已接入同一启动流程：正常成功保持隐藏，需要安装内核或确认共享代理变更时打开中文前台窗口；选择返回不继续启动。失败/未知结果用系统消息框保留诊断、原启动/代理操作编号和数据目录。缺失的数据目录不会自动重建；没有 `--notify` 时只返回退出码和错误，不弹窗口或自动安装。

在“管理实例 → 桌面快捷方式”中创建、删除或继续未完成操作；桌面位置、固定 host 和完整 EXE 图标自动选择。被用户修改或替换的链接保留并提示冲突，原有应用入口不覆盖。等价 CLI：

```powershell
.\target\debug\app-proxy.exe shortcut create <实例ID> --json
.\target\debug\app-proxy.exe shortcut status <实例ID> --json
.\target\debug\app-proxy.exe shortcut check <实例ID> --json
.\target\debug\app-proxy.exe shortcut repair <实例ID> --json
.\target\debug\app-proxy.exe shortcut remove <实例ID> --json
.\target\debug\app-proxy.exe shortcut request <请求ID> --json
.\target\debug\app-proxy.exe shortcut resume <请求ID> --json
```

`status`/`request` 只查询登记和历史结果，不操作桌面文件；`resume` 显式继续原请求。创建/删除失败或响应中断保留原编号，Ctrl+C 结束等待不代表协调进程已撤销操作。菜单取消待创建入口绑定当时显示的创建编号，配置或登记改变时需重新核对。安装资源查询在配置锁外进行，单个快捷方式工作任务不会占满查询连接。真实桌面创建/核验/删除和菜单流程已验证；真实 Shell 点击启动和完整产品验收仍待续。

`shortcut check` 只读核验登记、链接的原文件身份与内容、启动器文件和缓存图标，区分未登记、待恢复、已核验、丢失和受阻。`status` 仍只返回登记及历史回执。菜单“桌面快捷方式”显示核验结果，可恢复丢失入口或移除入口。恢复使用新的持久请求，绑定原创建 ID 和配置版本；中断后用原 `shortcut resume` 继续，查询不会产生恢复动作。已有完整链接保持原文件不变；用户修改或替换的链接保留并报冲突。恢复完成的回执仍是历史证据，之后再次删除链接需要新的恢复请求。当前只恢复原位置、原启动器和原图标，不修复已移动的数据目录、缺失的启动器或损坏图标，跨发行目录升级已取消。

Guard 自动扫描及纠正已接入：已授权监听组件触发事件检查和周期补扫，只针对已登记且归属明确的误启动主进程，先关闭再准备代理；失败保持关闭，已正确使用代理的应用不因网络故障被关闭。未安装或无法核验监听组件时不会自动纠正。监听组件前台授权及受保护 host 入口已接入，真实提权后的完整事件链路仍待实机验收。普通登录任务已接入 Guard 启用及恢复，实际登录触发尚未验收。

内部只读扫描已能排除未管理原版、识别参数合规及误启动，并保留运行会话的历史绑定。扫描不创建分身目录，不自行启用保护或关闭进程；未决启动、多个主进程或身份不明时阻止纠正建议。

Guard 配置与诊断入口：

```powershell
.\target\debug\app-proxy.exe guard status <实例ID> --json
.\target\debug\app-proxy.exe guard enable <实例ID> --json
.\target\debug\app-proxy.exe guard disable <实例ID> --json
```

`status` 同时返回启用意图、实际状态、监听组件状态和只读进程观察。缺少监听组件时显示 `needs_authorization`；已有组件由普通协调进程核验并按需启动、认证连接及接收心跳。自动扫描尚未完成时显示 `starting`，原版与分身检查通过均可显示 `active`；已授权监听中断后使用轮询并显示 `degraded`，未知归属或纠正失败显示 `blocked`。状态查询不会新启用 Guard，但已启用的自动保护会继续运行。进程扫描与组件核验共享 3 秒截止，超时返回配置状态和诊断；实际未结束工作仍持有名额，避免不断累积线程。扫描繁忙时以 `GUARD_SCAN_BUSY` 表示本次没有新的进程观察。

`enable/disable` 使用持久配置请求，未改变目标状态时不重复提交。交互终端中的 `guard enable <实例ID>` 在缺少监听组件时询问是否安装，选择安装才触发 Windows UAC，目录自动选择；JSON、非交互和状态查询不会触发授权。选择暂不安装或取消 UAC 保留实例配置。安装开始后 Ctrl+C 可停止等待，但不宣称撤销已经提交的系统变更，返回结果未确认，先查询 `guard status`，不自动重试。Windows 授权窗口仍在时应选择取消。

监听组件安装复用受保护记录中的同一代 host/任务；来源不同或陌生任务报冲突，不覆盖。注册与回读不等于 Guard active，协调进程还需接入检查；自动保护可以关闭并纠正已启用实例的误启动主进程。启用意图保存后若保护未完全就绪，输出 `requires_action` 并返回退出码 5；关闭保护保留应用和数据。Codex/Claude 代理实例创建、克隆及绑定也会报告待处理状态，不因退出码 5 重复创建实例。状态查询成功返回 0，不代表保护 active；保存后的状态查询失败或安装结果不明返回 6。普通登录任务的登记与恢复已接入；真实登录触发、UAC/提权部署仍待实机验收。

手动代理配置入口（HTTP/SOCKS5；本地入口自动分配）：

```powershell
.\target\debug\app-proxy.exe proxy create --name "工作代理" --protocol http --host 127.0.0.1 --port 8080
.\target\debug\app-proxy.exe proxy list --json
.\target\debug\app-proxy.exe proxy show <代理ID>
.\target\debug\app-proxy.exe proxy rename <代理ID> "新名称"
.\target\debug\app-proxy.exe proxy update <代理ID> --protocol socks5 --host 127.0.0.1 --port 1080 --no-auth
.\target\debug\app-proxy.exe proxy remove <代理ID>
.\target\debug\app-proxy.exe proxy request <请求ID> --json
```

认证使用 `--username <用户名> --password-stdin`，密码通过重定向标准输入传入，不接受密码命令参数；读取 UTF-8 并去除一个末尾换行。当前没有交互密码输入框。更新替换完整上游和认证，必须明确选择无认证或提供认证，避免遗漏参数时清空旧凭据。改名和更新保持 ID、本地入口与实例绑定；列表只显示认证是否已配置。密码保存在受保护的独立文件内，manifest 和持久化请求记录仅引用其 ID。移除要求实例及下载网络均不再引用此配置，保留秘密文件。仍在活动共享内核中的代理，使用 `proxy remove <代理ID>` 或菜单先预览影响，再确认切换；JSON/非交互默认只返回计划及退出码 5。可用 `--apply-to-running` 明确同意本次移除，或执行 `core apply-update <计划ID>`。保留其他入口，检查失败恢复原集合且不删除配置；最后一个入口移除后内核停止，再提交删除。Down 状态保留恢复集合，需要先显式 `core stop` 再移除。

订阅入口（六协议 URI/Base64、Clash YAML 和支持的客户端文本格式）：

```powershell
.\target\debug\app-proxy.exe proxy import # 名称自动生成；也可显式 --name
.\target\debug\app-proxy.exe proxy nodes <代理ID>
.\target\debug\app-proxy.exe proxy select <代理ID>
.\target\debug\app-proxy.exe proxy select <代理ID> <节点ID> --json
.\target\debug\app-proxy.exe proxy refresh <代理ID>
.\target\debug\app-proxy.exe proxy refresh <代理ID> --via <下载代理ID>
```

菜单“添加代理”直接进入订阅导入，默认直接下载，名称自动生成为“订阅标题（无标题则域名） · 所选地区 · 本地时间”，重复时加序号，可在管理中改名；手动 HTTP/SOCKS5 另有独立入口。交互导入会隐藏输入的订阅地址，再按名称/旗帜识别地区分组（无法识别放“其他”）。输入 `1,3-5` 多选节点、`G1,G3` 选择整个地区组、`all` 全选，`0` 或回车返回；本地入口自动分配。地址不接受命令行参数。自动化使用 `--url-stdin --node "准确节点名" --json`（可重复 `--node` 多选；`proxy select <代理ID> <节点ID1> <节点ID2>` 可重新选择），通过标准输入提供一行 UTF-8 地址；地址可能含 token，不要写入命令历史。节点列表只显示 ID、名称、协议和服务器地址；不会输出 URL、密码、UUID 凭据或传输秘密。输入结束或取消选择时保留原配置。

下载默认直连，可用 `--via` 选择本工具管理的代理；已有自有内核可用就复用，需要启动时沿用现有健康检查、缺失程序安装提示和扩容影响确认。不会连接外部 sing-box 服务或静默改回直连。直接导入只保存配置，未使用代理下载时不要求安装 sing-box；保存成功不代表节点已通过联网验证。

单选使用固定节点；多选生成 sing-box `urltest`，用设置中的测试 URL 每 3 分钟检测，50 ms 容差避免频繁切换，已有连接不会因优选变化而主动断开。首次探测完成前不保证已选出最快节点；失败检测和测速切换也不是即时完成，地区是名称提示而非实际出口定位。摘要显示候选节点数量，不代表当前出口。刷新按名称/稳定 ID 保留仍存在的已选节点并报告新增、删除和不支持条目；不会自动加入新节点，剩一个时使用固定节点，已选节点全部消失或来源已变则保留旧配置。`nodes/select` 不重新下载。仅未选节点变化、或选择连接参数相同的另一名字不重启；实际影响当前连接时先返回具体计划，与 `proxy update` 一样需交互确认或 `--apply-to-running`。非交互默认以退出码 5 表示等待确认，可用 `core apply-update <计划ID>` 执行。结果不明时保留打印的编号，配置请求用 `proxy request`，共享内核请求用 `core request` 查询；不要通过重新下载当作原请求重试。

修改当前运行的共享内核所用代理时，`proxy update` 先生成并检查候选配置，列出受影响的代理及其绑定实例，再提示是否应用（默认返回）。明确指定 `--apply-to-running` 可同意这次变更；JSON/非交互默认只返回计划并以退出码 5 表示尚未应用。改名不触发重启。删除活动代理及已退出内核的恢复集合编辑仍待后续流程接入。启动前必须先恢复已接受的配置事务，再核对候选配置，避免中断写入在内核启动后悄悄生效。

候选准备不会修改当前代理；确认针对具体计划 ID，期间配置或原进程状态改变会拒绝陈旧计划。执行使用原内核程序，检查全部入口归属并验证被修改的出口；失败时恢复旧 generation，只要至少一个旧出口仍可用便保留恢复后的共享内核。原有故障出口不会拖停其余可用代理。所有旧出口都无法通过时保留旧配置并报告代理未恢复，不关闭应用。创建结果无法核对时保留待处理状态，不按进程名猜测或盲目再启动。

`discover sing-box` 自动探测 store 内完整版本目录、绝对 PATH、Scoop 和 WinGet Links 中的程序，执行 version 并输出位置/版本/来源，不创建 store，不接入外部服务。具体代理配置仍须执行 check；此只读命令不代表代理可用。探测子进程每次限时 3 秒、总异步等待预算 20 秒；同步文件系统访问（例如网络盘）仍受 Windows I/O 超时约束。只接受稳定版本，Scoop 的转发 shim 不作为内核执行。

共享内核的开发入口（需要已有代理配置）：

```powershell
.\target\debug\app-proxy.exe core start <代理ID> [其他代理ID] --required <本次检查的代理ID>
.\target\debug\app-proxy.exe core status --json
.\target\debug\app-proxy.exe core stop
.\target\debug\app-proxy.exe core request <请求ID> --json
.\target\debug\app-proxy.exe core install
.\target\debug\app-proxy.exe core cancel <安装请求ID>
.\target\debug\app-proxy.exe core apply-update <已检查的计划ID>
.\target\debug\app-proxy.exe core recover-update <中断的计划ID>
.\target\debug\app-proxy.exe core recover-start
```

`start` 默认检查第一个代理，使用保存的 HTTPS 健康目标与允许状态码；只创建本工具拥有的进程，支持共享入口。请求只包含已有入口时直接复用；缺少新增代理时，先检查原集合与请求集合的并集配置，展示原入口中断影响和新增入口，再确认重启（默认返回）。可用 `--apply-to-running` 明确同意本次扩容，JSON/非交互默认返回计划及退出码 5，也可随后运行 `core apply-update <计划ID>`。扩容不删除原入口、不改保存的代理配置及 revision；检查失败恢复旧集合。已有代理的上游编辑使用上述更新流程。`stop` 停止自有共享内核，保留应用。写请求先持久化再执行，客户端断开仍继续；终态保留 7 天，未决记录保留到核对处理。同编号只返回历史结果，不能将历史 Ready 当成当前健康。`status` 核验进程和端口归属，并显示最近重配置计划与阶段，不执行网络请求。协调进程在有后台任务、活动内核或未决请求时不空闲退出；后台任务不占短连接槽位，查询与取消保持可用。未知启动结果不自动重放。应用启动许可已接入普通 EXE 启动；活动集合移除已实现，持续故障通知仍待实现。

重配置期间普通启动、停止及配置写入会被拒绝，避免破坏恢复依据。`recover-update` 核对已有 journal：提交意图已保存时只完成配置提交；切换或回滚中断时按已记录的精确进程身份尝试恢复旧代理；新版创建留下的 Starting 通过创建前持久化的 Windows Job 归属证据核对；确认候选进程身份后才停止并恢复旧代理。旧版缺少证据或证据冲突时仍保留未知，不盲目重放。核对成功会更新原请求及相关恢复请求的回执。仅准备好的计划可以查询/恢复其准备回执，不因此开始切换。

`core recover-start` 显式核对当前代内核创建：发现精确归属的存活进程则记录其身份，确认无存活内核则记录 Down；不自动重启、不宣称代理健康。操作绑定查询时的 generation，期间换代会拒绝执行。原 Start 及同代未完成恢复请求的回执可由核对结果解除未知。缺少旧版启动证据、原创建者仍在运行或 Job 成员不唯一时，保留待确认状态。

交互终端中，`core start` 确认缺少程序后提供“安装并继续 / 返回”，失败提供“重试 / 返回”。选择安装才开始从固定[官方 1.14.1 发布](https://github.com/SagerNet/sing-box/releases/tag/v1.14.1)下载，校验 zip、EXE、DLL 和 LICENSE，version/check 通过后发布到 `<store>/bin/sing-box/1.14.1/`；无路径选择。下载明确直连，总时限 600 秒、连接 10 秒、读取停滞 20 秒，展示阶段与下载字节；并发请求共享一次尝试。JSON/重定向输入不会自动下载，可显式执行 `core install`。

安装等待时 Ctrl+C 会提交取消请求；也可用 `core cancel <安装请求ID>`，最终状态以原请求查询为准。取消当前请求不撤销其他已授权安装，发布已经完成时可能返回 Installed，但退出的原流程不会继续启动。单独安装成功不启动 core 或应用。当前固定版本已验证 HTTP/SOCKS5 及安装链路，其他订阅协议及完整发行验收仍待实现。

`probe package claude` 仅在已安装 Claude 的包身份下启动本产品测试 helper，验证回执和独立临时目录读写，不启动 Claude 界面或修改其登录数据。`probe process` 验证普通进程参数、环境和身份；调试脱离实验已移除。

目标是全新设计一套 Rust 实现的实例启动器：普通用户选择应用、原版或空白实例以及代理即可启动；平台层处理进程身份、MSIX、快捷方式和 Guard。代理协议仍由 sing-box 实现。按用户要求，不承担旧版配置、命令、数据目录、任务或快捷方式兼容，不提供旧版迁移和回退。现有实现只作为功能经验与平台行为的参考。

**从简原则**

按用户指定的 [andrej-karpathy-skills](https://github.com/multica-ai/andrej-karpathy-skills) 原则推进：先明确问题，采用满足需求的最小实现，只修改当前任务需要的内容，并用可验证结果判断完成。每个新增选项、模块或抽象都必须能说明它解决的当前需求；没有实际需求就不加。

- 普通流程只让用户选择应用、原版/空白实例和代理。能自动确定的程序位置、目录、端口等不增加询问；实际歧义再提示。
- 缺少 sing-box 时就在当前流程提供“安装并继续 / 返回”，程序管理安装位置；失败提供“重试 / 返回”。
- sing-box 安装失败详情保存在数据目录的 `state/core-requests/<请求编号>.json`（默认 `%USERPROFILE%\AppProxy\data`），也可用 `app-proxy.exe core request <请求编号> --json` 查询。详情包含失败阶段、文件操作名、Windows 错误码、IO 类型、已下载字节和本次尝试耗时；不记录原始错误消息、订阅地址或凭据。旧版本只留下错误码的历史记录无法补回详情。
- Guard 使用 ETW 启动后检查及周期补扫，原版与分身共用就绪标准；只处理已登记实例，不处理未管理原版。2026-09-20 已取消 IFEO。
- 共用一套启动流程和一个状态所有者；内部业务优先使用普通函数与结构体，仅为真实的系统边界或故障测试需要引入接口。不建立通用插件、工作流或平台框架。
- 按当前功能需要实现命令、字段和恢复步骤。详细章节中的接口、CLI 清单及目录划分是设计参考，不要求提前逐项搭空壳；新增范围需另行讨论。
- 保留与真实副作用有关的检查：不重复启动、不误杀进程、不覆盖他人配置、配置失败可恢复。测试围绕这些行为与实际启动闭环，不为简单实现机械配套测试。

目标日常链路：选择应用与实例 → 选择/配置代理 → 自动查找 sing-box，缺少则询问安装 → 准备代理 → 启动应用。Guard 在创建流程中完成必要授权；运行中代理故障只提示、保留应用。用户已确认开始实现，目前交付上述基础功能和验证入口；后续按实际验收结果逐步补齐。

**阅读入口**

| 文档 | 内容 |
|---|---|
| [01-architecture.md](docs/01-architecture.md) | 范围、进程与 crate 划分、依赖、接口、关键决策 |
| [02-model-and-storage.md](docs/02-model-and-storage.md) | 应用/模板/实例模型、新配置格式、存储、环境变量、事务 |
| [03-launch-and-guard.md](docs/03-launch-and-guard.md) | 启动状态机、并发、取消与恢复、Guard 策略和资源所有权 |
| [04-windows-platform.md](docs/04-windows-platform.md) | 原生进程、MSIX、ETW、权限、管道、快捷方式、升级 |
| [05-proxy-and-subscriptions.md](docs/05-proxy-and-subscriptions.md) | sing-box 发现/复用、托管内核、订阅、联网探测与回滚 |
| [06-product-and-protocol.md](docs/06-product-and-protocol.md) | 用户流程、CLI、IPC、事件、错误及诊断 |
| [07-implementation-and-validation.md](docs/07-implementation-and-validation.md) | 从零实现的批次、接口验收、测试和发布门槛 |
| [08-evidence-and-decisions.md](docs/08-evidence-and-decisions.md) | 当前源码基线、上游来源、决定及待验证事项 |
| [09-ifeo-launch-interception.md](docs/09-ifeo-launch-interception.md) | IFEO 取消决定、Guard 边界与配置兼容占位 |
| [16-installation-and-upgrade.md](docs/16-installation-and-upgrade.md) | 安装位置、单文件安装器、成套升级与中断恢复 |
| [docs/decisions/](docs/decisions/) | 按日期记录的单项决定与实测：Guard 时延与唤醒、订阅多节点、快捷方式路径、监听升级修复等 |
| [21-refactoring-plan.md](docs/21-refactoring-plan.md) | 代码结构现状、重构目标、分阶段计划与进度 |
| [manifest.json](examples/manifest.json) | 无凭据的配置示例，含原版和独立实例 |
| [launch-events.ndjson](examples/launch-events.ndjson) | 启动事件协议示例，不是实测日志 |

**关键决定**

2026-09-21 发行命名：产品统一称为 AppProxy，目录、计划任务、ETW 会话及快捷方式标记不再使用 AppProxyRust。默认数据目录为 `%USERPROFILE%\AppProxy\data`，受保护监听组件位于 `%USERPROFILE%\AppProxy\guard`，正式布局下包内辅助进程与分身共享同一 `data`，资源登记归入同根 `resources`。单文件 `AppProxy-Setup.exe` 内嵌同版本前后台 EXE，安装到固定的 `%USERPROFILE%\AppProxy\app`，后续运行新版 Setup 在原目录升级；详见 [安装与升级](docs/16-installation-and-upgrade.md)。持久化文件格式和归属校验标识不因展示名称调整而更改。现有开发版数据、系统任务和运行程序未就地迁移；首次切换正式安装时需处理已有登记并核验新入口，不能把新二进制直接覆盖后视为完成升级。

2026-09-20 范围收敛：不做跨目录升级或移动后入口重定向，也不做 Windows 登录任务被外部删除后的专用检测、自愈或修复流程。这两项不再列为待办或发布门槛。保留固定目录使用、正常登录自启的创建/查询/移除，以及已接受但未完成操作的显式恢复。已有只读查询仍如实报告入口不可用，不能把历史创建回执当作当前就绪。

- 首发目标 Windows x64，先保持中文菜单与 CLI；GUI 和 macOS 实现后置，平台边界从第一版建立。
- 三个 crate、两个普通发行入口，共用一个启动核心；用户态 coordinator 统一管理配置写入、启动任务和 Guard。
- Guard 默认保护已管理的 Codex/Claude 代理实例，监听授权后按实际证据报告状态。系统原始入口不被接管；外部启动到检查之间存在执行和联网窗口，不保证启动前拦截。
- 发行包不携带 sing-box；优先复用本机程序文件，缺少时一键安装。所有代理进程均由本工具用独立配置启动，不接入其他工具已运行的服务。
- 同一 store 的多个代理共用一个自有 sing-box 进程，以不同本地入口路由到对应出口；多个应用实例可以绑定同一代理。
- 运行期间代理故障只提示并保留应用，不自动改直连；新的代理绑定启动仍要求验证通过。
- Rust 直接实现常规 Windows 集成。MSIX 首版保留受限 PowerShell 桥接，包内执行和回执改用 Rust helper。
- 应用安装信息、适配模板、运行实例分别建模；改名不改 ID 或数据目录；复制配置默认创建空白数据。
- Guard 默认沿用“检测到未按要求代理的进程就关闭”的保护意图；代理不可用时明确报告停止且阻止重启。首版不提供静默保留直连的替代策略。
- 只有完整身份确认后才能停止进程；未知结果不重复启动；外部 sing-box 永远不由本工具停止。
- Rust 新格式从 schema_version=1 开始，使用独立格式标识、数据根和系统资源命名空间，首次启动从空配置开始。

**基线变化**

本次以 `D:\app_proxy` 的 `main`、提交 `a38a29865578035ba8ad0d85d0e319bbb63880e2` 及读取时工作区为基线。当前已经有 Codex/Claude 自动识别、串联代理创建和桌面预设默认 Guard；这些属于保留能力。更早的开源调研文档（`Launcher-开源调研与流程建议.md`，已随旧脚本实现移除，可从 git 历史查看）对创建菜单的描述早于该提交，以本文档为准。

本文记录已确定的产品规则；详细接口按实际实现需要收敛，不为未来假设预留功能。crate 版本、MSRV、Windows 最低 build、性能阈值在首轮技术验证后锁定；此处没有未经测试的兼容性和性能保证。
