# Round-8 收口 — 双 CLI 共存：checksum 家族回归官方默认 + B1 根治（2026-09-16）

## 票池终态
- 票 35 checksum-family-official-default（返修 r1-r7）= done：fork 回归官方平台默认家族（Windows=CRLF），auto=跟随内嵌家族 + mismatch fail-loud 指路零改写，一次性指纹引导，CI 三层防线（platform-family-assert / release 产物 byte-scan / e2e 官方二进制共存演练），恢复 SOP 入 docs/fork-checksum-family.md。收口轮 run：fork-health 34944762437 + fork-cli-test-release 34963402248（@mwn 双绿，release 史上首过，三资产 URL 200×3）。
- 票 36 release-tag-upsert-idempotent（返修 r1-r3）= done：**B1（round1 挂账）正式关闭**——发布步 refs-API force-move upsert 零 delete + create-or-heal + clobber + concurrency 串行；R4/R5（35013234811/35013249759 @d8a0ebd77d）同 tag 在场连发双绿。人工删 tag 应急流程即日退役（再执行=流程违规）。
- A 账本结算：A-019~A-025 + A-029 implemented；A-026~028 stale（rejected 留痕）。D 账本 D-001~D-009 随 .scratch 归档。

## 终跑（verify-build @main）
- main=feat/tri-wire-api=8151694908（2c27→ch×7→36×3→nr 镜像重放，线性链）；fork-health 35054458696 ✓ / fork-cli-test-release 35054462097 ✓（含 release job——新幂等发布路径在 main 上再度实证，滚动 tag 已指向 main 正式构建）。
- 启动测活：artifact 下载→codex.exe（sha256=70db2d1a47489d95…）--version=codex-cli 0.0.0 ✓；fix-checksums 帮助面已呈票 35 新语义。归档 build-evidence/codex-round8-closure-20260916.exe。

## R1 本机六库恢复：执行受阻（生产缺陷，见 D-009）
- 全量备份（六库+wal/shm）永久化 .scratch/build-evidence/r1-backup-20260916/（state_5 sha 与在线库逐字节一致）。
- 副本先验：fail-loud 文案全对（双侧家族/line-ending-only/SQL contents verified identical/修复命令），但被指路的 fix-checksums 在 P4 拒绝——真实现场带 pending migrations（state missing=[53,54,55]、memories missing=[2]），P4 版本集合等式 all-or-nothing 中止 = deadlock（官方拒开+恢复命令拒执）。CI e2e 因同 build 建库未捕获。
- 真实现场零改动；旧 fork exe（auto=LF）继续可用，无回归。建议新票 37：P4 放宽为 stored⊆embedded（pending 由 sqlx 启动语义应用）+ e2e 补「DB 落后于二进制」用例 + fail-loud help 行重复小瑕疵顺手清。**待用户拍板立票，R1 挂起至 37 落地**。
