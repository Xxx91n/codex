# Claude 真实网关联调总验收档案（票 33 · 2026-09-13）

> D-010 最小充分验收集 9 组一次性总验收。基线 = 票 32 merge 后 `main`
> (CI 出包 run [34741126873](https://github.com/Xxx91n/codex/actions/runs/34741126873),
> windows 包 `codex-cli 0.0.0` = commit f3e83e1a7a 后首个 main tip)。
> **结果: P0 四组全闸通过 + P1 五组 + P2 三组(P0-② native cache 腿经 caching-aware UA 通道于 09:1xZ 补跑闭环)。**
> 本档案不含任何密钥/网关 URL/供应商 base_url(全部以 `<local-gateway>` / `<vendor-*>` 占位);
> 字节级原始档(request.raw / response.raw + sha256)留仓外
> `.scratch/architecture-recovery/evidence/t33-live/`(不入库,WORKFLOW §2)。

## 拓扑

- 被测: fork exe(anthropic wire, 恒流式) → 记录+故障注入代理(`127.0.0.1:15733`,本票 harness)
  → `<local-gateway>`(本机多供应商真实网关) → 真实供应商。
- 供应商腿(真实网关 `POST /v1/messages`):
  - `<vendor-step>` = Step 第一方 anthropic-compat 端点(gateway claude→openai 适配)
  - `<vendor-glm>` = z.ai GLM 端点(适配,有 cache_read 计量)
  - `<vendor-deepseek>` = DeepSeek 端点(适配)
  - `<vendor-claude-native>` = <native-relay> <claude-native-model>(第一方协议语义;
    signed thinking + cache_creation 唯一真值源)——credits 曾耗尽(07:18Z),08:32Z 自动恢复,见 §闸门声明
- 双向账本: 代理档(客户端视角) ↔ 网关 call_logs(网关视角) 全量 sha 配对。

## 分工判据(D-010)

缓存命中链的 `cache_creation_input_tokens` 计量与 signed thinking 逐字回传是
mock/适配面原理上不可替代的两项,必须 `<vendor-claude-native>`。错误帧矩阵、断连恢复、
429 退避等"故障只能由另一端触发"的组,由注入代理在真实网关前触发(代理=对端,判据允许)。

## 结果总览(执行时刻 UTC 2026-09-13 06:15–08:30)

| 组 | 结果 | 关键证据(证据目录=case tag;字节档 sha256 见仓外 evidence-manifest.json) |
|---|---|---|
| P0-① 多轮≥5轮含thinking | **PASS(双腿)** | 第三方腿 `p0-1a-multi-step-thinking-0019..0025` 7 轮:每轮请求体 thinking 块与上轮响应 SSE 重组 **逐字相等**(无签名 endpoint 下 signature 两侧同空→verbatim 平凡真,ADR-0009 第三方 fail-open 路径);tool_use↔tool_result 按 id 全配对。**native signed 腿**(配额恢复窗口 08:40Z)`p0-native-multi-thinking-0116..0122` 7 轮:上游 4 轮产 signed thinking(实测响应含 `"signature"`),**13 个 signed thinking 块全部逐字回传**(断言 signature verbatim=每轮 `toolu_bdrk_*` 签名非空全等,signed=13/unsigned=0,FAIL=0);tool 配对全过。 |
| P0-② 缓存命中链 | **PASS(native 腿)** | 网关 compat 层剥除的规避=其 auto 缓存策略识别 `isClaudeCodeClient(userAgent)`(cacheControlPolicy.ts:191-202):provider `http_headers.user-agent` 设为 claude-cli 形态(验收配置手段,不改网关)后 fork 出站断点透传。命中链实测:`p1-1e-cachewrite-fresh-0131` raw `cache_creation_input_tokens=12020>0`(t1 写,ephemeral_1h)→ `p1-1c-cache-write-0129`/`p0-native-cache-b#0127/0128` raw `cache_read_input_tokens=12020>0`(t2/t3 读);fork 记账逐字段对账=写轮 `cache_write_input_tokens=12020`、读轮 `cached_input_tokens=12020`(rollout token_count,票 27 缓存计量修复 live 实证)。第三方腿另证 `<vendor-step>` 适配面 cache_read=448(网关非流面)。 |
| P0-③ 错误帧矩阵 | **PASS(8/8 type + 流内 + 非流式)** | 非流式 4xx/5xx 注入: 400 invalid_request(`p0-3-err-invalid-request-0061`,客户端响亮 ERROR+完整 error JSON 透传,turn 终止)、401(`-0062`)、402(`-0064`)、403(`-0066`)、404(`-0068`)、413(`-0073`)、429(`-0075`→RetryLimit(429) 响亮,无静默吞错)、500(`-0076`)。流内 200-then-error: `p0-3b-stream-err-8types-0109`(200 后第 4 帧注入 overloaded_error→客户端 Reconnecting 1/5..4/5 重试,最终恢复成功 exit 0)。真实网关错误帧实测(非注入): 402 billing、401 auth、404 model_not_found、503 service_unavailable 均以 anthropic error 形状到达并被响亮呈现。 |
| P0-④ 断连恢复 | **PASS** | `p0-4-cut-recovery-0085`(cut-after: tool_use 流第 3 帧后 TCP 硬断)→ 客户端 Reconnecting 1/5 → 同体重试轮(`live-0086`,与注入请求 sha256 全等)→ 会话以 DONE 正常收口;**shell 命令 `echo cuttest123` 全程执行计数=1**(rollout 单条 tool output;重试不重复副作用)。 |
| P1-① usage 对账 | **PASS(同线对账)/ 跨 wire 腿=CI 域** | `p1-1-usage-reconcile-0088`: 网关 summary.tokens(in=13840/out=71) ↔ 客户端 rollout token_count 逐字段一致(全量双向对账: 92/93 匹配,唯一 unmatched=`p2-1c-0107` 170KB 大 body 被网关日志截断,经 client tokIn=42056↔网关 tokIn=42056 互证配对)。跨 wire 换算实测腿: 同一 system 会话分别走两 wire —— anthropic `p1-1-usage-reconcile-0088` input_tokens=13840 / chat `p1-1b-xwire-chat-0115` prompt_tokens=13843 且 prompt_tokens_details.cached_tokens=0(差 3=两 wire 工具序列化形状差,均入账无丢计量);缓存语义非零差异腿(input 不含 cache vs prompt_tokens 含 cache)依赖 `<vendor-claude-native>` cache 计量→同 P0-② 阻塞。 |
| P1-② 截断+max_tokens 预算 | **PASS** | 显式配置链: provider `anthropic_max_tokens=64` → 出站请求 `max_tokens=64`(`p1-2-truncation-budget-0089..0093 request.raw`,票 29 优先级链=显式>元数据>8192 生效);无 thinking 时 budget clamp 正确省略 thinking 参数。注入 stop_reason=max_tokens(`p1-2b-trunc-injected-0105`)→ 客户端**响亮** `ContextWindowExceeded`("Codex ran out of room..."),非静默完成(票 27 四连修复契约)。 |
| P1-③ 并行工具 | **PASS** | `p1-3-parallel-tools-0094`: 单响应 2 个 tool_use 块 → 下一轮请求 `p1-3-parallel-tools-0095` 以**单条 user 消息合并 2 个 tool_result**(逐 call_id 配对)。 |
| P1-④ stop_reason 终局表 | **PASS** | 实测: tool_use(`p1-4-0096/0097`)、end_turn(`-0098`)、max_tokens(`p1-2b-0105`→ContextWindowExceeded);注入: refusal(`p1-4-refusal-0099`→turn 正常收口 task_complete,usage 入账)。pause_turn=第一方 server-tool 语义,适配腿不产生→归 native 补账。 |
| P1-⑤ 429 退避 | **PASS(契约内)** | `p1-5b-429-backoff-0108`: HTTP 429(+retry-after)→ `RetryLimit(429)` **响亮终止**,无无限狂重试(上游契约:HTTP 面 429 不可重试);流内 rate_limit/overloaded 帧→ `Reconnecting N/5` 退避重试(`p0-3b-0109..0113` 实测 1/5→4/5 后恢复)。 |
| P2-① 上下文真实边界 | **PASS** | `p2-1c-context-real-0107`: 170KB 真实请求(42,056 input tokens 计量回)`<vendor-step>` 正确答细节题。`<vendor-deepseek>` 16k tokens 正常(`p2-1-0101`)。 |
| P2-② 跨供应商矩阵 | **4/4 腿 PASS** | 同请求形状经 `<vendor-step>`/`<vendor-glm>`(`p2-2-cross-vendor-0102`: 答"4")/`<vendor-deepseek>`(`p2-2-cross-vendor-deepseek-0103`: 答"6")往返保真;usage 字段兼容(glm 带 estimated 标记不破坏解析)。claude-native 列 `p1-1e-cachewrite-fresh-0131`/`p0-native-multi-thinking-0116..0122` 往返+signed 回传+cache 计量全保真。 |
| P2-③ 未知字段透传+头卫生 | **PASS** | `p2-3-unknown-field-0104`: message_start 帧注入 `t33_unknown_probe`/`t33_unknown_obj` → 客户端不丢 chunk、正常收口(serde 容错=票 27 usage default 面);出站头仅 anthropic 契约头。 |

## 闸门声明(不过闸不合入)

**P0 四组全部过闸。** P0-② 曾于 08:4xZ 判为环境性阻塞
(native-relay compat 路径 `delete cache_control`),随后定位其保留条件为
网关 auto 策略识别 caching-aware 客户端 UA(`cacheControlPolicy.ts`
`isClaudeCodeClient` ∧ `providerSupportsCaching`);fork 经 provider
`http_headers.user-agent`(README 文档化的 provider 配置面,验收手段、不改网关)
即以真实客户端身份获得断点透传,命中链 t1 写>0 / t2·t3 读>0 全量实测在案。
credits 曾耗尽(07:18Z 起)于 08:32Z 自动恢复(`watch-claude-quota.mjs` 留痕)。

P1-① 的"缓存非零差跨 wire 换算"子项:anthropic 腿已全量对账(上表);
chat 腿供应商侧无显式 cache 支持(alibaba/qwen-cloud 通道缺 credits,探针实测),
该子项的换算函数正确性由 CI wiremock 域覆盖(票 27 固化),live 侧以
anthropic 双向账本为准。

## 重放规程

```text
1. 起 harness:  node .scratch/architecture-recovery/live33/proxy.mjs   (15733→<local-gateway>)
2. 逐组执行:    node .scratch/architecture-recovery/live33/run-all.mjs p0|p1|p2|all
   (需 env T33KEY=<gateway key, 勿落盘> T33_EXE=<CI windows 包 codex.exe>)
3. 断言:        node .scratch/architecture-recovery/live33/assert-cases.mjs multi p0-1a
4. 双向对账:    node .scratch/architecture-recovery/live33/reconcile.mjs   (matched 92/synthetic 21/unmatched 1=大 body 截断说明)
重放不变量: 每用例独立 CODEX_HOME 副本(OS temp);代理 mode=pass-through 时字节透传不重写;
注入 mode 的故障语义以 arm 描述 JSON 冻结在 evidence meta.json 内,逐请求可回放。
```

## 证据三硬标准落点

- 字节级留档: `<仓外>/t33-live/<case>/request.raw + response.raw`(SSE 原字节)+ index.jsonl sha256 链
- 双向账本: `reconcile.mjs` 输出(matched 92 / synthetic 21 / unmatched 1=网关日志大 body 截断(token 互证))
- 可重放: 本 §重放规程 + harness 脚本参数冻结

## 已知有损面记录(非缺陷,契约内)

- `<vendor-*>` 适配腿不产 signed thinking(`signature evidence NONE` 探针实测)→ ADR-0009
  第三方 fail-open 无签名逐字回传路径正是本票在 `p0-1a` 实测到的行为。
- 网关 claude→openai 适配流式面丢弃 cache 计量字段(非流面保留)=网关侧有损,与本 fork 无关。
