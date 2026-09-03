# ADR-0003 — 不接 Gemini wire(有条件重开)

- 状态: Accepted
- 日期: 2026-09-02
- 决策人: architecture-recovery 票 05(Gemini wire 缺口评估与裁决)
- 调研依据: atomcode 串行调研「Gemini wire 表面积调研报告」(2026-09-02, 已索引 ctx; 来源含 Gemini 官方 docs、LiteLLM 源码与 issues #17949/#25322/#30224/#37849、LLM-Rosetta arXiv 2604.09360、apiyi/ZenMux 网关文档)
- 关联: ARCH-tri-wire-20260902.md, spec.md 票05, ADR-0001/0002
- 模板: Michael Nygard ADR 模板(Status / Context / Decision / Consequences)
- 迁移注: 2026-09-03 自仓外 docs/adr 迁入 codex/docs/adr/,与 ADR-0001/0002 同位统一(大脑轨裁决:追认票05收口有效 + ADR 统一仓内;WORKFLOW §2/D6「仓外」条款对裁决类 ADR 由该裁决替代)

## 背景Context

本 fork 已有三条原生 wire([OI] Responses / Chat Completions / Anthropic Messages),架构为 hub-and-spoke IR(ResponseItem 为中心 IR,出站 per-wire builder,入站 per-wire SSE 状态机)。票05要回答:要不要新增第四条原生 Gemini wire(generateContent)。

调研确认的 Gemini wire 事实表(要点):
1. 消息模型差异大: contents[{role: user/model, parts[]}] 无消息 id、无 assistant 命名; systemInstruction 是单个 Content; stateless。
2. 工具调用无调用 id: functionCall/functionResponse 按位置配对(数量与顺序必须严格一致),与 Responses 的 call_id、Anthropic 的 tool_use_id 关联模型完全不同,转换层需自建 id 或位置索引; responseSchema 是 OpenAPI 子集方言; Malformed_Function_Call 是独立 finishReason。
3. thoughtSignature 跨轮回传是头号故障源: LiteLLM 五大事故(#17949 parts 数不匹配 400、#25322 不回传导致多轮退化死循环、#37849 签名被嵌入 tool_call_id 破坏 id 归一化、#25357 签名挂在 sibling part、#37145 版本子串误判跳过校验硬 400)。Gemini 3 thinking 模式下 functionCall 必须带 thoughtSignature 否则 400。
4. 流式形态异构: 无 alt=sse 时是裸 NDJSON,alt=sse 才是 data: 帧; 无 [DONE] 无事件类型; 每 chunk 都带 finishReason(首块即 STOP,不可作终止判据); usageMetadata 每 chunk 累计; functionCall 不流式(整块一次性到达)。
5. thinking 语义分轨: 2.5 用 thinkingBudget、3 用 thinkingLevel; 2.5 Pro/3 Pro 不可关; 只给 thought 摘要不给原始思维,但按完整思维计输出价; 官方页面已标 Legacy,Google 把新能力优先投给 Interactions API。
6. 缓存是第三种心智模型: implicit 默认开(门槛 2048/4096 tokens, 75% off)+ explicit cachedContents 独立 REST 资源; 与 Anthropic cache_control 块注解、Responses 自动前缀缓存均不同构。
7. 图片: AI Studio 侧不支持 HTTP(S) URL,必须转 base64(inlineData); fileData 走 Files API/gs://。

## 决策Decision

**不接 Gemini wire**(不新增第四条原生 wire)。

理由(按权重):
1. **主流场景已被覆盖**: chat/anthropic 两条 wire 已覆盖本 fork 的主流供应商面; Gemini 用户可经其官方 [OI] 兼容端点(base_url 指向 generativelanguage.googleapis.com/v1beta/openai)走现有 Responses/Chat wire,能力边界(thinking/流式/FC/多模态)由 Google 官方兼容层承诺。
2. **内部 IR 不对应**: ResponseItem 是 typed-item + call_id 关联模型,Gemini 是 parts + 位置配对 + 无 id 模型; 工具调用往返需自建 id 索引层,是三条 wire 里唯一要改 IR 语义假设的接入面。
3. **测试成本不成比例**: thoughtSignature 回传、NDJSON/半帧缓冲、位置配对校验、finishReason 误判,每项都是独立故障面(LiteLLM 五大事故即证据); 本 fork 无 Gemini 真实端点的 live 验收通道,单测覆盖不了签名续链这类黑盒行为。
4. **目标 API 本身在迁移中**: generateContent 已被官方标 Legacy,Google 把能力优先投给 Interactions API; 现在投入原生实现,绑定的是一个迁移中的目标。

## 条件与重开口子Reopen Triggers

满足以下任一条件时重开本决策(重开时按下列顺序评估):

- R1 **官方 [OI] 兼容端点能力缺口被实测证实**: 若 Gemini 官方兼容端点在本 fork 的实际场景(thinking 真模验证、tool_call 往返、图片块)中实测存在硬缺口(如 tool_choice 强制、并行调用、缓存字段不可见),且用户明确需要原生 Gemini,则重开。
- R2 **Interactions API 走型稳定**: 若 Google Interactions API(带 call_id、typed steps、previous_interaction_id)正式 GA 且生态(LiteLLM 等)完成适配,其 id 模型与 IR 对应度显著改善,可评估直接接 Interactions 而非 Legacy generateContent,则重开。
- R3 **用户出现真实 Gemini 原生需求**: 若出现依赖 Gemini 特有能力(cachedContents 显式缓存、videoMetadata、Files API)且无法经兼容端点满足的实际用例,则重开。

重开时的接入面预案(届时生效,现在不实现):
- 注册点沿用三处硬约束: WireApi 枚举(model-provider-info/src/lib.rs)、ModelProviderInfo、client.rs 派发 match,新增 `WireApi::Gemini` 枚举值 + per-wire 新模块(新文件+导入,不动高层控制流)。
- 实现面差异清单(相对现有 chat/anthropic wire): (a) 请求 builder: contents/parts 折叠 + systemInstruction 单 Content + functionResponse 位置配对(需 call_id→位置索引); (b) SSE 状态机: NDJSON 与 alt=sse 双帧解析 + 按 chunk 缓冲(不可透传字节, LiteLLM #30224 教训) + 无 [DONE] 以流关闭为终止 + 末块 usageMetadata 取累计值; (c) thinking: thinkingBudget/thinkingLevel 分轨映射 reasoning effort + thoughtSignature 原样回传(红线, 同 anthropic 签名红线同级); (d) 缓存: 首版只透传 implicit 缓存计数,不实现 cachedContents 资源管理; (e) 图片: URL→base64 转换层。

## 后果Consequences

- 正面: 零实现/零测试成本; 消灭一个迁移中目标的维护面; Gemini 用户走官方兼容端点即可用,三注册点红线与 client.rs 冲突面维持现状。
- 负面/代价: 放弃 Gemini 原生能力面(cachedContents 显式缓存管理、原生 thought 摘要透传的完整控制); 兼容端点的能力边界依赖 Google 承诺,若其收缩需按 R1 重开。
- 中性: 本 ADR 不改任何代码; 票05为评估票,产出即本 ADR。初版落仓外,2026-09-03 依大脑轨落点统一裁决迁入仓内(见头部迁移注),but lane 05-gemini-gap-assessment 提交。
