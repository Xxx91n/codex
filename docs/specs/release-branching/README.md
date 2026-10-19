# 上游稳定版发行分支规格

> **部分条款已被取代（2026-10-18）**：本文的基点与每版流程（§1、§3）基于“从上游 tag 提交切分支再合 fork”，已被 [ADR-0016](../../adr/0016-release-train-and-branch-base.md) 推翻——实测那条路只能产出“当前 fork 代码 + 旧版本号”。现役规则见 [release-train 规格](../release-train/README.md) R-05~R-07。
> 命名（§2）与验收门（§4）仍有效。下文保留为历史依据，不再作为执行依据。

状态（历史）：首版样板当时只在 Windows CI 通过（run 38708780625）；2026-10-18 三平台已全绿（run 39107264573）。规则依据：[ADR-0012](../../adr/0012-release-branching-scheme.md)。

## 1. 基线（基点条款已废，见页首）
- 上游稳定 tag：220 个，格式 rust-vX.Y.Z（无 alpha/beta）。
- 基点 = tag 指向的提交（ADR-0012）。
- 首版：rust-v0.162.1（指向 092d3acd）。批量阶段按同一规则回溯。
- 规模（测量日期 2026-10-14）：fork 比该提交多 127 个提交；该提交比 fork 多 1280 个提交；merge-tree 显示 12 个冲突文件，含注册点文件 codex-rs/model-provider-info/src/lib.rs。
- 上游 main 已删 chat wire（CHAT_WIRE_API_REMOVED_ERROR，提交 6fff9955f1，#16508），上游无 anthropic wire。fork 的这两个 wire 每版都要保留。
- AGENTS.md 不存在于 0.162.1（上游删除于 18131270fe，#49713）；fork 中该文件对 0.162.1 是纯新增，不产生冲突。

## 2. 命名
- 分支：tri/vX.Y.Z，例 tri/v0.162.1。
- 标签：tri-vX.Y.Z-tri.N，N 从 1 起，不复用。禁止 rust-v 前缀。
- Cargo 版本：X.Y.Z-tri.N。
- 主分支名：tri-wire-api（2026-10-15 已改名，远端默认分支已切换，旧名 feat/tri-wire-api 已删除）。

## 3. 每版流程（已废，见 release-train R-05）
先用首版跑通全流程，再批量回溯其余版本；每批有可验证的每版验收标准。
1. 从 tag 提交建分支 tri/vX.Y.Z（不推送）。
2. merge fork 改动（不 rebase，ADR-0001）。
3. 冲突按第 5 节裁决；表外停下。
4. 按第 4 节验收门逐项通过。
5. 本地打 tag tri-vX.Y.Z-tri.N。推送与发布见第 6 节。

## 4. 验收门（全部通过才打 tag）
- a 注册点核对（grep）：WireApi 含 Responses/Chat/Anthropic；ModelProviderInfo.wire_api 存在；ModelClientSession::stream 派发 match 三分支。
- b 构建：以仓库 CI 为准。本地通过不等于 CI 通过。
- c 相关 crate 测试：just test -p codex-model-provider-info、-p codex-state、-p codex-api。core 全量需用户授权。
- d ADR-0011 相关 checksum 测试通过。
- e 票 33 相关 live 验证需凭据，暂停等用户授权。e 未授权前不得声称 live 验证完成。

## 5. 冲突规则
- 注册点三处（WireApi 枚举、wire_api 字段、stream 派发 match）保留 fork 侧，上游新增变体/字段手工并入（ADR-0006 adapt）。
- 删除上游 CHAT_WIRE_API_REMOVED_ERROR 拒绝逻辑，恢复 chat wire（ADR-0002/0003）。
- checksum/state 保留 fork 的 ADR-0011 实现，不用上游版本覆盖。
- 与 fork 无关的上游改动取上游。
- 一句话判定不了的冲突停下，记入 backlog，不由 bot 裁决。

## 6. 远端动作分界
- 本地打 tag：允许。
- 推送 tag/分支、发布 release、删除远端内容、改 origin/main、重命名远端主分支：每项须用户逐项批准。

## 7. backlog 归位
- (a) 首版前必须做：backlog #12 基线 3 例环境性失败（先在 0.162.1 基线上确认哪些是上游自身失败）；Issue #2 TUI 拒绝 wire_api=chat。
- (b) 样板内完成：#1 注册点扩列（由第 4 节 a 覆盖）；#4 票 33 pause_turn（待凭据）。
- (c) 批量阶段：#2 事件对齐（A-008/009）、#3 transport 抗重复（A-010/011）、#6 H2/H3 复核、#11 重复显示监控。
- (d) 仅记录或关闭：#5、#7、#8、#9、#10（已由 ADR-0012 取代）、#13；Issue #1（旧 SHA 8b86dfb1，先重跑确认）。

## 8. 逐版裁决记录
每版的冲突裁决写入 FORK_DIVERGENCE（ADR-0006 流程），不逐版写 ADR。
