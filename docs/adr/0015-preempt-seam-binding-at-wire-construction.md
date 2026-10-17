# ADR-0015 — preempt 对账绑定在各 wire 的构造点，turn 循环保持协议无关

- Status: Accepted
- Date: 2026-10-17
- Relates: ADR-0013（preempt 语义与对账规则）、ADR-0001（三注册点）、ADR-0014（同一轮的出站失败位）；CONTEXT.md「ResponseItem」「per-wire 模块」；审计轮 backlog 1.1 / 1.3

## Context

ADR-0013 首轮实现把对账入口做成 `wire::reconcile_preempted(wire_api, pending)`，由
`session/turn.rs` 在读到 preempt 时带着 `provider.info().wire_api` 调用它，并在 turn 循环里
手工排空 stream 的残余事件。留下三处张力：

1. turn 循环出现协议感知，直接撞 CONTEXT.md「三 wire 入站 SSE 只产出
   `ResponseItem`/`ResponseEvent` 流，turn 循环无 wire 感知」以及「per-wire 模块」条的禁止用法
   （"把 wire 翻译逻辑写进 client.rs/turn.rs 主体"）。
2. `wire/mod.rs` 里因此多出第二处 `match wire_api`，与注册点 3（`client.rs::stream()` 的
   dispatch）同义重复，正是 wire/AGENTS.md Boundary 所警惕的"在 wire/ 内增加注册点"形态。
3. stream 生命周期（关流 → 排空 → 对账 → 丢弃）散落在 turn 循环里，属 Leaky Lifecycle 气味。

## Decision

1. 取消统一分派：每个 wire 在自己的 stream 构造点把**自己的** `reconcile_preempted` 交给
   `map_response_stream(.., preempt_reconciler)`；Responses 的两条路径（SSE 与 websocket）传
   `/*preempt_reconciler*/ None`，其 `preempt` 字段保持 `None`（spec N-02 不变）。
2. `ResponseStream` 持有 `PreemptSeam { handle, reconcile }`——关流句柄与该 wire 的对账函数
   成对存在（两个字段 `pub(crate)`，构造点只有 `map_response_stream` 一处；根 AGENTS.md
   「Do not create small helper methods that are referenced only once」明确否定为单次调用
   再包一层 `new()`）；单一方法 `take_preempted_history()` 负责"关上游读 → 收下 wire 已发出的
   事件 → 交给本 wire 对账 → 返回应入史的条目"。
3. `turn.rs` 只调用该方法，随后追加中断标记并写史；不再出现 `wire_api`、不再手工排空、
   不再 import 任何对账符号。
4. seam 只触发一次（`Option::take`），再次调用返回 `None`，同一条 pending IR 不可能重复入史。

## Consequences

- 正面：协议判定回到唯一注册点；turn 循环恢复协议无关；stream 的终结生命周期归其所有者；
  行为与 ADR-0013 逐条等价，S-01..S-11 与规格文件未改写。
- 代价：`map_response_stream` 多一个形参（4 个生产调用点 + 1 个测试调用点）；
  `client_common.rs` 增加约 45 行；`wire/mod.rs` 只保留共享的合成结果常量与构造器。
- 形参形状：新形参是 `Option<fn(Vec<ResponseItem>) -> Vec<ResponseItem>>`，Responses 侧写
  `/*preempt_reconciler*/ None`。根 AGENTS.md 更偏好枚举/newtype 而非可空位参；这里保留
  `Option` 是因为它表达的正是"这条 wire 有没有客户端 preempt 缝"这一存在性判定，且
  `argument_comment_lint` 只认字面量 `None` 的注释形式，换成枚举会让三个注册点侧的分支变重。
  已核：`Some(reconcile_preempted)` 是 call expr，不需要注释；三处 `None` 都带精确形参名注释。
- 隐藏约束：`turn.rs` 在 `if let Some(..) = stream.take_preempted_history().await { drop(stream) }`
  的臂内移动 `stream`，依赖 edition 2024 的 `if_let_rescope`（edition 2021 下该借用活过整个臂，
  编译不过）。旧代码同样如此，不是本轮新引入的脆弱点，但同步若回退 edition 会先在这里炸。
- 测试口径：本重构不改出站形状，行为证明继续由两条 wiremock preempt 场景承担
  （`chat_wire_preempt_reconciles_the_interrupted_turn` 与
  `anthropic_wire_preempt_reconciles_the_interrupted_turn`，断言的是捕获到的出站请求体）。
  没有为测试而从 codex-api 导出 `preempt_channel`——句柄只能在 wire 侧产生，导出它属 test-only
  API 增殖（根 AGENTS.md「Crate API surface」）。
- 台账：ADJ-8 刷新为"seam 绑定形态"；WIR-1 去掉"`reconcile_preempted` 按 WireApi 分派"的表述；
  WIR-6 / WIR-7（各 wire 自己的 `reconcile_preempted`）语义不变。
