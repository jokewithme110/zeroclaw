use std::path::Path;

use anyhow::{Context, Result};

use zeroclaw_config::schema::{Config, SkillBundleConfig};
use zeroclaw_config::skill_bundles::{default_directory, resolve_directory};

use super::init_skills_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BootstrapSummary {
    pub files_created: usize,
}

pub fn bootstrap_builtin_template_skills(config: &mut Config) -> Result<BootstrapSummary> {
    ensure_default_skill_bundle(config);
    let install_root = config.install_root_dir();
    let skills_root = resolve_directory(config, &install_root, "default")
        .context("failed to resolve default skill bundle directory")?;
    init_skills_dir(&skills_root).context("failed to initialize skills directory")?;

    let mut created = 0usize;
    created += write_template_file(&skills_root.join("a2a-setup/SKILL.md"), A2A_SETUP_SKILL_MD)?;
    created += write_template_file(
        &skills_root.join("a2a-setup/references/tools-md-template.md"),
        A2A_SETUP_TOOLS_REFERENCE_MD,
    )?;
    created += write_template_file(
        &skills_root.join("co-skill-creator/SKILL.md"),
        CO_SKILL_CREATOR_SKILL_MD,
    )?;
    created += write_template_file(
        &skills_root.join("co-skill-creator/references/multi_skill_orchestration_template.md"),
        CO_SKILL_CREATOR_ORCHESTRATION_REFERENCE_MD,
    )?;

    Ok(BootstrapSummary {
        files_created: created,
    })
}

fn ensure_default_skill_bundle(config: &mut Config) {
    if !config.skill_bundles.contains_key("default") {
        let default_dir = default_directory(&config.install_root_dir(), "default");
        config.skill_bundles.insert(
            "default".to_string(),
            SkillBundleConfig {
                directory: Some(default_dir.display().to_string()),
                ..Default::default()
            },
        );
    }
}

fn write_template_file(path: &Path, content: &str) -> Result<usize> {
    if path.exists() {
        return Ok(0);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create template directory: {}", parent.display())
        })?;
    }

    std::fs::write(path, content)
        .with_context(|| format!("failed to write template file: {}", path.display()))?;
    Ok(1)
}

const A2A_SETUP_SKILL_MD: &str = r###"---
name: a2a-setup
description: configure the ZeroClaw A2A Gateway for cross-server agent communication. Use whenever the user wants to connect one or more peer agents and can provide `agent_url` plus token/auth info. Always execute: (1) collect `agent_url` + token, (2) deterministically build Agent Card URL (no inference), (3) fetch and parse capabilities/auth details, (4) write/update `TOOLS.md` with multi-peer profile, (5) return verification checklist.
---

# A2A Gateway Setup

Configure the ZeroClaw A2A Gateway for cross-server agent-to-agent communication using the A2A v0.3.0 protocol.

## Step 1: Collect User Input (Required)

Ask the user for:
- `agent_url` (required): the peer JSON-RPC endpoint, e.g. `http://100.x.x.x:42617/a2a`
- `token` (required if peer is protected): bearer/API token for peer authentication

If the user gives a base URL instead of `agent_url`, normalize it to:
- `<base_url>/a2a` as `agent_url`

Do not ask the user for a full agent card path.

## Step 2: Build and Fetch Agent Card URL (Required, No Inference)

Given `agent_url`, use exactly one deterministic rule:

- If `agent_url` ends with `/a2a`, then:
  - `agent_card_url = <agent_url>/a2a/.well-known/agent-card.json`
- Otherwise:
  - `agent_card_url = <agent_url>/.well-known/agent-card.json`

Do not generate candidate lists.
Do not attempt any extra URL fallback or path guessing.

Fetch this resolved `agent_card_url` directly.
Include auth header when token exists:
- `Authorization: Bearer <token>`
- Request tool policy:
  - Prefer `curl` when shell supports it.
  - If `curl` is unavailable, use built-in `http_request` tool (user may call it `http_reqeust`) to perform the same GET request with equivalent headers.

Example:
```bash
curl -fsSL -H "Authorization: Bearer <token>" "<agent_card_url>"
```

If this URL fails, stop and return:
- the exact resolved `agent_card_url`
- HTTP/status error
- suggested fixes (network reachability, token validity, reverse proxy path, TLS/cert)

## Step 3: Parse Agent Profile and Capabilities (Required)

From fetched Agent Card JSON, extract and summarize clearly:
- Identity: `name`, `description`, `version`, `provider` (if present)
- Endpoints: `agent_url`, `agentCardUrl`, other transport endpoints
- Capabilities: all declared capabilities/features with short meaning
- Skills/tools:
  - each item `name`
  - each item `description` or purpose
  - invocation hints/limits if present
- Input/output modalities if present
- Auth/security:
  - security scheme(s)
  - required header names
  - whether bearer token is required
  - scopes/permissions if present

### URL field rules

| Field          | Points to                        | Example                                                  |
| -------------- | -------------------------------- | -------------------------------------------------------- |
| `agent_url`    | JSON-RPC endpoint (default)      | `http://100.x.x.x:42617/a2a`                             |
| `agentCardUrl` | Agent Card discovery (preferred) | `http://100.x.x.x:42617/a2a/.well-known/agent-card.json` |

**Do NOT confuse these two.** `agent_url` is for sending messages. `agentCardUrl` is for discovery.

## Step 4: Update TOOLS.md Using Multi-Peer Template (Mandatory, never skip)

**This step is mandatory.** Always write or append an A2A section into the agent's `TOOLS.md` after capability discovery.

Requirements (single or multiple peers):
- Record user-provided `agent_url`
- Record resolved `agentCardUrl` (final successful URL)
- Record token usage and auth requirements (do not leak full secret; mask it)
- Record complete capability/skills/tools profile with descriptions
- Record practical invocation instructions for `a2a_client`
- If connecting multiple agents, append one peer block per agent in the same A2A registry section

Use `references/tools-md-template.md` as the base template and replace placeholders with real values from the card and user config.

## Step 5: Final Verification Output (Required)

Confirm all of the following in the final response:
1. `agent_url` and token were received
2. Agent Card URL was auto-discovered and fetched successfully
3. Capabilities + skills/tools + auth were extracted
4. `TOOLS.md` was written/updated from template
5. Peer is ready for invocation via `a2a_client`

If any item failed, explicitly mark it as failed and explain next action.
"###;

const A2A_SETUP_TOOLS_REFERENCE_MD: &str = r###"
# TOOLS.md A2A Section Template

Append this section to the agent's `TOOLS.md` file, replacing all `<PLACEHOLDERS>` with actual values.
---

```markdown
## A2A Gateway (Agent-to-Agent Communication)

Use `a2a_client` to communicate with peer agents through A2A.

### Peer Registry (Overview)
| Peer ID | Peer Name | Agent URL | Agent Card URL | Auth | Status |
| ------- | --------- | --------- | -------------- | ---- | ------ |
| `<PEER_ID_1>` | `<PEER_NAME_1>` | `<PEER_AGENT_URL_1>` | `<PEER_AGENT_CARD_URL_1>` | `<AUTH_SCHEME_1>` | `<PEER_STATUS_1>` |
| `<PEER_ID_2>` | `<PEER_NAME_2>` | `<PEER_AGENT_URL_2>` | `<PEER_AGENT_CARD_URL_2>` | `<AUTH_SCHEME_2>` | `<PEER_STATUS_2>` |

> Add one row per connected peer.

### Peer Detail: <PEER_ID_1>
- Peer Name: <PEER_NAME_1>
- Agent URL: <PEER_AGENT_URL_1>
- Agent Card URL: <PEER_AGENT_CARD_URL_1>
- Base Origin: <PEER_BASE_ORIGIN_1>
- Network Reachability: <NETWORK_REACHABILITY_SUMMARY_1>

#### Authentication
- Scheme: <AUTH_SCHEME_1>
- Header: <AUTH_HEADER_NAME_1>
- Token (masked): <PEER_TOKEN_MASKED_1>
- Required Scopes/Permissions: <AUTH_SCOPES_OR_NA_1>

#### Agent Identity
- Version: <AGENT_VERSION_OR_NA_1>
- Provider: <AGENT_PROVIDER_OR_NA_1>
- Description: <AGENT_DESCRIPTION_1>

#### Capabilities (from Agent Card)
- Capability Summary: <CAPABILITY_SUMMARY_1>
- Input Modalities: <INPUT_MODALITIES_OR_NA_1>
- Output Modalities: <OUTPUT_MODALITIES_OR_NA_1>
- Other Constraints/Limits: <CONSTRAINTS_OR_NA_1>

#### Skills / Tools Exposed by Peer
| Name | Type | Description | Invocation Notes |
| ---- | ---- | ----------- | ---------------- |
| <ITEM_NAME_1_1> | <ITEM_TYPE_1_1> | <ITEM_DESC_1_1> | <ITEM_NOTES_1_1> |
| <ITEM_NAME_1_2> | <ITEM_TYPE_1_2> | <ITEM_DESC_1_2> | <ITEM_NOTES_1_2> |

> If no skills/tools are listed in the card, write: `No explicit skills/tools declared in Agent Card`.

#### How to invoke this peer
When the user says "通过 A2A 让 <PEER_NAME_1> 做 xxx" / "Send to <PEER_NAME_1>: xxx" / "Ask <PEER_NAME_1> to ...", use `a2a_client` with:
- `agent_url`: `<PEER_AGENT_URL_1>`
- auth header: `<AUTH_HEADER_NAME_1>: <TOKEN_AT_RUNTIME>`
- request intent: the user's original task

#### Last Verified
- Verified At: <VERIFIED_AT_ISO8601_1>
- Verification Result: <VERIFICATION_RESULT_1>
- Notes: <VERIFICATION_NOTES_1>

---

### Peer Detail: <PEER_ID_2>
Repeat the same detail block for each additional peer.
```

---

## Placeholder Reference

Use suffix `_N` for peer index (for example `_1`, `_2`, `_3`).

| Placeholder Pattern | Description | Example |
| ------------------- | ----------- | ------- |
| `<PEER_ID_N>` | Stable peer identifier | `peer-server-a` |
| `<PEER_NAME_N>` | Display name of peer agent | `Server-A` |
| `<PEER_AGENT_URL_N>` | Peer JSON-RPC endpoint | `http://100.76.43.74:42617/a2a` |
| `<PEER_AGENT_CARD_URL_N>` | Resolved working Agent Card URL | `http://100.76.43.74:42617/a2a/.well-known/agent-card.json` |
| `<PEER_BASE_ORIGIN_N>` | Scheme + host (+port) of peer | `http://100.76.43.74:42617` |
| `<NETWORK_REACHABILITY_SUMMARY_N>` | Reachability notes from verification | `reachable via tailscale` |
| `<AUTH_SCHEME_N>` | Auth scheme used by peer | `Bearer` |
| `<AUTH_HEADER_NAME_N>` | Auth header key | `Authorization` |
| `<PEER_TOKEN_MASKED_N>` | Token with masking applied | `9489c2...10ab` |
| `<AUTH_SCOPES_OR_NA_N>` | Required scopes/permissions | `a2a:invoke` |
| `<AGENT_VERSION_OR_NA_N>` | Agent version | `0.3.0` |
| `<AGENT_PROVIDER_OR_NA_N>` | Agent provider/owner | `ZeroClaw` |
| `<AGENT_DESCRIPTION_N>` | Agent purpose summary | `Handles incident triage tasks` |
| `<CAPABILITY_SUMMARY_N>` | Human-readable capability summary | `supports task execution and tool routing` |
| `<INPUT_MODALITIES_OR_NA_N>` | Accepted input modalities | `text` |
| `<OUTPUT_MODALITIES_OR_NA_N>` | Produced output modalities | `text, json` |
| `<CONSTRAINTS_OR_NA_N>` | Limits and constraints | `max payload 64KB` |
| `<PEER_STATUS_N>` | Connectivity/health state | `ready` |
| `<VERIFIED_AT_ISO8601_N>` | Last verification timestamp | `2026-03-25T10:30:00Z` |
| `<VERIFICATION_RESULT_N>` | Verification result | `success` |
| `<VERIFICATION_NOTES_N>` | Verification notes | `card fetched and call test passed` |

For multiple peers, keep one shared `## A2A Gateway` section and append one `### Peer Detail: <PEER_ID_N>` block per peer.
"###;

const CO_SKILL_CREATOR_SKILL_MD: &str = r###"---
name: co-skill-creator
description: 编排型技能生成器。当用户明确表达想要创建技能时例如:「要创建技能」「创建编排技能」「基于已有技能编排」「组合技能做流程」使用本技能。
version: 1.0.0
author: XYDT Studio/Dep3
composable: false
---

# 技能：编排型技能生成器（co-skill-creator）

本技能**仅**负责生成**编排型技能**：多技能链路、输入输出契约、字段映射与异常策略，全部写入新技能的 `SKILL.md`。**MUST** 从步骤 1 起顺序执行。

**MUST** 凡由本技能生成的新技能，其 frontmatter **必须**包含 **`composable: false`**（固定值），**MUST NOT** 写 `composable: true`、**MUST NOT** 省略该字段，以免被更上层编排自动选入。

## 规范语言

- **MUST**：强制性要求，禁止跳过。
- **MUST NOT**：禁止行为。
- **SHOULD**：强烈建议。

---

## 步骤 1：收集编排元数据与目标

当用户明确表达“创建技能 / 新建技能 / 做一个技能”时，**MUST** 先获取可组合技能列表。

### 发现候选技能

按以下顺序获取候选技能：

1. 扫描：
   ```text
   workspace/skills
   ```
2. 目录扫描时，**MUST** 先用 `glob_search` 获取所有 `SKILL.md` 和 `SKILL.toml` 路径，再用 `content_search` 统一检索 `composable: false`
3. 命中 `composable: false` 的技能排除；未命中的技能默认可用
4. 对结果去重后得到最终可编排技能列表

发现候选技能时的约束：

- **MUST NOT** 在步骤 1 逐个 `file_read` 技能文件
- **MUST NOT** 因为未检索到 `composable` 字段，就回退为逐个读取文件确认
- **MUST** 将“未命中 `composable: false`”直接解释为“可用候选”
- 若没有可用候选，**MUST** 明确告知用户当前无法创建该编排技能；若一个都没有，再回复“当前不支持该能力”

### 判断是否可编排

拿到候选技能后，**MUST** 先判断这些能力是否足以支撑用户当前目标，再决定是否继续追问。

- 若用户目标需要某个核心原子能力，而当前候选技能中完全不存在该能力，**MUST** 直接判定无法创建
- 若当前目标与候选技能明显不匹配，**MUST** 直接告知用户当前无法创建该编排技能，并列出当前可编排技能
- 若用户提出的目标本身就不属于当前候选技能的能力边界，**MUST** 直接判定不可行；**MUST NOT** 再追问“想实现什么功能”“具体想要什么能力”之类的澄清问题
- 例如用户要“打麻将技能”，而当前候选技能仅覆盖提醒、日程、设备控制等外围能力，不包含麻将本体玩法能力时，**MUST** 直接告知不可行，**MUST NOT** 继续追问记录、提醒或其他附属功能
- **MUST NOT** 在能力不足时提供替代路线、编号选项、独立技能路线或近似方案
- **MUST NOT** 勉强拼接部分命中的技能，生成“近似可用”的编排方案
- 只有当目标与候选技能存在明确可编排空间时，才可继续追问用户想组合哪些能力、想达到什么效果

若用户需要创建的技能已判定不可行，**MUST** 使用以下稳定格式回复：

```markdown
当前无法创建该编排技能。

当前可编排的技能有：
- <技能A>
- <技能B>
- <技能C>
```

要求：
- 只说明“无法创建”并列出当前可编排技能
- **MUST NOT** 追加追问、替代方案、功能猜测或需求引导
- 若当前没有任何可编排技能，直接回复：`当前不支持该能力`

在用户确认“要组合的能力与目标”后，补齐生成 `SKILL.md` 所需的必要信息即可。能从上下文直接推断的就直接推断；只有缺失时再追问用户。

---

## 步骤 2：确认复用链路

只有在用户目标已经被判定为“可编排”，且用户确认了要组合的候选技能后，才进入本步骤。

对已入选技能：**MUST** 阅读对应 `SKILL.md`（必要时 `references/`），归纳可衔接的 I/O、前置条件、输出字段与失败信号，并确认是否能组成完整链路。

- 与用户确认组合方案时，**MUST** 用自然语言描述链路、每步职责、预期结果与主要限制
- **MUST NOT** 默认展示 `JSON`、字段映射表或大段内部结构化内容
- 若在本步骤发现仍存在能力缺口，**MUST** 立即告知用户当前无法创建该编排技能，并列出当前可编排技能；**MUST NOT** 继续生成技能目录或 `SKILL.md`

---

## 步骤 3：初始化技能目录

在已确认的 `<workspace_skills_dir>` 下创建：

```
<workspace_skills_dir>/
└── <skill-name>/
    ├── SKILL.md        ← 步骤 4 填写
    └── references/     ← 可选：仅当有补充参考文档时创建
        └── ...
```

约束：

- 目录名、`SKILL.md` 内 frontmatter 的 `name`、步骤 1 中的 `<skill-name>` **MUST** 三者一致。
- **MUST NOT** 在 `<skill-name>/` 根下放置除 `SKILL.md` 外的 Markdown；补充说明 **MUST** 放在 `references/`。
- **MUST NOT** 创建空 `references/` 占位。

初始化后 **MUST** 自检：至少存在 `<skill-name>/SKILL.md`。

---

## 步骤 4：写入 `SKILL.md`

在步骤 3 目录内写入完整 `SKILL.md`。

### Frontmatter

**MUST** 包含以下字段：

```yaml
---
name: <skill-name>
description: <第三人称：编排能力 + 触发场景>
version: 1.0.0
generator: co-skill-creator
composable: false
---
```

- `name` **MUST** 与目录名一致
- `description` **MUST** 用第三人称，写清“做什么 + 何时触发”
- `composable` **MUST** 恒为 `false`；**MUST NOT** 改为 `true` 或省略
- **MUST NOT** 使用无 `---` 包裹的松散 frontmatter

### 正文

**SHOULD** 以 `references/multi_skill_orchestration_template.md` 作为骨架填写，并确保至少包含：复用技能清单、字段映射、路由规则、执行流程、异常处理、输出约定。

正文要求：

- 中间技能输入按上一步输出 + 路由条件组装。
- 路由步骤必须产出 `next_skill`、`next_input`（建议含 `reason`）。
- 映射中的关键字段必须可在 I/O 契约定位，且 `on_missing` 明确。
- `references/multi_skill_orchestration_template.md` 仅作占位模板；**MUST NOT** 将模板示例或占位文本原样写入目标技能 `SKILL.md`。
- **SHOULD** 优先复用已有技能能力边界；**MUST NOT** 在编排层虚构不存在的原子能力或字段。

---

## 完成前自检

- [ ] `name` 与目录名一致
- [ ] frontmatter 含 **`composable: false`** 且未写 `true`、未省略
- [ ] 未违反已有技能的 `composable` 过滤规则
- [ ] 正文含复用说明、字段映射、路由规则与异常策略
- [ ] 已定义输出驱动的下一步路由规则（条件、目标技能、输入组装）
- [ ] 路由步骤能产出 `next_skill` 与 `next_input`，下游技能可直接消费
"###;

const CO_SKILL_CREATOR_ORCHESTRATION_REFERENCE_MD: &str = r###"
---
name: <skill-name>
description: <适用场景>
version: 1.0.0
generator: co-skill-creator
composable: false
---

## 目标

- 目标描述：<在什么触发场景下，为实现什么业务目标，最终输出什么结果形态>

## 复用能力说明

- 复用 `<skill-a>`：<职责>
- 复用 `<skill-b>`：<职责>

## 技能链路（上下游）

1. 上游 `<skill-a>`：输入 `<...>` → 输出 `<...>`
2. 决策 Agent `<router-agent>`：读取上游输出，产出 `next_skill` + `next_input`
3. 下游 `<skill-c>`：输入 `<...>` → 输出 `<...>`

## 复用技能清单（I/O 契约）

- `<skill-a>`（上游）：输入 `<...>`；输出 `<...>`；职责 `<...>`；前置条件 `<可选>`
- `<skill-c>`（下游）：输入 `<...>`；输出 `<...>`；职责 `<...>`；前置条件 `<可选>`

## 字段映射（上下游）

- `<skill-a>.<output_field>` -> `<skill-b>.<input_field>`；转换 `<透传/格式转换/枚举映射>`；缺失处理 `<报错/默认值/跳过>`；备注 `<可选>`
- `<skill-b>.<output_field>` -> `<skill-c>.<input_field>`；转换 `<...>`；缺失处理 `<...>`；备注 `<可选>`

## 路由规则（按输出决定下一步）

- 条件 `<condition-a>` 命中时：调用 `<next-skill-a>`，输入组装 `<from pipeline/intermediate/...>`
- 条件 `<condition-b>` 命中时：调用 `<next-skill-b>`，输入组装 `<...>`
- 未命中任何条件：`<默认分支：结束/兜底技能/追问用户>`
## 执行流程

1. …
2. …
3. …

## 异常处理

- …

## 输出约定

- 成功：…
- 失败：…
"###;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.config_path = dir.path().join("config.toml");
        cfg.data_dir = dir.path().join("data");
        (dir, cfg)
    }

    #[test]
    fn bootstrap_templates_writes_expected_files() {
        let (temp, mut cfg) = fixture();

        let summary = bootstrap_builtin_template_skills(&mut cfg).expect("bootstrap");
        assert_eq!(summary.files_created, 4);

        let skills_root = temp.path().join("shared/skills/default");
        assert!(skills_root.is_dir());
        assert!(skills_root.join("README.md").exists());
        assert!(cfg.skill_bundles.contains_key("default"));

        let a2a_skill = std::fs::read_to_string(skills_root.join("a2a-setup/SKILL.md"))
            .expect("a2a skill should exist");
        assert!(a2a_skill.contains("name: a2a-setup"));
        assert!(a2a_skill.contains("agent_card_url"));

        let a2a_reference =
            std::fs::read_to_string(skills_root.join("a2a-setup/references/tools-md-template.md"))
                .expect("a2a reference should exist");
        assert!(a2a_reference.contains("TOOLS.md A2A Section Template"));

        let creator_skill = std::fs::read_to_string(skills_root.join("co-skill-creator/SKILL.md"))
            .expect("creator skill should exist");
        assert!(creator_skill.contains("name: co-skill-creator"));
        assert!(creator_skill.contains("composable: false"));

        let orchestration_reference = std::fs::read_to_string(
            skills_root.join("co-skill-creator/references/multi_skill_orchestration_template.md"),
        )
        .expect("orchestration reference should exist");
        assert!(orchestration_reference.contains("next_skill"));
    }

    #[test]
    fn bootstrap_templates_is_non_destructive_for_existing_files() {
        let (temp, mut cfg) = fixture();
        bootstrap_builtin_template_skills(&mut cfg).expect("initial bootstrap");

        let template_path = temp.path().join("shared/skills/default/a2a-setup/SKILL.md");
        std::fs::write(&template_path, "custom").expect("seed custom file");

        let summary = bootstrap_builtin_template_skills(&mut cfg).expect("second bootstrap");
        assert_eq!(summary.files_created, 0);

        let content = std::fs::read_to_string(&template_path).expect("custom file should remain");
        assert_eq!(content, "custom");
    }
}
