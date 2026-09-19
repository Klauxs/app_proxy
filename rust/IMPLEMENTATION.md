**实现与验收跟踪**

目标：按已确认设计完整实现 Rust 版，每项功能经独立 review、修复与验证后单独 commit。此文件记录进度，不缩小设计范围；不把实验、编译或局部测试作为整个产品完成的证据。

| 功能 | 当前状态 | 完成证据 / 下一项验证 |
|---|---|---|
| M0 普通进程/身份/调试创建候选 | 已实现，独立 review 通过 | `TEST-RESULTS.md`，普通/调试 child 契约测试；不等于完整 M0 通过 |
| MSIX 包内 helper | 仅实验通过 | Claude 包内回执；生产启动、取消/迟到、应用实例仍待实现 |
| 配置模型 | 基础模型已实现，独立 review 通过 | 严格 JSON、引用/路径/Guard/IFEO 校验；13 项 core 测试；当前节点仅手动 HTTP/SOCKS5，订阅与其他协议后续实现 |
| 配置存储 | 基础存储已实现，独立 review 通过 | 归属/ACL、revision、锁、原子替换/上一份备份、不可变秘密与坏文件拒绝；10 项 store 测试；恢复界面与运行 journal 待实现 |
| IPC 传输 | 已实现，独立 review 通过 | 双向身份/普通权限/会话/映像验证、管道 ACL、1 MiB 帧与超时/取消；4 项真实管道 + 2 项帧测试 |
| coordinator | 引导、配置写及 core 控制 RPC 已实现，独立 review 通过 | 单所有者、握手、配置与 core 请求去重/查询；丢应答继续执行，任务/内核/未决请求保活；CLI 跨进程与重启查询已验证，未知副作用核对待实现 |
| 启动模板 | 纯合成已实现，独立 review 通过 | 原版/分身参数环境、直连/代理、秘密引用、路径变量；7 项模板测试；不表示代理就绪或可以启动 |
| 实例数据目录 | 已实现，独立 review 通过 | store/LocalState 命名空间、归属/ACL/目录句柄、空白创建、保留数据；6 项目录测试 + 2 项模板子进程测试；真实包内访问待验证 |
| 实例配置编辑 | 已实现，独立 review 通过 | 已接入 coordinator 与 CLI；7 项规则 + 4 项真实 CLI 写测试，物理去重、保留数据及列表脱敏；菜单及外部集成清理待实现 |
| 实例 CLI / 列表摘要 | 已实现，独立 review 通过 | create/list/clone/rename/bind/remove/request，按 revision 和字节预算分页；尚无启动/保护授权操作 |
| 手动代理配置 / CLI | 已实现，独立 review 通过 | HTTP/SOCKS5 create/list/show/update/rename/remove/request；自动入口、认证分存与脱敏、引用及活动 generation 保护；运行中更新已接确认/回滚，交互菜单待实现 |
| 共享 core 手动上游重配置 | 已实现，独立 review 通过 | 检查候选、影响预览/具体计划确认、原程序切换、失败回滚、持久化阶段/回执恢复；活动集合增减、未知 Starting 的完整核对及未来应用启动许可仍待续 |
| 配置请求恢复/去重 | 已实现，独立 review 通过 | 8 项事务测试覆盖 pending 两侧恢复、结果重放、文件占用、损坏及配置回退；已接入配置 IPC，查询可恢复纯配置 pending |
| 实例启动 | 待实现 | 统一状态机、安装解析和已运行判断 |
| 安装解析 | 已实现，独立 review 通过 | EXE 文件身份/别名、短期句柄、MSIX 稳定定位/旧计划拒绝；3 项安装 + 2 项桥接夹具测试，真实 Codex/Claude 只读发现通过；已接入登记，启动待实现 |
| sing-box 管理/一键安装 | 初始生命周期、coordinator/CLI、一键安装/取消已实现，独立 review 通过；产品集成待续 | 自有共享进程；固定官方包校验、自动目录、交互确认/重试、并发安装、进度与取消；启动许可、重配置/回滚、未知副作用核对及持续故障通知待实现 |
| sing-box 配置生成 | 已实现，独立 review 通过 | 活动 profile 合并、固定入口→出口、末尾拒绝、HTTP/SOCKS5 认证及稳定配置；4 项规则测试和 1 项真实内核转发测试，已接入初始内核管理组件 |
| sing-box 程序发现/检查 | 已实现，独立 review 通过 | 自动发现/真实 version、有界子进程输出及 check；CLI discover sing-box 已接入，安装及生产运行待实现 |
| HTTPS 代理健康检查 | 已实现，独立 review 通过 | 显式指定入口、CONNECT/TLS/主机名验证、无重定向、10 秒/1 MiB、错误脱敏；6 项本地行为测试和真实 sing-box TLS/管理集成，已接初始内核管理，应用启动/运行监控待实现 |
| 订阅解析 | 待实现 | 六协议、URI/Base64/Clash/文本格式、兼容 fixtures、刷新保持选择 |
| IFEO | 仅调试创建候选 | 实际注册匹配/防递归/子进程/调用语义/恢复；未管理原版不受干预 |
| Guard/ETW | 待实现 | 默认范围、普通/提权分工、事件/扫描、身份未知、限流及未管理进程存活 |
| 中文菜单/快捷方式/维护 | 待实现 | 端到端创建启动、改名/绑定/克隆/移除、保护授权、入口修复及保留数据卸载 |
| 发布与实机验收 | 待实现 | 干净环境、Codex/Claude 原版/分身、升级、故障恢复、性能和完整发行清单 |

具体测试矩阵、平台限制和发布门槛仍以 `docs/07-implementation-and-validation.md` 与相关章节为准。用户最近确认的范围优先：不复用外部 sing-box 服务；只创建分身不接管原版；安装路径自动管理；运行中代理故障保留应用；从简实现。

**review / commit 记录**

每项记录具体检查、修复结果与 commit；独立 review 没有通过时不提交该功能。实验性代码的 commit 不表示相应生产功能或整批里程碑已经完成。

- M0 基础代码：独立审查 `review_m0` 未发现提交阻塞问题，复跑 7 项测试通过、1 项需 Claude 的测试默认忽略；Claude 包内验证此前单独通过。提交主题 `feat(rust): add reviewed Windows process and package probes`。
- 基础配置模型：独立审查发现并修复 Windows Chromium 参数别名绕过与嵌套未知字段遗漏；复审通过，13 项 core 测试通过。提交主题 `feat(rust): validate typed instance and proxy configuration`。仅配置校验，不表示运行时物理身份、订阅或持久化层已完成。
- 平台配置存储：独立审查发现并修复 ACL 继承标志遗漏与秘密文件写入前权限核验；复审通过，10 项 store 测试通过，clippy 通过。提交主题 `feat(rust): add protected single-owner configuration store`。覆盖文件占用替换失败、坏配置保留、权限变化、junction、秘密引用与版本冲突；不包含恢复界面或运行 journal。
- IPC 传输：独立审查通过，6 项 IPC 测试通过，clippy 通过。提交主题 `feat(rust): authenticate local named-pipe transport`。此批真实管道两端仍在同一测试进程；CLI/host 跨进程验证留给 coordinator 集成测试。
- coordinator 引导：独立审查发现并修复客户端认证失败退出 owner 和空闲计时偏差；复审通过。6 项协议/跨进程测试覆盖竞启、重连、配置保留、短连接、慢客户端和空闲退出。提交主题 `feat(rust): bootstrap one coordinator for status requests`。测试中修复首次初始化竞态和 Windows 后台 host 继承 CLI 输出句柄；仅只读状态功能，不含应用启动/保护。
- 启动模板：独立审查通过，20 项 core 测试（7 项新增模板行为）和 clippy 通过。提交主题 `feat(rust): compile instance launch arguments and environment`。原版清除分身变量，分身生成独立目录参数；已知路径变量只展开一次。目录归属、继承环境总量、代理就绪和实际应用隔离仍需平台验证。
- 实例数据目录：独立审查通过，6 项目录/包命名空间夹具及 2 项真实模板子进程测试通过，clippy 通过。提交主题 `feat(rust): prepare owned isolated instance directories`。默认目录使用 Windows Known Folder；原版零创建、陌生目录拒绝、改名复用、移除保留。包夹具不证明真实 MSIX 可访问，子进程回执不证明 Codex/Claude 隔离或网络代理已经生效。
- 实例配置编辑：独立审查通过，7 项新增行为测试及 core clippy 通过。提交主题 `feat(rust): add instance configuration editing rules`。默认原版；只创建分身不登记原版/IFEO；克隆分配新目录；移除保留数据，已有外部集成时要求先清理。此批仅纯配置规则，不含运行态操作或用户界面。
- 配置请求恢复/去重：独立审查发现并修复完成记录超前于还原后配置的误报；复审通过，8 项事务测试及 workspace clippy 通过。提交主题 `feat(rust): recover durable configuration requests`。全量测试此前 68 项通过，后续修复与新增 2 项回归已在事务套件通过。保护目录内先记录 intent，提交 manifest，再记录回执；不覆盖外部副作用及启动会话恢复。
- 安装解析：独立审查及桥接测试增量复审通过，3 项安装身份/更新夹具与 2 项 PowerShell 桥接夹具通过；真实 Codex/Claude discover 只读查询成功。提交主题 `feat(rust): resolve installation identity and package updates`。本批全量 75 项测试通过，3 项顶层 ignored（其中 2 项由父测试显式调用的子进程 fixture 已运行）；clippy 通过。尚未接入创建/启动，也未验证真实应用启动、多开或代理。
- 配置写服务 / coordinator RPC：独立审查发现并修复旧状态快照覆盖 Guard 空闲策略的竞态；复审及查询恢复增量审查通过。提交主题 `feat(rust): coordinate durable configuration edits over IPC`。全量 81 项测试通过；随后新增临时文件占用查询恢复回归，4 项服务测试通过（共 82 项已验证行为测试）；clippy/fmt 通过。新增管道写测试仍为同测试进程两端；实际 CLI 写链路下一批验证。开发版协议 major 2；尚未实现启动、网络准备或 Guard 执行。
- 实例配置 CLI：独立审查发现并修复长 Unicode locator 超出 IPC 帧限制的分页边界；复审通过。提交主题 `feat(rust): expose instance configuration commands`。4 项真实 CLI→host 测试验证创建/克隆/改名/绑定/移除/查询、只登记分身、安装别名复用及数据保留；列表隐藏参数/环境值，另有字节预算分页回归。全量 87 项测试通过（3 项顶层 ignored 的含义同前），clippy/fmt 通过；不包含真实应用启动、代理或保护执行。
- sing-box 配置生成：独立审查通过，4 项配置契约及 1 项 sing-box 1.14.1 真实进程测试通过。提交主题 `feat(rust): compile shared sing-box proxy configurations`。真实测试覆盖同一内核的双上游认证、未匹配入口拒绝和故障不串线；全量 91 项测试通过、4 项顶层 ignored（其中真实内核测试另行显式运行），clippy/fmt 通过。尚未实现生产内核生命周期、安装或 CONNECT/TLS 健康检查。
- sing-box 程序发现/检查：独立审查发现并修复版本字符串排序错误，跨位数回归及增量复审通过。提交主题 `feat(rust): discover and validate sing-box binaries`。4 项单测、扩展后的真实内核集成、全量 95 项测试及 clippy/fmt 通过。只查找程序并执行 version/check，未实现安装、运行管理或网络健康检查；同步文件 I/O 不受异步超时强制取消。
- HTTPS 代理健康检查：独立审查及真实 sing-box TLS 增量复审通过。提交主题 `feat(rust): verify HTTPS connectivity through explicit proxies`。6 项新行为测试通过，全量 101 项通过（6 项顶层 ignored，其中两个真实内核测试分别显式运行，另三个为父测试调用的 helper）；clippy/fmt 通过。真实 core→HTTP 上游→TLS 夹具验证 204 与 core 退出后失败，不表示生产生命周期、目标应用代理或 Guard 已完成。
- 共享 core 初始生命周期：独立审查及多轮增量复审通过。提交主题 `feat(rust): persist and supervise owned shared core processes`。4 项 generation/journal 契约、1 项原生双栈 PID 归属测试、真实 core 生命周期集成通过；全量 106 项通过，clippy/fmt 通过。修复探测配置竞态、profile 端口映射、恢复失败后丢失共享入口；尚无 coordinator 启动 RPC/启动许可、重配置回滚、无身份启动核对、持续通知及安装。
- core 控制 RPC / CLI：独立审查和增量复审通过。提交主题 `feat(rust): coordinate durable shared core operations`。新增 11 项测试，全量 117 项通过、7 项顶层 ignored，clippy/fmt 通过。覆盖请求规范化、跨操作编号冲突、时钟回拨、终态保留/未决不清理、任务取消、回执占用、并发/丢 ACK 及真实 CLI 跨 host 重启。CLI 接入 start/stop/status/request；未决副作用不自动重放，历史结果与当前监听状态分开。尚无安装、重配置确认/回滚、持续健康通知或应用启动许可。
- sing-box 一键安装 / 取消：独立审查和增量复审通过，提交主题 `feat(rust): install and cancel verified sing-box downloads`。固定官方 1.14.1 artifact，zip/EXE/DLL/LICENSE 校验，protected staging、版本/check、完整目录发布及安装回执；下载/校验移出配置锁而保留 store owner lease。真实受污染代理环境下 CLI 下载、安装、重启查询及复用通过（407.18 秒）；Windows PTY Ctrl+C 持久取消通过。修复长任务占满 16 个 IPC 槽导致查询/取消饥饿，增加独立后台计数和 32 个普通任务上限。全量 126 项通过、9 项顶层 ignored，clippy/fmt 通过；完整协议、应用与 Guard 验收未完成。
- 手动代理配置 / CLI：独立审查与修复复审通过，提交主题 `feat(rust): manage manual proxy profiles and protected credentials`。新增 4 项编辑/认证规则、5 项秘密/事务/运行保护、2 项真实 CLI 测试；全量 137 项通过、9 项顶层 ignored，clippy/fmt 通过，两项真实 sing-box TLS/生命周期集成另行通过。审查发现并修复 pending 配置在旧内核启动后恢复提交的竞态，以及保存阶段遗漏协议认证限制。改名不重启，更新保持 ID/端口，移除检查实例/下载引用；密码独立原子存储，请求回执不含原文。当前活动 generation 编辑返回需重配置，具体影响确认与回滚仍待接入；不表示应用启动或 Guard 完成。

- 共享 core 手动上游重配置：独立审查和增量复审通过，提交主题 `feat(rust): confirm and recover shared core proxy updates`。候选与当前 manifest 分离，确认前检查原程序及配置，具体计划绑定 revision/原 generation/进程；切换期间阻止普通生命周期及配置写入。候选验证修改出口，回滚以旧集合至少一个可用为成功、所有入口核对 PID，最多 4 个并发且每出口获得探测机会。6 项平台 journal、1 项服务恢复、1 项并发探测默认回归新增；全量 145 项通过、11 项顶层 ignored；两项新真实 core/CLI 集成另行通过，clippy/fmt 通过。修复无副作用 busy 请求永久未决、恢复回执摘要未绑定及过期原回执使恢复请求挂起；新增准备回执恢复、计划状态查询。未知 Starting 不盲目重放，活动集合增减/应用启动许可/持续监控未完成。
