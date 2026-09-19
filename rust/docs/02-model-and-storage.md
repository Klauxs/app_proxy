**模型、配置与持久化**

采用全新的 `format: "app-proxy-rust"`、`schema_version: 1`，把安装位置、适配能力、实例配置和运行记录分开。该版本号属于 Rust 格式自身，不表示与 TS schema 1 兼容。字段使用 snake_case；磁盘 JSON 与 IPC DTO 单独定义，避免直接序列化含句柄或秘密的内部对象。

**1. 领域对象**

| 对象 | 关键字段 | 不变量 |
|---|---|---|
| StoreHeader | format、schema_version=1、store_id、revision、owner_sid | UUID 标识不从当前目录名推导；移动需显式位置变更流程 |
| Application | id、name、locator、template_ref、revision | 表达一个安装来源；实际 EXE 可随 MSIX 更新变化 |
| Instance | id、application_id、name、data、args、env、cwd、network、guard、revision | ID 不随改名变化；数据模式不能在运行中切换 |
| ProxyProfile | id、name、kind=managed、endpoint、selected_node_id、source/nodes、revision | 所有入口由自有 ManagedCore 提供；不登记其他工具运行的服务 |
| IfeoRegistration | id、application_id、default_instance_id、desired、installed_target、归属和 generation | default_instance_id 只指向该应用已登记且开启 Guard 的原版；只有分身时没有注册；系统实际状态单独核验 |
| LaunchAttempt | id、instance_id、阶段、配置摘要、deadline、身份结果 | 同一物理实例至多一个未决 attempt |
| ProcessIdentity | pid、creation_time、user_sid、session_id、image_identity、证据 | PID 单独不能授权任何终止操作 |
| RunningSession | launch_id、instance_key、identity、实际绑定快照、观测状态 | 配置改变不自动改写已运行会话的事实 |

所有新实体使用 UUID，不保留旧 App.id 或旧路径别名。相同包身份共享 Application；普通 EXE 在规范化 locator 与模板相同后复用安装记录。不同模板指向同一实际程序时仍通过物理实例 key 检查重复管理，不靠 Application.id 隔离它们。

`integrations.ifeo` 为 IFEO 注册数组，无注册时为空。主进程 LaunchAttempt 另记 `LaunchOrigin = interactive | shortcut | guard | ifeo`；辅助进程 continuation 使用独立恢复记录。IFEO 完整字段、机器级归属及默认实例规则见 [第九章](D:/app_proxy/rust/docs/09-ifeo-launch-interception.md)。受保护的安装恢复清单独立于可写 manifest，不能仅凭 manifest 授权修改系统注册表。

管理范围由显式登记的 Instance 决定：Application 存在不代表原版已受管理。只创建分身不自动创建原版记录、选择原版代理或要求补齐原版配置。Guard 配置保留在实例上；原版 Guard 驱动对应 IFEO 注册，分身 Guard 只管理可确认属于该分身的进程。

**2. Tagged union 定义**

```text
ApplicationLocator =
  {kind: "exe", path: AbsolutePath}
  | {kind: "msix", family_name, app_id}

InstanceData =
  {kind: "original"}
  | {kind: "isolated", location: StorageLocation}

StorageLocation =
  {kind: "store", relative_path}
  | {kind: "package_local_state", family_name, namespace, relative_path}

NetworkBinding = {kind: "direct"} | {kind: "profile", profile_id}
WorkingDirectory = {kind: "application"} | {kind: "explicit", path}
EnvPatch = {set: Map<EnvName, EnvValue>, unset: List<EnvName>}
EnvValue = {kind: "literal", value} | {kind: "secret_ref", id}
GuardConfig = {desired: "enabled"|"disabled", policy: "stop_unproxied"}
```

首版只有 original 和 isolated 两种数据模式。独立数据位置由程序分配；自由 args 不允许另传 `--user-data-dir`，不开放任意外部数据目录，避免创建流程出现多套互相冲突的目录语义。

首版没有 inherit network 值。将来加入时必须显式编码；缺字段是错误，创建流程必须选择 direct 或 profile。自定义环境支持 set/unset，Windows 名称按大小写不敏感去重，重复歧义拒绝；unset 与空字符串 set 的含义不同。

**3. 模板及合成规则**

首版内置 `builtin.codex@1`、`builtin.claude@1`、`builtin.chromium@1`、`builtin.environment@1`，模板随程序发布，实例使用应用记录固定的模板版本。升级模板需能展示受影响实例和变更，不在后台切换数据规则。

模板描述 capabilities（代理参数、独立目录、Guard、身份识别）、受管 args、环境规则、辅助进程规则和验证范围。Codex/Claude 按包 family + AppId 自动识别，不按可编辑显示名称或单独 EXE 名判断；手动 EXE 的应用类型允许用户明确指定，自动推荐不能视为兼容性证据。

生成过程：选择进程基础环境 → 去除已知继承代理变量和模板管理的目录变量 → 应用经过模板校验的用户 env → 模板模式变量 → 强制应用 network 与实例目录约束。禁止用户覆盖受管代理变量和目录变量，报字段冲突，不依赖“最后一个参数赢”。original 模式不注入分身 home，使用目标应用默认数据位置；即使从另一份分身内启动本工具，也不能继承其受管目录变量而误打开另一实例。

Codex isolated 生成 `--user-data-dir`、CODEX_HOME、CODEX_ELECTRON_USER_DATA_PATH；Claude isolated 生成 `--user-data-dir` 和 CLAUDE_CONFIG_DIR，后者不构成所有 Desktop 组件隔离保证。generic Chromium 仅具代理能力，未验证的多实例能力不开放。direct 清除代理 env；Chromium direct 增加 `--no-proxy-server`，该参数不保证非 Chromium 网络组件的行为。

args 内拒绝 NUL、受管参数冲突、含混 `--` 分隔位置。只支持已知参数规则，禁止 shell 执行模板。模板路径变量仅 `${app_dir}`、`${instance_root}`、`${user_data}`、`${app_home}`，替换得到单个参数/路径，不二次解析成 shell 字符串。用户输入中未知占位符拒绝。

普通 EXE 的基础环境来自已验证请求方提交的允许继承环境快照（IPC 内存，不持久化）；Guard 无请求方时使用 coordinator 登录环境，经同一过滤。拒绝提权请求方把环境传给普通启动器。MSIX 以包内环境为基础，叠加同一 EnvPatch；包上下文必需变量不被外部整包环境覆盖。两类启动保证显式变更一致，不宣称基础环境逐字一致。快照大小上限 256 KiB，秘密不得进入日志。

**4. LaunchPlan 与诊断摘要分离**

`LaunchPlan` 只在内存持有 resolved application、实例 key、argv、cwd、EnvPatch、存储路径、proxy lease、application/instance/profile/template 的依赖 revision 和 attempt capability。敏感字段没有自动 Debug。MSIX 需要磁盘传递时使用受限目录中的短期 request。

`LaunchSummary` 只保存 ID、阶段、耗时、错误码、可公开的本机监听端点及环境变量名称。真实订阅 URL、节点凭据、任意参数值、完整环境值、原始命令输出均不进入普通诊断。

实例 key 不等于登记 ID：original 使用稳定安装身份 + 用户/session + original 标记；isolated 使用同一安装身份 + 规范化实际数据目录。两个登记指向同一 EXE 和同一数据目录时不能并发启动或配置两个 Guard。物理路径身份比较由平台层完成；不存在目录先校验父目录，创建后重新确认。MSIX 稳定 key 使用 family/AppId，解析后的版本化 EXE 用作本次进程核验。

**5. 数据目录布局**

```text
<rust-store>/
  .app-proxy-rust-owned.json
  manifest.json
  secrets/<secret-id>.json
  instances/<instance-id>/user-data/ ...
  state/owner.lock
  state/launch.json
  state/operations/<operation-id>.json
  state/requests/...
  config/generations/<generation-id>/sing-box.json
  config/active.json
  logs/events.ndjson
  icons/<instance-id>.<content-hash>.ico
  backups/ bin/
```

`state/launch.json` 在同一次受保护原子替换中保存启动请求别名、attempt 和确认会话，避免请求映射与进程回执分开提交。只保存依赖摘要、资源 key、实际网络绑定和完整进程身份，不保存 argv/env 原文。文件上限 8 MiB，最多 4096 个请求映射；可释放的终态保留 7 天，未决及尚未确认退出的会话不按时间清理。容量耗尽拒绝新增请求，但仍可查询、重放、取消和核对已有记录。

manifest 是单个权威配置快照。第一次创建只接受空目录或正确归属标记；ACL 限制为当前用户、管理员及系统。读到未知 schema、损坏 JSON 或 owner 不匹配时停止写入，提供诊断，禁止初始化成空配置掩盖错误。

机密按不可变 secret-id 单独保存，改机密创建新 ID；manifest 引用新 ID 后再延迟清理旧值。首版沿用 ACL 保护的本机明文机密存储，不宣称加密保险箱；sing-box 运行配置同样受 ACL 保护。持久化 JSON 参数也视为敏感存储。默认配置导出排除 secrets、机器绝对路径、登录数据和运行状态；导入要求重新绑定资源。

订阅来源现有 `kind: "subscription"`、`url_secret_id`、独立 `revision` 和节点列表。节点 manifest 只保存 id、name、protocol、server、port、secret_id；URL 整体单独保存，节点完整连接参数（包括 UUID、密码、TLS/传输字段和路径）编码为版本 1 的严格 typed 秘密文档，最大 64 KiB。它不是原始订阅正文或任意 sing-box JSON。每个来源最多 4096 个节点，source revision 必须非零，选中 ID 必须存在；列表名称和 ID 不能重复。

读取与提交 store 时校验 URL 和全部节点秘密，节点文档须与 manifest 的名称、协议、服务器、端口一致，并重新通过协议组合校验；坏引用、未知字段、版本或损坏内容拒绝且不重置原文件。生成内核配置只解析所选节点的秘密，沿用每 profile 的固定入口/出口路由。启动依赖摘要包含所选节点的不可变 secret_id，来源 URL、来源 revision 和其他节点变化不使其误判为连接变化；既有手动代理摘要字节格式保留。

来源导入/刷新 staging API 现可生成只含引用的 ConfigRequest 和新增/删除/不支持节点摘要。解析由调用方先在锁外完成，staging 不提交 manifest、不启动应用或内核；来源 revision 与 URL 引用来自下载前快照，staging 使用当前全局 revision，提交时再执行 CAS。节点名和请求 ID 派生有域区分的 SHA256/UUIDv8，稳定重试不会重复创建秘密；终态重放也只读核验同一 ID 的秘密内容，换 URL/密码重用请求 ID 会拒绝。未变化节点直接复用旧秘密，新增秘密先写后引用，失败残留不擅自清理。下载协调服务、CLI 写入口及秘密延迟清理仍待接入。

MSIX LocalState 可能在 store 之外。每个 location 都记录自己的归属标记、namespace 和路径约束；不能用一个“必须位于 store 内”的检查错误拒绝合法包目录，也不能允许用户 arbitrary path 绕过归属校验。原版数据不接管、不清理。

**6. 写入与恢复**

普通配置写入：短锁内检查 expected_revision → 校验全部引用 → 在同卷同目录以 create-new 写临时文件 → flush → 原子替换 → 更新 revision。Windows 替换失败做有界退避；始终保留最后一份可读快照。原子替换不等于断电后绝对持久，恢复仍验证文件完整性和备份。

配置请求的去重记录存放在受相同 ACL 保护的 `state/requests/<request_id>.json`。纯配置编辑先保存包含前后快照摘要、目标配置和预期结果的 pending 记录，再替换 manifest，最后保存终态回执。重开 store 时，当前配置匹配前快照就完成提交，匹配后快照就只补回执；两者都不匹配、多个 pending 或终态版本超前于当前配置时，报告冲突并保留现场。同 ID 同请求返回原结果，同 ID 不同请求拒绝；业务拒绝也保留原结果。终态记录保留 7 天，未决记录不自动过期。输入超过大小限制或记录未成功写入不算已接受。此机制不负责进程、快捷方式或 IFEO 等外部副作用。

运行 journal 独立于配置 revision：先写 intent 再做外部副作用，写完成结果。事件日志仅供诊断，不能靠重放截断日志决定杀进程或重启。创建实例默认空目录可延迟至启动；若预创建目录后配置提交失败，只回收本次空暂存目录，不删除已有数据。

跨文件、跨进程、跨 store 的操作没有虚构的全局原子性；代理切换及本产品集成安装使用 operation journal 分阶段恢复。保存 manifest 与安装快捷方式/UAC 不能作为一个原子事务，创建结果分别记录主实体和附加操作状态。

保留当前版本、一个已确认 previous generation 及未决 operation 引用的配置；旧机密仅在无 manifest/备份/回退引用时才能进入显式清理。删除实例默认移除管理记录与本工具入口、保留数据。首版不实现隐式 purge。

**7. 校验规则与限制**

所有 ID 唯一、引用有效；同一 normalized endpoint 不能被重复声明为两个独立所有者；Guard enabled 必须绑定代理且模板可识别其代理参数；original 同一物理实例只有一个有效登记。自由显示名称可重复，快捷方式名称用 name + 短 ID 去重。

单个 manifest 上限 8 MiB；超出时拒绝写入并给出容量错误，不截断。revision 为单调 u64，溢出视为不可写。数据路径使用 PathBuf/OsString，Windows 边界不做 lossy 转换；首版磁盘 JSON 仅接受可无损表示为 Unicode 的用户输入路径，遇到其他路径明确拒绝。比较使用 Windows 路径语义和必要的文件身份，不能直接全局 lowercase 代替规范化。
