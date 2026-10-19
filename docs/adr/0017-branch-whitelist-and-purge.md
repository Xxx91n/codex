# ADR-0017 — 分支白名单与上游继承分支一次性清理

- Status: Accepted
- Date: 2026-10-19
- Relates: ADR-0016（发行列车分支拓扑）、ADR-0012（命名与标签）

## Context

GitHub fork 建仓时从 openai/codex 继承了全部贡献者分支：远端分支共 4635 条，其中 4632 条为上游继承，无一条承载 fork 自身工作。它们拖慢每次 fetch 与分支补全，且 fork 不再需要本地翻阅上游 PR 分支（正确去处是 openai/codex 原仓）。

同时存在一条 probe 诊断分支（`tri/v0.162.1-probe`，4 个独有提交），其实验结论已收割进现行 CI（RUST_MIN_STACK、seam-check 选择器），分支沦为脚手架。

## Decision

1. **白名单**：远端仅允许三类角色——trunk（`tri-wire-api`）、上游镜像（`main`，ADR-0016）、已发行冻结版本线（`tri/vX.Y.Z`）。
2. **一次性清理**：4632 条上游继承分支批量删除；fork 同步上游只同步默认分支提交、不会自动新建分支，故清理是一次性的，无长期维护负担。
3. **不动 tag**：删除分支不删提交对象，上游 tag 引用的提交仍可达；所有 `tri-v*` 与 `rust-v*` tag 一律不碰。
4. **probe 处置**：删除本地与远端 `tri/v0.162.1-probe`；删除前将其 4 个提交 SHA 留档于 grill backlog（`b795d9e56e` / `399bcb0bcc` / `48747eebc4` / `18aa1370f7`），禁止合入 trunk（内含与现行规则冲突的实验提交）。
5. **白名单纪律**：今后任何远端新分支必须落入白名单三类角色之一，否则不得推送。

## Consequences

- 上游 PR 分支不再可在 fork 内翻阅；需要时去 openai/codex 原仓。
- 未被 tag 引用的上游分支 tip 最终会被 GC——本 ADR 的删除不可逆，因此白名单纪律是唯一防再生与防误删的闸门。
- 执行细节（批量删除配方、留档核验、终态核验）见 `.scratch/branch-cleanup/handoffs/next-round.md`（仓外工件）。

