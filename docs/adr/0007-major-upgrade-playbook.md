# ADR-0007 — 上游 1.0 大版本升级预案（major upgrade playbook）

- 状态: Accepted（**预案性质**：本 ADR 预置的条款、模板与开关设计即时生效；其中迁移
  checklist 仅在上游升 1.0 触发时执行——触发点未到前不实施、不落地任何代码）
- 日期: 2026-09-05
- 决策人: architecture-recovery 票 16（major-upgrade-playbook）
- 模板: Michael Nygard ADR 模板（Status / Context / Decision / Consequences）
- 调研依据: atomcode 串行调研「跟随上游大版本升级的 fork 应预置哪些迁移预案要素」
  （2026-09-05，转写落 `.scratch/architecture-recovery/research/16-major-upgrade-playbook-20260905.md`；
  11 轮三引擎 + 11 次一手核验：semver 2.0.0 规范、Kubernetes 弃用政策与版本倾斜政策、
  OpenSSH 7.6 SSHv1 退役序列、RFC 8996、Elasticsearch REST API compatibility、
  Fowler/Pete Hodgson feature toggles、Node.js 2026 发布模型、monday.com Vibe 3 major 发布、
  Tom Preston-Werner "Major Version Numbers are Not Sacred"）
- 关联: ADR-0001（fork 基线：merge-never-rebase / 三注册点 / 同步节奏）、ADR-0006
  （语义补丁队列——本预案的逐条裁决消费其 FORK_DIVERGENCE 清单与五步流程）、ADR-0002
  （adapt 重放与合成语义的裁决先例）、CONTEXT.md（词汇权威）、票 12（上游同步自动化——
  常态同步轨，与本预案 major 闸轨共用 fork-health 载体）、fork-health.yml（既有
  `full_matrix` dispatch 输入为窗口开关的形态先例）

## 背景 Context

上游 `openai/codex` 准连续发布：近 12 个月 945 个 release（stable 中位间隔 1.9 天）、
0.x 滚动从未升主版本；近 6 个月协议层 743 commits。fork 因此采 release-train 持续同步
（ADR-0001 节奏）+ 语义补丁队列逐条裁决（ADR-0006）。major 从未出现 → 全量验证从未被
触发，也没有任何预案——上游若发布 1.0，fork 将在没有迁移清单、没有验证窗口设计、没有
退役条款的情况下临场应对（round3 报告 §6 裁决 c：预案先立）。

行业成熟形态（调研结论，四要素）：
1. **迁移清单**以 semver 规则为骨架（public API 声明义务 + breaking 只进 major），
   monday.com Vibe 3 实证「迁移系统而非版本」：dev 期警告、changelog 单一事实源、
   codemod 覆盖机械变更；Ochoa et al. 2022 实证 semver 实践中大量被违反 → 上游实际
   破坏必须独立检测，不能只信版本号。
2. **冻结基线与兼容窗口**：ES 兼容窗口固定「一个 major 宽」且明言「是升级的桥，
   不是长期策略」；K8s 按稳定性分级给最低寿命（GA 不可在一个 major 内移除）且
   **升级不得跳 minor**；OpenSearch 仅支持相邻 major 滚动升级。
3. **受控全量验证开关**：全量验证是可开关、可豁免、可回滚的预算动作，不是默认每次
   都全跑——ES `requires/skip` 显式豁免（带 issue 号）+ Fowler ops toggle（kill switch
   无需回滚代码）+ Google SRE canary 分级放量。
4. **显式退役条款**：OpenSSH SSHv1 教科书序列（公告 → 逐版 release notes 警告 →
   分级移除 → 最终删除）；RFC 8996 教训：标准弃用 ≠ 实际退役，必须自定停服时点。

本 fork 破坏面的结构性事实：上游 `client.rs` 单文件布局 vs fork 的 client/ 目录 +
`core/src/client.rs` +747 分歧面（merge 冲突热点）；`wire/` 四模块在上游无对应路径；
上游曾于 2026-02 移除 Chat wire（PR #10157）——上游删 wire 有先例，1.0 不排除再动
协议面；共享事件字段演进有 `usage_metadata`（ADR-0002 Ruling 2）与
`handle_unauthorized` plumbing（Ruling 3）两类既成裁决形态。

## 决策 Decision

### 1. 触发定义与预案性质

触发点 = 上游 `openai/codex` 发布首个 ≥1.0 的 stable tag（如 `rust-v1.0.0`）。触发前
本 ADR 仅预置，不执行迁移；0.x 期间按 ADR-0001 节奏 + ADR-0006 清单裁决（release-train
常态），不做全量验证。上游 1.0 是**唯一**强制全量验证闸——此后每个 major 重复本预案。

### 2. 协议层破坏面预判（触发时按此表定检视优先级）

| # | 破坏面 | 上游动作预判 | 冲突概率 | 首响应 |
|---|---|---|---|---|
| S1 | IR 面（`protocol` models / `ResponseItem`） | 新增字段/变体（guardian 类），0.y.z 无兼容承诺下可能改型 | 高频演进、低概率硬破坏 | 按 ADR-0004 同源判据：IR 已对应则复用现型；新变体逐条评估三 wire 消费面 |
| S2 | 派发面（`core/src/client.rs` dispatch + `stream()` 签名） | 签名/控制流演进（ADR-0002 Ruling 3 型） | **高**（+747 分歧面热点） | adapt 重放 fork 意图，per FORK_DIVERGENCE 清单；禁止把 wire 逻辑拉回 client.rs 解冲突 |
| S3 | 共享事件/usage 面（Responses SSE 与合成事件的公共字段） | 加字段（`usage_metadata` 型） | 中 | 合成语义裁决（非 Responses wire 填 None 或对应物），落 ADR ruling |
| S4 | provider 旋钮面（`model-provider-info`） | 加字段/改反序列化 | 中 | wire_api 字段反序列化重放（adapt）；`WireApi` 枚举本身红线不动 |
| S5 | wire/ 四模块 + fork 常量 | 上游无对应路径，理论不触碰；1.0 若重构 transport 结构则波及 | 低（但 1.0 上不封顶） | apply 原样保留；若上游重构波及，按影响分析模板逐条评估，新增分歧先登记再改 |
| S6 | 测试面（client_tests 共享用例 / wiremock 群） | 改共享用例签名/夹具 | 中 | 共享用例 adapt；fork-only fixture apply；本轮 fork-health 绿证为准 |

预判表是**触发时的检视清单而非结论**：触发时必须先用独立破坏检测校准（决策 5 的
迁移 checklist 第一步），以上游实际 diff 为准，不照本表照搬。

### 3. 三注册点影响分析模板（逐条强制填写，并入 FORK_DIVERGENCE 裁决纪要）

对 1.0 触发的每一条上游破坏，逐条填写（字段定义沿用 ADR-0006 清单字段并扩展）：

| 字段 | 填法 |
|---|---|
| 变更描述 | 上游做了什么（commit/PR + 语义一句话） |
| 上游锚点 | merge-base 口径 commit + 版本/PR（禁 tag 口径，per ADR-0006） |
| 触碰注册点 | `WireApi` 枚举 / `ModelProviderInfo.wire_api` / `client.rs` dispatch match——三选多；都不触碰则写「无（仅渗透影响）」 |
| 分类 | apply / adapt / ignore（per ADR-0006 决策 1；ignore 必须留痕） |
| 迁移动作 | 重放意图 / 原样保留 / 留痕放弃；adapt 必须写「在新形态上的意图重述」 |
| 验证断言 | 对应 wiremock 用例名或 cross_wire 差分条目（无断言条目不得标完成） |
| 裁决日期与纪要指针 | 日期 + FORK_DIVERGENCE 条目 id |

规则：三注册点本身（注册点位置的存续与形态）永久 apply，per 根 AGENTS.md
「Registration points (red line)」；若 1.0 的上游形态实质上要求移动/重命名注册点，
默认立场是**反驳**并重开 ADR-0001 走显式裁决，不得静默移动。

### 4. 受控全量 CI 窗口开关设计（预案设计；实现归票 12 与后续票，本 ADR 零代码）

- **常态（开关关）**：fork-health 每日 cron 增量（ubuntu-only）+ 0.x 每次 merge 的
  check job + wiremock 三件套（ADR-0006 五步⑤）。全量验证默认关闭——日常同步禁全量
  重跑（CI 预算纪律，ES requires/skip 同源）。
- **触发窗口（开关开）**：fork-health `workflow_dispatch` 新增输入（形态沿用既有
  `full_matrix` 布尔输入），开启后跑：三平台全矩阵 + 全 workspace 测试 + wiremock
  三件套 + cross_wire 差分 + 三注册点 grep 断言 + 破坏面预判表 S1-S6 逐面检视记录。
- **豁免纪律**：任何 skip 必须显式声明并带 issue 号（ES 模型）；红灯不得带未裁决
  豁免合并。
- **预算**：窗口期自 merge 1.0 起 ≤ 2 周；逾期未绿 → 返修启动器重开窗口，不无限
  续期。
- **kill switch**：窗口关闭（输入复位）即回常态，无需回滚任何代码（Fowler ops toggle
  语义）；窗口内发现的 fork 侧行为变更按 ADR 裁决，不静默跟随上游。
- **绿证口径**：只认 CI run/artifact（全局 CI-only 红线）；fork-health debug profile
  下 suite::* SIGABRT 基建问题（README 第二轮 frontier 遗留）须在触发前收敛，否则
  全量窗口无有效绿证。

### 5. 冻结基线与 EOL 条款

- **冻结基线**：触发时以 merge-base commit（口径 per ADR-0006 决策 3①，禁 tag 口径）
  + FORK_DIVERGENCE.md 全量快照为「1.0 前冻结基线」；1.0 迁移在独立 but lane 进行，
  不与常态同步混栈；基线快照是迁移后差分复核（checklist F2）的对照物。
- **兼容窗口**：fork 对上游只承诺**相邻 major** 的升级路径，禁止跳级直升（K8s /
  OpenSearch 同款）；1.0 期间为过渡设置的任何兼容措施必须显式标注「是升级的桥，
  不是长期契约」（ES 模型），并写入退役表。
- **fork 自身对外契约面的 EOL（退役四步，OpenSSH 模型）**：`wire_api` 配置值、
  provider 旋钮等 fork 对外契约的移除/改名必须——①公告（提前 ≥1 个 release 周期，
  README + CONTEXT.md 同步）→ ②release notes 逐版警告 → ③分级禁用（先默认关闭/
  弃用路径，保留旧值反序列化报错信息）→ ④major 版本一次性删除，并写明替代物 +
  迁移路径 + 停服版号。同一 major 内不移除契约面（K8s Rule #1 型条款）。
- **逃生舱**：删除后保留一个 release 周期的 debug 日志告警（K8s hidden-metric 型），
  视实现成本可裁。

### 6. 迁移 checklist（触发时逐项执行并留痕；源自调研 A-F 组的本仓裁剪）

- [ ] T1 **独立破坏检测校准预判表**：diff 审计上游 protocol/client 面（S1-S6），
      以上游实际 diff 修正预判（semver 不可信，Ochoa 2022 实证）
- [ ] T2 冻结基线快照（merge-base + FORK_DIVERGENCE 快照）
- [ ] T3 时机与代价评估（不升的代价 vs 升的代价；上游生态吸收带宽）
- [ ] T4 逐条影响分析模板填写（决策 3），新分歧先登记再裁决（清单先于实现）
- [ ] T5 merge 1.0（never rebase）+ adapt 条目意图重放 + apply 条目未被误改确认
      （ADR-0006 五步①-④）
- [ ] T6 受控全量 CI 窗口开启并全绿（决策 4；豁免带 issue 号）
- [ ] T7 FORK_DIVERGENCE 刷新 + 新 ADR 落裁决（若 1.0 引入新决策）
- [ ] T8 新基线沉淀：1.0 后首个 merge-base 成为下一常态轨基线；窗口关闭回常态
- [ ] T9 CI 绿证归档（run id/artifact 路径写入本票报告），完成宣告只认绿证

### 7. 范围边界

本 ADR 不落地任何代码，不改现有 workflow/配置/文档导航；checklist 执行、窗口开关
实现、基建问题收敛均归未来票（票 12 落常态同步自动化，窗口开关实现随其或后续轮次）；
PR 上游消灭差异不在本决策内（本工程管理差异而非消灭差异）；Gemini 接入面与重开条件
仍由 ADR-0003 独立管辖，本预案不改变其判据。

## 后果 Consequences

- 正面：上游 1.0 从「临场应对」变为「按预案走闸」——破坏面在触发前已知（预判表）、
  逐条裁决有模板可依（注册点影响分析并入既有清单纪律）、全量验证有开关与预算
  （不污染常态同步的 CI 成本）、fork 自身契约面的退役有可审计条款（对 fork 用户
  满足「企业排期」预期，Node 版本对齐公历同源动机）。
- 负面/代价：预判表与 checklist 存在腐烂风险——1.0 未至期间上游演进可能使预判失准，
  依赖触发时的 T1 校准步骤兜底；预案类 ADR 长期不触发，维护成本为零但心智负担常驻；
  窗口开关实现依赖票 12 落地，触发若早于票 12 完成则以手工 dispatch 现有输入替代。
- 风险与对策：预案被遗忘 → fork-health drift probe 的事实上提醒 + ADR-0006 五步流程
  的 merge 纪律在 1.0 时自然衔接本预案；预判表误导（照搬不校准）→ checklist T1 为
  强制第一步，本 ADR 明文「预判表是检视清单而非结论」；窗口期红灯久拖 → 预算上限
  ≤ 2 周 + 返修启动器机制，禁止带未裁决豁免合并。
