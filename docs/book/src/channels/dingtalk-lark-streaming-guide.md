# 钉钉与飞书 AI 卡片流式响应使用指导

本文档说明 ZeroClaw 在 **钉钉** 和 **飞书 / Lark** 两个 IM 渠道上启用 LLM 流式响应的方法。钉钉使用 AI 卡片流式更新 API，飞书使用交互式卡片草稿更新 API。两者都通过节流窗口合并 LLM 中间帧，避免触发 IM 平台限流。

---

## 1. `stream_mode` 可选项

钉钉和飞书共用同一个枚举 `StreamMode`,可选项如下:

| 取值 | 含义 | 钉钉行为 | 飞书行为 |
|------|------|---------|---------|
| `"off"` | **不启用流式**(默认) | LLM 生成完整响应后,通过普通 `send` 一次性发送,不会创建 AI 卡片 | 同上,一次性 PATCH 一条消息,不会创建草稿卡片 |
| `"partial"` | **启用流式**(推荐) | LLM 流式生成时,创建 AI 卡片并通过 `streamingUpdate` 增量更新,LLM 流结束时 `finalize_draft` 固化 | 创建可编辑的交互式卡片,通过 `update_draft` 增量 PATCH,`finalize_draft` 固化 |
| `"multi_message"` | **多消息流式** | **不支持** — 钉钉 API 没有多消息流式表面,daemon 自动降级为 `"off"` 并打 WARN 日志(`DingTalk: stream_mode=multi_message is not supported; falling back to off`) | **不支持** — 飞书 API 限制,自动降级为 `"off"` |

**选择建议**:

- 大多数 IM 场景用 `"partial"` — 用户能看到"打字机"效果。
- 简单回显 / 不想消耗额外 API 配额 — 用 `"off"`,LLM 整段生成完一次性发。
- 不要写 `"multi_message"` — 写了也会被自动降级,徒增一条 WARN 日志。

**示例**:

```toml
# 钉钉 / 飞书通用
stream_mode = "partial"   # 启用流式 (推荐)
# stream_mode = "off"    # 关闭流式,完整响应一次性发
# stream_mode = "multi_message"  # 不要写,会被降级
```

配置完后重启 daemon 生效。

---

## 2. 钉钉


### 2.1 申请 AI 卡片模板（前置步骤）

钉钉的 `stream_mode = "partial"` 必须配合一个**已在钉钉开放平台创建并发布**的 AI 卡片模板才能生效，步骤如下：

1. **登录钉钉开放平台**  
   访问 https://open.dingtalk.com/ ，用企业管理员账号登录。

2. **进入企业应用**  
   左侧导航 → **应用开发** → **企业自建应用** → 选中你要接入的机器人应用（没有就先创建一个，类型选 "企业内部应用"）。

3. **添加 AI 卡片模板**  
   左侧导航 → **卡片平台** → **我的模板** → **新建模板**。  
   - 模板类型选 **AI 卡片**（不是 "普通消息卡片"）  
   - 回调类型选 **STREAM**（streaming 模式）  
   - 在模板编辑器里添加一个 **Markdown** / **富文本** 控件，**变量 key 必须叫 `content`**（钉钉流式 API 只会更新名为 `content` 的字段，其他字段名不会更新）  
   - 模板可加 status / title 等辅助字段，按需设置

4. **发布模板**  
   模板编辑完成后点 **发布**，钉钉会分配一个模板 ID，格式形如：  
   ```
   142127fd-cfc7-4236-a6a4-957c04587311.schema
   ```
   把这串 ID 复制下来，填到 `config.toml` 的 `ai_card_template_id`。

5. **把应用发布到目标组织**  
   应用开发 → 版本管理与发布 → 创建新版本 → 填写版本号和说明 → 发布。  
   只有发布后模板才能在企业内被机器人调用。

> **常见失败原因**：
> - 模板类型选成"普通消息卡片"而不是"AI 卡片"→ streaming 会被拒。
> - Markdown 控件 key 不是 `content`→ 流式更新会失败，daemon 日志里会报 `card instance create failed` 或 `streaming update failed`。
> - 模板未发布→ 报 "template not found" 类错误。

### 2.2 钉钉 config.toml 配置

最小可用配置（把下面这段加到 `config.toml`）：

```toml
[channels.dingtalk.default]
enabled = true
client_id = "你的 AppKey"
client_secret = "你的 AppSecret"

# 启用 AI 卡片流式响应
stream_mode = "partial"
streaming_update_interval_ms = 1000   # 见下方"调优"节
ai_card_template_id = "142127fd-cfc7-4236-a6a4-957c04587311.schema"
```

### 2.3 钉钉字段说明

| 字段 | 必填 | 默认 | 说明 |
|------|------|------|------|
| `stream_mode` | 否 | `"off"` | `"off"` = 不启用流式；`"partial"` = 启用 AI 卡片流式；`"multi_message"` 不支持（自动降级为 `off` 并 warn） |
| `streaming_update_interval_ms` | 否 | `1000` | 连续两次 streamingUpdate 调用的最小间隔（毫秒） |
| `ai_card_template_id` | **是**（流式时） | `None` | 钉钉开放平台创建的 AI 卡片模板 ID，格式 `<uuid>.schema` |

### 2.4 钉钉调优（`streaming_update_interval_ms`）

| 场景 | 推荐值 |
|------|--------|
| 短对话 / 高频问答 | `500–800ms` |
| 长文档 / 报告生成 | `1000–1500ms` |
| 高并发 / 大集群 | `1500–2000ms` |
| 企业版（更高 QPS 配额） | `200–400ms` |

调到 LLM 单 token 平均生成时间的 1.0–1.5 倍效果最好。

---

## 3. 飞书 / Lark

### 3.1 前置步骤

飞书**不需要**额外创建 AI 卡片模板——runtime 自动用 `Card JSON 2.0` schema + `markdown` 元素构造草稿卡片。只需要：

1. 在 [飞书开放平台](https://open.feishu.cn/) 或 [Lark Developer](https://developers.larksuite.com/) 创建企业自建应用。
2. 在应用后台拿到 `App ID` / `App Secret`。
3. 开启 **机器人** 能力。
4. 配置事件订阅：
   - WebSocket 模式（推荐）：应用后台 → 事件订阅 → 订阅方式选 **使用长连接接收事件** → 勾选 `im.message.receive_v1` 等事件类型。
   - Webhook 模式：填回调 URL 和 Encrypt Key / Verification Token，本地 daemon 暴露对应端口。
5. 把应用发布到目标企业（版本管理与发布 → 创建版本 → 发布）。

### 3.2 飞书 config.toml 配置

```toml
[channels.lark.default]
enabled = true
app_id = "cli_xxxxxxxxxxxx"
app_secret = "你的 AppSecret"
# use_feishu = true   # true=飞书（默认），false=Lark 国际版

# 启用交互式卡片流式响应
stream_mode = "partial"
draft_update_interval_ms = 1000

# 收消息方式
receive_mode = "websocket"  # 或 "webhook"
# port = 8080  # 仅 webhook 模式需要
```

### 3.3 飞书字段说明

| 字段 | 必填 | 默认 | 说明 |
|------|------|------|------|
| `app_id` | 是 | — | 飞书应用 App ID，格式 `cli_xxx` |
| `app_secret` | 是 | — | 飞书应用 App Secret |
| `stream_mode` | 否 | `"off"` | `"off"` = 不启用；`"partial"` = 启用交互式卡片草稿流式；`"multi_message"` 不支持 |
| `draft_update_interval_ms` | 否 | `1000` | 连续两次 PATCH 同一张卡片的最小间隔（毫秒） |
| `receive_mode` | 否 | `"websocket"` | 收消息方式：`"websocket"`（长连接，推荐）或 `"webhook"` |
| `use_feishu` | 否 | `true` | `true` 走 `open.feishu.cn`（中国版飞书），`false` 走 `open.larksuite.com`（Lark 国际版） |
| `port` | 否 | — | Webhook 模式监听端口 |
| `encrypt_key` | 否 | — | 消息加密密钥（推荐启用） |
| `verification_token` | 否 | — | Webhook 验证令牌 |
| `mention_only` | 否 | `false` | 群聊中仅响应 @ 机器人的消息 |
| `per_user_session` | 否 | `false` | 群聊中每人独立会话 |
| `approval_timeout_secs` | 否 | `300` | 审批卡片等待超时（秒） |
| `proxy_url` | 否 | — | 代理服务器 URL |
| `excluded_tools` | 否 | `[]` | 排除的工具列表 |

### 3.4 飞书调优（`draft_update_interval_ms`）

| 场景 | 推荐值 |
|------|--------|
| 短对话 / 高频问答 | `500–800ms` |
| 长文档 / 报告生成 | `1000–1500ms` |
| 高并发 / 大集群 | `1500–2000ms` |
| 企业版（更高 QPS 配额） | `200–400ms` |

---

## 4. 两边行为对比

| 维度 | 钉钉 | 飞书 / Lark |
|------|------|-------------|
| 流式 API | `PUT https://api.dingtalk.com/v1.0/card/streaming` | `PATCH https://open.feishu.cn/open-apis/im/v1/messages/{id}` |
| 卡片创建 | AI 卡片实例（两步：create + deliver） | 普通交互式卡片（单步） |
| 流式内容模式 | `isFull: true` 整段替换 | 整段 markdown 替换 |
| 节流字段 | `streaming_update_interval_ms`（默认 `1000`） | `draft_update_interval_ms`（默认 `1000`） |
| 错误处理 | 软失败 + warn（不打断流） | 软失败 + warn（不打断流） |
| 401 token 刷新 | 下次 update 时重取 token | 401 → 刷新 tenant token → 重试 |
| 收尾 | `isFinalize: true` | PATCH 整段 |
| 取消 | `isFinalize: true` + `"[回答已取消]"` | PATCH `_(cancelled)_` |
| 占位文案 | `正在思考中…` | `_processing…_` |
| 需要 AI 卡片模板 | **是**（`ai_card_template_id`） | 否（runtime 自动构造） |
| 官方文档 | https://open.dingtalk.com/document/orgapp/interface-for-creating-a-card-instance | https://open.feishu.cn/document/server-docs/im-v1/message-card/update |

---

## 5. 故障排查

### 5.1 钉钉：完全没流式效果

- 检查 `stream_mode = "partial"` 且 `ai_card_template_id` 不为空。
- daemon 日志应出现 `DingTalk: AI card created successfully` 与 `DingTalk: send_draft opened streaming card`。
- 如果出现 `send_draft failed, falling back to non-streaming send()` → AI 卡片创建失败，常见原因：
  - 模板类型不是 "AI 卡片"
  - 模板未发布
  - 应用未发布到目标企业
  - 模板 Markdown 控件 key 不是 `content`

### 5.2 钉钉：卡片创建了但内容不更新

- 确认 `Delivering AI card` → `AI card delivered` 两个日志都出现。
- 如果 `AI card deliver failed` 出现，看 HTTP 状态码和响应体里的 `code` / `message`：
  - `openSpaceId` 格式不对（runtime 已用 `dtv1.card//IM_ROBOT.{recipient}`，如仍报错可贴错误体里的 `code` / `message` 反查）
  - 机器人未在用户单聊窗口被激活（用户从未点击"开始使用"）

### 5.3 飞书：完全没流式效果

- 确认 `stream_mode = "partial"`，`app_id` / `app_secret` 正确。
- daemon 日志应无 `Lark: send_draft non-success`；如果 fallback 到 `send()` → 应用权限不足，去飞书开放平台检查应用是否勾选 `im:message` 等 scope。

### 5.4 飞书：PATCH 报限流

- 日志里出现 `Lark: draft PATCH rate-limited` 说明触发了飞书 5 QPS 限流（错误码 `230020`）。runtime 已自动 drop 该次 PATCH，下一个窗口到了会继续，可考虑增大 `draft_update_interval_ms`。

### 5.5 流式"卡顿" / 文字出现得太慢

- LLM 真实生成速度 < `interval_ms` 是正常合并；LLM 慢时（典型如 thinking 模型）增大 `interval_ms` 反而首字延迟更大，建议把 `interval_ms` 调到 LLM 单 token 平均间隔的 1.0–1.5 倍。
- 钉钉无 token refresh retry，单次失败软 warn 不重试，下一次窗口到了会重发。

### 5.6 流式"飞快" / 看着不流畅

- `interval_ms` 调到 `800–1000`，等齐 LLM 大约 1 个 token 的生成时间。
- 检查 `runtime_profiles.default.thinking.default_level`，`"medium"` / `"high"` 开启 thinking 会显著拖慢首字延迟。

### 5.7 关键日志关键字

| 关键字 | 含义 |
|--------|------|
| `DingTalk: AI card created successfully` | AI 卡片实例创建成功 |
| `DingTalk: Delivering AI card` | 准备 deliver 卡片到用户 |
| `DingTalk: AI card delivered` | 卡片已 deliver |
| `DingTalk: AI card deliver failed` | deliver 失败 |
| `DingTalk: send_draft opened streaming card` | 进入流式模式 |
| `DingTalk: send_draft failed, falling back to non-streaming send()` | fallback 到非流式 |
| `DingTalk: update_draft flush` | 节流窗口到了，flush 累积 buffer |
| `DingTalk: finalize_draft streaming card` | LLM 流结束，finalize 卡片 |
| `Lark: send_draft non-success` | 飞书 send_draft 返回非 0 业务码 |
| `Lark: draft PATCH rate-limited` | 飞书 230020 限流（自动 drop） |
| `Lark: draft PATCH transport-failed` | 飞书 PATCH 传输失败（软失败） |

---
