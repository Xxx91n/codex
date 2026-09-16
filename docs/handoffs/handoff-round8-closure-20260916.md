# Handoff — 第八轮收口（dual-cli-coexistence：checksum 家族回归 + B1 根治 + R1 受阻待票 37）

> 生成 2026-09-16 ｜ 交接对象：下一轮开工会话 ｜ 正本=OS temp，仓内镜像=docs/handoffs/handoff-round8-closure-20260916.md
> 数据源权威：.scratch/dual-cli-coexistence/decision-ledger.md（D-001~D-009）+ .scratch/architecture-recovery/decision-ledger.md（A-019~A-029 已结算）

## 一句话现状
第八轮票 35/36 全部 done 并已合入 main=feat/tri-wire-api=8151694908（终跑双绿 35054458696/35054462097；滚动 tag tri-wire-test 已指向 main 构建）。**用户理想态（双 CLI 同 home 自由切换）差最后一步**：R1 六库恢复被票 35 生产缺陷 P4（版本集合等式拒绝带 pending migrations 的真实现场）阻塞成 deadlock——证据链与修复方案已呈报（D-009），待用户拍板立票 37。

## 下一轮任务（每项声明覆盖 D/A-xxx）
- T1 票 37 执行窗（待用户批准立票+派窗）：P4 放宽 stored⊆embedded + 逐行哈希不变式先行 + CI e2e 补「DB 落后于二进制」用例 + help 行重复小清。覆盖：D-009、A-020 执行面复通。启动器待立票后出。
- T2 R1 六库恢复执行（T1 落地后，用户安静窗口+明确指令）：备份已在 .scratch/build-evidence/r1-backup-20260916/；SOP=docs/fork-checksum-family.md；恢复后双向启动测活+用户重开 Codex app 终验。覆盖：D-001/D-004/D-007、A-020。
- T3 backlog 7 项裁决（T4 移交）：deferred 5 项立票建议（A-001→拟票、A-008/009/010/011 合并票）/票 33 计数口径/票 32 plumbing 追认/串行登记条款/凭据披露条款/omniroute UA 反馈/三分支处置（已随第八轮收口删除，登记于 README）。
- T4 收口滚动：T1/T2 完成后 $handoff 更新本文件。

## 红线（全量继承）
零本地构建（证据只认 CI run/artifact）；~/.codex 六库恢复前零写（T2 是唯一授权写窗口）；token/PAT/网关零入库；push 停等用户；人工删 tag=流程违规；atomcode 串行；共享登记文件串行读-改-写+复读。

## Suggested skills（下一会话）
- $but: T1 lane 提交/（授权后）push 合并
- $atomcode-research: T1 立票前如需 P4 放宽边界的工业先例复核（Flyway pending-migration 语义对照）
- $to-spec / $to-tickets: 用户批准票 37 后按对账闸出工件三件套
- $code-review: T1 收口签核
- $handoff: T4 滚动更新
