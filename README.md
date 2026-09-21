# AppProxy

Windows x64 应用实例与代理管理工具。当前实现位于 `rust/`，产品名称统一为 **AppProxy**。

双击 `AppProxy-Setup.exe` 安装。前台与后台程序已嵌入这个单文件安装包，不需要另外下载。安装结束后从开始菜单打开 **AppProxy**，选择应用、配置代理并创建实例；需要监听保护时按提示完成 Windows 授权。

所有文件集中在 `%USERPROFILE%\AppProxy`：

| 内容 | 固定位置 |
|---|---|
| 程序 | `%USERPROFILE%\AppProxy\app` |
| 配置、订阅、实例数据及托管内核 | `%USERPROFILE%\AppProxy\data` |
| 跨 store 的资源占用记录 | `%USERPROFILE%\AppProxy\resources` |
| 受保护监听组件，按用户、store 和 generation 分目录 | `%USERPROFILE%\AppProxy\guard` |

升级时退出 AppProxy 菜单，运行新版 Setup。安装器等待协调进程结束当前工作后成套替换程序，再更新监听及核验启动入口；已有应用和 sing-box 保留。授权取消或流程中断时保留配置，按安装提示重新运行同一个安装包继续。

已有旧脚本版数据占用默认数据目录时会提示冲突，不自动覆盖或导入。开发版 `AppProxyRust` 数据不自动迁移。安装包当前未签名，不含卸载向导。

开发构建安装包：

```powershell
.\rust\scripts\package.ps1
```

产物：`rust\target\release\AppProxy-Setup.exe`。只需分发这一个文件。`target/package` 是构建缓存，不是需要另行安装的程序目录。

- [安装与升级设计](rust/docs/16-installation-and-upgrade.md)
- [使用与开发说明](rust/README.md)
- [验证记录](rust/TEST-RESULTS.md)

旧的脚本实现（`windows/` 的 Node/PowerShell 版和 `AppProxyInstaller-scripts/` 的 macOS 脚本版）及其历史文档（`CHANGELOG.md`、`Windows-App-Proxy-*.md`、`Launcher-开源调研与流程建议.md`）已从仓库移除，需要时从 git 历史查看。
