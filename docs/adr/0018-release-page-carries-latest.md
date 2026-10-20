# ADR-0018 — 发行页承担 Latest 徽标，滚动索引页退役

- Status: Accepted
- Date: 2026-10-20
- Relates: ADR-0012（命名与 prerelease 字符串）、ADR-0016（发行列车）；取代 release-train 账本 D-020

## Context

D-020 把 `tri-wire-test` 定为“最新版索引页”，资产只挂版本页。实测后果：

- 三个 release 全部 `prerelease=true`，`isLatest` 全为 false；
- `https://github.com/Xxx91n/codex/releases/latest` 302 回到 `/releases` 列表页（上游同地址则落到 `releases/tag/rust-v0.163.0`）；
- 列表顶部是零资产的索引页，正文写着“这不是下载页”。

GitHub 的 Latest 只认非 draft、非 prerelease 的 release；`/releases/latest` 与 `/releases/latest/download/<asset>` 是官方永久链接。零资产页置顶，等于把唯一的全局锚点让给了一个空壳。

同时，索引页要求 trunk 跑一次发布作业，而该作业与三平台构建同 run 门控，导致一次 runner 回收就停住整个队列（Issue #4）。

## Decision

1. 版本页即下载页；删除 `tri-wire-test` release 与其 tag；`/releases/latest` 成为唯一稳定入口。
2. 版本页一律 `prerelease=false`；`make_latest=true` 只给最新版本，判定按版本号（X.Y.Z.tri.N）比较，不按 `created_at`。
3. 正文下载优先：平台下载表与直链在前，元数据在后；附 `SHA256SUMS` 资产与 build provenance attestation。
4. 发布与构建分离：`fork-publish.yml` 只消费某个已完成 run 的产物，且 headSha 必须等于被发布 ref 的 head。
5. 版本字符串维持 `X.Y.Z-tri.N`；版本页必须声明其在 semver 上低于同号上游版本。

## Consequences

- D-020 与 D-017 的页模板被取代（对应账本已追加 revised 指针，原文保留）。
- 旧版本不再是“预发布”，Pre-release 徽章消失；Latest 只有一个。
- 补发旧格 hotfix 时 `make_latest=false`，不会抢走徽标（spring-boot#50927 的教训）。
- trunk 不再有任何发布动作；Issue #4 的失败面整体消失，而不是靠跳过构建绕开。
- 代价：用户升级需手动下载（二进制无自更新），已在版本页与 README 写明。
