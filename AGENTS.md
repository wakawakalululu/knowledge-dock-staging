# AGENTS.md — knowledge-dock 协作约定（agent 必读）

本仓库的**权威工程基线**是 [`BASELINE.md`](BASELINE.md)。开工前先读它；本文件只列最关键的执行约束。

## 执行约束

1. **迭代闭环**：修改 → `node --check ui/app.js` → `cargo check` → commit（`vX.Y.Z: 摘要`）→
   push **staging** → 等 CI 绿灯 → 下载 artifact → 实机验证。任何一步不能跳过。
2. **推送通道**：只有 `staging` 可推（PAT 内嵌 URL，见 BASELINE §4/§9）。`origin` 仅记录，勿假设可推。
3. **版本三处同步**：`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`、`package.json`。
4. **依赖红线**：网络统一 `curl` CLI / `std::net`；新增 crate 需在 BASELINE 说明理由。
5. **配置类改动**：必须直接读 `config.json` 断言落盘；手工注入配置前跑
   `cargo run --bin import_diag -- <a> <b>`；新字段必须 `serde(default)`/`Option`。
6. **验证脚本**：`tools/quiz-test.js`（答题检索）、`tools/syntax-test.js`（高级语法）在应用运行时可回归。
7. **文件操作姿势**：含反引号/复杂字符串的编辑一律写脚本文件执行，不用 bash 内联 `node -e`；
   Git Bash 下 `reg` 命令加 `MSYS_NO_PATHCONV=1`。
8. **每轮收官**：同步 README（特性/语法/配置表）；新踩的坑追加到 BASELINE §9。
