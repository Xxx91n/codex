# ADR-0016 — 发行列车：版本分支基点、队列与交付判据

- Status: Accepted
- Date: 2026-10-18
- Relates: ADR-0001（merge 不 rebase）、ADR-0006（FORK_DIVERGENCE 与 apply/adapt/ignore）、ADR-0012（命名与标签；其基点条款被本文修订）

## Context

ADR-0012 定下“版本分支基点 = 上游稳定 tag 指向的提交”。实测发现两件事：

1. 稳定 tag 打在 main 之外的版本提交上（例：0.163.0 = `83086188ae`，其父 `30c1058df6` 也不在 main 里；切点 `merge-base(tag, main)` = `703d253e`）。tag 分支比切点多 5 个提交：4 个 cherry-pick + 1 个 Cargo 版本号提交。
2. 因此“从 tag 切再 merge trunk”得到的不是“旧版代码 + fork 补丁”。以 0.100.0 为例：其 tag 提交不是 fork 祖先、其父是，merge-base 落在 tag 的父上，合并结果树包含 fork 独有的 `wire/anthropic.rs`，文件数 9083 ≈ fork tip，而 0.100.0 本身只有 2630 个文件。即：得到的是“今天的 fork 代码 + 旧版本号”。

同时，发行面需要一个能机械执行、可被脚本门禁校验的流程：每版交付三平台资产与独立 release 页，执行者可能是不熟悉仓库历史的 bot。

## Decision

1. **基点改为 trunk**：从 trunk 上“已含切点”的提交切版本分支，不从 tag 切。切点 `cut = merge-base(tag, upstream/main)`；trunk 未合到 cut 则先合。
2. **补齐 release 独有提交**：把 `cut..tag` 的提交 cherry-pick 进版本分支，跳过上游版本号提交，改用 `X.Y.Z-tri.N`。
3. **“与官方同版”由 diff 门证明**：`diff(tag, 版本分支)` 必须恰好等于 fork 修改集；多一个文件即漂移。同版从“靠合并保证”变为“靠验证保证”。
4. **严格版本队列**：按版本号升序逐格做完，不追新版，新版只追加到队尾。
5. **单版完成判据为全档**：分支 + Cargo 版本 + 标签 + 三平台资产 + 独立 release 页 + 四门，缺一不算完成。
6. **两条同步管线分离**：`main` 是上游 main 的纯镜像（只快进、零 fork 提交）；`tri-wire-api` 是工作区，永不自动合并上游。

## Consequences

- ADR-0012 的 Decision 1（基点 = tag 提交）被本文修订；其命名、标签与 N 规则继续有效。
- 实测冲突面：合 tag 0.163.0 为 6 个文件，合切点 `703d253e` 为 5 个文件。冲突面小到可以写成逐文件白名单，表外一律停（见规格 R-07）。
- 代价：每版多一步 cherry-pick（通常 4~5 个提交），且必须先确认 trunk 已合到切点。
- 已知风险：上游约每周 2 个稳定版；若单格耗时超过发版间隔，队列会持续变长。这是“不追新版”的自然后果，不是缺陷。
- `0.162.1` 之前的历史版本不回溯；旧版本只能通过重写方式重建，不在发行列车范围内。
- 流程细节（每格步骤、门禁、停止条件、交付物）见 `docs/specs/release-train/README.md`。
