# ISSUE-0002 · Windows Shell 沙箱写入范围复验失败

- 类别 `Environment`
- 状态 `Open`
- 首次记录 2026-11-22

在本机 Windows 上，DSH `0.2.2-rc.5` 随附的 ACL 运行器对新目录的限制生效。测试目录先由 `workspace-write` 授权后，改用 `read-only` 仍可写入测试文件。两个已授权的临时工作区之间也观察到了写入。另一个仓库内测试目录在应用授权时返回 Win32 错误 5。

在本机用户临时目录创建两个独立工作区，只使用合成测试文件。分别测试全新目录的只读操作、由可写切换到只读的操作，以及一个工作区对另一个已授权工作区的操作。分别核对命令退出状态和宿主文件是否存在。以上结果由随附的官方运行器直接复现，Rust 宿主集成也复现了切换到只读后的写入。

dshr 在 Windows 保留执行前审批。后续需要在候选运行器上验证新目录、已授权目录、权限切换和跨工作区写入，再接入按沙箱结果决定审批的行为。[对齐计划](../plan/dsh-command-approval.md) · [DSH 进程沙箱参考](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/subsystems/sandbox.md)
