**Rust M0 首批实现验证 — 2026-09-19**

本记录只描述已运行的代码，设计文档不等同于已实现功能。当前完成 M0 的进程创建/身份及包内 helper 验证部分，M0 尚未全部通过。

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
