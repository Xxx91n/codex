# ADR-0006 — 语义补丁队列（FORK_DIVERGENCE 清单与上游合并裁决流程）

- 状态: Accepted
- 日期: 2026-09-05
- 决策人: architecture-recovery 票 13（fork-divergence-patch-queue）
- 模板: Michael Nygard ADR 模板（Status / Context / Decision / Consequences）
- 调研依据: atomcode 串行调研「软件 fork 维护中语义补丁队列的工业化落地」（2026-09-05，转写落
  `.scratch/architecture-recovery/research/13-fork-divergence-patch-queue-20260905.md`；22 查询三引擎
  五角度 + 13 篇一手核验：DEP-3、SUSE kernel-source、Linux -stable 规则、rustc-dev-guide LLVM
  fork 流程、llamafile update_llamacpp SOP、gitworkflows、GitHub fork sync docs、Copybara、
  FOSDEM 2025 Forking Android、AOSP 停 tag 事件、jj/Pijul/GitButler）
- 关联: ADR-0001（三注册点 + merge-never-rebase + 每次合并落 ADR）、ADR-0002（首批裁决实例）、
  ADR-0003（不接 Gemini 判据）、ADR-0004/0005（reasoning 域裁决）、CONTEXT.md（词汇权威，
  「语义补丁队列 / apply-adapt-ignore」词条）、FORK_DIVERGENCE.md（仓外运行清单，
  .scratch/architecture-recovery/）、票 12（上游同步自动化，本 ADR 是其裁决输入）

## 背景 Context

本 fork 对上游 `openai/codex` 的分歧面约 43 文件（+4013/-9，ADR-0001 基线），集中在三注册点、
`core/src/client.rs` 分歧面（+747，上游冲突热点）、wire/ per-wire 模块（fork-only 四文件）、
codex-api 双 wire 端点与 SSE、model-provider-info 旋钮、tool_spec 与 wiremock 测试群。上游为准连续
发布：stable 中位间隔 1.9 天、近 12 个月 945 release、近 6 个月协议层 743 commits；fork 落后
8 天 / 335 commits / 30 releases（merge-base 6be2a6ca952a）。分歧面目前靠人脑记忆 + ADR 散落记录，
每次上游合并都要现场重新推导「哪些差异是本 fork 刻意为之、哪些可放弃」——漂移无声、裁决不可审计。

行业成熟形态（调研结论）：语义补丁队列 = 「补丁集合 + 顺序清单 + 结构化头部元数据 + 裁决工具链」
四件套。DEP-3 用 Origin/Forwarded/Applied-Upstream 字段、SUSE 用 Patch-mainline 五档 + Git-commit
锚点、kernel -stable 用 [Upstream commit <sha1>] + 偏离必须说明、llamafile 用 triage 脚本输出
Reconcile/Drop/Split 三命运 + verify-clean 往返验证。通式：每条差异必有「分类 + 上游锚点 + 偏离
说明」，锚点要冗余（commit id + 版本 + 清单条目），不依赖单一平台引用（AOSP 2026-08 停 tag 事件
的实证教训）。与上游合并的配合 = 同步管道 → 队列裁决（triage，机器筛干净补丁、人工裁决冲突）→
验证往返。

## 决策 Decision

1. **分类语义（apply / adapt / ignore 三分类）**：fork 对上游的每一条分歧必须落入且仅落入
   一类——
   - **apply**：fork 刻意保留、与上游无语义纠缠的差异。合并时原样保留，不重推导。例：wire/
     四模块（上游无对应路径）、fork 常量（ADR-0002 Ruling 1）、wiremock 测试群。
   - **adapt**：fork 差异与上游演进纠缠同一代码区，每次合并需在其新形态上重放 fork 意图的差异。
     例：`client.rs` dispatch match（上游改 `stream()` 签名时 fork 三分支要跟改）、
     `handle_unauthorized` 参数 plumbing（ADR-0002 Ruling 3）、`client_tests.rs` 共享用例、
     `model-provider-info/src/lib.rs`（上游加字段时 fork 旋钮要重放）。
   - **ignore**：曾经是差异、现已无需保留——上游已吸收（fork 删侧留记录）或 fork 放弃（删实现
     留记录）。条目不物理删除，标 `Resolved: absorbed-by-upstream <commit>` 或
     `Resolved: dropped <理由>`，保证可审计。

2. **运行清单（FORK_DIVERGENCE.md）**：仓外 `.scratch/architecture-recovery/FORK_DIVERGENCE.md`
   是逐条登记的唯一运行清单（仓外 per WORKFLOW §2/D6：运行时台账不入库）。每条必填字段：id、
   区域/文件:符号、分类（apply/adapt/ignore）、上游锚点（merge-base commit 或上游 PR/版本）、
   红线指向（AGENTS.md 条款 / ADR 编号）、最后裁决日期与裁决纪要指针。协议层条目（wire/ 四模块 +
   `client.rs` 分歧面 + 三注册点）首版随本 ADR 全量登记。

3. **merge 裁决流程（每次上游合并强制走五步）**：
   ① **同步前**：`git fetch upstream`，以 merge-base 为口径量化 ahead/behind（上游 squash-merge
   使 `git tag --merged` 失真，禁用 tag 口径）；
   ② **merge**：`git merge upstream/main`（never rebase，per ADR-0001），冲突预期收敛在清单中
   所有 **adapt** 条目；
   ③ **逐条裁决**：对每条 adapt 条目在新上游形态上重放意图，apply 条目确认未被误改，结果
   追加到该条目的裁决纪要；出现清单外的新分歧 → 先登记再裁决（清单先于实现）；
   ④ **落 ADR**：有新裁决且改变既有决策 → 新 Nygard ADR（`docs/adr/NNNN-*.md` 递增，per
   ADR-0001 Decision）；仅重放无新决策 → 只更新清单条目的最后裁决日期；
   ⑤ **验证往返**：CI 全绿（fork-health wiremock 三件套 + check job）后才允许推远端；
   本机零构建（CI-only 红线）。

4. **与三注册点的关系**：三注册点（`WireApi` 枚举 / `ModelProviderInfo.wire_api` /
   `client.rs` dispatch match，per ADR-0001）在清单中是最高的 seam 锚——所有三注册点条目永久
   apply（注册点本身不动，per 根 AGENTS.md「Registration points (red line)」）；包住注册点调用面
   的差异（新增枚举值 + wire_api 字段反序列化 + match 分支）按 adapt 管理并逐条标注 seam 归属。
   新 wire 接入（ADR-0003 重开预案）必须同时新增三条清单条目。

5. **范围边界**：本 ADR 不改任何代码；FORK_DIVERGENCE.md 只登记协议层（wire/ 四模块 +
   `client.rs` 分歧面 + 三注册点 + 协议相邻面），测试群与非协议面后续按需扩充；PR 上游消灭差异
   不在本决策内（本工程管理差异而非消灭差异）。

## 后果 Consequences

- 正面：fork 分歧从「人脑记忆」变为可审计台账，上游合并从现场推导变为按清单裁决；adapt 面在
  合并前已知（冲突预期收敛、工作量可预估）；ignore 条目留痕使「为何没有这个差异」也可审计；
  票 12（同步自动化）可直接消费本清单做分叉红灯与 ahead/behind 度量。
- 负面/代价：每次合并多一份逐条裁决纪要的簿记成本；清单与代码可能漂移——依赖「清单先于实现」
  纪律（新分歧先登记）与每次 merge 后的条目刷新；仓外清单不入库，跨仓库克隆不随行（可审计性
  依赖 .scratch 工作区存续，必要时按本 ADR 字段定义重建）。
- 风险与对策：清单腐烂（条目与实物不一致）→ 每次 merge 强制刷新 + CONTEXT.md 维护条款 grep
  校验；裁决跳步 → 票 12 自动化把「清单未刷新」做成红灯信号（后续轮次）。
