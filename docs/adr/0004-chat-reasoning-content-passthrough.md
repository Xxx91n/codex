# ADR-0004 — Chat wire reasoning_content 透传（IR 复用 ResponseItem::Reasoning）

- 状态: Accepted
- 日期: 2026-09-04
- 决策人: architecture-recovery 票 10（chat-reasoning-content-passthrough）
- 模板: Michael Nygard ADR 模板（Status / Context / Decision / Consequences）
- 调研依据: atomcode 串行调研「reasoning_content 跨厂语义与 IR 设计」（2026-09-04，转写落
  `.scratch/architecture-recovery/research/10-atomcode-reasoning-content-20260904.md`；
  13 搜索 + 16 URL 一手核验：LiteLLM docs/issues #24985/#29518/#30224/#20246/PR #27947/#28258、
  LLM-Rosetta arXiv 2604.09360、vLLM/SGLang/DeepSeek/Kimi/OpenAI/Anthropic 官方 docs、
  LibreChat #12775）
- 关联: ADR-0001（三注册点 + per-wire 模块）、ADR-0003（IR 语义不对应即拒绝 → 反证本场景
  IR 已对应）、CONTEXT.md（ResponseItem / per-wire 模块 / thinking signature 回传词条）、
  spec 票 10 / 架构审查报告候选 E

## 背景 Context

Chat wire（OpenAI Chat Completions 兼容面，服务 DeepSeek/Kimi/Qwen3/vLLM/SGLang 等推理
模型）是三条原生 wire 之一。这些上游的思维链以 `reasoning_content`（message/delta 顶层
字符串，DeepSeek-R1 确立的开放生态事实标准）返回，其中 DeepSeek（带 tools 时）/Kimi
（Preserved Thinking 常开时）要求历史 reasoning_content **原样回传**，缺失 → 400 /
推理续链退化，与 Anthropic signature 回传同属「跨轮保真」纪律。

现状盘点（代码直读，行级证据）：本 wire **双向剥除**——
- 入站 `ChatDelta`（codex-api/src/sse/chat_completions.rs:50-56）只有 `content` +
  `tool_calls` 字段，无 `deny_unknown_fields`，serde 静默丢弃 reasoning_content；
- usage 侧 `reasoning_output_tokens: 0` 硬编码（同文件 :90）；
- 出站 `build_chat_messages`（core/src/client/wire/chat.rs）以 `_ => {}` 跳过
  `ResponseItem::Reasoning`，历史 thinking 不回传；
- reasoning-only turn（DeepSeek/Kimi 工具中间轮 content=""）整体静默。

IR 侧 `ResponseItem::Reasoning{summary, content, encrypted_content}`
（protocol/src/models.rs:1011-1023）已为 Anthropic thinking（含 signature）与 Responses
reasoning item 建型，`ResponseEvent::ReasoningContentDelta` 已存在，Responses wire
（sse/responses.rs `response.reasoning_text.delta`）与 Anthropic wire（sse/messages.rs
thinking 三事件协议）均已接线消费，core/TUI/history 全链路支持。

跨厂语义（调研结论）：`reasoning_content`（DeepSeek/SGLang/Kimi/Qwen，stateless 条件化
回传）、`reasoning_effort`（请求侧参数，票 11 范围）、`thinking_blocks`（Anthropic
stateless 硬契约，官方 OAI 兼容层不吐）、Responses reasoning item（stateful 服务端续链）
是同一抽象（Rosetta ReasoningPart，"thinking content with optional signature"）的四种
传输形态；字段名生态正处于 vLLM 0.18+ `reasoning_content → reasoning` 迁移期。

## 决策 Decision

1. **IR 复用现型，不新增 ReasoningPart 枚举变体**：Chat `reasoning_content` 映射到
   `ResponseItem::Reasoning{ content: [ReasoningItemContent::ReasoningText{text}],
   encrypted_content: None }`。Rosetta ReasoningPart 与 ResponseItem::Reasoning 是同一
   抽象的两个粒度（本项目 IR 是 item 粒度），现有类型即对应物；新增变体是伪需求（per
   ADR-0003「IR 已对应则不重复建模」同源判据）。
2. **入站三事件合成**（参照 messages.rs 既成先例）：首个非空 reasoning delta →
   `OutputItemAdded(Reasoning 占位)`（start），每个 delta 立即转发
   `ReasoningContentDelta{delta, content_index: 0}`（增量，绝不整段后补发），首个
   content/tool_calls delta 或流结束 → `OutputItemDone(完整 Reasoning 项)`（stop，
   切换点即边界）。入站字段双名 coalesce：`reasoning_content ?? reasoning`（vLLM 0.18+
   更名迁移期）。
3. **出站回传**：`build_chat_messages` 增加 `ResponseItem::Reasoning` 分支 → assistant
   message 带 `reasoning_content`（多 ReasoningText/Text concat）；不伪造签名，
   per-backend strip 白名单保留给未来不接受该字段的上游。
4. **usage 计量**：`ChatUsage` 增 `completion_tokens_details.reasoning_tokens` 解析
   （serde default 兼容旧上游），`reasoning_output_tokens` 不再硬编码 0。
5. 范围边界：不实现 reasoning_effort → thinking 翻译（票 11 / ADR-0005 范围）、不触碰
   anthropic 签名红线（chat reasoning_content 无签名，encrypted_content 恒 None）、
   不改 Responses wire、不动三注册点。

## 后果 Consequences

- 正面：DeepSeek/Kimi/Qwen3 推理模型经 chat wire 不再丢思维链；工具多轮回传不再
  400/退化（LiteLLM #24985/#25322 同族坑规避）；reasoning-only turn 正确收口；usage
  计量保真；core/TUI 零新增消费面；三 wire 在 IR 收敛（与 Responses reasoning item、
  Anthropic thinking block 同型）。
- 负面/代价：chat SSE 状态机 + chat builder 各一处行为改动（wiremock/sse 单测面扩大）；
  出站字段名分歧（reasoning vs reasoning_content）需维护能力映射；跨 wire 迁移仍受
  「无签名 thinking 不伪造」约束（chat → anthropic 迁移不产 thinking_blocks）。
- 风险与对策：合成顺序错误 → messages.rs 三事件协议为模板 + 事件序断言单测锁死；
  盲等/整段后补 → delta 逐块转发纪律（LibreChat #12775 教训）；半帧 → eventsource()
  帧级消费不变（LiteLLM #30224 教训）；空 reasoning 不发 Added、空文本不产出项
  （复用 should_serialize_reasoning_content 语义）。
