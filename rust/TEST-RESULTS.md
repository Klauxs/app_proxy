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
