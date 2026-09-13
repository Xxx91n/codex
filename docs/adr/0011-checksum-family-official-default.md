# ADR-0011 — checksum 家族回归官方默认裁决（票 35，2026-09-14）

> 状态 Status：Accepted。本裁决 **supersede 票 25「六迁移目录 LF 锁 + 启动自愈」裁决与 ADR-0010 决策表第 1 行中「保留 fork 六迁移目录 LF 锁（STATE-1）」的部分**（上游 `third_party/voice/sources.json` LF pin 不受影响）。ADR append-only：原裁决原文就地保留，不回溯编辑。
> 决策链：第八轮 grill 账本 .scratch/dual-cli-coexistence/decision-ledger.md D-001（用户裁决废弃 LF）→ D-002（atomcode 深度调研）→ D-003/D-004（五件套全案采纳）；摩兄登记 A-019~A-025；调研底稿 .scratch/architecture-recovery/research/35-checksum-family-research-20260914.md（S1-S12 三源交叉）。

## 背景 Context

票 18 引入仓根 .gitattributes 后票 25 把六个迁移目录锁到 LF 并在启动路径做单向自愈（CRLF→LF 逆转）。sqlx::migrate! 按 checkout 字节内嵌 SHA-384 校验和，官方 Windows CLI（构建机 autocrlf=true）内嵌 CRLF 家族、逐字节比对、无自愈 —— 共享 home 下官方 CLI 永远拒开 fork 维护的库（D-012 事故，用户被锁死在 fork 侧）。用户 D-001 裁决：「肯定不能一直保持 LF 下去啊，必须回归官方默认」。

调研结论（research/35 ⑧/⑨节）：工业界唯一有成熟先例的修复心智模型 = Flyway repair 式「显式命令 + 元数据对齐」（S5/S6/S9）；「启动时自动反向重写」零公开先例（#38528 未合并，S8），D-015 该约束持续有效。

## 决策 Decision

1. **废除 LF 锁**：.gitattributes 六迁移目录改 `text !eol`（存储侧仍 LF；checkout 侧交给平台 git 配置：Windows runner autocrlf=true → CRLF，Linux/macOS → LF，与官方构建机同构）。禁全仓 renormalize。fork 构建回归官方平台家族。（A-019）
2. **auto 语义翻转**：`[state] migration_checksum_family` 默认 auto = 跟随二进制内嵌家族；启动家族不匹配时做只读诊断 → fail-loud（错误信息含「库=X 家族 / 二进制=Y 家族 / codex state fix-checksums --family Y --apply」），零自动改写；真漂移/篡改保留 sqlx 原始错误且不给修复命令。（A-021/A-022）
3. **显式档维持**：crlf/lf 显式配置保留票 31 启动维持行为（先重定位到内嵌家族过验证，迁移后再落到配置家族，六前提门不变）。
4. **存量引导**：首次检测漂移指纹（库家族, 二进制家族, 平台）→ 一次性提示 + 持久 marker `$CODEX_HOME/.checksum-family-notices.json`（指纹变化再提示；CODEX_DISABLE_CHECKSUM_FAMILY_NOTICE 可关闭）；release notes 明示家族回归与迁移命令。（A-023）
5. **CI 防线替换**：删 fork-health LF-only scan step 与六目录 eol=lf 锁；新增 platform-family-assert job（checkout 字节层，每日 ubuntu+windows）；fork-cli-test-release 新增产物 SHA-384 字节扫描（权威层）与 e2e-official-coexistence job（隔离 CODEX_HOME 副本演练：fork 建库 → `--family lf` 模拟遗留漂移 → auto fail-loud 文案断言 → `--family crlf --apply` 恢复 → 官方 Windows release 二进制冷启动打开）。（A-024）
6. **兜底**：`--family lf` 逃生口与应急 FAQ 入 docs/fork-checksum-family.md；幂等语义有测试。（A-025）
7. **恢复 SOP 固化**：备份（含 -wal/-shm）→ 副本先验 → 逐项验证（integrity_check/行数/逐条家族判据/双二进制冒烟）→ 真库 → 官方冷启动，六库全量，写真库前需用户明确指令 + 安静窗口 —— 执行本身是计划表执行项 R1（用户门控），不属于本 ADR 代码面。（A-020 固化面）

## 后果 Consequences

* 同平台 fork↔官方切换不再产生家族墙；新库默认即官方家族，之后装官方 CLI 即开即用。
* 遗留 LF 库首次启动必输出 fail-loud 错误 + 一次性提示，需用户跑一条显式命令恢复；这是有意为之的可见性取舍（禁止静默改写共享库）。
* 跨平台搬 home 从「自愈无声翻转」变为可见 fail-loud 事件，用户按提示显式决策。
* 票 31 显式档与既有测试族不回归；家族判定函数（checksum_family/family_checksum/detect）双侧对称化后，单元测试在 LF 与 CRLF 两种 checkout 下均成立（平台中立化）。
* FORK_DIVERGENCE 运行台账的 STATE-1 条目（.gitattributes 六目录 LF 锁）在下次上游 merge 重放时按本 ADR 改判为「已废除，改 text !eol」（运行台账仓外，由大脑轨刷新，本票报告登记）。
* 若官方未来采纳 #38528 转 LF canonical，家族战略经 lf 逃生口可再翻转（本裁决不锁死方向）。

## 验证 Validation

重放验证只认 CI：fork-health（platform-family-assert 双平台 + seam 套件含新增家族测试）与 fork-cli-test-release（digest 断言 + e2e 官方二进制共存演练）双绿 run 号见 .scratch/architecture-recovery/reports/35-checksum-family-official-default-report.md；本机零构建（CI-only 纪律）。
