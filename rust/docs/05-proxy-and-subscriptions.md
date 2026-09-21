**代理内核、订阅和网络验证**

Rust 负责配置、验证和生命周期；sing-box 继续实现协议。产品边界为“程序文件可复用，代理进程全部由本工具自行启动”。不接入其他工具已经运行的 sing-box 服务。

发行包不携带 sing-box，也不把它嵌入 Rust 进程。Rust 生成本产品的配置并调用外部 sing-box 可执行文件；本机没有可用程序时提供一键安装。代理流量由我们启动的 sing-box 转发，Rust 不实现第二套协议内核。

**1. 区分二进制与运行实例**

`CoreBinary` 表示经 version/check 验证的可执行程序及其来源；`ManagedCore` 表示本 store 启动、记录了身份和 generation 的进程。模型不再提供 ExistingSingBox 或外部服务登记。

程序自动发现本机有效安装及本工具此前安装的副本，没有可用程序时在当前流程提示是否安装；不要求用户指定 EXE 或安装路径，不提供随包内核候选。PATH/Scoop/WinGet 等程序发现只提供 EXE 候选，不发现可绑定的运行服务；必须实际验证 version，自有配置必须用选中版本执行 check，不以版本字符串匹配代替兼容检查。

一键安装采用固定官方 artifact 和预期 digest，校验压缩包、EXE 及配套 DLL，保留来源与许可证。程序自动安装到 `<store>/bin/sing-box/<version>/`，不询问安装路径，不覆盖用户已有安装、配置或包管理器文件。安装版本锁定为经过协议/网络验收的 sing-box 版本；不要求与旧工具相同，不在后台自动替换用户版本。zip 解压拒绝绝对路径、穿越和重解析文件；候选目录完整校验后提交。用户取消安装时保留待配置状态，不自动转直连。

复用仅限 EXE：使用本机已有程序加载我们的独立配置，创建并管理新的进程。其他工具已经运行的 sing-box 不作为应用入口，也不读取或修改其配置。我们此前启动且身份/归属仍可核验的 ManagedCore 可以继续使用，无需每次应用启动都另起内核。生成配置前不必下载内核，实际 check/run 时才要求有效程序。

首版中文终端流程提示“未找到可用的 sing-box，是否安装？”，仅提供“安装并继续（默认）/返回”。用户选择安装后自动下载并显示进度 → 校验 → 解压到暂存目录 → version/check 验证 → 提交安装记录 → 继续原来的代理准备或启动流程。不设置独立安装页面，不要求手工下载、解压、选择 EXE、安装路径或配置 PATH；可写的普通用户 store 不需要管理员安装。并发安装请求合并成同一 operation，支持取消、失败原因和重试，失败不留下可被选中的半成品。已生成的用户配置保留；取消原启动后，即使安装成功也不能擅自启动应用。

首次下载使用明确直连，不能依赖尚未安装的内核；失败时说明原因并提供“重试/返回”，不要求用户改用手动路径，也不自动复用系统中已有 sing-box 服务。`core install` 成功仅代表程序安装完成，代理仍需上游配置和真实联网验证。

**2. Profile 模型**

| kind | 保存内容 | 可执行操作 |
|---|---|---|
| managed | 回环监听、选中节点、订阅/手动来源、revision | 修改节点/端口、生成配置、启动自有内核 |

全部 profile 都属于 managed，不提供“绑定已有 sing-box 端口”选项。本地入口只绑定回环地址，IPv6 输出用标准括号形式。检查端口监听者时必须对应自有 ManagedCore 的已确认身份；端口可用不等于内核归属成立。端口被其他服务占用时提示换端口，不能认领或停止占用者。

手动来源使用 `source: {kind: "manual", nodes: [...]}`，节点含 id、name、protocol 及协议字段，profile 的 selected_node_id 引用唯一节点。订阅来源使用 `source: {kind: "subscription", url_secret_id, revision, nodes: [...]}`；节点连接字段另存受保护秘密文档，manifest 保留显示元数据和引用。两类 profile 使用同一共享内核配置编译、生命周期和本地 HTTP 入口。旧手动更新操作不能覆盖订阅来源；订阅刷新/选节点已有配置动作和共享内核重配置入口，下载协调与 CLI 导入、刷新、列表和选择流程已接入，完整菜单待续。示例配置中的 HTTP 上游仅为远端占位地址，需用户替换；不以手动上游入口变相提供已有本机 sing-box 服务复用。

**3. 代理证据分层**

```text
configured → listener_verified → proxy_request_verified
                                  → exit_ip_observed（可选）
target_process_confirmed 是独立事实
target_request_observed 必须来自目标应用自身的实测证据
```

端口开放只证明有 listener。HTTPS probe 必须经指定 HTTP proxy 的 CONNECT，再进行目标 TLS 证书/主机校验；不允许 HTTP 普通服务器假冒可用代理。出口 IP 查询失败不必推翻已成功的主健康请求，诊断分别展示。

HTTP 客户端按用途创建：代理健康/出口请求显式指定 profile；订阅下载和内核下载默认明确直连，可由用户选择已有 profile，不继承启动器进程的 HTTP_PROXY 等环境；不写系统代理。reqwest 的代理发现、超时和重定向必须显式配置，防止隐式 fallback。[ClientBuilder 官方 API](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html)。

健康端点可配置、默认 HTTPS。默认期望 200/204，可显式设置其他成功状态；重定向不跟随且 3xx 不视为健康。TLS 不关闭验证。测试可注入本地 CA，生产界面不提供忽略证书开关。

单次请求超时 10 秒、probe 响应上限 1 MiB；正文只在内存验证，日志保存状态码/阶段和安全错误类别。出口正文仅当成功且可解析 IP 时显示 IP；其余正文不输出。参数、完整 URL、请求头和代理凭据不进入 raw error。

**4. 托管内核生命周期**

一个 store 首版维护一个 sing-box 进程，多个 managed profiles 对应多个本地 inbound/出口。多个实例可共享同一 profile。进程归属绑定 PID + 创建时间 + image + 已确认的 config generation；同名进程或同端口不能被判为自有。

用户已确认采用这一共用方式。sing-box 官方配置提供 inbounds/outbounds 数组，路由规则可按 inbound tag 匹配，再用 route action 指向 outbound tag。[配置结构](https://sing-box.sagernet.org/configuration/)、[入口匹配规则](https://sing-box.sagernet.org/configuration/route/rule/)、[出口路由动作](https://sing-box.sagernet.org/configuration/route/rule_action/)。本产品为每个 profile 分配稳定本地端口及标签，生成明确的入口到出口映射，不依赖列表顺序或隐式默认出口。不同应用选择同一 profile 就共用端口；选择不同 profile 就使用各自端口，但仍由同一进程转发。

共用进程的代价是内核退出或必须重启的配置变更可能影响全部 profile；按后文已有影响提示和恢复流程处理，不另外引入每实例独立内核模式。内核故障仍保留用户应用并提示。

首次启动：检查端口占用 → 选择 binary → 生成候选 → check → 启动 → 确认身份 → 等待 listener → 探测所需 profile → 保存 active generation。端口被外部占用时直接失败，不杀外部进程，也不把它当网卡失败反复重试。

网卡选择沿用 Wi-Fi、其他物理网卡、auto 的有序尝试，每次均由真实代理请求验证；仅对自有配置生效。通用 core start 至少一个出口可用才就绪；实例 launch 必须其绑定 profile 可用。内核已运行时不重复创建，但仍探测本次所需 profile。

失败只停止已记录且复核归属的本次内核；若创建后来不及保存身份，则 core operation 同样进入待核对状态，不能清理同名进程。普通应用不因 core 退出自动改用直连。现有代理参数能否约束全部组件继续由应用适配能力说明。

运行期间代理探测失败或内核退出时，报告故障并保留已运行应用，不因网络异常关闭实例。再次启动其他代理绑定实例仍须完成验证；不通过查找其他正在运行的 sing-box 来替代故障内核。

**5. 配置更新与恢复**

每次更新用新 generation 目录和 operation journal：

1. 读取旧 manifest、active generation、内核身份及上游选择，保存依赖 revision。
2. 在不修改 active 的情况下生成候选并执行选中 binary 的 `check`。命令输出可能含凭据，只保留安全错误类别。
3. 获取 core gate 和启动许可屏障，确认没有尚未判定的 spawn；重新核验 revision。
4. 写 `prepared` journal，保存旧状态指针；停止确认归属的旧进程，切换候选并启动验证。
5. 成功后提交新 manifest/active 指针及 `committed`。多个文件之间的断点靠 journal 恢复，不宣称全局原子替换。
6. 失败恢复旧 generation、旧 manifest 并尝试恢复原二进制和上游；若恢复启动也失败，标记 `restored_config_core_down`，提示 doctor，禁止报告完全成功。

仅改 profile 显示名称不重启内核。修改现用端口、节点或全局 DNS/网卡配置可能影响多个应用，提交前返回 affected_instances；CLI 需要 `--apply-to-running` 或交互确认此具体变更。编辑实例的 profile binding 只对下次启动生效，但改 profile 自身的网络配置会影响当前连接，这两个行为不能混淆。

启动取消不会回滚已经成功提交的用户配置，也不会因某个应用失败自动停止共享内核。uninstall 按本产品的资源清单解除管理，保留应用数据。

**6. 订阅解析**

首版保留 AnyTLS、VLESS、VMess、Shadowsocks、Trojan、Hysteria2 六类协议，以及现有 Clash YAML、Loon/Surge/Shadowrocket/Quantumult X 风格、URI、Base64 列表。手动上游支持 HTTP/SOCKS5，认证信息交给托管 sing-box 转接，本地入口仍无认证回环 HTTP。

下载采用经过 fixture 验证的客户端 user-agent 列表，单次最多 15 秒，总预算 120 秒；流式读取解压后的总大小不得超过 8 MiB。成功获取正文后进入解析，不能通过反复换 UA 掩盖确定的语法错误。URL 只允许 HTTP/HTTPS，不允许 userinfo；query 可能含 token，整体视为秘密。重定向数量有界，禁止 HTTPS 降级到 HTTP。

下载传输层现已实现：明确直连或指定回环 HTTP 入口，均忽略环境/系统代理；最多 5 次重定向，完整请求链禁止 HTTPS 降级。HTTP 非成功状态、请求超时和网络错误才继续下一 UA，成功正文立即返回供解析。URL/正文不持久化，错误只保留安全类别与 HTTP 状态码。调用方仍须核验指定入口属于自有 ManagedCore，并在提交时核对 source revision；该 API 自身不认领端口，也未接入生产订阅流程。

原始 HTTP 正文和解压后正文分别限制 8 MiB；禁用 reqwest 自动解压以先检查原始 Content-Encoding，拒绝重复、叠加和未知编码。支持 gzip/br/deflate（zlib）/zstd，gzip 多成员与 zstd 多帧完整解码并累计限制，所有解码器拒绝未消费尾部或损坏数据。完整接收网络正文后才解码，网络截断与压缩损坏分别处理；空正文和非 UTF-8 正文拒绝，不换 UA。当前 UA 兼容证据为本地 fixture，未宣称对真实订阅提供商逐一验证。

解析输出为 typed Node enum + 规范化公共字段 + 受允许的协议扩展。保留 raw 中间表示仅用于受保护内存中的转换，不能直接把任意用户 JSON 当完整 sing-box config 执行。TLS/transport 等字段建立协议级映射；未知字段报告兼容性信息，必需字段或影响连接的未知组合导致该节点不可选，不默默降级安全选项。

特殊兼容点单独建 fixture：URI 凭据中的字面 `%XX` 不被无条件二次解码；节点名/查询字段按各格式规则处理；ALPN 块列表不能变成额外节点；Base64 的 padding/URL-safe 差异；IPv6 地址；国家旗帜及地区归类；重复名称；VMess JSON；非法端口及空必需字段。

URI/Base64 适配与 typed Node 现已实现：六协议、标准/URL-safe Base64、有无 padding、VMess 严格 JSON、IPv6/IDN、名称地区推断、重名拒绝。输入最大 8 MiB、单行 64 KiB、最多 4096 个有效或不支持条目；诊断仅含解码后来源行号和固定错误类别。原始 URI 凭据只百分号解码一次，Base64 解出的 Shadowsocks 密码和 VMess JSON 字段不再百分号解码；节点名中的加号按 fragment 字面保留。Node/Protocol 没有 Debug/Serialize；只有私有磁盘适配器能编码其秘密文档，TLS/transport 使用严格 serde 结构，连接参数不直接写入 manifest。

允许的扩展包括 TLS SNI/证书验证开关/ALPN/uTLS、Reality、VLESS Vision、VMess 加密/alter-id、Hysteria2 salamander/带宽，以及 WebSocket、HTTP、gRPC、HTTPUpgrade、QUIC。只从已验证类型生成单个 outbound，不能注入任意 JSON。Reality 公钥输出规范化为 URL-safe 无 padding，SS2022 每个密钥输出标准带 padding Base64；ChaCha20 的 SS2022 多密钥链拒绝，AES 可用。字段以 [sing-box 出口文档](https://sing-box.sagernet.org/configuration/outbound/)、[TLS](https://sing-box.sagernet.org/configuration/shared/tls/)、[传输](https://sing-box.sagernet.org/configuration/shared/v2ray-transport/) 为依据，并用本机固定 1.14.1 实测 check。

未知参数、重复参数/别名、未知 VMess JSON 字段及无法保持含义的组合让该节点不可选。URI 当前明确拒绝 tcp/headerType=http、无 TLS 的 h2、QUIC 上的 uTLS/Reality、AnyTLS/Hysteria2/SS 的 V2Ray transport，以及尚未映射的 SS plugin 等扩展。URI 显式 http 在无 TLS 下为 HTTP/1，有 TLS 下为 HTTP/2，与 [1.14.1 内核实现](https://github.com/SagerNet/sing-box/blob/v1.14.1/transport/v2rayhttp/client.go) 一致。

统一 `subscription::parse` 现可识别 Clash YAML、客户端文本、URI 列表及它们的一层 Base64 包装。YAML 使用事件解析器，只导入根 `proxies`，不执行规则、provider 或外部引用；支持有界锚点、别名和普通标量 `<<` 合并，拒绝标签、多文档、重复键、前向/自引用及超限展开。物理和别名逻辑深度均限 32，每次合并有工作预算；引号内的 `"<<"` 不作为合并键。ALPN 等块列表按层级读取，来源行号保留前导空行。

文本读取 `[Proxy]`、`[server_local]` 或无节名节点列表，支持命名节点和 Quantumult X 协议前缀、位置参数和键值参数、带引号的逗号/等号/反斜杠。其他节忽略，仅支持整行注释；客户端的全部扩展并未覆盖。未知连接选项、证书约束、SS 插件、自定义传输头和不等价组合会报告不可选，不能静默删除。`client-fingerprint` 才映射 uTLS，证书 `fingerprint` 当前不支持；`udp: false` 保留 TCP 限制（AnyTLS 无等价字段则拒绝）。

Clash `network: http` 与 URI 显式 http 分开处理：无 TLS 时明确保留默认 GET 或 `http-opts.method`，有 TLS 时拒绝，避免把 TCP 伪装转成 HTTP/2。真实 sing-box 1.14.1 本地收包夹具已验证 GET/POST 方法，另有 YAML/文本六协议共 12 个配置通过 check；这些不是完整协议握手或真实订阅服务验收。秘密分存和共享编译已接入，六订阅 profile 加一个手动 profile 的完整配置也通过真实 check；下载协调和 CLI 写入口已接入，真实订阅及实网协议验收仍待续。

刷新先锁外下载/解析，再对 source revision 做 compare-and-swap。按节点名保留选中项；重名拒绝；报告新增和删除；若刷新清空选择则保留旧配置。source 已变化就丢弃旧响应，不能写回覆盖。导入成功不等于节点实网可用，需生成配置 check 和对应探测。

CLI 提供 `proxy import/nodes/select/refresh`，地址通过无回显交互输入或显式 stdin 接收，不使用命令参数。交互导入显示可选节点，非交互必须提供准确名称；节点选择 EOF/空输入返回。预览最多保留 4 个会话、10 分钟到期，可取消且不改 manifest；stage 仅发布不可变秘密和引用请求，持久提交仍走原配置事务。丢回复保留同一操作编号，确定的准入拒绝立即报告。只读节点分页绑定全局 revision，不返回秘密或其引用。--via 指定自有下载路由，需要程序时沿用当前流程的安装提示，新增入口时先确认共享 core 影响；预览服务始终核验自有进程和监听归属。

导入/刷新 staging 和 Refresh/Select 配置动作现已实现。刷新同时核对下载前的来源 revision 和 URL secret ID，保留已有名字对应的节点 ID，禁止将其挪给另一个名字。提交前才取得全局 revision，因此下载期间改显示名称或其他 profile 不会必然使结果失效；staging 后的全局变更仍拒绝陈旧提交。刷新保留当前选择（包括下载期间用户新选的节点），若该名字消失则旧来源、选择和配置全部保留。刷新递增 source revision，单纯选节点只递增 profile revision。

活动 generation 完整配置字节不变时可纯提交（例如仅未选节点更新，或选择连接参数相同的另一名字）；字节变化仍要求具体重启预览。PrepareSubscription 复用已有候选 check、影响范围、ApplyUpdate 确认、旧配置恢复及持久回执，不新建第二套重启流程。订阅计划由 before/after 重建引用动作并重放 registry，整份 after 必须一致，防止混入其他编辑；运行状态不明时仍保留已有禁止自动重启的边界。实际本地 Shadowsocks 上游已验证选择切换、失败刷新回滚两路及成功刷新保留选择；其他协议与真实订阅服务仍需后续验收。

**7. 可复用的测试经验**

参考旧脚本实现（已于 2026-09-21 移除，见 git 历史）的 `windows/src/core.ts`、`windows/src/singbox.ts`、`windows/src/proxy.ts`、`windows/src/subscription.ts` 和 `windows/tests/subscription.test.ts` 中的已知失败案例，为 Rust 建立独立 fixtures。验收以本文定义的格式和 sing-box 实际行为为准，不要求逐字段或逐 bug 复制 TS 解析结果。Rust 发行和运行不调用 Node。
