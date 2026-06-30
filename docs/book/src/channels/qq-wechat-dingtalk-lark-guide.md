# QQ、微信、钉钉、飞书渠道使用指导

本文档详细介绍 ZeroClaw v0.8.x 中 QQ、微信个人号、钉钉、飞书（Lark）四个主要即时通讯渠道的配置和使用方法。

## 目录

- [v0.8 版本说明](#v08-版本说明)
- [QQ 官方机器人](#qq-官方机器人)
- [微信个人号 iLink Bot](#微信个人号-ilink-bot)
- [钉钉 Stream Mode](#钉钉-stream-mode)
- [飞书/Lark](#飞书lark)
- [常见问题](#常见问题)

> **关于 AI 卡片流式响应**：钉钉和飞书的 `stream_mode` / `streaming_update_interval_ms` / `draft_update_interval_ms` / `ai_card_template_id` 等流式相关配置、机制和故障排查已统一在《[钉钉与飞书 AI 卡片流式响应使用指导](./dingtalk-lark-streaming-guide.md)》里维护，请跳转阅读。

---

## v0.8 版本说明

### 当前架构版本

ZeroClaw v0.8.x 使用 **Schema Version 3** 配置架构。

**检查你的配置版本**：
```toml
# config.toml 文件开头
schema_version = 3  # v0.8 当前版本
```

**v0.8 重要变更**：

1. **Provider 配置重构**
   - 旧版：`[model_providers]` 直接定义
   - v0.8：`[providers.models]` 结构化定义
   - 迁移：自动执行（启动时检测并升级）

2. **渠道配置标准化**
   - 所有渠道统一使用 `[channels.<type>.<alias>]` 格式
   - 支持多实例（如 `[channels.qq.default]`, `[channels.qq.secondary]`）
   - 新增 `excluded_tools` 配置项

3. **安全配置增强**
   - 新增 `[security]` 顶层配置块
   - 支持审计日志、OTP、紧急停止等功能
   - 敏感字段自动加密存储

4. **微信渠道更新**
   - 新增 iLink Bot 支持（个人号自动化）
   - QR 码配对绑定机制
   - 状态持久化（token、游标）

5. **飞书/Lark 增强**
   - 支持流式响应（`stream_mode`）
   - 交互式卡片草稿更新
   - 审批卡片超时控制

### 自动迁移

ZeroClaw v0.8 会自动迁移旧版本配置：

```bash
# 启动时自动检测并迁移
./zeroclaw

# 日志输出示例：
# [system] INFO ... Migrating config from schema_version 2 to 3
# [system] INFO ... Config migration completed, backup saved to config.toml.backup
```

**手动迁移**（可选）：
```bash
# 使用迁移工具
zeroclaw migrate-config --input config.toml --output config-v3.toml
```

### 配置验证

v0.8 提供配置验证工具：

```bash
# 验证配置语法和结构
zeroclaw validate-config

# 输出示例：
# ✓ Config schema_version: 3
# ✓ All required fields present
# ✓ Secret fields properly masked
# ✓ No unknown keys detected
```

---

## QQ 官方机器人

### 概述

ZeroClaw 通过腾讯官方 QQ Bot SDK 实现 QQ 渠道集成，支持：
- 个人聊天（C2C）
- 群组聊天（需@机器人）
- 富媒体消息（图片、文件、语音、视频）
- 语音转文字（需配置）

### 前置条件

1. **注册 QQ 机器人**
   - 访问 [QQ 开放平台](https://q.qq.com/)
   - 创建机器人应用
   - 获取 `AppID` 和 `AppSecret`

2. **配置机器人能力**
   - 在 QQ 开放平台配置机器人权限
   - 启用"私聊消息"和"群聊消息"能力
   - 设置消息回调地址（可选，ZeroClaw 使用 WebSocket 主动连接）

### 配置方法（v0.8）

在 `config.toml` 中添加：

```toml
# v0.8 Schema Version 3 格式
[channels.qq.default]
enabled = true
app_id = "你的 AppID"
app_secret = "你的 AppSecret"

# v0.8 新增：代理配置
proxy_url = "http://proxy.example.com:8080"

# v0.8 新增：工具排除
excluded_tools = ["shell", "file_write"]

# v0.8 新增：语音转文字配置
[channels.qq.default.transcription]
enabled = true
provider = "groq"  # 或 "whisper", "azure" 等
```

**v0.8 变更说明**：
- `excluded_tools`：新增字段，控制渠道级别工具可见性
- `proxy_url`：支持每渠道独立代理配置
- `transcription`：内嵌转录配置（之前在全局配置）

### 配置项说明

| 字段 | 必填 | 说明 |
|------|------|------|
| `enabled` | 是 | 是否启用该渠道，默认 `false` |
| `app_id` | 是 | QQ 开放平台的 AppID |
| `app_secret` | 是 | QQ 开放平台的 AppSecret（敏感信息） |
| `proxy_url` | 否 | 代理服务器 URL，支持 http/https/socks5 |
| `excluded_tools` | 否 | 不在此渠道暴露的工具列表 |

### 功能特性

#### 1. 消息处理
- **文本消息**：直接处理
- **富文本**：支持 QQ 原生富文本格式
- **@机器人**：群聊中必须@机器人才能触发响应

#### 2. 富媒体支持
ZeroClaw 使用 `[TYPE:path]` 标记语法处理富媒体：

```
[IMAGE:/path/to/image.png]
[VIDEO:/path/to/video.mp4]
[DOCUMENT:/path/to/file.pdf]
[VOICE:/path/to/audio.wav]
```

**发送示例**：
```toml
# 技能输出中包含上述标记即可发送对应媒体
```

**接收处理**：
- 自动下载附件到工作目录
- 生成对应的 `[TYPE:local_path]` 标记
- 语音消息优先使用 WAV 格式（`voice_wav_url`）

#### 3. 语音转文字
配置转录服务后，QQ 语音消息可自动转文字：

```toml
[transcription]
enabled = true
# 配置转录提供商...
```

支持的语音格式：
- 原生支持：`.wav`, `.mp3`, `.silk`
- 转录支持：`.flac`, `.m4a`, `.ogg`, `.opus`, `.webm` 等

#### 4. 上传缓存
为避免重复上传相同文件，ZeroClaw 实现了上传缓存：
- 缓存容量：500 条记录
- 基于文件内容哈希
- TTL 由 QQ API 返回

### 技术细节

#### 认证流程
1. 使用 `AppID` + `AppSecret` 获取 access_token
2. Token 缓存并自动续期（提前 60 秒刷新）
3. 失败重试机制（最多 4 次，指数退避）

#### WebSocket 连接
- 协议：QQ Bot WebSocket Gateway
- 心跳：自动维持连接
- 断线重连：支持会话恢复（resume）

#### 消息去重
- 基于消息 ID 去重
- 容量：10,000 条
- 自动淘汰旧记录

### 调试技巧

1. **启用 DEBUG 日志**：
```bash
RUST_LOG=zeroclaw_channels::qq=DEBUG ./zeroclaw
```

2. **查看连接状态**：
```
[qq.default] ... INFO ... authenticating...
[qq.default] ... INFO ... fetching gateway URL...
[qq.default] ... INFO ... connected and sent Identify
[qq.default] ... INFO ... session established (session_id=...)
```

3. **消息处理日志**：
```
[qq.default] ... DEBUG ... QQ: event received
[qq.default] ... DEBUG ... QQ: processing C2C message
[qq.default] ... DEBUG ... QQ: C2C message composed
```

---

## 微信个人号 iLink Bot

### 概述

ZeroClaw 通过微信 iLink Bot API 实现个人微信号的自动化，支持：
- 个人聊天
- 富媒体消息（图片、文件、语音、视频）
- 语音转文字
- 二维码配对绑定

### ⚠️ 重要说明

**iLink Bot 是微信官方提供的个人号自动化方案**，需要：
1. 在微信 iLink 开发者平台注册
2. 使用个人微信号扫描二维码绑定
3. 遵循微信的使用规范

### 前置条件

1. **注册 iLink Bot**
   - 访问微信 iLink 开发者平台
   - 创建 Bot 应用
   - 获取 Bot 凭证

2. **准备绑定微信号**
   - 一个可用的个人微信号
   - 该微信号需要扫描 Bot 二维码进行绑定

### 配置方法（v0.8）

在 `config.toml` 中添加：

```toml
# v0.8 Schema Version 3 格式
[channels.wechat.default]
enabled = true

# v0.8 新增：API 端点自定义
api_base_url = "https://ilinkai.weixin.qq.com"  # 默认，可选
cdn_base_url = "https://novac2c.cdn.weixin.qq.com/c2c"  # 可选

# v0.8 新增：状态持久化目录
state_dir = "~/.zeroclaw/wechat"  # 默认路径

# v0.8 新增：工具排除
excluded_tools = ["shell"]

# v0.8 新增：语音转文字（内嵌配置）
[channels.wechat.default.transcription]
enabled = true
provider = "groq"
```

**v0.8 变更说明**：
- `api_base_url` / `cdn_base_url`：支持自定义端点（测试/特殊网络环境）
- `state_dir`：明确状态持久化路径（之前硬编码）
- `excluded_tools`：新增渠道级工具控制
- `transcription`：内嵌转录配置，支持每渠道独立配置

### 配置项说明

| 字段 | 必填 | 说明 |
|------|------|------|
| `enabled` | 是 | 是否启用，默认 `false` |
| `api_base_url` | 否 | iLink API 基础 URL，默认自动 |
| `cdn_base_url` | 否 | CDN 基础 URL，默认自动 |
| `state_dir` | 否 | Bot token 和游标持久化目录 |
| `excluded_tools` | 否 | 排除的工具列表 |

### 绑定流程

#### 1. 启动 ZeroClaw

```bash
./zeroclaw
```

#### 2. 触发绑定

在 ZeroClaw 启动时，如果微信渠道未绑定，会自动进入绑定流程：

```
[wechat.default] ... INFO ... waiting for QR code scan...
```

#### 3. 扫描二维码

- ZeroClaw 会显示二维码（或保存到文件）
- 使用已登录的微信扫描二维码
- 在微信中确认绑定

#### 4. 绑定完成

```
[wechat.default] ... INFO ... bot token persisted
[wechat.default] ... INFO ... channel listening for messages...
```

**绑定信息持久化**：
- Bot token 保存到 `state_dir`
- 重启后无需重新绑定
- 会话游标（sync cursor）也会保存

### 功能特性

#### 1. 消息类型支持

| 类型 | 发送 | 接收 | 说明 |
|------|------|------|------|
| 文本 | ✅ | ✅ | 普通文本消息 |
| 图片 | ✅ | ✅ | JPG/PNG/GIF 等 |
| 语音 | ✅ | ✅ | AMR/SILK，可转文字 |
| 文件 | ✅ | ✅ | 最大 100MB |
| 视频 | ✅ | ✅ | MP4 等格式 |
| 表情 | ✅ | ✅ | 微信表情 |

#### 2. 富媒体标记语法

与 QQ 相同的标记系统：

```
[IMAGE:/path/to/image.jpg]
[DOCUMENT:/path/to/document.pdf]
[VIDEO:/path/to/video.mp4]
[AUDIO:/path/to/audio.mp3]
[VOICE:/path/to/voice.amr]
```

#### 3. 语音转文字

配置转录服务后，微信语音消息自动转文字：

```toml
[transcription]
enabled = true
# 配置转录提供商...
```

**输出格式**：
```
<VOICE_TRANSCRIPTION>转录的文本内容</VOICE_TRANSCRIPTION>
[VOICE:local_path_to_voice_file]
```

#### 4. 媒体文件处理

**接收**：
- 自动下载到工作目录
- 生成唯一文件名（避免冲突）
- 支持最大 100MB 文件

**发送**：
- 自动上传到微信 CDN
- 支持上传缓存（避免重复上传）
- 失败自动重试

### 技术细节

#### 长轮询机制
- 使用 `getUpdates` API 长轮询
- 超时时间：35 秒
- 客户端超时：40 秒（含 5 秒缓冲）

#### 错误处理
- **会话过期**（error -14）：暂停 1 小时后重试
- **连续失败**：达到 3 次后暂停 30 秒
- **单次失败**：2 秒后重试

#### AES 加密
iLink API 使用 AES-128-ECB 加密敏感数据：
- Bot token 加密存储
- 媒体上传凭证加密

### 调试技巧

1. **查看绑定状态**：
```
[wechat.default] ... INFO ... loaded persisted bot token
[wechat.default] ... INFO ... loaded persisted sync cursor
```

2. **消息处理日志**：
```
[wechat.default] ... DEBUG ... processing message from user
[wechat.default] ... DEBUG ... composed response with 1 attachments
```

3. **上传/下载日志**：
```
[wechat.default] ... INFO ... uploaded media: image (12345 bytes)
[wechat.default] ... INFO ... downloaded attachment: document.pdf
```

---

## 钉钉 Stream Mode

### 概述

ZeroClaw 通过钉钉 Stream Mode WebSocket 实现企业级集成，支持：
- 个人聊天（单聊）
- 群组聊天
- 富媒体消息
- 高可靠性（断线重连、消息确认）
- **AI 卡片流式响应**（v0.8 新增）

### 前置条件

1. **创建钉钉机器人**
   - 访问 [钉钉开放平台](https://open.dingtalk.com/)
   - 创建企业应用
   - 启用"Stream Mode"（流模式）

2. **获取凭证**
   - `ClientID`（AppKey）
   - `ClientSecret`（AppSecret）

3. **配置机器人**
   - 设置机器人头像和名称
   - 配置可见范围
   - 启用所需权限

### 配置方法（v0.8）

#### 基础配置

```toml
# v0.8 Schema Version 3 格式
[channels.dingtalk.default]
enabled = true
client_id = "你的 ClientID"
client_secret = "你的 ClientSecret"

# v0.8 新增：代理配置
proxy_url = "http://proxy.example.com:8080"

# v0.8 新增：工具排除
excluded_tools = ["shell", "http_request"]
```

#### 流式响应配置

钉钉使用 **AI 卡片流式更新 API** 实现 LLM 渐进式输出。详细配置、模板 ID 获取、调优建议与故障排查统一在《[钉钉与飞书 AI 卡片流式响应使用指导](./dingtalk-lark-streaming-guide.md)》维护。

最小示例（需要在钉钉开放平台先创建 AI 卡片模板）：

```toml
[channels.dingtalk.default]
enabled = true
client_id = "你的 AppKey"
client_secret = "你的 AppSecret"
stream_mode = "partial"
streaming_update_interval_ms = 1000
ai_card_template_id = "<你的 AI 卡片模板 ID>.schema"
```

**v0.8 变更说明**：
- `proxy_url`：支持每渠道独立代理配置
- `excluded_tools`：渠道级工具可见性控制
- `stream_mode`：新增流式响应模式
- `streaming_update_interval_ms`：精细控制流式更新频率

### 配置项说明

#### 基础配置

| 字段 | 必填 | 说明 |
|------|------|------|
| `enabled` | 是 | 是否启用 |
| `client_id` | 是 | 钉钉 ClientID（AppKey） |
| `client_secret` | 是 | 钉钉 ClientSecret（敏感信息） |
| `proxy_url` | 否 | 代理服务器 URL |
| `excluded_tools` | 否 | 排除的工具列表 |

#### 流式响应配置

> 字段说明、调优推荐值与故障排查见《[钉钉与飞书 AI 卡片流式响应使用指导](./dingtalk-lark-streaming-guide.md)》。

### 功能特性

#### 1. Stream Mode 优势

相比传统的 Webhook 模式，Stream Mode 提供：
- **实时性**：WebSocket 长连接，消息即时推送
- **可靠性**：内置消息确认和重传机制
- **双向通信**：同一连接收发双向消息
- **会话管理**：自动维护会话状态

#### 2. AI 卡片流式响应（v0.8 新增）

当启用 `stream_mode = "partial"` 时，ZeroClaw 会自动使用钉钉的 AI 卡片流式更新 API：

**工作流程**：
1. **创建 AI 卡片**：发送初始消息（显示"正在思考中..."）
2. **流式更新**：按自然边界（换行、句号等）分块更新卡片内容
3. **完成固化**：最后一次更新设置 `finish: true`，卡片内容固化

**触发条件**：
- `stream_mode == "partial"`
- 消息不包含图片
- 内容长度 > 100 字符

**API 调用**：
```
POST /v1.0/robot/sendAICard      # 创建卡片实例
POST /v1.0/card/streamingUpdate  # 流式更新（多次）
```

**示例效果**：
```
用户：请分析一下这个数据报告...

机器人：[AI 卡片]
  正在思考中...
  
  → 正在检索相关数据...
  
  → 已找到 3 条相关信息...
  
  → 分析如下：
     1. 销售趋势：同比增长 15%
     2. 用户活跃度：环比提升 8%
     3. 市场占比：稳定在 23%
     
  → 建议采取以下措施...
  
  [完成]
```

**错误处理**：
- 流式更新失败时自动降级为普通文本消息
- 日志记录完整流程便于排查

#### 3. 消息路由

ZeroClaw 自动处理消息路由：
- **单聊**：直接回复到发送者
- **群聊**：回复到原群组
- @机器人：群聊中可@机器人触发

#### 4. 富媒体支持

使用标准标记语法：

```
[IMAGE:/path/to/image.png]
[DOCUMENT:/path/to/file.pdf]
[VIDEO:/path/to/video.mp4]
[VOICE:/path/to/audio.wav]
```

**注意**：流式响应不支持图片消息。如果消息包含 `[IMAGE:...]` 标记，自动降级为传统发送模式。

#### 5. 会话 Webhook

钉钉为每个会话提供临时 webhook：
- ZeroClaw 自动提取和缓存
- 优先使用 webhook 回复（低延迟）
- 失败时降级到 API 调用

### 技术细节

#### WebSocket 连接流程

1. **获取 Stream URL**
   ```
   POST /v1.0/im/chatbot/stream/get
   Authorization: Bearer <access_token>
   ```

2. **建立连接**
   ```
   WebSocket wss://<stream_url>
   ```

3. **心跳保活**
   - 自动发送心跳
   - 检测连接状态
   - 断线自动重连

#### 重试机制

| 场景 | 策略 |
|------|------|
| 消息发送失败 | 最多 4 次，500ms 间隔 |
| 连接失败 | 最多 3 次，1 秒间隔 |
| 连接卡死 | 120 秒超时检测 |

#### 消息确认

- 钉钉要求消息确认（ACK）
- ZeroClaw 自动处理 ACK
- 未确认消息自动重发

### 调试技巧

1. **连接状态**：
```
[dingtalk.default] ... INFO ... DingTalk: registering gateway connection
[dingtalk.default] ... INFO ... DingTalk: connected and listening for messages
```

2. **消息处理**：
```
[dingtalk.default] ... DEBUG ... received message from chatID: xxx
[dingtalk.default] ... DEBUG ... sending reply via session webhook
```

3. **错误日志**：
```
[dingtalk.default] ... WARN ... webhook send failed, falling back to API
[dingtalk.default] ... INFO ... API send succeeded
```

---

## 飞书/Lark

### 概述

ZeroClaw 支持飞书（中国版）和 Lark（国际版）两个版本，提供：
- WebSocket 和 Webhook 两种接收模式
- 交互式卡片消息
- 富媒体支持
- 审批卡片（高级功能）

### 版本区别

| 特性 | 飞书（Feishu） | Lark（国际版） |
|------|----------------|----------------|
| API 域名 | `open.feishu.cn` | `open.larksuite.com` |
| 语言 | 中文界面 | 英文界面 |
| 审批卡片 | ✅ 支持 | ❌ 不支持 |
| 流式响应 | ✅ 完整支持 | ⚠️ 有限支持 |

### 前置条件

1. **创建企业应用**
   - 飞书：[飞书开放平台](https://open.feishu.cn/)
   - Lark：[Lark Developer Platform](https://developers.larksuite.com/)
   - 创建自建应用

2. **配置权限**
   - 启用"机器人"能力
   - 添加所需权限：
     - `im:message`
     - `im:chat`
     - `im:resource`（发送媒体文件）

3. **获取凭证**
   - `AppID`
   - `AppSecret`
   - `EncryptKey`（可选，用于消息加密）
   - `VerificationToken`（可选，用于 Webhook 验证）

### 配置方法（v0.8）

在 `config.toml` 中添加：

```toml
# v0.8 Schema Version 3 格式
[channels.lark.default]
enabled = true
app_id = "你的 AppID"
app_secret = "你的 AppSecret"

# v0.8：安全配置（推荐启用）
encrypt_key = "你的加密密钥"  # 消息加密
verification_token = "验证令牌"  # Webhook验证

# v0.8：行为配置
mention_only = false  # 群聊中仅响应@消息
use_feishu = true  # true=飞书，false=Lark
receive_mode = "websocket"  # "websocket" 或 "webhook"

# v0.8：Webhook模式需要
port = 8080

# v0.8 新增：代理配置
proxy_url = "http://proxy.example.com:8080"

# v0.8 新增：工具排除
excluded_tools = ["shell"]

# v0.8 新增：会话管理
per_user_session = false  # 群聊中每人独立会话

# v0.8 新增：审批卡片
approval_timeout_secs = 300  # 5分钟超时

# v0.8 新增：流式响应
stream_mode = "off"  # "off" | "partial" | "multi_message"
draft_update_interval_ms = 1000  # 卡片更新间隔（毫秒）
```

**v0.8 变更说明**：
- `encrypt_key` / `verification_token`：从可选变为推荐启用（安全性提升）
- `stream_mode`：新增流式响应模式（飞书专属）
- `draft_update_interval_ms`：精细控制卡片更新频率
- `per_user_session`：新增群聊会话管理策略
- `approval_timeout_secs`：审批卡片超时控制
- `excluded_tools` / `proxy_url`：标准化渠道配置项

**流式模式说明**：

飞书的 `stream_mode` / `draft_update_interval_ms` 配置、节流机制、与钉钉的差异对比、故障排查统一在《[钉钉与飞书 AI 卡片流式响应使用指导](./dingtalk-lark-streaming-guide.md)》维护。

### 配置项详解

#### 基础配置

| 字段 | 必填 | 说明 |
|------|------|------|
| `enabled` | 是 | 是否启用 |
| `app_id` | 是 | AppID |
| `app_secret` | 是 | AppSecret（敏感） |

#### 安全配置

| 字段 | 必填 | 说明 |
|------|------|------|
| `encrypt_key` | 否 | 消息加密密钥（推荐启用） |
| `verification_token` | 否 | Webhook 验证令牌 |

#### 行为配置

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `mention_only` | false | 群聊中仅响应@机器人的消息 |
| `use_feishu` | false | true=飞书，false=Lark |
| `per_user_session` | false | 群聊中每人独立会话 |
| `approval_timeout_secs` | 300 | 审批卡片等待超时（秒） |

#### 高级配置

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `receive_mode` | "websocket" | 消息接收模式 |
| `port` | - | Webhook 模式监听端口 |
| `proxy_url` | - | 代理服务器 |
| `excluded_tools` | [] | 排除的工具 |
| `stream_mode` / `draft_update_interval_ms` | "off" / 1000 | 流式响应配置；详见《[钉钉与飞书 AI 卡片流式响应使用指导](./dingtalk-lark-streaming-guide.md)》 |

**流式响应工作原理**与**推荐配置**见《[钉钉与飞书 AI 卡片流式响应使用指导](./dingtalk-lark-streaming-guide.md)》。

### 接收模式对比

#### WebSocket 模式（推荐）

**优点**：
- 实时推送，低延迟
- 无需公网 IP
- 内置重连机制

**配置**：
```toml
receive_mode = "websocket"
```

#### Webhook 模式

**适用场景**：
- 已有公网服务器
- 需要负载均衡
- 传统架构集成

**配置**：
```toml
receive_mode = "webhook"
port = 8080
```

**飞书开放平台配置**：
1. 事件订阅 → 添加 URL
2. 填写 `http://your-server:8080/lark/webhook`
3. 使用 `verification_token` 验证

### 功能特性

#### 1. 流式响应（Stream Mode）

ZeroClaw 支持三种流式响应模式：

**`off`（默认）**：
- 完整响应后一次性发送
- 兼容所有渠道

**`partial`（飞书专属）**：
- 使用交互式卡片
- 逐步更新卡片内容
- 显示"思考中..."状态

**`multi_message`**：
- 分多条消息发送
- Lark 不支持（降级为 `off`）

配置示例：
```toml
stream_mode = "partial"
draft_update_interval_ms = 1000  # 1 秒更新间隔
```

#### 2. 交互式卡片

飞书支持丰富的交互式卡片：
- 文本卡片
- 图文卡片
- 按钮卡片
- 表单卡片
- 审批卡片

**审批卡片**：
```toml
approval_timeout_secs = 300  # 5 分钟超时
```

用户未在超时内审批时，自动拒绝。

#### 3. 富媒体支持

标准标记语法：
```
[IMAGE:/path/to/image.png]
[DOCUMENT:/path/to/file.pdf]
[VIDEO:/path/to/video.mp4]
[VOICE:/path/to/audio.wav]
```

**飞书特有**：
- 支持发送"消息卡片"
- 支持富文本格式
- 支持@提及

#### 4. 会话管理

**默认行为**：
- 单聊：每个用户独立会话
- 群聊：整个群组共享会话

**`per_user_session = true`**：
- 群聊中每人独立会话
- 适合多用户场景

### 技术细节

#### 认证流程

1. **获取 tenant_access_token**
   ```
   POST /open-apis/auth/v3/tenant_access_token/internal
   {
     "app_id": "...",
     "app_secret": "..."
   }
   ```

2. **获取 user_access_token**（可选）
   - 用于需要用户权限的操作

3. **Token 缓存**
   - 自动缓存和续期
   - 失败重试

#### 消息加密

启用 `encrypt_key` 后：
- 飞书使用 AES-256-CBC 加密消息
- ZeroClaw 自动解密
- 推荐生产环境启用

#### WebSocket 重连

| 参数 | 值 | 说明 |
|------|-----|------|
| 最大重连次数 | 3 | 连接失败重试 |
| 重连间隔 | 1 秒 | 指数退避 |
| 心跳检测 | 自动 | 维持连接 |

### 调试技巧

1. **查看版本**：
```
[lark.default] ... INFO ... Lark: connecting to stream WebSocket
# 或
[lark.default] ... INFO ... Lark: webhook server listening on port 8080
```

2. **消息处理**：
```
[lark.default] ... DEBUG ... received message type: text
[lark.default] ... DEBUG ... sending interactive card
```

3. **流式响应**：
```
[lark.default] ... DEBUG ... updating draft card (iteration 2)
[lark.default] ... INFO ... finalized card after 5 updates
```

---

## 常见问题

### QQ

#### Q: WebSocket 连接失败
**A**: 检查：
1. `AppID` 和 `AppSecret` 是否正确
2. 机器人是否已发布（未发布只能测试）
3. 网络是否可访问 `bots.qq.com`

#### Q: 群聊中机器人无响应
**A**: QQ 群聊需要@机器人才能触发，检查消息是否包含@。

#### Q: 语音消息无法转录
**A**: 检查：
1. `[transcription]` 是否启用
2. 转录服务商是否配置
3. 语音格式是否支持

### 微信

#### Q: 二维码扫描后仍显示未绑定
**A**: 等待 1-2 分钟，微信后台同步需要时间。

#### Q: Bot token 丢失
**A**: 检查 `state_dir` 目录权限，确保可写。

#### Q: 消息发送失败
**A**: 检查：
1. 微信号是否被封禁
2. Bot 权限是否正常
3. 消息内容是否违规

### 钉钉

#### Q: Stream Mode 连接不上
**A**: 检查：
1. `ClientID` 和 `ClientSecret` 是否正确
2. 企业应用是否启用 Stream Mode
3. 防火墙是否允许 WebSocket

#### Q: 消息重复发送
**A**: ZeroClaw 有消息去重机制，如仍重复检查：
1. 钉钉是否重发消息
2. 消息 ID 是否正常

### 飞书/Lark

#### Q: WebSocket 和 Webhook 选哪个？
**A**: 
- 有公网服务器 → Webhook
- 无公网/内网部署 → WebSocket（推荐）

#### Q: 卡片更新太快被限流
**A**: 调整 `draft_update_interval_ms`，飞书限制 5 QPS/消息。

#### Q: 审批卡片不显示
**A**: 仅飞书支持，Lark 会自动降级为普通卡片。

### 通用问题

#### Q: 如何启用 DEBUG 日志？
**A**: 
```bash
RUST_LOG=zeroclaw_channels=DEBUG ./zeroclaw
```

#### Q: 代理配置
**A**: 
```toml
[channels.xxx.default]
proxy_url = "http://user:pass@proxy:8080"
# 或 socks5
proxy_url = "socks5://proxy:1080"
```

#### Q: 如何排除某些工具？
**A**: 
```toml
[channels.xxx.default]
excluded_tools = ["shell", "file_write"]
```

#### Q: 多实例部署
**A**: 每个渠道只能在一个实例运行，避免消息重复处理。

---

## 最佳实践

### 安全

1. **启用消息加密**（飞书）
   ```toml
   encrypt_key = "随机生成的密钥"
   ```

2. **使用代理**（敏感环境）
   ```toml
   proxy_url = "socks5://internal-proxy:1080"
   ```

3. **限制工具**（高风险环境）
   ```toml
   excluded_tools = ["shell", "file_write", "http_request"]
   ```

### 性能

1. **调整日志级别**
   ```bash
   RUST_LOG=zeroclaw_channels=INFO  # 生产环境
   ```

2. **合理设置超时**
   ```toml
   approval_timeout_secs = 300  # 避免长时间等待
   ```

3. **使用上传缓存**（QQ/微信）
   - 自动启用，无需配置

### 可靠性

1. **监控连接状态**
   - 定期检查日志中的连接状态
   - 设置告警（如连续断线）

2. **备份配置文件**
   - 特别是敏感凭证
   - 使用加密存储

3. **测试故障恢复**
   - 定期测试断线重连
   - 验证消息不丢失

---

## 参考资料

- [ZeroClaw 架构文档](../architecture/overview.md)
- [渠道编排器](./orchestrator.md)
- [对等组配置](./peer-groups.md)
- [安全策略](../security/policies.md)

---

## 附录：v0.8 完整配置示例

### 多渠道路由配置

```toml
# v0.8 Schema Version 3
schema_version = 3

# ==================== Provider 配置 ====================
[providers.models.doubao]
api_key = "sk-xxx"
base_url = "https://ark.cn-beijing.volces.com/api/v3"

[providers.models.openai]
api_key = "sk-xxx"

# ==================== 渠道配置 ====================

# QQ 渠道
[channels.qq.default]
enabled = true
app_id = "12345678"
app_secret = "secret_xxx"
excluded_tools = ["shell"]

# 微信渠道
[channels.wechat.default]
enabled = true
state_dir = "~/.zeroclaw/wechat"
excluded_tools = ["shell", "file_write"]

# 钉钉渠道
[channels.dingtalk.default]
enabled = true
client_id = "ding_xxx"
client_secret = "secret_xxx"
proxy_url = "http://proxy:8080"

# 飞书渠道（国内）
[channels.lark.feishu]
enabled = true
app_id = "cli_xxx"
app_secret = "secret_xxx"
use_feishu = true
receive_mode = "websocket"
encrypt_key = "encrypt_xxx"  # 推荐启用
mention_only = false
stream_mode = "partial"  # 流式响应
draft_update_interval_ms = 1000
approval_timeout_secs = 300

# 飞书渠道（国际）
[channels.lark.lark]
enabled = false  # 备用
app_id = "cli_xxx"
app_secret = "secret_xxx"
use_feishu = false
receive_mode = "websocket"

# ==================== 对等组配置 ====================
# v0.8 新增：细粒度访问控制

# QQ 允许的用户
[peer_groups.qq_default_users]
channel = "qq"
external_peers = ["*"]  # 允许所有用户

# 微信允许的用户
[peer_groups.wechat_default_users]
channel = "wechat"
external_peers = ["*"]  # 需要绑定后自动添加

# 钉钉允许的用户
[peer_groups.dingtalk_default_users]
channel = "dingtalk"
external_peers = ["*"]

# 飞书允许的用户
[peer_groups.lark_feishu_users]
channel = "lark"
external_peers = ["*"]

# ==================== 安全配置 ====================
# v0.8 新增

[security.audit]
enabled = true
log_path = "~/.zeroclaw/audit.log"
max_size_mb = 100
sign_events = false  # 启用事件签名（生产环境推荐true）

[security.otp]
enabled = false  # OTP 验证
# required_domains = ["example.com"]  # 需要OTP的域名
# timeout_secs = 300

[security.estop]
enabled = true  # 紧急停止
# trigger_patterns = ["停止", "暂停", "e-stop"]

# ==================== 转录配置 ====================
# v0.8：支持每渠道独立配置

[transcription]
enabled = true
api_key = "sk-xxx"  # Groq API key
model = "whisper-large-v3"
language = "zh"  # 中文

# 也可以针对特定渠道覆盖
[channels.qq.default.transcription]
enabled = true
provider = "groq"

[channels.wechat.default.transcription]
enabled = true
provider = "azure"
api_key = "azure_xxx"

# ==================== 代理配置 ====================
# v0.8：全局代理

[proxy]
enabled = false
# url = "http://user:pass@proxy:8080"
# bypass_domains = ["localhost", "127.0.0.1"]

# ==================== Agent 配置 ====================

[agents.router]
model_provider = "doubao"
model = "doubao-seed-2-0-pro-260215"
temperature = 0.3

# 技能配置
[[agents.router.skills]]
path = "./skills/router-account-qr"
enabled = true
```

### 配置说明

**v0.8 关键变化**：

1. **Provider 重构**
   - `[providers.models.<name>]` 替代旧的 `[model_providers.<name>]`
   - 支持更多字段（`base_url`, `extra_headers` 等）

2. **渠道标准化**
   - 统一 `[channels.<type>.<alias>]` 格式
   - 支持多实例（如 `lark.feishu` 和 `lark.lark`）
   - 新增 `excluded_tools` 控制

3. **对等组系统**
   - `[peer_groups.<name>]` 替代旧的 `allowed_users`
   - 支持通配符和复杂匹配规则
   - 细粒度访问控制

4. **安全增强**
   - `[security]` 顶层配置块
   - 审计日志、OTP、紧急停止
   - 敏感字段自动加密

5. **转录配置**
   - 全局 `[transcription]` + 渠道级覆盖
   - 支持多提供商

6. **代理配置**
   - 全局 `[proxy]` + 渠道级 `proxy_url`
   - 支持绕过域名

### 迁移检查清单

从旧版本升级到v0.8：

- [ ] 检查 `schema_version`（应为3）
- [ ] 迁移 `model_providers` → `providers.models`
- [ ] 迁移 `channels.<type>` → `channels.<type>.<alias>`
- [ ] 迁移 `allowed_users` → `peer_groups`
- [ ] 添加 `excluded_tools`（如需要）
- [ ] 配置 `security`（推荐）
- [ ] 验证敏感字段加密

**自动迁移工具**：
```bash
# 备份原配置
cp config.toml config.toml.backup

# 启动自动迁移
./zeroclaw

# 验证迁移结果
zeroclaw validate-config
```

---
