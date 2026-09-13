> 耐久镜像（2026-09-14，用户指令：but commit 防丢失）。正本 = 仓外 .scratch/dual-cli-coexistence/handoffs/next-round.md；本镜像为只读快照，更新以正本为准。

# Next-Round 常驻任务书 — dual-cli-coexistence（第八轮）

- 生成: 2026-09-14 ｜ 数据源: .scratch/dual-cli-coexistence/decision-ledger.md（D-001~D-004）+ .scratch/architecture-recovery/decision-ledger.md（A-019~A-028）｜上轮 handoff: handoff-round7-tri-wire-hardening-20260914.md（OS temp）
- 仓内耐久镜像: codex/docs/handoffs/next-round-dual-cli-coexistence-20260914.md（正本=本文件；镜像防丢失用）

## 任务清单（每项声明覆盖 D-xxx）

### T1 票 35 执行窗口（ready-for-agent）
- 覆盖: D-001/D-003/D-004 → A-019、A-020（固化面）、A-021、A-022、A-023、A-024、A-025
- 启动器: .scratch/architecture-recovery/prompts/35-checksum-family-official-default.md
- 工件: issues/35… / handoffs/35… / spec.md 第八轮段 / research/35-checksum-family-research-20260914.md
- 收尾: 报告 reports/35-checksum-family-official-default-report.md；push 停等用户

### T2 票 35 大脑轨复核（T1 收口后）
- 覆盖: D-004（采纳裁决兑现核验）+ A-019~A-025 逐条
- 流程: 不信报告自述——实物验证（测试门/守卫脚本/but 状态/rg）+ 声明→证据→结论对照表 + A-xxx 逐条核对 + README 第八轮状态表登记 + 重算 frontier；返工则重发 rN 启动器（先复核主 Agent 检查结果/重跑同一套验收/报告追加返工轮次不覆盖）

### T3 本机六库恢复 R1（用户门控，非票件）
- 覆盖: D-004① → A-020 执行面
- 前置: 票 35 出包 + 用户指定无 codex 内核安静窗口 + 明确指令；桥接 config migration_checksum_family="crlf"；SOP=docs/fork-checksum-family.md

### T4 Backlog 7 项裁决（用户）
- 覆盖: D-004③（待逐项裁决）
- 议题: 第七轮 handoff §5 全项（①deferred 5 项立票建议 ②票 33 计数口径 ③票 32 plumbing 裁决 ④串行登记条款 ⑤凭据披露条款 ⑥omniroute UA 反馈 ⑦本地三分支 arch/23、pr-10157、pr-12234 处置）；对话中已有实物证据与推荐但未裁决未入账本，裁决后补录 D-005+

### T5 整轮收口（T1-T4 后）
- 覆盖: D-001~D-004 账本结算 + A-019~A-028 终态 + 三层一致性（CONTEXT.md/docs/adr/代码现状）+ handoff 滚动更新

## Suggested skills

- $but: T1 报告后 lane 提交与（授权后）push；T5 合并
- $atomcode-research: T1 窗口内新增设计点补调研（串行硬规，一次一个在途）
- $code-review: T2 签核
- $grill-with-docs: T4 裁决若需新议题
- $handoff: T5 滚动更新本文件

## 红线（全量继承第七轮 + 本轮）

- ~/.codex 六库真实库严禁写入/挪动/删除/重建（端到端一律副本）
- token/PAT/SSH key/网关 URL/API key 永不写入 commit/报告/handoff
- 本机禁止一切构建/编译/打包/测试运行；证据只认 CI run/artifact
- push 前必须停等用户明确指令
- 使用用户凭据或消耗外部配额必须显式呈报（第七轮票 33 教训）
- 调研冲突协议（第七轮 D-005）继续适用：结论与 current 冲突禁止静默改向
