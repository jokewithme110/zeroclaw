# Skills

Skills are reusable instructions and optional tool definitions that ZeroClaw can load into an agent session. Use them for repeatable workflows such as code review checklists, deployment runbooks, support playbooks, or domain-specific tool wrappers.

Skills live in the agent's workspace under `skills/<name>/`. For the default agent this is:

```text
~/.zeroclaw/agents/<agent-alias>/workspace/skills/<name>/
```

For the `router` agent: `~/.zeroclaw/agents/router/workspace/skills/<name>/`.

Skills installed via the SkillHub agent tools or CLI use this same directory. The legacy `~/.zeroclaw/data/skills/` directory is no longer used — if you have skills there from an older version, move them into the agent workspace directory.

For hand-authored local skills, use `SKILL.md` or `SKILL.toml`. Use `SKILL.md` for instructions plus simple metadata. Use `SKILL.toml` when the skill needs structured prompts or tool definitions. ZeroClaw also understands `manifest.toml` for registry-style skill packages, but `SKILL.md` and `SKILL.toml` are the recommended local authoring formats.

## Create a Markdown skill

A minimal instruction-only skill can be just a Markdown file:

<div class="os-tabs-src">

#### sh

```sh
mkdir -p ~/.zeroclaw/agents/router/workspace/skills/release-check
$EDITOR ~/.zeroclaw/agents/router/workspace/skills/release-check/SKILL.md
```

</div>

```markdown
# Release check

Review the release notes, changelog, version tags, and migration notes before confirming that a release is ready.
```

The directory name becomes the skill name. ZeroClaw uses the first non-heading paragraph as the description when no frontmatter description is present.

`SKILL.md` also supports simple frontmatter for metadata:

```markdown
---
name: release-check
description: Check release readiness before tagging
version: 0.1.0
author: zeroclaw_user
tags: [release, docs]
---

# Release check

Review the release notes, changelog, version tags, and migration notes before confirming that a release is ready.
```

Supported frontmatter fields are `name`, `description`, `version`, `author`, and `tags`.

## Create a TOML skill

A skill can also be a structured TOML manifest (`SKILL.toml`). The `[skill]` table requires `name` and `description`; `version` defaults to `0.1.0` when omitted; `author`, `tags`, and `prompts` are optional. Tool entries may use `kind = "shell"`, `kind = "http"`, or `kind = "script"`. Keep tool descriptions narrow and concrete so the model knows when to use them.

## Manage installed skills

List installed skills:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills list
```

</div>

Audit an installed skill or a local skill directory:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills audit release-check
zeroclaw skills audit ./release-check
```

</div>

Install a skill from a local directory, Git URL, registry name, or ClawHub source:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills install ./release-check
zeroclaw skills install https://example.com/zeroclaw-release-check.git
zeroclaw skills install release-check
zeroclaw skills install clawhub:release-check
```

</div>

Remove an installed skill:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills remove release-check
```

</div>

Run `TEST.sh` validation for one skill, or omit the name to test all installed skills:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills test release-check
zeroclaw skills test --verbose
```

</div>

`zeroclaw skills test` runs the skill's `TEST.sh` file when one exists. Inspect `TEST.sh` before running tests from a skill source you do not already trust.

## Prompt-triggered capability suggestions

ZeroClaw can optionally suggest an installable skill capability when a submitted prompt clearly names something that exists in cached registry metadata but is not installed. The server-side path runs after submission and before the normal LLM turn. It only returns a suggestion; it does not install the skill, enable it, write memory, or treat the skill body as global instructions.

Enable it via the `skills` config (gateway, zerocode, or `zeroclaw config set`). The suggestion matcher uses installed skill names and cached registry metadata such as names, aliases, and frontmatter. It intentionally avoids matching unapproved skill bodies. Plugin/package-level discovery remains follow-up scope until the plugin registry search/install surface is available. Exact composer-time suggestions while the user is still typing require ACP, gateway, or client UI support and are outside this server-only path.

## Script safety

ZeroClaw audits skills before loading or installing them. Script-like files such as `.sh`, `.bash`, `.ps1`, and files with shell shebangs are blocked by default.

If you intentionally use script-bearing skills, enable `skills.allow_scripts`. Keep this disabled unless you trust the skill source and have reviewed what the scripts do.

When `allow_scripts = false` and a skill installed from a SkillHub contains `.sh` files, the audit rejects the installation with `script-like files are blocked by skill security policy`. The skill will not be written to disk. To accept script-bearing skills, set `allow_scripts = true` and ensure `[skills.scan] enabled = true` as a second line of defence.

For Python-specific execution patterns, interpreter policy, and native versus Docker trade-offs, see [Running Python skills](./python-skills.md).

## Loading community skills

Community open-skills loading is opt-in via the `skills` config. When enabled, ZeroClaw loads skills from the configured `open_skills_dir`, or from `$HOME/open-skills` when no directory is set. If that directory does not exist, ZeroClaw may clone the community open-skills repository; if it does exist and is a git checkout, ZeroClaw may pull updates. Enable this only for community sources you trust, or point `open_skills_dir` at a reviewed local copy.

## Advanced config

The default prompt injection mode is `full`, which includes full skill instructions in the system prompt. Use `compact` to keep only compact metadata in context and load skill details on demand:

## Self-hosted SkillHub integration

ZeroClaw's default `clawhub:` install source points at the public ZeroClaw SkillHub at `https://clawhub.ai`. Operators who run a private or curated set (e.g. the ICT production curated set) can point ZeroClaw at their own SkillHub with one config key:

```toml
[skills]
# Base URL of the SkillHub (skill search / list / download APIs).
# Default when omitted: https://clawhub.ai
# ICT production curated set (8 skills): https://skillhubictst.mec189.cn/enhance
# ICT full self-hosted registry (35 skills): https://skillhubictst.mec189.cn
skillhub_base_url = "https://skillhubictst.mec189.cn/enhance"
```

### Dual-entry architecture (ICT)

The ICT self-hosted SkillHub exposes two entry points from the same server:

| Base URL | Skills | When to use |
|---|---|---|
| `https://skillhubictst.mec189.cn/enhance` | 8 curated production skills | Agent default configuration |
| `https://skillhubictst.mec189.cn` | 35 full-registry skills | Development, debugging, skill authoring |

Switch between them by changing `skillhub_base_url` — no other config changes needed. The `/enhance` prefix is part of the base URL; all API paths (`/api/v1/skills`, `/api/v1/search`, `/api/v1/download`) are appended automatically.

### CLI install with version pinning

The CLI `skills install` command supports three source forms against the configured hub:

```sh
# Install the latest version (auto-resolves from /api/v1/skills/<slug>)
zeroclaw skills install clawhub:attendance-query-lite

# Pin a specific version
zeroclaw skills install clawhub:attendance-query-lite@20260528.071446

# ICT date-format versions (yyyyMMdd.HHmmss)
zeroclaw skills install clawhub:ftto-skills-lite@20260520.030403
```

When `version` is omitted, the CLI calls the detail endpoint to resolve `latestVersion.version`. When specified after `@`, the version is passed directly to the download endpoint — saving one HTTP round-trip.

### Agent-driven skill lifecycle

When `[skills] enable_agent_skill_management = true`, the running agent has three new tools that operate against the configured SkillHub:

| Tool | Risk | Behaviour |
|---|---|---|
| `skill_search` | Low | Read-only query against `/api/v1/skills` (list) and `/api/v1/search?q=…` (search). Returns slug, name, latest version, description, download count. Pass no query to list all skills; pass a keyword to search. |
| `skill_install` | Medium-High | Downloads + extracts a skill archive into `agents/<alias>/workspace/skills/<slug>/`, then **hot-loads** the skill's tools into the live `ToolRegistry`. If `version` is omitted, auto-resolves the latest from the detail endpoint. |
| `skill_remove` | Medium | Deletes the local skill directory and **hot-unloads** its tools from the running agent. Does not affect the remote hub. |

**Default is `false` (opt-in) for security.** Set `enable_agent_skill_management = true` to let the agent discover, install, and remove skills autonomously. With the flag off the three tools are not registered and the agent cannot autonomously install or remove skills — CLI remains the only management path.

### Installation and security flow

When the agent installs a skill:

1. `skill_search` queries the hub to discover available skills
2. `skill_install` downloads the ZIP archive (follows HTTP 302 redirect to the actual download URL)
3. The ZIP is extracted to `agents/<alias>/workspace/skills/<slug>/`
4. `SKILL.md` / `SKILL.toml` is parsed; a stub manifest is generated if none exists
5. Security audit runs against the extracted directory:
   - `allow_scripts` check: `.sh`, `.bash`, `.ps1`, and shebang files are blocked unless `allow_scripts = true`
   - Path traversal check: `..`, absolute paths, backslashes, and colons in ZIP entries are rejected
   - Size limit: archives larger than 50 MiB are rejected
   - External scan (if `[skills.scan] enabled = true`): the skill is uploaded to the configured scan service for malware detection
6. On audit failure, the extracted directory is rolled back (deleted)
7. On success, the skill's tools are hot-loaded into the live `ToolRegistry`

### Hot-load behaviour

- **Tools become available on the NEXT message** — the orchestrator holds a read lock on the tool registry during the current turn. Hot-load runs in a background task (retries every 5 s for up to 5 min) and completes once the lock is released at turn end. The agent response will say *"new tools will be available on your NEXT message"*.
- Existing tool names are **not overwritten** — a hot-reload whose tool name collides with a builtin (e.g. `shell`, `file_read`) logs a warning and skips the duplicate
- **DAG plans** that were already in flight before the install do not see the new tools. The install result includes a note: restart the agent if a running DAG plan needs the new tools
- Hot-unload (`skill_remove`) follows the same pattern: directory is deleted immediately, tool unregistration completes in background on the next turn

### Updating an installed skill

To update a skill to a newer version, first `skill_remove` the old version, then `skill_install` the new one:

```sh
# Via CLI
zeroclaw skills remove attendance-query-lite
zeroclaw skills install clawhub:attendance-query-lite

# Or via agent dialogue: "update attendance-query-lite to latest"
```

> **Limitation**: `skill_install` does not support overwriting an already-installed skill. A `force` parameter (to remove + reinstall in one step) is planned for a future release.

### Current limitations

| Limitation | Impact | Workaround |
|---|---|---|
| **No overwrite install** | `skill_install` on an existing slug returns an error | Use `skill_remove` → `skill_install` to update |
| **Tools not available in same turn** | Newly installed tools are only callable on the NEXT user message | Mention "new tools生效" in agent response; user sends a follow-up message |
| **DAG plans don't see new tools** | `execute_pipeline` plans started before install won't use the new skill's tools | Restart the agent or start a new pipeline |
| **No local status in search results** | `skill_search` output doesn't mark which skills are already installed locally | Agent can supplement with `read_skill` or `ls` to check local state |
| **No built-in version comparison** | For cron-based update checks, the agent must compare `yyyyMMdd.HHmmss` strings manually (LLM reasoning) | Reliable for date-based versions; semver requires LLM judgement |

### Example agent dialogue

```
User: "有没有能查考勤的 skill？给我装上"

Agent:
  → skill_search(query="考勤")              → returns attendance-query-lite v20260528.071446
  → skill_install(slug="attendance-query-lite") → downloads, audits, spawns background hot-load
  → "装好了！新工具将在下一条消息时生效。"

User: "那查下 6 月考勤"（下一条消息 — 工具此时已加载）

Agent:
  → attendance_query_lite__<tool> → "2026年6月考勤：全勤"

User: "卸了它"

Agent:
  → skill_remove(slug="attendance-query-lite") → deletes dir, spawns background unload
  → "已卸载，工具将在下条消息时消失。"
```

For autonomy-level and `auto_approve` guidance on these three tools, see [Autonomy levels → Recommended per-tool settings](../security/autonomy.md#recommended-per-tool-settings).

## See also

- [Tools overview](./overview.md)
- [Security overview](../security/overview.md)
- [Tool receipts](../security/tool-receipts.md)
- [Autonomy levels](../security/autonomy.md)
