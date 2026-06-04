# Langfuse 调用链观测 — 技术方案

> 版本: v1.0 | 日期: 2026-05-09 | 状态: 已实现

## 1. 背景与目标

### 1.1 问题描述

ZeroClaw 是一个多 Provider、多 Channel 的 AI Agent 平台，每次用户交互涉及多轮 LLM 调用和工具执行。当前缺乏统一的调用链可视化工具来回答以下问题：

- 一次会话中 LLM 被调用了多少次？每次耗时和 token 消耗是多少？
- 工具调用的链路是怎样的？哪些工具调用失败或超时？
- 不同 Provider / Model 组合的成本对比如何？

### 1.2 目标

为 ZeroClaw 接入 Langfuse 调用链观测，实现：

1. **自动追踪**：Agent 会话自动映射为 Langfuse Trace → Generation → Span 层级
2. **零侵入**：复用现有 `Observer` trait 架构，不修改核心 Agent 逻辑
3. **可选开启**：通过 feature flag + 配置控制，默认关闭，不影响现有行为
4. **Best-effort 语义**：Langfuse 不可达不影响 Agent 正常运行

### 1.3 方案对比

| 方案 | 优点 | 缺点 |
|---|---|---|
| Langfuse 官方 SDK (Python/JS) | 功能完整 | ZeroClaw 是 Rust 项目，无法直接使用 |
| 第三方 `langfuse` crate (v0.1.8) | 纯 Rust | 只有一个 `send_interaction()` 函数，不支持多轮 Trace/Generation/Span 嵌套 |
| Langfuse Legacy Ingestion API | 完全控制 | 需手写 HTTP 调用、batching、retry；API 已被标记 legacy |
| **OTLP 协议 + 现有 OTel SDK（采用）** | 标准化协议、复用现有 `opentelemetry*` crates、SDK 自带 batching/retry | 依赖 Langfuse OTLP attribute mapping |

## 2. 总体架构

### 2.1 系统层次

```
┌─────────────────────────────────────────────────────┐
│                  Agent Loop (loop_.rs)               │
│  ┌─────────┐  ┌──────────┐  ┌────────┐  ┌────────┐ │
│  │AgentStart│  │LlmRequest│  │ToolCall│  │AgentEnd│ │
│  └────┬─────┘  └────┬─────┘  └───┬────┘  └───┬────┘ │
│       │              │            │           │      │
│       ▼              ▼            ▼           ▼      │
│  ┌───────────────────────────────────────────────┐   │
│  │              Observer trait                    │   │
│  │  record_event(&ObserverEvent) - sync, hot path│   │
│  └───────────────────────────────────────────────┘   │
│       │                                              │
│       ├── NoopObserver (none/noop)                    │
│       ├── LogObserver (log) — tracing::info!          │
│       ├── VerboseObserver (verbose) — eprintln!       │
│       ├── PrometheusObserver — metrics                │
│       ├── OtelObserver — OTLP collector               │
│       └── LangfuseObserver — 本方案 ←                │
└─────────────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────┐
│  LangfuseObserver (crates/zeroclaw-runtime/src/observability/langfuse.rs)   │
│                                                     │
│  ┌───────────────────────┐  ┌─────────────────────┐ │
│  │ SdkTracerProvider     │  │ Span Context Tracking│ │
│  │ + BatchSpanProcessor  │  │ (root span + children)│ │
│  └──────────┬────────────┘  └─────────────────────┘ │
│             │                                        │
│             ▼                                        │
│  ┌───────────────────────────────────────────────┐   │
│  │ OTLP Exporter                                  │   │
│  │ - Endpoint: {base}/api/public/otel/v1/traces  │   │
│  │ - Auth: HTTP Basic (pk:sk)                    │   │
│  │ - Header: x-langfuse-ingestion-version: 4     │   │
│  └───────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────┘
                         │
                         ▼
              ┌─────────────────────┐
              │   Langfuse Cloud    │
              │  / Self-hosted      │
              └─────────────────────┘
```

### 2.2 数据模型映射

ZeroClaw 的一次 Agent 会话映射为以下 Langfuse 层级：

```
Trace: "ZeroClaw Agent Session"     ← AgentStart → AgentEnd
  │
  ├── Generation: "llm.call"        ← 第 1 次 LLM 调用 (LlmResponse)
  │     input:  系统提示 + 用户消息（仅首次完整）
  │     output: LLM 响应文本（include_io=true 时）
  │     model:  gpt-4o
  │     usage:  {promptTokens, completionTokens, totalTokens}
  │
  ├── Span: "tool.execute"          ← 工具执行 (ToolCall)
  │     input:  工具调用参数
  │     metadata: {tool, success}
  │
  ├── Generation: "llm.call"        ← 第 2 次 LLM 调用
  │     input:  仅增量消息（工具结果等）
  │     ...
  └── Span: "tool.execute"          ← ...
```

### 2.3 Span 父子关系

所有 Generation 和 Span 都是 Root Span（agent.invocation）的直接子节点：

```
agent.invocation (root)
  ├── llm.call (child, linked via SpanContext)
  ├── tool.execute (child, linked via SpanContext)
  ├── llm.call (child)
  └── tool.execute (child)
```

技术实现：`AgentStart` 创建 root span，存储其 `SpanContext`。后续 Generation/Span 通过 `Context::new().with_remote_span_context(parent_sc)` 建立父子关系。

## 3. 核心设计决策

### 3.1 为什么用 OTLP 而不是 Legacy Ingestion API

- **标准化**：OTLP 是 CNCF 标准协议，Langfuse 原生支持（`/api/public/otel/v1/traces`）
- **复用 SDK**：ZeroClaw 已有 `opentelemetry*` 0.31 crates（被 `observability-otel` 使用），`observability-langfuse` 共享同一组依赖
- **内置能力**：OTel SDK 自带 `BatchSpanProcessor`，提供 batching、retry、async export，无需自己实现
- **属性映射**：Langfuse 自动识别 `langfuse.*` 前缀属性，正确区分 generation vs span

### 3.2 为什么是同步 record_event

`Observer::record_event(&self, event: &ObserverEvent)` 是同步方法（hot path），但需要 HTTP 导出。解决方式：

- 使用 OTel SDK 的 `SdkTracerProvider` + `BatchSpanProcessor`
- `tracer.build()` 创建 span 是同步的（写入内存 buffer）
- SDK 后台线程异步批量发送，不阻塞 Agent 主线程
- `flush()` 和 `Drop` 时调用 `force_flush()` 确保数据不丢失

### 3.3 Span 不可 Clone 的处理

`opentelemetry_sdk::trace::Span` 内部包含 `Arc<Mutex<...>>`，不实现 `Clone`。无法直接 clone root span 给多个子 span 使用。解决方式：

- 提取 `root.span_context().clone()` —— `SpanContext` 是 `Clone` 的
- 通过 `Context::new().with_remote_span_context(parent_sc)` 创建子 span 的父上下文
- Root span 本身保存在 `current_root: ParkingMutex<Option<Span>>` 中，在 `AgentEnd` 时取出并 end

### 3.4 输入/输出内容的策略

- **配置控制**：`langfuse_include_io` 默认 `false`，不包含敏感内容
- **总是序列化**：`loop_.rs` 无条件填充 `input_json` / `output_text` 字段（序列化成本极低）
- **Observer 决定**：`LangfuseObserver` 根据自身 `include_io` 决定是否设置 `langfuse.observation.input/output`
- **增量输入**：首次 LLM 调用发完整输入，后续调用仅发新增消息（避免每轮重复发送全量历史）

### 3.5 事件选择性处理

| ObserverEvent | Langfuse 映射 | 原因 |
|---|---|---|
| `AgentStart` | Trace 创建（root span） | 会话开始 |
| `LlmResponse` | Generation 创建 | 包含完整 LLM 调用上下文 |
| `LlmRequest` | 暂存 `input_json` | 数据在 LlmResponse 时使用 |
| `ToolCall` | Span 创建 | 工具执行 |
| `ToolCallStart` | 暂存 `arguments` | 数据在 ToolCall 时使用 |
| `AgentEnd` | Trace 结束 | 设置聚合 token/cost |
| `TurnComplete` 等 | 忽略 | 无对应 Langfuse 模型 |

## 4. 关键属性映射

Langfuse 通过 `langfuse.*` 前缀的 OTel 属性识别 observation 类型：

### 4.1 Generation（LLM 调用）

| OTel 属性 | Langfuse 含义 | 数据来源 |
|---|---|---|
| `langfuse.observation.type` = `"generation"` | 标识为 LLM 调用 | 硬编码 |
| `langfuse.observation.model.name` | 模型名 | `LlmResponse.model` |
| `langfuse.observation.usage_details` | Token 使用 | `LlmResponse.input_tokens/output_tokens`，JSON string |
| `langfuse.observation.input` | 输入消息 | `LlmRequest.input_json`（仅 `include_io=true`） |
| `langfuse.observation.output` | 输出文本 | `LlmResponse.output_text`（仅 `include_io=true`） |
| `langfuse.observation.metadata.provider` | Provider 名 | `LlmResponse.provider` |
| `langfuse.observation.metadata.success` | 成功/失败 | `LlmResponse.success` |
| `langfuse.observation.status_message` | 错误信息 | `LlmResponse.error_message` |
| `duration_s` | 耗时 | `LlmResponse.duration` |
| `provider`, `model` | 通用属性（Trace UI 可见） | 同上 |

### 4.2 Span（工具执行）

| OTel 属性 | Langfuse 含义 | 数据来源 |
|---|---|---|
| `langfuse.observation.type` = `"span"` | 标识为工具调用 | 硬编码 |
| `langfuse.observation.input` | 工具参数 | `ToolCallStart.arguments` |
| `langfuse.observation.metadata.tool` | 工具名 | `ToolCall.tool` |
| `langfuse.observation.metadata.success` | 成功/失败 | `ToolCall.success` |
| `tool.name`, `duration_s` | 通用属性 | 同上 |

### 4.3 Trace（会话）

| OTel 属性 | Langfuse 含义 | 数据来源 |
|---|---|---|
| `langfuse.trace.name` | Trace 名称 | 硬编码 `"ZeroClaw Agent Session"` |
| `provider`, `model` | Provider/Model | `AgentStart.provider/model` |
| `duration_s` | 会话总耗时 | `AgentEnd.duration` |
| `tokens_used` | 总 token | `AgentEnd.tokens_used` |
| `langfuse.observation.cost_details` | 成本 | `AgentEnd.cost_usd`，JSON string `{"total": N}` |

## 5. 配置与编译

### 5.1 Feature Flag

项目采用 workspace 结构，feature flag 需在两级配置：

**Root `Cargo.toml`**（转发到子 crate）：
```toml
[features]
observability-langfuse = ["zeroclaw-runtime/observability-langfuse"]
```

**`crates/zeroclaw-runtime/Cargo.toml`**（实际依赖声明）：
```toml
[features]
observability-langfuse = ["dep:opentelemetry", "dep:opentelemetry_sdk", "dep:opentelemetry-otlp"]
```

编译：
```bash
cargo build --features observability-langfuse
```

### 5.2 运行时配置

```toml
# ~/.zeroclaw/config.toml
[observability]
backend = "langfuse"
langfuse_public_key = "pk-lf-..."     # 必填
langfuse_secret_key = "sk-lf-..."     # 必填
langfuse_base_url = "https://cloud.langfuse.com"  # 可选，默认值
langfuse_include_io = false            # 可选，默认 false
```

配置项说明：

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `backend` | string | 是 | `"none"` | 设为 `"langfuse"` |
| `langfuse_public_key` | string | 是 | — | 以 `pk-lf-` 开头，自动加密存储 |
| `langfuse_secret_key` | string | 是 | — | 以 `sk-lf-` 开头，自动加密存储 |
| `langfuse_base_url` | string | 否 | `https://cloud.langfuse.com` | 支持 EU/US/自部署 |
| `langfuse_include_io` | bool | 否 | `false` | 是否在 trace 中包含输入/输出内容 |

### 5.3 安全机制

- `public_key` / `secret_key` 标注 `#[secret]`，通过 ChaCha20-Poly1305 加密落盘
- `include_io` 默认 `false`，避免意外泄露 prompt/response 内容
- HTTP 认证使用 Basic Auth（public_key:secret_key base64），通过 TLS 传输

## 6. 数据流与推送

### 6.1 推送协议

```
POST https://cloud.langfuse.com/api/public/otel/v1/traces
Authorization: Basic <base64(pk:sk)>
x-langfuse-ingestion-version: 4
Content-Type: application/x-protobuf
```

### 6.2 推送时机

使用 OTel SDK `BatchSpanProcessor` 默认参数：

| 参数 | 默认值 | 说明 |
|---|---|---|
| `scheduled_delay` | 5000ms | 定时发送间隔 |
| `max_export_batch_size` | 512 spans | 单次最大发送量 |
| `max_queue_size` | 2048 spans | 超出后丢弃 |

触发条件：
1. **定时**：每 5 秒检查并发送
2. **批量满**：积压 ≥ 512 条立即发送
3. **AgentEnd**：span 关闭后进入待发送队列
4. **显式 flush**：程序退出时调用 `observer.flush()`
5. **Drop**：`LangfuseObserver` Drop 时自动 `force_flush()`

### 6.3 容错

- 发送失败 → OTel SDK 自动重试
- 持续失败 → 丢弃批次，输出 `tracing::warn!`
- Langfuse 完全不可达 → Agent 正常运行，无性能影响

## 7. 文件清单

| 文件 | 变更类型 | 说明 |
|---|---|---|
| `Cargo.toml` | 修改 | Root：新增 `observability-langfuse` feature 转发 |
| `crates/zeroclaw-runtime/Cargo.toml` | 修改 | 新增 `observability-langfuse` feature 依赖声明 |
| `crates/zeroclaw-api/src/observability_traits.rs` | 修改 | `LlmRequest`/`LlmResponse` 新增 I/O 可选字段 |
| `crates/zeroclaw-config/src/schema.rs` | 修改 | `ObservabilityConfig` 新增 4 个 langfuse 字段 |
| `crates/zeroclaw-runtime/src/observability/langfuse.rs` | **新增** | LangfuseObserver 核心实现 (~420 行) |
| `crates/zeroclaw-runtime/src/observability/mod.rs` | 修改 | 模块注册 + 工厂 `"langfuse"` 分支 |
| `crates/zeroclaw-runtime/src/observability/log.rs` | 修改 | 模式匹配适配新字段 |
| `crates/zeroclaw-runtime/src/observability/verbose.rs` | 修改 | 同上 |
| `crates/zeroclaw-runtime/src/observability/otel.rs` | 修改 | 同上 |
| `crates/zeroclaw-runtime/src/observability/prometheus.rs` | 修改 | 同上 |
| `crates/zeroclaw-runtime/src/agent/loop_.rs` | 修改 | 填充 I/O 字段 + 增量输入逻辑 |
| `crates/zeroclaw-gateway/src/lib.rs` | 修改 | Gateway API 构造点适配 |
| `docs/langfuse/langfuse-usage-guide.md` | 新增 | 使用指南 |
| `docs/langfuse/langfuse-tech-design.md` | 新增 | 本文档 |

## 8. 待办事项 / 后续规划

| 优先级 | 事项 | 说明 |
|---|---|---|
| P1 | `CacheHit`/`CacheMiss` 信息附加到 Generation | 将缓存命中信息作为 generation metadata |
| P1 | `Error` 事件关联到当前 Span | 组件错误附加到最近的 active span |
| P2 | `HandStarted`/`HandCompleted` 映射为 Span | 后台 Hand 执行也纳入 trace |
| P2 | `MultiObserver` 支持 Langfuse + 其他后端共存 | 当前 `backend` 只能选一个 |
| P2 | `userId` / `sessionId` 透传 | 从 Channel 信息提取用户/会话标识 |
| P3 | 环境变量覆盖配置 | `LANGFUSE_PUBLIC_KEY` 等环境变量支持 |
| P3 | Score 上报（`langfuse.score.*`） | 将 token 消耗、成本作为 Score 上报 |

## 9. 风险评估

| 风险 | 影响 | 缓解措施 |
|---|---|---|
| Langfuse OTLP endpoint 变更 | trace 上报中断 | OTLP 是标准协议，Langfuse 承诺长期支持 |
| OTel SDK 版本升级 | 编译/行为变更 | `opentelemetry*` 0.31 已稳定；升级时同步调整 |
| `ObserverEvent` 字段膨胀 | 内存/性能 | 新增字段均为 `Option<String>`，默认 `None`，零开销 |
| 大型 history 序列化耗时 | Agent loop 延迟 | `serde_json::to_string` 极快；增量输入策略确保只序列化新增消息 |
| 密钥泄露 | 安全风险 | `#[secret]` 加密存储 + TLS 传输 |
