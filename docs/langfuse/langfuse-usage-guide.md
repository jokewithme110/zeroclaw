# Langfuse 调用链观测使用指南

## 概述

LangfuseObserver 是 ZeroClaw 观察者系统的一个后端实现，通过 OTLP 协议将 Agent 调用链数据发送到 Langfuse 平台进行可视化追踪和分析。

## 与其他 Observer 的关系

ZeroClaw 的观察者系统（`crates/zeroclaw-runtime/src/observability/`）定义了 `Observer` trait，支持多种后端：

| 后端 | `backend` 值 | 用途 |
|---|---|---|
| NoopObserver | `"none"` / `"noop"` | 关闭观测，零开销 |
| LogObserver | `"log"` | 通过 `tracing::info!` 输出结构化日志 |
| VerboseObserver | `"verbose"` | 终端 `eprintln!` 人类可读输出 |
| PrometheusObserver | `"prometheus"` | 导出 Prometheus 指标（需 `observability-prometheus` feature） |
| OtelObserver | `"otel"` | 导出到通用 OTLP collector（需 `observability-otel` feature） |
| **LangfuseObserver** | **`"langfuse"`** | **导出到 Langfuse OTLP 端点（需 `observability-langfuse` feature）** |

**重要**：`backend` 只能选择一个值。如果需要同时使用多个后端（如 Prometheus + Langfuse），使用 `MultiObserver`（配置 `backend = "multi"`）。

### LangfuseObserver vs OtelObserver

两者都使用 OpenTelemetry SDK 导出 trace，但有以下关键区别：

| | OtelObserver | LangfuseObserver |
|---|---|---|
| 导出端点 | 通用 OTLP collector | Langfuse `/api/public/otel/v1/traces` |
| 认证 | 无 | HTTP Basic Auth |
| Span 关系 | 独立扁平 span | 父子层级（Trace → Generation → Span） |
| Observation 类型 | 全是 generic span | 区分 `generation`（LLM 调用）和 `span`（工具调用） |
| Langfuse 属性 | 无 | `langfuse.*` 前缀属性，自动映射 model name、token usage 等 |

## 编译

LangfuseObserver 通过 Cargo feature flag 控制，项目为 workspace 结构，需两级配置：

**Root `Cargo.toml`**（转发）：
```toml
[features]
observability-langfuse = ["zeroclaw-runtime/observability-langfuse"]
```

**`crates/zeroclaw-runtime/Cargo.toml`**（实际依赖）：
```toml
[features]
observability-langfuse = ["dep:opentelemetry", "dep:opentelemetry_sdk", "dep:opentelemetry-otlp"]
```

编译：
```bash
# 带 Langfuse 支持编译
cargo build --features observability-langfuse

# 不带 Langfuse（默认）
cargo build
```

该 feature 与 `observability-otel` 共享同一组 OTel crates 依赖，可以独立开启或同时开启。

## 配置

### 方式一：TOML 配置文件

在 `~/.zeroclaw/config.toml` 中添加：

```toml
[observability]
backend = "langfuse"
langfuse_public_key = "pk-lf-..."
langfuse_secret_key = "sk-lf-..."
langfuse_base_url = "https://cloud.langfuse.com"  # 可选，默认值
langfuse_include_io = false  # 可选，默认 false。设为 true 则在 trace 中包含 LLM 输入/输出内容
```

配置项说明：

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `backend` | string | 是 | `"none"` | 设为 `"langfuse"` 启用 |
| `langfuse_public_key` | string | 是 | 无 | Langfuse 项目公钥，以 `pk-lf-` 开头 |
| `langfuse_secret_key` | string | 是 | 无 | Langfuse 项目私钥，以 `sk-lf-` 开头 |
| `langfuse_base_url` | string | 否 | `https://cloud.langfuse.com` | Langfuse 实例地址（自部署时改为自建地址） |
| `langfuse_include_io` | bool | 否 | `false` | 是否在 generation 中包含 LLM 输入消息和输出文本 |

公钥和私钥在配置文件中会通过 ChaCha20-Poly1305 加密存储（`#[secret]` 机制），不会以明文落盘。

### 方式二：环境变量（计划支持）

当前版本通过配置文件设置。未来可以支持环境变量覆盖：

```bash
export LANGFUSE_PUBLIC_KEY=pk-lf-...
export LANGFUSE_SECRET_KEY=sk-lf-...
export LANGFUSE_HOST=https://cloud.langfuse.com
```

### 多区域 / 自部署

```toml
# EU 区域（默认）
langfuse_base_url = "https://cloud.langfuse.com"

# US 区域
langfuse_base_url = "https://us.cloud.langfuse.com"

# 自部署
langfuse_base_url = "http://10.0.0.5:3000"
```

### 启动时校验

如果 `backend = "langfuse"` 但未配置 key，启动时会产生 warning 并自动 fallback 到 `NoopObserver`（不会影响 Agent 正常运行）：

```
WARN Langfuse backend requested but `langfuse_public_key` is not set; falling back to noop.
```

## 数据模型映射

每次 Agent 会话产生一条 **Trace**，其中包含多轮 LLM 调用和工具执行：

```
Trace: "ZeroClaw Agent Session"        ← AgentStart → AgentEnd
  ├── Generation: "llm.call"           ← LlmResponse（第 1 次 LLM 调用）
  ├── Span: "tool.execute"             ← ToolCall（工具执行）
  ├── Generation: "llm.call"           ← LlmResponse（第 2 次 LLM 调用）
  ├── Span: "tool.execute"             ← ToolCall（工具执行）
  └── ...
```

### 各 Observation 携带的属性

**Trace（Root Span）**：
- `langfuse.trace.name` — `"ZeroClaw Agent Session"`
- `provider` / `model` — 使用的 LLM provider 和模型
- `duration_s` — 会话总耗时
- `tokens_used` — 总 token 消耗
- `langfuse.observation.cost_details` — 成本（USD）

**Generation（LLM 调用）**：
- `langfuse.observation.type` — `"generation"`
- `langfuse.observation.model.name` — 模型名称
- `langfuse.observation.usage_details` — token 使用详情（JSON: `promptTokens`, `completionTokens`, `totalTokens`）
- `langfuse.observation.metadata.provider` — Provider 名称
- `langfuse.observation.metadata.success` — 调用是否成功
- `langfuse.observation.status_message` — 错误信息（失败时）
- `duration_s` — LLM 调用耗时
- `startTime` / `endTime` — 精确的调用时间窗口

**Span（工具执行）**：
- `langfuse.observation.type` — `"span"`
- `langfuse.observation.input` — 工具调用参数
- `langfuse.observation.metadata.tool` — 工具名称
- `langfuse.observation.metadata.success` — 执行是否成功
- `duration_s` — 工具执行耗时

## Trace 推送机制

### 推送协议

使用 **OTLP over HTTP**（`HTTP/protobuf`）协议，发送到 Langfuse 的 OTLP 端点：

```
POST https://cloud.langfuse.com/api/public/otel/v1/traces
Authorization: Basic <base64(pk:sk)>
x-langfuse-ingestion-version: 4
Content-Type: application/x-protobuf
```

### 推送时机

使用 OpenTelemetry SDK 的 **BatchSpanProcessor** 管理发送，遵循以下触发条件：

| 触发条件 | 说明 |
|---|---|
| **定时发送** | 每 **5 秒**（`scheduled_delay`）检查并发送积压的 span |
| **批量满** | 积压 span 数量达到 **512 个**（`max_export_batch_size`）时立即发送 |
| **会话结束** | `AgentEnd` 事件触发时，root span 被关闭，span 进入待发送队列 |
| **显式 flush** | 程序优雅退出时调用 `observer.flush()`，强制发送所有待发送 span |
| **Observer Drop** | `LangfuseObserver` 被 dropped 时，自动 `force_flush()` 并结束未关闭的 root span |

### 批量处理参数

OTel SDK 默认参数（均在 `SdkTracerProvider` 内部）：

| 参数 | 默认值 | 说明 |
|---|---|---|
| `scheduled_delay` | 5000ms | 定时发送间隔 |
| `max_export_batch_size` | 512 | 单次最大发送 span 数 |
| `max_queue_size` | 2048 | 最大积压 span 数（超出后丢弃） |

### 发送失败处理

- **发送失败（网络错误 / 4xx / 5xx）**：OTel SDK 内置重试机制，失败后自动重试
- **连续失败**：丢弃该批次 span，输出 `tracing::warn!` 日志
- **Agent 不受影响**：观测数据是最佳努力（best-effort）语义，任何 Langfuse 相关错误不会影响 Agent 正常运行

## 在 Langfuse UI 中查看 Trace

1. 打开 Langfuse UI（如 `https://cloud.langfuse.com`）
2. 进入项目 → Traces
3. 可以看到名为 `"ZeroClaw Agent Session"` 的 trace
4. 点开 trace 查看完整的调用链，包括每步 LLM 调用的 token 消耗和工具执行详情

## Debug 与排错

### 确认 LangfuseObserver 是否初始化成功

查看启动日志，正常情况应看到：

```
INFO Langfuse observer initialized, base_url: https://cloud.langfuse.com
```

### 常见问题

**Q: 配置了但 Langfuse UI 中没有 trace？**

1. 确认编译时带了 feature：`cargo build --features observability-langfuse`
2. 确认 `backend = "langfuse"` 配置正确
3. 确认 public_key / secret_key 正确（可在 Langfuse UI → Settings → API Keys 查看）
4. 确认网络可达 Langfuse 端点（特别是自部署场景）
5. 等待至少 5-10 秒（BatchSpanProcessor 的定时 flush 间隔 + Langfuse 处理延迟）

**Q: 如何查看 OTLP 是否成功发送？**

可以设置 RUST_LOG 查看详细日志：

```bash
RUST_LOG=info,zeroclaw_runtime::observability=debug,opentelemetry_otlp=debug,opentelemetry_sdk=debug \
cargo run --bin zeroclaw --features observability-langfuse -- agent -m "hello"
```

**Q: key 在配置文件中安全吗？**

是的。配置项标注了 `#[secret]`，ZeroClaw 会使用 ChaCha20-Poly1305 加密后存储。读取时自动解密。

## 与 OtelObserver 共存

如果同时开启 `observability-otel` 和 `observability-langfuse`，可以通过 `MultiObserver` 将数据同时发送到通用 OTLP collector 和 Langfuse：

```toml
[observability]
backend = "multi"
# 需要在代码层配置 MultiObserver 的 observers 列表
# 此功能需进一步开发
```

目前推荐只选择一个后端。如需 Prometheus 指标 + Langfuse trace，建议在不同环境或部署中使用。
