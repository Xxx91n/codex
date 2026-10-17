# ADR-0014 — Anthropic 出站 tool_result 落地 is_error 失败标记

- Status: Accepted
- Date: 2026-10-17
- Relates: ADR-0008（tool 配对契约）、ADR-0013（preempt 合成未执行结果）、决策账本 D-005

## Context

D-005 的原话要求「JSON 已完整的 tool call 保留，并补一条 `is_error: true` 的合成结果，
写明中断未执行」。ADR-0013 那一轮只做到 IR 侧：合成结果置
`FunctionCallOutputPayload.success = Some(false)`，出站 `tool_result` 块仍只写
`type` / `tool_use_id` / `content`，没有任何失败位。后果有两层：Anthropic 侧模型只能靠
读文案判断这次调用算不算失败；而真实的失败工具结果（非 preempt 路径）同样被当成成功重放。

## Decision

1. `push_anthropic_tool_result` 新增 `is_error` 形参；失败时在出站块写 `"is_error": true`。
2. 失败判定唯一来源是 IR 的 `FunctionCallOutputPayload.success == Some(false)`；
   `None` 与 `Some(true)` 都算成功——与 `core/src/tools/context.rs` 里
   `success.unwrap_or(true)` 的既有读法保持一致，不新造第二个真值来源。
3. 成功时省略字段，不写 `"is_error": false`：Messages API 的默认值本就是 false，省略既
   保持载荷最小，也不改变既有 tool_result 的稳定形状。
4. 覆盖面是全部 tool_result 出站（`FunctionCallOutput` 与 `CustomToolCallOutput` 两个入口），
   不只 preempt 合成结果。
5. Chat 线不加对等字段：Chat Completions 的 `role: "tool"` 载荷没有错误位，失败语义继续
   只由文案承载（wire/AGENTS.md 红线 11「不得假装能力」）。

## Consequences

- 正面：D-005 逐字落地；上游模型可按协议字段决定重试；合成未执行结果不可能再被读成成功。
- 代价：出站载荷变化——对 Anthropic 请求体做逐字节断言的测试要跟着改。本仓现存两处
  （`core/src/client/wire/anthropic_tests.rs`、`core/tests/suite/anthropic_wiremock.rs`）已更新，
  并补了一条覆盖 `Some(false)` / `None` / `Some(true)` 三态的映射单测。
- 规格关系：`docs/specs/chat-messages-preempt/README.md` 的 S-06 只定文案，本 ADR 不改写规格
  （任务书红线「实现中不得改写规格」）；`is_error` 是协议位而非文案，故以本 ADR 记录，S-06 原文继续有效。
- 被否决的方案：只给 preempt 合成结果打标记（wire 侧要按固定文案反查，脆弱）；无条件写
  `is_error: false`（噪声载荷，且改写每一条既有 tool_result 的形状）。
- 台账：仓外运行台账 `.scratch/architecture-recovery/FORK_DIVERGENCE.md` 新增 **WIR-9**
  （`push_anthropic_tool_result` 的出站失败位，分类 apply）；wire/AGENTS.md 红线 8 同步补该出站位。
- 已知债：`anthropic.rs` 现 936 行，已过根 AGENTS.md「Avoid large modules」的 800 行目标；本轮
  `+26/-4` 属同地修补，模块拆分另票处理，不在本决策里顺手做。
