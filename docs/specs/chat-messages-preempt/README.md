# chat/messages preempt 中断规格

- 状态：Accepted（2026-10-15）
- 决策依据：[ADR-0013](../../adr/0013-chat-messages-preempt-interrupt.md)（中断方式取舍，本文件不重复论证）
- 适用：chat 与 Anthropic Messages 两条 HTTP 流式 wire。Responses 路径不在本规格内（见 N-02）。
- 触发：流式生成中途收到用户新输入，turn 的 preempt 分支执行，且当前流的 `interrupt` 为 `None`（`codex-rs/core/src/session/turn.rs`，搜索 `preempt`）。

## 条目

- **S-01 中断方式**：立即关闭 HTTP 流，不排空到完成。
- **S-02 标记机制**：中断标记复用 `interrupted_turn_history_marker` 与 `agent_interrupt_message_enabled` 开关，不新增开关。preempt 场景使用专用文案（S-07），不沿用为用户 Esc 写的文案。
- **S-03 tool call 分派**：参数 JSON 已完整的 tool call 保留，并补一条合成结果（S-06）；参数 JSON 截断的 tool call 整体丢弃。不得保留未配对的 tool call。
- **S-04 顺序**：合成结果紧贴其 tool call（OpenAI 的 `tool` 消息、Anthropic 的 `tool_result` 块）；中断标记与新用户输入排在合成结果之后。
- **S-05 标记写入条件**：`agent_interrupt_message_enabled` 开启时，每次 preempt 打断都写中断标记，不论被打断的响应是否已产出内容。
- **S-06 合成结果**：所有被中断且未执行的 tool call 使用同一句固定文案：

  > The tool call was interrupted before it ran and was not executed. Do not assume it succeeded.

  不按工具名分模板；不复用中断标记作为单个调用的结果；不得声称工具已成功。
- **S-07 中断标记文案**：

  > The previous response was interrupted by a new user message before it finished. Text that was fully received has been kept; the last text segment may be incomplete. Unfinished tool calls were not executed. Continue from the new user message.

  文案不得包含"工具可能已部分执行"，也不得包含"用户有意中断"。
- **S-08 半截文字段**：正在输出、未收到该段结束事件的文字段，已收到的部分保留进历史；中断标记（S-07）说明最后一段文字可能不完整。
- **S-09 开关边界**：`agent_interrupt_message_enabled` 只控制 S-05 的中断标记写入。S-06 的合成结果与 S-08 保留的文字始终写入，不随开关变化。
- **S-10 thinking 块**：未完成的 thinking 块（无签名、内容不完整）丢弃，不进入历史、不回传；不得伪造签名。与 ADR-0009 的 G1 守卫一致。
- **S-11 reasoning_content**：chat 的 `reasoning_content` 与 thinking 同一规则：未完成则丢弃，已完整则保留。

## 红线约束

- S-10 与 `codex-rs/core/src/client/wire/AGENTS.md` 红线 3、4 一致（思维块不混入文字块，不截断或改写签名）。
- 合成与对账逻辑放在 wire 模块内，`client.rs` 只保留 dispatch（同文件 Boundary 与红线 2）。
- preempt 是客户端主动中断，不是协议的停止条件，不得据此改变 `stop_reason` / finish 的判断（红线 12）。
- 若本路径产生 `Completed` 事件，它仍带 `usage_metadata: None`（红线 10）。

## 范围外

- **N-01**：GitButler 与 git 的引用不一致。这是工具流程问题，不属于产品规格。
- **N-02**：Responses 路径（SSE 的 `interrupt: None`，以及 websocket 的优雅中断）。另立议题，单独定义。
- **N-03**：实现、测试与远端操作。本规格只定义行为，不含实现步骤。
