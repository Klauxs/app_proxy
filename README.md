# App Proxy

Windows 实现位于 `windows/`；`AppProxyInstaller-scripts/` 是 macOS 原版只读参考。

- 使用说明：[windows/README.md](windows/README.md)
- 当前设计：[Windows-App-Proxy-设计与实现.md](Windows-App-Proxy-设计与实现.md)
- 实测记录：[windows/TEST-RESULTS.md](windows/TEST-RESULTS.md)
- 决策与改动：[CHANGELOG.md](CHANGELOG.md)

实现计划及最初交接文档保留历史背景；与当前实现冲突时，以当前设计、使用说明和验证记录为准。

## 本地版本管理

2026-09-18 建立 Git 基线，保存当前源码、macOS 参考、文档和测试。建立仓库之前的逐次改动无法还原为真实提交历史。Git 不会自动保存聊天，关键决策须写入上述文档再提交。

依赖、运行时二进制、发行包、测试临时数据及认证截图不入库；依赖通过 `npm ci` 恢复，运行时及发行包通过 `npm run package` 生成。本地仓库没有远端备份。
