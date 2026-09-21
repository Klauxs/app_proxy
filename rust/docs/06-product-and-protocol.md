**用户流程、命令和通信契约**

用户面对的是应用实例与代理选择。阶段名、错误码和内部组件用于诊断；普通流程不要求用户理解 MSIX、环境变量或 ETW。

**1. 添加与日常使用**

保留当前 Codex/Claude 自动识别及代理创建串联。添加流程：选择 Codex/Claude/其他 → 确认原版或空白实例 → 名称 → 选择已配置代理/添加代理/明确直连 → 摘要 → 保存并完成附加操作。其他应用的路径、适配器、参数和 cwd 放高级输入。未识别或匹配多安装时让用户选择，不擅自选 EXE。

没有代理记录时仍明确提供直连/返回；选择添加代理后配置订阅或手动上游，自动发现可用 sing-box 程序文件并用我们的配置启动。找不到可用程序时在当前终端流程提示“未找到可用的 sing-box，是否安装？”，仅提供“安装并继续（默认）/返回”。选择安装后自动下载、校验、安装并继续原流程；程序决定安装位置，不提供手动指定 EXE 或安装路径选项，不让用户手工解压或配置 PATH。不提供接入已有 sing-box 服务选项。新 proxy 验证失败时保存它供修改，实例创建停在网络准备阶段并返回明确状态。不会自动改直连。

绑定代理的 Codex/Claude 预设默认开启 Guard；原版与分身均通过 ETW 启动后检查保护。其他支持 Chromium 代理参数的应用显式选用。创建原版时说明误启动可能被关闭并代理重启；只创建分身则不处理未管理原版。系统原始入口不被接管，外部启动可能在检查前已经联网。所需 UAC 只发生在用户前台操作。取消授权不抹掉已创建实例，结果展示保护未完成和可重试动作。低层 instance create 在非交互场景若需要授权，返回 requires_action，不后台弹 UAC。

实例列表至少展示名称、应用、原版/独立数据、网络绑定、运行状态、Guard 实际状态。菜单提供启动、详情、复制配置创建空白实例、改绑定、改名、高级设置、快捷方式、保护、移除登记。首版不做复制登录数据按钮。

应用详情在 Guard 内展示监听和扫描状态。原版与分身采用相同就绪标准；只创建分身时原版显示“未管理”。取消 IFEO 后不再提供相关注册、解除、修复、命令或状态。

运行中代理故障只提示故障并保留应用，不自动切直连；启动前的代理验证失败仍阻止新目标创建。

复制配置默认继承应用/模板、参数、环境设置和网络绑定，生成新 ID/目录；显示 Guard 默认建议，由本次前台流程完成授权/启用，不复制源实例的运行状态或隐式激活副作用。源实例的已保存 secret_ref 可复用，但不复制运行环境快照。

绑定/参数编辑在当前运行中显示“下次启动生效”。original → isolated 或反向转换不能原地重解释已有数据，创建新实例并保留旧记录。删除默认保留数据，原始应用不卸载。

**2. CLI 草案**

以下是可用命令形态的参考，不是首批实现清单。优先完成中文菜单、快捷方式启动和必要的故障恢复入口；命令按实际自动化或维护需要加入，不把每个内部组件都变成用户选项。菜单与 CLI 共用业务函数，不为参数预留未实现能力。

```text
app-proxy                         中文菜单
app-proxy status --json
app-proxy application discover [codex|claude]
app-proxy instance create --preset codex --data original --proxy <id>
app-proxy instance create --preset claude --data isolated --direct
app-proxy instance create --file <配置.json>
app-proxy instance clone <id> --name <名称> [--proxy <id>|--direct]
app-proxy instance inspect|remove <id>
app-proxy instance settings <id> [--json]
app-proxy instance edit <id> --file <patch.json> --revision <n>
app-proxy instance bind <id> <proxy-id|direct>
app-proxy launch <id> [--request-id <uuid>] [--json]
app-proxy launch inspect|cancel <attempt-id>
app-proxy instance stop <id>
app-proxy proxy list
app-proxy proxy add --file <配置.json>
app-proxy proxy refresh|inspect|remove <id>
app-proxy core discover|install|verify|start|stop|restart|check
app-proxy doctor [--instance <id>|--proxy <id>] [--json]
app-proxy guard enable|disable|status <id>
app-proxy guard events enable|disable|status
app-proxy uninstall [--keep-data]
```

统一支持 `--home <rust-store>`，默认 `%USERPROFILE%\AppProxy\data`。首次启动只接受空目录或有效 Rust 归属标记，不读取旧工具配置。机密放受保护输入文件或交互输入，不放命令行。详细命令 flags 在实现时由 clap 定义并同步 help，但不能改变本设计中的业务语义。

已实现的高级设置入口为“管理实例 → 高级设置”及 instance settings/edit。settings 只返回数量、有限变量名称、目录模式和全局 revision；edit 读取 ACL 验证后的最大 128 KiB 普通文件，省略/null 保持、args 空数组清空，env 支持 set/unset/inherit，字段详见 README。菜单的参数 JSON 与变量值隐藏输入，最终确认固定使用之前摘要的 revision。只影响下次启动，不触发应用或共享内核重启；应用已卸载仍可保存。文件解析错误不回显内容；不修改输入 ACL 或删除输入文件。

所有命令使用新模型，不提供旧 app 子命令兼容 alias。uninstall 默认保留数据，`--keep-data` 只是明确表达默认行为；首版没有 purge。只清理 Rust 归属可确认的入口、任务和托管组件。


2026-09-20 范围收敛：不做跨目录升级或移动后入口重定向，也不做 Windows 登录任务被外部删除后的专用检测、自愈或修复流程。这两项不再列为待办或发布门槛。保留固定目录使用、正常登录自启的创建/查询/移除，以及已接受但未完成操作的显式恢复。已有只读查询仍如实报告入口不可用，不能把历史创建回执当作当前就绪。

**3. IPC 编码和请求**

管道采用 4 字节 little-endian 长度 + UTF-8 JSON。单帧上限 1 MiB，先验证长度再分配；订阅正文和文件导入走单独受限文件/流式服务，不通过不断增大单帧传输。握手包含 protocol_major/minor、client/server version、store ID、session/epoch；major 不同拒绝，minor 只允许向后兼容的新增可选字段。

请求 envelope：`protocol_major, request_id, operation, expected_revision?, payload`。操作由固定 enum 解析，不能用字符串反射调用任意函数。响应 `request_id, status, result?, error?, attempt_id?`。创建和启动等写操作必须有 request ID；同 ID + 同规范化 payload 重放返回原结果，同 ID + 不同 payload 返回 REQUEST_ID_CONFLICT。

请求去重记录先落盘再产生副作用，首版保留终态记录 7 天；未决记录不按期限清除。过了保留期不再承诺 request ID 历史去重，但实例身份检查仍防当前进程重复启动。RPC 超时后客户端查询同一个 ID，不生成新 ID 重试副作用。

客户端取消订阅事件不取消任务。配置编辑用 expected_revision；冲突响应携带当前 revision 和冲突实体 ID，不回显完整配置或秘密。程序启动参数等敏感 payload 不写 access log。

当前 Rust coordinator 协议为 3.20。CoreStatus/CoreRequestStatus/ControlCore 双向要求 minor 至少为 20，覆盖 PrepareRemove、RecoverStart、ProfileRemoved、Reconciled 和移除影响字段。移除 GuardStatus.ifeo 和 Ifeo 启动来源属于不兼容变更，major 升为 3；旧 major 在握手时拒绝。更新需使用同一代 CLI/host/coordinator，不自动终止旧进程。启动、启动请求查询及显式取消要求服务端 minor 至少为 7，带 expected_revision 的启动要求至少为 8，Guard 状态要求至少为 10。订阅协议扩展后的 Catalog 要求两端 minor 至少为 11；订阅 Refresh/Select 配置动作和 PrepareSubscription 要求两端至少为 12；下载预览及 stage 要求至少为 13；保存节点列表要求至少为 14；独立只读 RuntimeStatus 要求两端至少为 15；ShortcutApply/Resume/Request/Status/Check 要求两端至少为 19（新增 Repair/Repaired，拒绝旧客户端误解恢复状态）；InstanceSettings 和 EditInstance 要求两端至少为 17；LoginApply/Resume/Request/Status 要求两端至少为 18，在 major 3 内 GuardStatus 门槛仍为 10。版本不足时不接纳对应请求。旧客户端不会收到无法解析的新协议枚举，旧服务端也不会被当作支持新操作。新启动请求只包含已登记实例 ID、来源及可选版本条件，回执不含参数、环境值或凭据；接纳 ACK 不表示创建成功，客户端查询持久 attempt 阶段。订阅、快捷方式 CLI 和日常中文菜单已接入，菜单与 CLI 共用业务函数；独立运行状态已接入菜单列表和 instance inspect，配置、进程与保护证据分别展示；高级设置已接入同一配置事务、CLI 和菜单；维护入口仍按本章设计继续实现。

快捷方式 RPC 不接受可选文件位置或启动器参数。ShortcutApply 的外层请求 ID 与内层 ID 必须一致；Remove 还需 expected_creation，对显示的原创建进行原子核对。Status 返回当前登记及优先的待删除操作，Created 回执是历史证据，不证明文件现在仍在。Request 只读；Resume 使用原意图且不重做安装解析。断线/超时/Ctrl+C 不当作取消服务工作，JSON 未确认报告保留原 ID；没有记录也不推断锁外准备已经停止。实际工作单槽有界，忙时额外变更立即拒绝，状态连接仍可使用。

**4. 事件及证据**

事件字段为 `protocol_major, sequence, at, attempt_id, instance_id, type, stage?, elapsed_ms?, result?`。sequence 在一个 attempt 内递增；诊断日志滚动不影响持久化 attempt 状态。客户端接收不到全部事件时主动拉取快照；慢客户端不阻塞启动，事件缓冲达到上限发送 resync_required。

UI/CLI JSON 结果分开表达：

```json
{
  "attempt_status": "confirmed",
  "process_status": "running",
  "proxy_evidence": "proxy_request_verified",
  "target_traffic_evidence": "not_observed"
}
```

此例是协议说明，不是实测成功。对应中文“应用进程已确认；代理入口验证通过；尚未验证应用实际流量”。doctor 发出的出口请求只能更新工具的 proxy_evidence；不能升级 target_traffic_evidence。

**5. 稳定错误及退出码**

错误 DTO 为 `code, stage, retryability, safe_message, entity_id?, attempt_id?, action?`。系统 HRESULT/Win32 code 可作为安全数值附带；原始 shell 输出、URL 和任意配置值不进入 safe_message。retryability 为 never、after_configuration_change、after_resource_change、query_existing_attempt；不使用一个布尔值暗示所有超时都能重试。

| 错误类别 | 典型 code | CLI exit |
|---|---|---|
| 成功/已确认的同配置已运行 | OK、ALREADY_RUNNING_MATCH | 0 |
| 输入和引用无效 | INVALID_ARGUMENT、BINDING_NOT_FOUND | 2 |
| 依赖/安装/代理不可用 | APP_NOT_INSTALLED、PROXY_UNAVAILABLE | 3 |
| 已运行但配置不同/配置竞争 | INSTANCE_BUSY、CONFIG_CHANGED、REVISION_CONFLICT | 4 |
| 前台动作或权限缺失 | AUTHORIZATION_REQUIRED、ACCESS_DENIED | 5 |
| 结果未知或仍在执行 | LAUNCH_INDETERMINATE、OPERATION_PENDING | 6 |
| 内部异常/存储损坏 | STORAGE_CORRUPT、INTERNAL | 10 |

更具体的 Guard 事件记录 `GUARD_STOPPED_PROXY_UNAVAILABLE`、`GUARD_RESTART_FAILED`、`GUARD_IDENTITY_UNKNOWN`、`GUARD_RATE_LIMITED`。它们分别说明动作结果，不用一个“保护成功”掩盖重启失败。

**6. 日志和诊断包**

NDJSON 日志采用字段白名单：时间、版本、store/instance/attempt ID、阶段、时长、错误类别、数字系统码、监听方式及队列丢弃计数。可保留本地监听端口用于排障；路径默认只显示类别和文件名，用户显式本机详情可看完整路径。日志每个文件 2 MiB、保留 5 份，容量限制不影响 journal。

导出诊断包包含版本、脱敏配置摘要、最近阶段记录和监听状态；排除 secrets、订阅正文、sing-box 配置、用户数据、MSIX request、任意环境值与参数值。采集和保存仅本地，上传属于独立用户动作。核心原始错误默认不落盘，诊断应优先保留 stage、字段类别、节点序号及系统错误码，避免只有模糊失败。

2026-09-20：shortcut check 只读核验原链接身份/内容、启动器文件和缓存图标；shortcut repair 使用独立 ID，在原位置恢复丢失链接。恢复绑定原创建与 revision，受持久状态和单变更槽保护；丢应答后查询或继续原请求。历史创建及恢复回执不重新发布后来被删除的链接。已修改/替换链接、坏图标、缺 host、移动 home 不自动覆盖或猜测修复；整体卸载仍待实现；跨目录升级和登录任务被删除后的修复已取消。
