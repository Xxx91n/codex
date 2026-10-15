# ADR-0013 — chat/messages 中途 preempt 的中断语义：立即关闭流并对账历史，不排空

- Status: Accepted
- Date: 2026-10-15
- Relates: ADR-0002（Ruling 2：非 Responses wire 的 `usage_metadata: None`）、ADR-0004（reasoning_content 透传）、ADR-0008（tool 配对与 wire 感知）、ADR-0009（thinking 条件 drop）

## Context

用户在模型流式输出中途插入新输入时，turn 会取消 preempt 信号。Responses websocket 能向服务端发送 `response.interrupt`，排空到完成后再采样。chat 与 Anthropic Messages 只有 HTTP SSE，没有等价的中断帧。旧实现直接 `drop(stream)`，已展示给用户的部分内容不进入历史，并且代码中留有未对账的 TODO。两家 API 都要求每个 tool call 有配对结果，否则下一次请求返回 400。

## Decision

1. 收到 preempt 时立即关闭 HTTP 流，不排空到完成。
2. 对账历史：完整收到的文字保留；未完成的 thinking 与 reasoning 丢弃；参数完整的 tool call 保留并补合成结果；参数截断的 tool call 整体丢弃；写入中断标记。
3. 具体条目见 `docs/specs/chat-messages-preempt/README.md`。本 ADR 只记录取舍，不重复条目。

## Consequences

- 正面：新输入不必等待生成结束，不额外消耗 token；历史与用户所见一致，且配对合法。
- 代价：模型丢失被打断的 tool call 意图，需要重新发起；需要新写合成结果与对账逻辑。
- 被否决的方案：排空到完成，代价是延迟和 token，而且 HTTP wire 没有连接复用带来的收益；保留未配对的 tool call，每次重放都会 400。
- 遗留：Responses SSE 路径的中断语义未决，本决策不改变 Responses 行为。
