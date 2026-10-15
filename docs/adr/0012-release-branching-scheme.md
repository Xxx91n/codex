# ADR-0012 — 上游稳定版发行分支：基点、命名与逐版裁决记录

- Status: Accepted
- Date: 2026-10-14
- Relates: ADR-0001（merge 不 rebase）、ADR-0006（FORK_DIVERGENCE 与 apply/adapt/ignore）、ADR-0007（0.x 期间不触发）

## Context
fork 需要为每个上游稳定版（rust-vX.Y.Z，共 220 个）提供一条发行分支。两个选择都有真实代价：标签要用 build metadata 还是 prerelease；分支从 main 切还是从 tag 提交切。

## Decision
1. 分支基点是上游稳定 tag 指向的提交，不是 upstream/main。稳定 tag 的提交不在 main 祖先链上（例 0.162.1 = 092d3acd），用 main 会得到错误基线。
2. 命名：分支 tri/vX.Y.Z；标签 tri-vX.Y.Z-tri.N（N 从 1 起，不复用）；Cargo 版本 X.Y.Z-tri.N。
3. 版本标记用 prerelease，不用 build metadata。理由：Cargo 忽略 build metadata，排序无规范承诺（cargo#17242），crates.io 禁止仅 + 不同的重复版本。
4. 标签不用 rust-v 前缀：fork 的 .github/workflows/rust-release.yml 监听 rust-v*.*.*。
5. 逐版裁决写入 FORK_DIVERGENCE，不逐版写 ADR；本 ADR 管规则。

## Consequences
- 已知代价：X.Y.Z-tri.N 低于 X.Y.Z，自更新与版本比较逻辑需在首版验证。
- 流程细节（验收门、冲突规则、bot 边界）见 docs/specs/release-branching/README.md。
- 推送标签或分支、发布 release、删除远端内容、改 origin/main、重命名远端主分支，均需用户逐项批准。
