# ADR-0010 — 上游 merge 裁决（2026-09-12，behind 710 / merge-base 6be2a6ca952a → c4017a87aa）

- 状态: Accepted
- 日期: 2026-09-12
- 决策人: architecture-recovery 票 32（upstream-merge-replay）
- 模板: Michael Nygard ADR 模板（Status / Context / Decision / Consequences）
- 关联: ADR-0001（merge-never-rebase）、ADR-0002（Ruling 1 常量清单）、ADR-0006（五步裁决流程 + 分类语义）、ADR-0007（破坏面预判表）、FORK_DIVERGENCE.md（仓外运行台账，本次裁决后已刷新）

## 背景 Context

票 32 按 ADR-0006 五步执行上游合并。前置检查点：票 27（a52773715d）/28（472dd47b19）/29（3e5feb1f85）/30（b40359c536）/31（1ba8c85ce9）全部合入后开工（`git merge-base --is-ancestor` 全部 YES）。

- 同步前口径（禁 tag）：`behind 710 / ahead 90`，merge-base `6be2a6ca952a`（#41260）。
- 合并方式：merge-don't-rebase（per ADR-0001）；HEAD 处于 GitButler workspace，故用 `git merge-tree --write-tree` 预演 + 逐冲突合成 + `git commit-tree`双亲（b40359c536 + c4017a87aa）+ `git update-ref` fast-forward 落 main，不动工作区、不违背 but 工作区管理。merge commit `0743507444`。
- 合并后口径：新 merge-base `c4017a87aa`（#44976），`behind 0 / ahead 91`。
- 冲突面：7 文件 / 9 冲突区，全部收敛在 FORK_DIVERGENCE 清单预测的 adapt 面（STATE-1 / ADJ-3 / CLI-1 / TOOL-1 带入口 / 生成物 schema / 测试面），无清单外新分歧（唯一新事实为下述 IGN-1，已先登记再裁决）。

## 决策 Decision（逐冲突裁决，per ADR-0006 五步③）

| # | 文件 | 裁决 | 依据 |
|---|---|---|---|
| 1 | `.gitattributes` | 保留 fork 六迁移目录 LF 锁（STATE-1）+ 上游 `third_party/voice/sources.json text eol=lf` pin 两全，上游 3 行逐字保留 | STATE-1 adapt 重放；上游 90 行 fork 占 88 强冲突面 |
| 2 | `codex-api/src/endpoint/mod.rs` + `lib.rs` | 保留 `chat_completions` 模块与导出（ADJ-3 apply）；随上游删 `CompactClient` 家族 | ADJ-3 apply；IGN-1（见下） |
| 3 | `core/src/client.rs` 常量区 | 保留 3 个 fork wire 常量（CHAT_COMPLETIONS_ENDPOINT / ANTHROPIC_MESSAGES_ENDPOINT / DEFAULT_ANTHROPIC_MAX_TOKENS，含票 29 降级注释）；删 2 个 compact 常量 | CLI-1 apply（ADR-0002 Ruling 1）；合并树上 compact 使用点已被上游删除，保留将挂死代码并违 clippy -D warnings |
| 4 | `core/config.schema.json` | 两个枚举都保留（票 31 `MigrationChecksumFamily` + 上游 `MemoryVersion`），按 schemars 字母序排列 | 生成物合成；票 31 + 上游 #44273 后继新增 |
| 5 | `core/src/tools/registry.rs` import 区 | 两侧 import 都保留（fork `DEFAULT_FUNCTION_NAMESPACE` + 上游 `codex_history::{CodexHarnessMetadata, ResponseItemEnvelope}`），按字母序 | TOOL-1 带入口重放；上游 #44336 新增 |
| 6 | `core/tests/suite/personality.rs` | 取上游重写（新测试不再内联构造 ModelInfo，fork 的 `max_output_tokens: None` 字面量补丁被吸收，无语义损失） | 测试面 adapt；票 29 字面量依赖消失但 `ModelInfo.max_output_tokens` 字段本身幸存 |

**IGN-1（清单外新分歧，先登记再裁决）**: `codex-api/src/endpoint/compact.rs` + `CompactClient` 导出 + `RESPONSES_COMPACT_ENDPOINT`/`COMPACT_REQUEST_TIMEOUT_IDLE_MULTIPLIER` 常量为基线继承的上游遗产（非 fork 刻意保留，ADR-0002 Ruling 1 常量清单不含它）。上游 `1ac689cc7d`（#44273 Remove the unused legacy remote compaction implementation）删除该家族；fork 侧随上游删除，登记为 ignore（`Resolved: absorbed-by-upstream 1ac689cc7d`）。

## 重放验证 Replay Verification（五步③逐条对照）

在合并树（`a7a38f0146591d0470216f511a4830c3e49f397c`）上逐条 grep 确认 fork 全部 adapt/apply 条目幸存：

| 条目 | 验证锚 | 结果 |
|---|---|---|
| REG-1/2/3 三注册点 | `WireApi::Anthropic` 枚举 / `pub wire_api` / `stream_anthropic_messages` dispatch + `mod wire;` | 幸存 |
| CLI-1/2/3 | 3 常量 + dispatch 分支 + `handle_unauthorized(event_sender, turn_id)` 传参 | 幸存 |
| WIR-1..5 | `wire/` 六文件（含 AGENTS.md 与票 29 `max_tokens.rs`） | 幸存 |
| ADJ-3/4/5/6 | endpoint/chat_completions+messages / SSE 状态机 / tool_spec + registry 回解 / `on_stop_reason` 钩子 | 幸存 |
| TOOL-1/2 | `resolve_qualified_fallback` + `search_tool_enabled` wire 门控（`wire_api == WireApi::Responses`） | 幸存 |
| STATE-1/2/3 | `.gitattributes` 六锁 + `eol_checksum_repair` 接线 + `fix-checksums` 子命令 + `MigrationChecksumFamily` 配置 + schema 段 | 幸存 |
| 票 27（A-002/004/005/006）| `redacted_thinking` / `cache_read_input_tokens` / ChatUsage serde default / `ContextWindowExceeded` | 幸存 |
| 票 28（A-003 / D-009） | 守卫 G1 出站预检 / G3 400 自适应恢复（ADR-0009） | 幸存 |
| 票 29（A-007 / D-006） | `ModelInfo.max_output_tokens` 字段 + `max_tokens::resolve_max_tokens` 优先级链 + `on_stop_reason` 遥测钩子 | 幸存（接口位移已逐条对照：字段在上游新形态下无位移） |
| 票 31（D-015） | `fix_checksums.rs`/`state_cmd.rs`/`state_db_settings.rs` + config 接线 + fork-health 过滤词 | 幸存 |

state/model-provider 域接口位移对照（票 32 检查点）：上游 710 中对 `model-provider-info` 与 state 域的改动（registry 元数据 / truncation 预算 / schema 段新增）均与 fork 字段无名称冲突；唯一实质位移为 IGN-1（legacy remote compaction 删除）。`ModelInfo` 在 `protocol/src/openai_models.rs`（票 29 后 fork 增加 `max_output_tokens`），上游同文件演进无冲突（merge-tree 自动合并，字段幸存）。

## 后果 Consequences

正面：

- 落后清零（behind 0），下一同步基线为 `c4017a87aa`（#44976）。
- fork 全部语义面（三 wire / 票 27-31 交付 / state 家族切换 / EOL 自愈）在新基线上完整幸存，重放面可审计。
- 死代码随上游删除清理，clippy -D warnings 门无新增风险。

负面/代价：

- `personality.rs` 取上游重写后 fork 字面量补丁消失（无语义影响，但票 29 在该测试文件的 fixture 覆盖随之消失，由上游新测试形态替代）。
- merge diff 巨大（3012 files / +288037 -66202），CI 全量验证为必要门（五步⑤）。

验证往返（五步⑤）：CI 全绿（fork-health wiremock 三件套 + check job）后才允许推远端；本机零构建（CI-only 红线）。
