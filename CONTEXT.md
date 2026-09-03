<!-- 落点决策（票 06, per WORKFLOW §2/D6）：仓内 codex/CONTEXT.md（fork 仓库根）。
D6 原「CONTEXT.md 仓外」由 2026-09-03 大脑轨裁决替代，证据链：ADR-0003 头部迁移注（ADR 统一
仓内）→ 审查报告候选 A（P0）→ issues/06 标题（仓内）。
词源=仓内 ADR-0001/0002/0003 + AGENTS.md Fork delta + 仓外 ADR-001/ARCH/domain.md。 -->

# CONTEXT.md — fork 仓唯一词汇权威

消费规则（per domain.md）：开工先读本文件；命名只用本表词汇，不造同义词；与 ADR 冲突时
显式标注「Contradicts ADR-NNN — 理由」；新词随其 ADR 入表；锚点只写「文件:符号」不写行号。

## 核心词汇

### 三注册点（WireApi / ModelProviderInfo / client.rs match）
- 本仓用法：fork 分歧只允许三处——(1) `WireApi` 枚举（model-provider-info/src/lib.rs，三变体
  serde lowercase，default=Responses）；(2) `ModelProviderInfo.wire_api` 字段；(3)
  `ModelClientSession::stream` 派发 match。新增 wire 三处同改；telemetry 中 wire_api 字符串是
  只读引用，非注册点。
- 禁止用法：改名/搬走注册点；只改一两处；把 wire 逻辑堆进 client.rs（应抽 per-wire 模块）。
- 源：ADR-0001 Decision；AGENTS.md Registration points；ADR-0003 重开预案。

### ResponseItem
- 本仓用法：全 fork 唯一 IR：`codex_protocol::models::ResponseItem`，typed-item + call_id 关联；
  三 wire 入站 SSE 只产出 ResponseItem/ResponseEvent 流，turn 循环无 wire 感知。
- 禁止用法：把线格式 JSON 称 IR；为某 wire 建第二 IR（2N 成本退回 O(N²)）。
- 源：ADR-0001 Context；ADR-0003 背景。

### per-wire 模块
- 本仓用法：隔离面=新文件：出站 builder（core/src/client/wire/{chat,anthropic}.rs）+ 入站
  SSE 状态机（codex-api/src/sse/{chat_completions,messages}.rs），每 wire 一对。
- 禁止用法：把 wire 翻译逻辑写进 client.rs/turn.rs 主体；跨 wire 共享状态机状态。
- 源：ADR-0001 Decision；AGENTS.md Fork delta；ADR-0003 重开预案。

### wire_api 字段
- 本仓用法：`ModelProviderInfo.wire_api: WireApi`，每 provider 协议选择，默认 Responses；
  运行期唯一协议判定入口。
- 禁止用法：用 endpoint 字符串/散配置另判协议（=造第四类注册点）；新增反序列化点不更新本表。
- 源：ADR-0001；AGENTS.md；ADR-0003。

### thinking signature 回传
- 本仓用法：带 signature 的 Reasoning 出站原样回传为 thinking 块（
  `encrypted_content: Some(signature)` guard）；无签名 drop 而非篡改；思维块永不混入
  content。永久红线。
- 禁止用法：改/编 signature 再回传（触发上游 400）；thinking 文本拼进 content。
- 源：ADR-0003（红线，同 anthropic 签名红线同级）；ARCH-tri-wire-20260902。

### reasoning_content 透传
- 本仓用法：Chat wire 入站把 `delta.reasoning_content ?? delta.reasoning`（vLLM 0.18+
  更名期双名 coalesce）合成为 `ResponseItem::Reasoning{content:[ReasoningText],
  encrypted_content: None}`，三事件协议 Added→ReasoningContentDelta→Done；出站
  builder 把 Reasoning 历史回传为 assistant `reasoning_content`。复用现型不加新枚举
  变体（per ADR-0004）。
- 禁止用法：为 chat 思维链新增 ReasoningPart 变体或第二 IR；伪造签名；透传原始字节
  不经 IR；把本词条与 thinking signature 红线混淆（chat 无签名）。
- 源：ADR-0004；research/10-atomcode-reasoning-content-20260904.md。

### cache_control 注入
- 本仓用法：仅 anthropic wire 出站侧注入；向 OpenAI 兼容方向无等价物，不出现。
- 禁止用法：往 responses/chat 输出写 cache_control 块；当跨 wire 通用语义。
- 源：仓外 ADR-001 决策/有损面；ARCH-tri-wire-20260902。

### hub-and-spoke IR
- 本仓用法：两跳定位——IR=ResponseItem 居中，每 wire 一对 builder/状态机；新增协议 O(N)
  而非 O(N²)；层级=客户端侧/进程内。
- 禁止用法：自称单跳直译（LiteLLM 式，不过 IR）；把外部网关/独立翻译进程称 hub-and-spoke。
- 源：ADR-0003 背景；ARCH-tri-wire-20260902 行业定位。

### Provider trait（goose 模式）
- 本仓用法：接入范式=自研 transport + 每 wire 一文件 + 注册表（goose-blueprint 进程内）；
  本仓落地=三注册点 + per-wire 模块。
- 禁止用法：外挂翻译进程/独立翻译层（用户三前提否决）；单文件吞多 wire（Kilo Code 教训）。
- 源：仓外 ADR-001 决策；ADR-0001 Context「goose-blueprint」；AGENTS.md。

## 相关词汇

- **三条 wire**：Responses / Chat Completions（上游 2026-02 移除 PR #10157 后本 fork 恢复）/
  Anthropic Messages（本 fork 新增）。
- **usage_metadata: None**：非 Responses wire 合成 Completed 必带 None（ADR-0002 Ruling 2）。
- **wire 常量**：CHAT_COMPLETIONS_ENDPOINT / ANTHROPIC_MESSAGES_ENDPOINT /
  DEFAULT_ANTHROPIC_MAX_TOKENS / ANTHROPIC_VERSION 保留不改（ADR-0002 Ruling 1）。
- **PONYTAIL**：刻意取舍台账（ADR-0002 记 max_tokens）；禁当普通 TODO。
- **翻译有损面**：thinking、cache_control、store/previous_response_id、compact 降级面。
- **thoughtSignature**：Gemini 术语，仅存于 ADR-0003 重开预案；禁用于 anthropic signature。

## 维护

- 变更随其 ADR 入仓（ADR 先于本表）；新 wire/概念先登记再实现。
- 票 09 fixture / 票 10 ReasoningPart 命名以本表为准。
- 每次 upstream merge 后 grep 词条与 docs/adr/ 校验一致性。
