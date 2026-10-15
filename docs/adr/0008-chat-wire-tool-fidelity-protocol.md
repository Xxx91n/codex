# ADR-0008 — Chat wire tool-calling 保真协议：邻接不变量 + namespace 限定名回环 + wire 感知工具暴露

- 状态: Accepted
- 日期: 2026-09-11
- 决策人: architecture-recovery 票 26（chatwire-mcp-tool-visibility；吸收票 19/25 相邻教训）
- 模板: Michael Nygard ADR 模板（Status / Context / Decision / Consequences）
- 实证依据: omniroute 网关日志实测（2026-09-10/11，tools 14→36、全限定名下发、
  round6 派发失败 → round7 派发成功全链）、CI run 34499616046 / 34562033037（260/260）、
  包 run 34562669001（round7 归档）、行业调研（atomcode 22 来源：chat completions wire
  无原生惰性加载，OpenRouter 上 tool_search 直接 400；平铺是业界唯一模式）
- 关联: ADR-0001（三注册点 + per-wire 模块）、ADR-0004/0005（chat wire 行为决策先例）、
  ADR-0006（分歧账本 apply/adapt——本 ADR 三宿主文件均为 adapt 重放面）、CONTEXT.md
  （「namespace 限定名回环」「tool_search wire 判据」词条）

## 背景 Context

票 19 修复了 chat wire 的 tool adjacency（孤儿 tool 结果 500），但端到端实测暴露
chat wire 工具链仍有三层结构性缺陷，使 MCP 工具（如 ctx 插件 mcp__context_mode__*）
在直连 v1/chat 时完全不可用，而经 Responses→Chat 中转（cc-switch）时正常：

1. **可见性**：`search_tool_enabled()`（spec_plan.rs）不含 wire 判据。chat wire 会话
   （模型 supports_search_tool=true）把全部 MCP 工具注册为 Deferred（惰性加载），
   而唯一加载器 tool_search 是 Responses-only spec，被
   `create_tools_json_for_chat_completions` 静默丢弃——工具既不下发也无法加载。
2. **出站形状**：namespace 工具以内层裸名下发（`chat_completions_function_tool_json`
   只取 function.name），模型照裸名回调后，dispatcher 按 `ToolName::new(namespace, name)`
   精确匹配 miss → `unsupported call`（registry.rs）。
3. **入站守卫**：chat SSE 构造 `FunctionCall { namespace: None }`，但
   `build_tool_call` 对全部调用应用 `with_default_namespace()`——真实 miss 到达时
   namespace 是 `Some(DEFAULT_FUNCTION_NAMESPACE)` 而非 `None`，任何按「namespace
   为 None」设计的回解都会被守卫短路。CI 单测用 `ToolName::plain` 直调绕过该形状，
   形成「CI 绿、线上红」错位（r5 教训）。

行业调研佐证：Chat Completions wire 无原生 tool_search/defer_loading（OpenAI/Azure/
OpenRouter 三方官方一致），LiteLLM/one-api/Portkey 在 chat 线上唯一模式是全量平铺
function tools。

## 决策 Decision

三段配对契约（三处必须同改，单侧重放会静默断链）：

1. **wire 感知暴露降级**（core/src/tools/spec_plan.rs `search_tool_enabled`）：
   首合取支增加 `wire_api == WireApi::Responses`。非 Responses wire（chat/anthropic）
   上 MCP 工具强制 `ToolExposure::Direct` 平铺暴露，不追加 tool_search 执行器；
   Responses wire 行为零变化（Deferred + tool_search 照旧）。
2. **出站限定名展开**（tools/src/tool_spec.rs，chat 与 anthropic 对称）：
   `ToolSpec::Namespace` 展开时非默认 namespace 下发 `<namespace>__<name>` 全限定名
   （`DEFAULT_FUNCTION_NAMESPACE` 维持裸名，对齐 responses_lite 合并语义）；
   Freeform 仍弃用（chat 无法执行 custom payload）。
3. **入站最长前缀回解**（core/src/tools/registry.rs `resolve_qualified_fallback`
   + dispatch 钩子）：registry 精确 miss 时，对「namespace ∈ {None,
   DEFAULT_FUNCTION_NAMESPACE}」的调用名，按 registry 实际注册 namespace 集合做
   最长前缀 `__` 匹配，余部两种合法形状（`__name` 前缀 / namespace 尾随 `__`），
   回解结果必须 `contains_key` 才采纳。禁止朴素 split（`mcp__1mcp__tool_invoke`
   多段 `__`）。守卫用 `match name.namespace.as_deref()` 借用形状（E0507 教训）。

配套（票 25，同轮入库）：sqlx 迁移 EOL 自愈（state/src/eol_checksum_repair.rs，
apply 类 fork-only）+ `.gitattributes` 六迁移目录 LF 锁 + fork-health 行尾扫描——
详见 FORK_DIVERGENCE.md STATE-1/STATE-2 条目，不另立 ADR（无协议语义、自执行守卫）。

## 后果 Consequences

- chat/anthropic wire 上 MCP 工具全量平铺（token 成本换可用性）；Responses wire
  的惰性加载体验不变。后续若需 chat 线省 token，走代理级工具检索（独立票）。
- 三宿主文件（registry.rs / spec_plan.rs / tool_spec.rs）均为上游活跃演进文件，
  上游合并时按 ADR-0006 adapt 重放；**三段契约必须同检**——FORK_DIVERGENCE.md
  TOOL-1/TOOL-2/ADJ-5 条目互为锚。
- 回归测试必须使用真实入站形状（`Some(DEFAULT_FUNCTION_NAMESPACE)`），禁止用
  `ToolName::plain` 直调伪造「已覆盖」——r5 的 CI 绿/线上红错位即源于此。
- 测试门：fork-health -E 词（chat_completions_ / anthropic_qualifies /
  anthropic_keeps_default / resolve_qualified_fallback /
  search_tool_enabled_requires_responses_wire）与代码零漂移纪律延续（票 19-r2 先例）。
