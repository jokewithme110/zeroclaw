# Skill 安全扫描需求（最终草案）

## 1. 目标

Skill 在运行前必须通过安全扫描门禁。  
仅允许满足风险阈值策略的 Skill 被加载和运行。

本方案约束：

- 扫描方式固定为压缩包上传。
- 完整性基线固定为压缩包 SHA256（`archive_sha256`），不使用 `skill.md` SHA256。
- 扫描功能代码统一放在 `src/skills/scan/`，保持结构清晰、职责单一。

## 2. 接口约定

### 2.1 上传扫描接口

- 协议：HTTP POST（multipart/form-data）
- 入参：压缩包文件（字段名 `file`）
- 出参关键字段：
  - `task_no`
  - `file_sha256`
  - `status` / `status_text`

### 2.2 查询结果接口

- 协议：HTTP GET
- 入参：`task_no`（query 参数）
- 出参关键字段：
  - `task_no`
  - `status` / `status_text`（pending/scanning/completed）
  - `is_safe`
  - `max_severity`
  - `file_sha256`
  - `result`（详细发现）

## 3. 触发机制

1. 配置开关控制是否启用扫描。  
2. 启动时触发扫描。  
3. 定时触发扫描。

## 4. 完整性策略

### 4.1 SHA256 基线

每个 Skill 的完整性比较对象为压缩包 SHA256（`archive_sha256`）：

1. 对 Skill 目录进行确定性打包（固定文件顺序和元信息策略）。
2. 基于上传的压缩包字节计算 `archive_sha256`。
3. 上传后将本地 `archive_sha256` 与接口返回 `file_sha256` 进行一致性校验。

### 4.2 篡改判定与处置

定时任务中若检测到当前 `archive_sha256` 与最近一次通过扫描的基线不一致：

1. 立即将 Skill 标记为 `blocked`（不可用）。
2. 触发重新扫描。
3. 重扫通过后再恢复为 `allowed`（可用）。

## 5. 风险等级与准入策略

### 5.1 风险等级枚举

`CRITICAL/HIGH/MEDIUM/LOW/INFO/SAFE`

风险由高到低顺序：

`CRITICAL > HIGH > MEDIUM > LOW > INFO > SAFE`

### 5.2 阈值配置语义

配置项 `max_allowed_severity` 表示“允许该等级及以下风险”。

示例：

- `LOW`：允许 `LOW/INFO/SAFE`
- `MEDIUM`：允许 `MEDIUM/LOW/INFO/SAFE`
- `SAFE`：仅允许 `SAFE`

## 6. 运行时状态机

Skill 扫描状态建议使用以下枚举：

- `pending_scan`：待扫描
- `scanning`：扫描中
- `allowed`：允许加载
- `blocked`：禁止加载
- `error`：扫描流程异常

运行时只加载 `allowed` 的 Skill。  
其余状态（`pending_scan/scanning/blocked/error`）一律不加载。

## 7. 配置契约

```toml
[skills.scan]
enabled = true
startup_scan = true
periodic_scan_enabled = true
interval_secs = 3600
poll_interval_secs = 10
poll_timeout_secs = 300
max_allowed_severity = "LOW"

[skills.scan.api]
upload_url = "https://skillscan.tokauth.com/oapi/v1/skill-scan/upload"
result_url = "https://skillscan.tokauth.com/oapi/v1/skill-scan/result"
```

### 7.1 字段说明

- `enabled`：总开关。
- `startup_scan`：是否启动即扫。
- `periodic_scan_enabled`：是否定时扫描。
- `interval_secs`：定时扫描间隔（秒）。
- `poll_interval_secs`：任务结果轮询间隔（秒）。
- `poll_timeout_secs`：单任务轮询超时（秒）。
- `max_allowed_severity`：最大允许风险等级阈值。
- `api.upload_url`：上传扫描完整地址。
- `api.result_url`：结果查询完整地址。

### 7.2 明确移除项

以下配置不再保留：

- `allow_low_risk`
- `fail_mode`
- `api.base_url`
- `max_zip_size_mb`
- `upload_timeout_secs`

## 8. 模块边界与代码整洁要求

扫描功能实现目录：

- `src/skills/scan/mod.rs`（统一导出）
- `src/skills/scan/types.rs`（类型定义）
- `src/skills/scan/archive.rs`（打包与 SHA）
- `src/skills/scan/client.rs`（接口调用）
- `src/skills/scan/policy.rs`（阈值判定）
- `src/skills/scan/store.rs`（状态持久化）
- `src/skills/scan/service.rs`（流程编排）

约束：

- 技能加载逻辑与扫描逻辑解耦，`skills/mod.rs` 不承载扫描细节。
- API 调用、策略判断、状态存储三层分离。
- 统一错误模型和日志上下文字段，便于排查。

## 9. 持久化数据模型（建议）

每个 Skill 至少保存以下字段：

- `skill_id`
- `archive_sha256`
- `scan_task_no`
- `scan_status`
- `max_severity`
- `is_safe`
- `last_scanned_at`
- `last_error`

## 10. 可观测性要求

日志建议统一输出以下键：

- `skill_id`
- `archive_sha256`
- `task_no`
- `scan_status`
- `max_severity`
- `decision`（allowed/blocked）
- `reason`

## 11. 异常处理规则

- 上传失败：保持 `blocked`，记录错误并按下一轮触发重试。
- 轮询超时：保持 `blocked`，记录超时原因。
- 返回字段缺失/非法：判为扫描失败，保持 `blocked`。
- 本地 SHA 与服务端 `file_sha256` 不一致：判定为异常，保持 `blocked`。

## 12. 验收标准

1. 未通过扫描的 Skill 不可加载。  
2. 风险等级判定严格遵循 `max_allowed_severity`。  
3. 启动扫描、定时扫描按配置生效。  
4. `archive_sha256` 变化时可立即阻断并触发重扫。  
5. 重扫通过后自动恢复可用。  
6. 扫描代码全部位于 `src/skills/scan/`，结构清晰、可维护。  
