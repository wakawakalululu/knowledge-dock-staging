# Rust CI Dev Loop History

- Repository: `wakawakalululu/knowledge-dock-staging`
- Workflow: `.github/workflows/build-loop.yml`
- Artifact: `knowledge-dock-windows-loop`
- Branch policy: local `agent/*` branch, push only to remote `staging`

### Iteration #1 - 2026-11-25T07:34:00Z

- **Commit**: `bd0f27ccdf28c5058bf3caffb806a99f3c4a2347`（基线提交 `7746903`）
- **改动要点**: 初始化 Windows `dev-release` 云端构建、版本锁门禁、`loop_driver.sh` 与结构化日志摘要。
- **CI 状态**: [run 45512130904](https://github.com/wakawakalululu/knowledge-dock-staging/actions/runs/45512130904) `success`；产物 `knowledge-dock-windows-loop`（19,017,578 字节）。
- **本地运行结果**: `precheck` 退出码 0；6 处版本一致、前端语法通过、`cargo check --locked` 通过。v0.8.40 产物实机启动、HTTP API、版本读取和截图通过；答题回归 9/9，语法回归 9/10。
- **根因诊断**: 本地 `cargo test` 在测试进程启动阶段报 `STATUS_ENTRYPOINT_NOT_FOUND`，未进入断言。语法回归暴露真实产品缺陷：全局 `mtime:>1d` 在 BM25 前 100 条之后过滤，近期低排名文档被漏召回；根限定查询可命中同一文件，排除夹具缺失。
- **下一步行动**: 将 mtime 写入数值索引并在 TopDocs 前做范围过滤，补充低排名回归用例后执行下一轮。

### Iteration #2 - 2026-11-25T08:01:00Z

- **Commit**: `13b71a93295baeb6b4f4cfb538b5037de8dea251`（v0.8.41）
- **改动要点**: schema 升至 6；新增 indexed/stored/fast `mtime` 字段；时间窗加入 `BooleanQuery` 的 Must 范围条件；结果直接读取索引中的 mtime；补充 120 条旧高排名文档 + 1 条近期低排名文档的召回回归；修复 Windows Node 后台管道与 CI 空结论字段解析。
- **CI 状态**: [run 45515816771](https://github.com/wakawakalululu/knowledge-dock-staging/actions/runs/45515816771) `success`。
- **本地运行结果**: `precheck` 退出码 0（6 处版本一致、前端语法、`cargo check --locked` 通过）；`cargo check --tests --locked` 通过。定向 `cargo test` 完成链接后仍在测试进程启动阶段报 `0xc0000139 / STATUS_ENTRYPOINT_NOT_FOUND`，未进入断言，因此不计为单测通过。
- **产物证据**: portable 21,584,896 字节，SHA-256 `d5f9855cdaf55ab4ffd0159f1277bf9ead2ea9aa4a630b686cb7b1faff95c02a`；NSIS 10,511,680 字节，SHA-256 `867330b3121fbc726803669512b147bb1a210db50ed59aa8d8b97aa35aa69049`；两者文件版本均为 0.8.41。
- **实机结果**: 索引重建完成（1,124 个源文件 / 83,513 个块）后，答题回归 9/9、语法回归 10/10；全局 `注入 mtime:>1d` 召回 2 条 `biglib/overview.md`，所有返回 mtime 均在时间窗内；单实例唤回后搜索框可见，截图已人工查看；验证结束进程数为 0。
- **根因诊断**: 首次实机回归在 schema 6 全量重建完成前运行，`/api/health` 已通但索引尚不可见，产生无效零结果；验证器改为等待 `/api/stats.stats.total_docs > 0` 后重跑，全部通过。
- **下一步行动**: 记录收官证据并完成最终审计；无未解决的产品缺陷。
