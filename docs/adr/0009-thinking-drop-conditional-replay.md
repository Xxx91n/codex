# ADR-0009 — thinking-drop 红线修订为条件规则（上游类型 × thinking 轨分派）+ 守卫 G1/G3/G6

- 状态: Accepted（部分 supersede ADR-0003 的「无签名无条件 drop」红线论证）
- 日期: 2026-09-12
- 决策人: architecture-recovery 票 28（执行 D-009 用户拍板）
- 模板: Michael Nygard ADR 模板（Status / Context / Decision / Consequences）
- 调研依据: thinking-drop 红线重审 atomcode 深度调研（17 来源：Anthropic 官方文档 +
  4 个独立 issue 交叉；2026-09-11，全文索引 ctx source=atomcode；结论与用户拍板记录见
  .scratch/tri-wire-hardening/decision-ledger.md D-009）
- 关联: ADR-0003（本记录不编辑其原文；signed 逐字回传红线继续有效，「无签名
  无条件 drop…永久红线」部分由本记录 supersede）；ADR-0005（effort → thinking
  轨翻译，manual 桶即本记录处理的最大暴露面）；票 27（redacted_thinking 逐字
  回传，本记录保留其契约）；ADR-0008（同类 wire 保真协议先例）；CONTEXT.md
  「thinking signature 回传」词条（随本记录同步修订，旧文标 superseded 留痕）；
  A-003（锐评摩擦点，核查证实 anthropic.rs 静默 drop）

## 背景 Context

2026-08-27 旧调研将「无签名 thinking 块」定为无条件红线：出站一律 drop
（而非篡改签名），并以此写入 ADR-0003 论证与 CONTEXT.md 词条。D-009 重审
调研（2026-09-11）以官方 2026 分级强制合约 + 四个独立事故交叉推翻了「无条件」
部分：

1. **分级强制**：工具回合内 thinking 必须原样回传，非工具回合可省略；
   adaptive 模式不强制首块，manual(`enabled`) 模式强制。fork 的
   reasoning_effort 翻译把 effort 落进 `{"type":"enabled","budget_tokens":N}`
   manual 桶，恰走最大 400 暴露面。
2. **400 精确触发条件**：该轮请求带 thinking 配置 ∧ 最后一条 assistant
   消息含 tool_use ∧ 其首块不是 thinking/redacted_thinking。旧红线在此形态下
   不是「降级」而是「把下一轮判死刑」——被静默 drop 的块让首块约束必然破防。
3. **三类 400 文本**：`Expected thinking or redacted_thinking` /
   `cannot be modified` / `Invalid signature`——thinking 回传被拒的三种
   服务端表达。
4. **第三方 /anthropic 兼容端点**（DeepSeek/Kimi 系网关）：其 native thinking
   天生无签名，必须逐字回传；按旧红线丢块 = 下一轮 400。
5. 2026-08 新增 preserved-thinking 机制（签名双重绑定 + drop_block beta 头 +
   前缀冻结）为旧调研未覆盖的新变量，进一步收紧签名域行为。

## 决策 Decision

「无签名 thinking drop」从无条件红线修订为**条件规则**，分派键 = 上游类型 ×
该轮 thinking 轨：

1. **第一方**（base_url host ∈ {anthropic.com, *.anthropic.com}）：
   - 无 thinking 配置 / adaptive 轨 / 最后 assistant 无 tool_use → 允许
     drop，但必须响亮（`warn` 级日志，不再静默）；
   - manual(`enabled`) + 最后 assistant 含 tool_use → **禁止裸发**，由守卫
     G1 出站预检降级（见 2）。
2. **第三方 /anthropic 兼容端点**（非 anthropic.com host，含 base_url 缺省，
   fail-open）：无签名 native thinking **逐字回传**（不带 signature 字段，
   禁止伪造）；此分支不存在「drop」。
3. **不变项**：带签名块逐字回传、禁篡改禁伪造（ADR-0003 红线保留）；
   redacted_thinking 逐字回传（票 27）；思维块永不混入 content。

守卫三件套（D-009 拍板清单）：

- **G1 出站预检**（`wire/anthropic.rs`）：manual 轨 + 最后 assistant 消息含
  tool_use 且其首块非 thinking/redacted_thinking → 移除该请求的 `thinking`
  参数与全部 thinking 家族块（signed/redacted/unsigned），`warn` 响亮降级。
  宁缺该轮思考，不裸发注定 400 的请求。
- **G3 三类 400 自适应恢复**（`stream_anthropic_messages`）：400 文本命中
  `thinking_400_recovery_class` 三模式之一 → 以强制降级形态（无 thinking 参数 +
  无 thinking 块）**恰好一次**有界重试；二次失败照旧上抛，不吞其它 400。
- **G6 三路径回归**（wiremock e2e + 单测）：带签名流逐字回传 / 无签名兼容端点
  逐字回传 / redacted_thinking 流逐字回传，另加 G3 400 降级重试用例与
  G1/分派/分类单测；fork-health `-E` 增加 `test(thinking_replay)`。

实现决策——**上游类型判据 = base_url host 分类**：`anthropic.com` 域为第一方，
其余（网关/兼容端点/本地 mock/缺省）fail-open 到 Compatible。理由：Compatible
分支的「回传」是契约中性选择（丢块才是必然 400），误判方向保守；host 匹配是
公开事实，不引入任何端点入仓；不新增配置旋钮，避免与「运行期唯一协议判定入口」
（CONTEXT.md wire_api 词条）冲突的第三判定源。

D-005 冲突协议检查：本记录是 D-009（current，用户已拍板「推荐方案采纳」）的
执行落点，不构成与任何 current 决策的新冲突；被修订对象 CONTEXT.md 旧词条按
「保留原文 + 标 superseded」处理。

## 后果 Consequences

- 正面：manual+tool_use 路径从「注定 400」变为可见降级可用；第三方兼容端点
  不再丢 native thinking 块；静默行为全部转响亮（warn/降级/一次自恢复）。
- 负面/代价：G1/G3 降级轮无思考（能力损失显式可见，优于整轮 400）；反代前置
  真实第一方的部署若使用非 anthropic.com 域名，unsigned 块会被逐字回传（对
  第一方等价于多带一个无签名块，服务端拒绝时由 G3 一次降级兜底）；e2e 断言
  依赖 turn 循环在历史中保留 Reasoning 项的既有行为。
- Append-only：不编辑 ADR-0003 原文；CONTEXT.md 词条修订保留旧文标 superseded。
- Merge 重放面：全部改动落 fork-only 文件（anthropic.rs / client_tests.rs /
  anthropic_wiremock.rs / fork-health.yml，FORK_DIVERGENCE WIR-3 台账 apply 类
  既有面），不新增上游冲突面；G3 挂载点在 stream_anthropic_messages 的
  `Err(err)` 通用臂，属 fork 自有流式循环。
- 测试门：单测 + wiremock 回归按票 28 完成定义交付；CI fork-health 绿证随
  push 窗口统一取证（CI-only 红线）。
