# 桌面模型连接设置

> 已完成，Windows 发布构建验收通过。参考本机 DSH Desktop `0.2.2-rc.5` 的模型提供商列表、编辑表单和凭据状态。

| 阶段 | 工作 | 验收 |
|---|---|---|
| 1 | 为 DeepSeek 和 OpenAI 兼容提供商接入显式连接参数，复用现有 Agent 和模型选择。 | 连接校验、模型切换与环境配置回归测试。 |
| 2 | 在设置的模型页编辑地址、模型 ID 和 API Key。使用 GPUI 系统凭据库保存各数据根的连接。 | 实窗验证掩码、保存、编辑、重启加载与移除。 |
| 3 | 通过桌面发送本地 HTTP 测试请求，核对收到的地址、模型和凭据。补充用户指南。 | Windows 发布构建、相关单测和文档链接检查。 |
| 4 | 支持多个 OpenAI 兼容模型，保留原有连接和首选项。参考提供商的模型列表，支持从现有连接沿用同源密钥。 | 在实际窗口新增、编辑、切换、恢复和删除单条连接，用两个网关确认同名模型的路由。 |
| 5 | 桌面默认只列出可用的真实模型。无模型时从输入区进入配置，草稿保留到模型可用后再发送。测试通过 `DSHR_FORCE_MOCK=1` 或 `DSHR_PROVIDER=mock` 显式启用本地模拟。 | 核对默认、真实模型和显式模拟三种启动状态。旧设置中的模拟选项不会在普通启动时恢复。 |

参考入口为安装包中 `dsh-client-ui-settings-models/src/client/ModelsSection.tsx`、`ProviderEditor.tsx`、`ApiKeyInput.tsx` 和 `ModelListEditor.tsx`。DSHR 为每个 OpenAI 兼容模型独立保存地址、模型 ID 和密钥。新增连接在数据根下登记系统凭据引用的 ID。旧版模型设置中的 `deepseek` 和 `openai` 仍对应原有凭据。环境变量配置继续用于命令行与桌面启动。

## 验收命令

```powershell
tools\build-env.cmd cargo build --offline --locked --release -p dshr-desktop-gpui --bin dshr
tools\build-env.cmd cargo test --offline --locked -p dshr-desktop-gpui -p dshr-ui-gpui -p dshr-llm-deepseek -p dshr-llm-openai --lib --bins
tools\build-env.cmd cargo test --offline --locked --release -p dshr-desktop-gpui --test headless_cli
node tools/check-links.mjs
```

## 验收结果

- Windows 浅色窗口和深色窄窗均打开并检查了模型页，保存按钮在普通窗口中完整可见。
- 本地 Chat Completions 测试服务收到 DeepSeek SSE 和 OpenAI 兼容请求，核对了 URL、模型 ID 和鉴权。保存后会话可直接使用，重启后恢复所选模型。
- 已验证同源编辑沿用密钥、更换端口要求填写新密钥，以及移除连接后回退到可用模型。重启后两条临时连接均已清除。
- Windows 发布窗口读取了旧版 OpenAI 连接并正常发送请求。新增两个模型后，两个本地网关收到了预期模型 ID、请求路径和各自的鉴权。模型菜单用地址区分同名模型，重启恢复了设置中选定的连接。
- 深色窄窗验证了从已有连接新增模型和跨端口密钥校验。编辑模型 ID 沿用了同源密钥。逐条移除当前连接和旧版连接后，剩余模型仍可发送请求。
- 普通启动的窗口即使读到旧的模拟偏好，也显示「配置模型」。配置前回车保留草稿，配置后通过本地网关成功发送。移除最后一条连接后恢复未配置状态。两种显式模拟环境变量均在发布版窗口显示本地模拟并生成回复。
- 151 项相关单元测试、9 项 Headless 回归测试和文档链接检查全部通过。
