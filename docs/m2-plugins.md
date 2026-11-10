# 插件管理（2026-11-11）

M2 之前 sidecar 的插件列表是写死的两条（workspace-dependencies + 只读 fs）。
这一轮把它变成 **可持久化的 profile**：启用/禁用、配置、注入关系，并加了一个
右侧插件面板。

## Profile 文件

`<DSHR_PROFILE_DIR>/plugins.json`（默认 `.tmp/profile/plugins.json`）：

```json
{ "plugins": [
  {
    "name": "@deepseek-ai/dsh-tool-workspace-dependencies",
    "enabled": true,
    "config": { "source": "<primary-runtime>" }
  },
  {
    "name": "@deepseek-ai/dsh-tool-fs",
    "enabled": true,
    "inject": ["tools", "fs", "systemPrompt"],
    "config": { "read": true, "readImage": false,
                "write": false, "edit": false, "applyPatch": false }
  }
]}
```

- 结构贴近 DSH 的插件条目（name / enabled / config / inject）；
- `source` 在首次生成默认值时从 `DSHR_DSH_SOURCE` 或 `LOCALAPPDATA` 下的
  DSH `primary-runtime` 推断；
- `crates/profile` 负责 load/save/toggle/`sidecar_json()`，4 个单元测试。

## 与 sidecar 的连接

`chat` 启动时读取 profile，把启用项序列化成 `DSHR_DSH_PLUGINS` 再启动 sidecar
（`sidecar-dsh.mjs` 本来就支持这个环境变量，之前只是没人设）。
所以"装什么插件"从"改代码"变成"改 profile"。

## UI

- 侧栏底部 `插件 · 1/2 启用（点击管理）` 打开右侧面板；
- 面板列出每个插件的短名、启用状态、"禁用/启用"按钮；
- 切换时：
  1. 写 `plugins.json`；
  2. 更新 `DSHR_DSH_PLUGINS`；
  3. **空闲时重启当前会话的 sidecar**（先撤销旧 generation 再重新注册工具），
     生成中只保存、下次启动生效；
- 面板打开时侧栏自动收起，避免 280+300 把正文挤没（窄窗口尤其重要）。

## 自检

`chat --selftest-profile`：

1. 默认 profile 有两条；
2. 禁用 fs 后 `sidecar_json()` 不再包含它、`plugins.json` 持久化；
3. 有 vendored 依赖时启动真实 sidecar：禁用 fs → `read` 工具消失、
   workspace-dependencies 仍在；重新启用 → `read` 回来。

截图核对：右侧插件面板、禁用后 `插件 · 1/2 启用` 与按钮状态、`plugins.json` 同步。

## 边界

- **安装**：还没有 npm 安装流程。目前"安装"等价于：用 `tools/dsh-vendor.mjs`
  抽取官方包到 `sidecar/dsh-official/node_modules`，再在 profile 里加条目。
  真正的离线安装/签名/审计是下一阶段。
- **Loader YAML**：`cordis.yml` / `cordis.patch.yml` 的 bundle 树还没解析；
  现在等价于"手写 entry 列表"。这是"装插件"和"复刻 DSH 宿主"的分界线。
- `source` 在生成默认值时固化；DSH 安装路径变化后需要重新生成 profile。
