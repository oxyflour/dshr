# 沙箱 seam（2026-11-11）

审批解决的是"用户同不同意"，沙箱解决的是"同意之后能碰什么"。这一轮先把后者的
最小底座做出来，并用一个原生写入工具跑通整条链路：

```text
probe_write（presentation=diff）
  → ToolPolicy: Ask
  → ApprovalBroker: 用户点"允许一次"（或 DSHR_APPROVAL_MODE=allow）
  → PluginHost::call_approved（只做 schema 校验，不再问策略）
  → Sandbox::Workspace::write_text
      路径根校验 → 大小限制 → 写前快照 → 原子写
```

## 工作区沙箱（`crates/sandbox`）

- **路径根**：只接受相对路径；绝对路径、`..`、Windows prefix 一律拒绝。
  父目录必须存在，且 `canonicalize` 后仍在工作区内——防符号链接逃逸。
- **写前快照**：覆盖已有文件前复制到 `<root>/.dshr/snapshots/<millis>-<name>`，
  返回 `SnapshotRef`，可 `restore` 回滚。
- **大小限制**：单次写入 ≤ 1 MiB（`MAX_WRITE_BYTES`）。
- **原子写**：同目录临时文件 + rename，避免半截文件。
- 工作区根：`DSHR_WORKSPACE`，默认当前目录。

5 个单元测试：写入/回执、覆盖前快照与恢复、绝对与逃逸路径、缺父目录/超限、
嵌套目录。

## 原生写工具 `probe_write`

- 注册在 `builtin.probe-write`，参数 `{ path, content }`，展示类型 `diff`；
- 返回 `{ path, bytes, snapshot }`；沙箱错误转成工具错误（`is_error=true`）；
- 默认策略下 `diff` 展示类型自动 Ask，所以它必须经过审批才会落盘；
- `ChatExecutor` 审批通过后走 `PluginHost::call_approved`（只做 schema 校验），
  避免"再次 authorize 又要求审批"的死循环。

## 自检

`chat --selftest-sandbox`：

- 在工作区写 `demo.txt`、覆盖一次（产生快照）、尝试 `../escape.txt`（被拒）；
- 断言文件内容为最新值、快照文件存在、三条审批记录、逃逸写入 `is_error`。

## 官方写入工具为什么还没打开

官方 `write` / `edit` / `apply_patch` 执行时调用的是 **DSH 自己的** `ctx.fs` 与
`ctx.sandbox` / `ctx.userApproval` 服务。要打开它们，需要在 Node 侧实现这些 service
的完整语义（版本、快照、审批 RPC 回 Rust），而不是复用我们这一层的 Rust 沙箱。
当前 sidecar 只提供只读 `fs`，官方写工具配置仍是关闭的。

下一步（D 的收尾或 E 之前）：

1. Rust → Node 的审批 RPC（sidecar 消息 `approval.request` / `approval.answer`），
   让官方工具也能弹同一张审批卡片；
2. Node 侧实现 `fs` 的写入方法（带工作区根与快照目录），接上 `dsh-fs` 的接口；
3. 打开官方 `write`（先不带 `bash`），用真实模型验证；
4. `bash`/`pwsh` 需要命令白名单与超时，放在更后面。
