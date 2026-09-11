# Fork Divergence Ledger — 骨架与快照（语义补丁队列）

> 状态: Accepted ｜ 落地: 2026-09-11（architecture-recovery 票 30）
> 决策依据: 用户裁决 D-003（grill 轮账本，.scratch/tri-wire-hardening/decision-ledger.md）
>   + ADR-0006（`docs/adr/0006-fork-divergence-patch-queue.md`，分类语义与五步流程的仓内权威）
> 词汇对齐: 仓库根 CONTEXT.md「语义补丁队列」「apply / adapt / ignore」词条

## 入仓豁免声明（docs 不入仓红线的例外）

WORKFLOW §2 规定重文档不入库（运行台账在 .scratch/ 仓外）。本目录按用户裁决 **D-003**
（2026-09-11，治理面三项中仅此一项采纳）例外入仓：**骨架 + 字段模板 + 当前条目快照**，
使 ADR-0006 五步裁决流程的第③步（逐条裁决）在任何干净 clone 上可执行。
运行时台账不入仓；仓外文件仍是唯一权威。本票验收与报告（票 30）引用本声明作为豁免留痕。

## 权威与同步纪律（声明，无自动同步）

- **唯一权威运行台账（仓外）**: `.scratch/architecture-recovery/FORK_DIVERGENCE.md`
  （相对 fork 工作区根；不入库，不随 clone 流转）。快照滞后期间的一切裁决以仓外台账为准。
- **本目录 = 骨架 + 字段模板 + 快照（非权威）**: `snapshot-2026-09-11.md` 是生成日
  2026-09-11 时点仓外台账的只读快照。
- **同步纪律**: 每次上游 merge 重放（ADR-0006 决策 3 五步流程）完成后，由执行 merge 的
  窗口**手工**刷新仓内快照（重新生成快照文件、更新文件名日期）。快照与运行台账之间的漂移
  以本声明为解释依据：不做自动同步、不建 CI 强制门（per ADR-0001「measured, not gated」
  与 D-003 负向约束）。新分歧先登记（仓外台账）再实现。

## 条目 ID 体系

前缀-序号（如 REG-1），前缀与运行台账分区一一对应：

| 前缀 | 分区 | 语义 |
|---|---|---|
| REG | 三注册点（WireApi 枚举 / wire_api 字段 / client.rs dispatch match） | seam 锚，永久 apply（ADR-0006 决策 4） |
| CLI | `core/src/client.rs` 分歧面 | 上游冲突热点，adapt 为主 |
| WIR | `core/src/client/wire/` 四模块 | fork-only，apply |
| ADJ | 协议相邻面（codex-api endpoint/SSE、provider 旋钮、tool_spec） | seam 相关，adapt 为主 |
| TOOL | tools plan/registry 面 | ADR-0008 三段配对契约 |
| STATE | state/基建面（.gitattributes 覆盖层、EOL checksum 自愈） | 独立于协议层 |
| （header 备案） | CI 面（fork-health.yml 等 fork-only workflow） | 不参与重放，仅在台账 header 备案 |

## 字段模板（每条目必填）

`| id | 区域（文件:符号） | 分类 | 上游锚点 | 红线指向 | 最后裁决 |`

- **id**: 上述前缀-序号，全局唯一，不复用。
- **区域**: 文件路径:符号（锚点只写「文件:符号」不写行号，per CONTEXT.md 消费规则）。
- **分类**: apply / adapt / ignore 三者之一；一条差异仅落入一类。
- **上游锚点**: merge-base 口径（禁 tag 口径——上游 squash-merge 使 `git tag --merged` 失真）
  或上游 PR/commit 引用；锚点要冗余（commit + PR/版本），不依赖单一平台引用。
- **红线指向**: AGENTS.md 条款 / ADR 编号 / CONTEXT.md 词条。
- **最后裁决**: 日期；adapt 重放后刷新。裁决纪要指针在仓外台账条目内维护。

## 分类词汇（apply / adapt / ignore，per ADR-0006 决策 1，对齐 CONTEXT.md）

- **apply** — fork 刻意保留、上游无对应路径；merge 时原样保留，不重推导。
- **adapt** — 与上游演进纠缠同一代码区；每次 merge 在新形态上重放 fork 意图，重放后刷新
  「最后裁决」日期。
- **ignore** — 已消解差异，留痕不删除：`Resolved: absorbed-by-upstream <commit>` 或
  `Resolved: dropped <理由>`。

## 五步裁决流程（per ADR-0006 决策 3，每次上游 merge 强制）

1. **同步前**: `git fetch upstream`，以 merge-base 口径量化 ahead/behind（禁 tag 口径）。
2. **merge**: `git merge upstream/main`（never rebase，per ADR-0001）；冲突预期收敛在
   清单全部 adapt 条目。
3. **逐条裁决**: adapt 条目在新上游形态上重放 fork 意图并刷新「最后裁决」；apply 条目确认
   未被误改；清单外新分歧 → 先登记（仓外台账）再裁决（清单先于实现）。
4. **落 ADR**: 有新决策且改变既有决策 → 新 Nygard ADR（`docs/adr/NNNN-*.md` 递增）；
   仅重放无新决策 → 只刷新条目日期。
5. **验证往返**: CI 全绿（fork-health wiremock 三件套 + check job）后才允许推远端；
   本机零构建（CI-only 红线）。

**干净 clone 可执行性**: 第③步所需的自洽材料 = 本文件（分类词汇 + 字段模板 + ID 体系 +
流程）+ 快照（snapshot-*.md）+ ADR-0006 全文（docs/adr/），仓内成环；仅裁决纪要历史与
最新条目状态需回仓外台账读取（见「权威与同步纪律」）。

## 快照

- [snapshot-2026-09-11.md](snapshot-2026-09-11.md) — 生成 2026-09-11。计数口径对账：
  21 条 ID 条目（REG×3 / CLI×5 / WIR×4 / ADJ×5 / TOOL×2 / STATE×2）+ 1 条 CI 面
  header 备案 = 22，与票单「当前 22 条目快照」口径一致。
