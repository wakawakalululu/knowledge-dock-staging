# tools/ — 验证脚本

检索回归脚本在应用运行（HTTP 接口默认 `127.0.0.1:18230`）时执行；`selection-ui-test.js` 使用 VM + DOM/Tauri mock，可独立运行：

| 脚本 | 用途 | 运行 |
|---|---|---|
| `quiz-test.js` | 答题式检索回归：主题命中 / root 与 type 过滤 / `ai=1` 筛选与降级 / health 探针 | `node tools/quiz-test.js` |
| `syntax-test.js` | 高级语法回归：短语 / 排除 / 必须词 / title: / path: / type: / root: / mtime: / 组合 | `node tools/syntax-test.js` |
| `selection-ui-test.js` | 前端划词回归：字面量参数 / chips 范围保留 / 不写历史 / IME / 请求竞态 / 启动监听顺序 / 失败提示 / 设置保存；`once` 验证重试成功，`always` 验证重试 3 次后不发 ready；无需运行应用 | `node tools/selection-ui-test.js [once\|always]` |
| `attach-ctf-source.js` | 将 Des-CTF-Knowledge 仓库注册为 git 源 + 知识根（需先 clone 到 `%APPDATA%/com.local.knowledgedock/repos/des-ctf-knowledge`） | `node tools/attach-ctf-source.js` |
| `check-versions.js` | 断言 npm/Cargo/Tauri 的 6 处版本与锁文件完全一致 | `node tools/check-versions.js` |

回归脚本在任一断言失败时返回非零退出码，可直接用于 CI/串行校验。
`attach-ctf-source.js` 写盘前调用 `cargo run --bin import_diag` 校验完整配置，并保留最近 5 份备份。

脚本为示例基线：断言中的库名/主题词按当前测试库（biglib / des-ctf-knowledge）编写，换库时相应调整。
