**代理内核、订阅和网络验证**

Rust 负责配置、验证和生命周期；sing-box 继续实现协议。产品边界为“已有程序可复用、已有服务不接管”。

**1. 区分二进制与运行实例**

`CoreBinary` 表示经 version/check 验证的可执行程序及其来源；`ManagedCore` 表示本 store 启动、记录了身份和 generation 的进程；`ExistingSingBox` 表示其他启动器拥有的本机 HTTP/mixed 服务。

二进制选择保持显式用户指定优先，其后发现本机有效安装、历史下载、随包副本，均缺失才提供安装操作。显式指定无效时返回错误，不默默换成另一版本。发现 PATH/运行进程/Scoop/WinGet 只提供候选，必须实际验证 version；自有配置必须用选中版本执行 check，不以版本字符串匹配代替兼容检查。

受管下载固定官方 artifact 和预期 digest，校验压缩包、EXE 及配套 DLL，保留来源与许可证。Rust 发行独立选择并锁定经过协议/网络验收的 sing-box 版本；不要求与旧工具相同。zip 解压拒绝绝对路径、穿越和重解析文件；候选目录完整校验后提交。

**2. Profile 模型**

| kind | 保存内容 | 可执行操作 |
|---|---|---|
| managed | 回环监听、选中节点、订阅/手动来源、revision | 修改节点/端口、生成配置、启动自有内核 |
| existing_singbox | 名称、回环地址/端口、绑定时证据摘要 | 重新验证、改名、解除登记 |

existing_singbox 不保存远端节点，不提供 stop/restart/导入原配置。只接受当前能确认监听进程是 sing-box 的无认证 HTTP/mixed 回环入口；只有 SOCKS/TUN、进程权限不足或来源不明则不自动认领。本机 IPv6 回环沿用现有能力，地址输出用标准括号形式。

用户原服务 PID 在重启后会变化，所以不永久 pin 旧 PID；每次使用重新验证端口所属进程、路径/版本和代理请求。路径或所有者变化无法确定时返回不可验证。验证前后确认 listener 身份未变；这仍不是对同用户恶意端口劫持的完整防护。

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

首次启动：检查端口占用 → 选择 binary → 生成候选 → check → 启动 → 确认身份 → 等待 listener → 探测所需 profile → 保存 active generation。端口被外部占用时直接失败，不杀外部进程，也不把它当网卡失败反复重试。

网卡选择沿用 Wi-Fi、其他物理网卡、auto 的有序尝试，每次均由真实代理请求验证；仅对自有配置生效。通用 core start 至少一个出口可用才就绪；实例 launch 必须其绑定 profile 可用。内核已运行时不重复创建，但仍探测本次所需 profile。

失败只停止已记录且复核归属的本次内核；若创建后来不及保存身份，则 core operation 同样进入待核对状态，不能清理同名进程。普通应用不因 core 退出自动改用直连。现有代理参数能否约束全部组件继续由应用适配能力说明。

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

解析输出为 typed Node enum + 规范化公共字段 + 受允许的协议扩展。保留 raw 中间表示仅用于受保护内存中的转换，不能直接把任意用户 JSON 当完整 sing-box config 执行。TLS/transport 等字段建立协议级映射；未知字段报告兼容性信息，必需字段或影响连接的未知组合导致该节点不可选，不默默降级安全选项。

特殊兼容点单独建 fixture：URI 凭据中的字面 `%XX` 不被无条件二次解码；节点名/查询字段按各格式规则处理；ALPN 块列表不能变成额外节点；Base64 的 padding/URL-safe 差异；IPv6 地址；国家旗帜及地区归类；重复名称；VMess JSON；非法端口及空必需字段。

刷新先锁外下载/解析，再对 source revision 做 compare-and-swap。按节点名保留选中项；重名拒绝；报告新增和删除；若刷新清空选择则保留旧配置。source 已变化就丢弃旧响应，不能写回覆盖。导入成功不等于节点实网可用，需生成配置 check 和对应探测。

**7. 可复用的测试经验**

参考 [core.ts](D:/app_proxy/windows/src/core.ts)、[singbox.ts](D:/app_proxy/windows/src/singbox.ts)、[proxy.ts](D:/app_proxy/windows/src/proxy.ts)、[subscription.ts](D:/app_proxy/windows/src/subscription.ts) 和 [现有订阅测试](D:/app_proxy/windows/tests/subscription.test.ts) 中的已知失败案例，为 Rust 建立独立 fixtures。验收以本文定义的格式和 sing-box 实际行为为准，不要求逐字段或逐 bug 复制 TS 解析结果。Rust 发行和运行不调用 Node。
