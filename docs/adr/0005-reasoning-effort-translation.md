# ADR-0005 — reasoning_effort → thinking 参数跨 wire 翻译（chat / manual / adaptive 三面）

- 状态: Accepted
- 日期: 2026-09-04
- 决策人: architecture-recovery 票 11（chat-reasoning-effort-translation）
- 模板: Michael Nygard ADR 模板（Status / Context / Decision / Consequences）
- 调研依据: atomcode 串行调研「reasoning_effort 跨厂语义与翻译层设计」（2026-09-04，
  ctx 索引 `atomcode-ticket11`：Exa/Tavily/AnySearch 三引擎 10 查询 + 12 URL 一手核验——
  platform.claude.com extended-thinking/steering、LiteLLM reasoning_effort_utils.py +
  constants.py + anthropic_effort 文档、OpenRouter reasoning-tokens + Claude 4.6 迁移指南、
  Vercel AI SDK reasoning、DeepSeek thinking_mode、Kimi model parameter reference、
  Anthropic 官方 OpenAI 兼容面）
- 关联: ADR-0004（票 10：ReasoningItem IR 落定；明示票 11 为 ADR-0005 范围）、ADR-0001
  （三注册点 + per-wire 模块纪律）、ADR-0003（IR 不对应即拒绝的判据）、CONTEXT.md
  （per-wire 模块 / degradation table 词条）、架构审查报告 §3 候选 F

## 背景 Context

Chat wire 与 Anthropic wire 是两条原生出站 wire。会话级 `reasoning_effort`
（protocol 层 `ReasoningEffort` 枚举已存在，Responses wire 已消费）此前在这两条
wire 上未接线：chat 请求体从不携带 effort 参数；anthropic 请求体只有 provider 级
`anthropic_thinking_budget` clamp 到 [1024, max_tokens−1] 后发 manual
`thinking:{type:"enabled",budget_tokens}`。同一用户档位跨 wire 行为不一致。

行业 2026-09 状态（调研要点）：
1. Anthropic 已从「budget 定思考」转向「effort 定思考」：Claude 4.6 起 manual
   `budget_tokens` 弃用（仍成功），4.7+ 直接 400；adaptive thinking
   （`thinking:{type:"adaptive"}` + `output_config:{effort}`）为官方推荐路径。
2. 跨厂不存在官方统一 effort↔budget 分桶：LiteLLM 用常量 1024/2048/4096（唯一带
   常量分桶的工业实现，`reasoning_effort_from_thinking_budget`），OpenRouter 用比例
   公式 `budget=max(min(max_tokens×ratio,128000),1024)`（ratio 0.1–0.95），Anthropic
   官方兼容面反而把 `reasoning_effort` 标为 Ignored。
3. 档位跨厂边界不一致：`minimal` 仅 OpenAI 系；Anthropic adaptive 无 `none`
   （低档会跳过思考）；DeepSeek medium/xhigh 收敛 high；Kimi 无 medium。
4. effort 语义不对等：Anthropic effort 控制整轮 token 花销（含工具调用），OpenAI/
   DeepSeek/Kimi effort ≈ 思考量上限——跨厂一致只能是「档位方向一致」，不是
   「token 数一致」。
5. effort/budget 值是 prompt-cache key 的一部分（Anthropic/Kimi 官方双证）：同会话
   改档失效缓存，因此翻译必须确定性。

## 决策 Decision

1. **统一入口**：会话级 `reasoning_effort`（复用 protocol `ReasoningEffort` 枚举，
   不造新类型——per ADR-0004 同源判据）成为跨 wire 用户输入，经 `ModelClientSession::stream()`
   既有 effort 形参传入两条 fork wire。新增纯函数模块 `core/src/client/wire/reasoning_effort.rs`
   承载全部映射（无 IO、确定性、可单测）。
2. **Chat wire 透传归一化 effort**：仅当会话设置了 effort 且该档位在 OpenAI 兼容面
   有可移植词汇（low/medium/high）时发顶层 `reasoning_effort`；`minimal`→`low`、
   `xhigh/max/ultra`→`high`（OpenRouter 同规则降档）；`none/persistent/custom`
   无可移植词汇 → 字段整体省略（模型默认生效，lossy 已文档化）。
3. **Anthropic wire 分轨翻译**，由 provider 显式旋钮 `anthropic_adaptive_thinking`
   （serde default false）选择轨道：
   - adaptive 轨（Claude 4.6+ 部署）：`thinking:{type:"adaptive"}` +
     `output_config:{effort}`，effort 直译不降档（minimal→low、xhigh/max/ultra→high
     为唯一降档），none/persistent/custom → 不发 thinking/output_config；
   - manual 轨（默认，兼容 ≤4.5 与网关）：分桶到 `budget_tokens`——
     minimal/low→1024、medium→2048、high/xhigh/max/ultra→4096（LiteLLM 兼容常量，
     反向经 `reasoning_effort_from_thinking_budget` round-trip 闭包），沿用
     clamp [1024, max_tokens−1]，无合法预算时省略 thinking 而非发送被拒值。
4. **优先序**：会话 effort > provider `anthropic_thinking_budget`（后者降级为
   legacy 无 effort 场景的路径，行为与改动前逐字节等价；两者并存时 effort 胜出并
   打 debug 日志）。
5. **确定性纪律**（写入 wire/AGENTS.md 红线 12）：same effort → same payload；
   弃用 OpenRouter 比例公式（随 max_tokens 漂移 → cache 失效 + 不可单测）。
6. **范围边界**：不改 Responses wire；不动 thinking 签名回传红线（ADR-0004）；
   三注册点零改动；不引入 effort→token 精确等价承诺。

## 后果 Consequences

- 正面：一份 reasoning_effort 在 chat/anthropic 两 wire 档位方向一致；4.7+ 模型经
  adaptive 轨不再因 manual budget 400；legacy 模型经网关与直连行为可比（LiteLLM
  常量桶）；翻译纯函数化全量单测；cache 断点稳定。
- 负面/代价（lossy 边界，明示）：Anthropic adaptive effort 语义 = 整轮 token 花销，
  与 OpenAI 系「思考量上限」机制不同；`none` 档在 adaptive 面不可表达（省略 =
  模型默认，Claude 5 默认开思考）；manual 面 minimal≡low（Anthropic 下限 1024）；
  `custom/persistent` 档两 wire 均省略（上游 Responses 面才有对应表达）。
- 风险与对策：轨道误判（把 4.7+ 当 legacy）→ 400 信息透出，用户可切
  `anthropic_adaptive_thinking`；未知 `Custom` 值 → 静默省略 + 模型默认（与现
  clamp 哲学一致，fail-soft 于参数、fail-loud 于流）。
- 待实测（重开口子）：adaptive 时代显式 `thinking:{type:"disabled"}` 合法性——决定
  none 档能否在 Claude 5 系硬关思考；`xhigh` 档的准确模型门槛需能力表支撑后直译。
- 落地面：`wire/reasoning_effort.rs`（新）+ chat.rs/anthropic.rs 两 builder 插入点 +
  client.rs 派发传参 + `anthropic_adaptive_thinking` provider 旋钮（config schema
  同步再生成）+ client_tests 单测 + wiremock 三 wire reasoning_effort fixture。
