# 工程基线（Baseline）

> 本文件是 knowledge-dock 的**权威工程基线**：开发环境、迭代闭环、发布规则、验证标准与已知环境坑。
> 所有改动必须遵循；偏离需在此文件记录原因与例外。
>
> 最后更新：v0.8.42（可选划词自动搜索：UIA 优先 + 受控 Ctrl+C 回退）

## 1. 仓库与发布通道

| 项 | 值 |
|---|---|
| 工作目录 | `C:\Users\Admin\Desktop\code\SerachDock` |
| origin | `https://github.com/Snow0xcc/knowledge-dock.git`（主仓；本机 git 凭证无名权推送，待 MCP 或主仓 token 恢复后一次性补发） |
| staging | `https://github.com/wakawakalululu/knowledge-dock-staging.git`（**唯一可推送通道**，CI 在此触发） |
| CI | `.github/workflows/build.yml`：push main → 构建 artifact；push tag `v*` → 自动创建 GitHub Release（附 portable + NSIS） |
| 产物 | `knowledge-dock-portable.exe`（免安装）+ `knowledge-dock_X.Y.Z_x64-setup.exe` |

## 2. 目录职责

```
ui/                     零打包器前端（vanilla JS + 内联 SVG）
src-tauri/src/
  lib.rs                窗口形态/快捷键/托盘/IPC/文件监听/位置记忆/命令注册
  selection.rs          UI Automation 只读划词 + 纯文本剪贴板快照/受控 Ctrl+C 回退
  engine.rs             Tantivy BM25 + jieba + 持久化索引 + 同义词 + 高级语法(parse_advanced)
  indexer.rs            差异化切块（法规/手册/问答/笔记 → DocumentChunk）
  matching.rs           范围规则匹配（glob 纯函数）
  gitsync.rs            Git 浅克隆/拉取/分支切换（git CLI）
  webdav.rs             WebDAV 镜像同步 + DPAPI 凭据（curl CLI）
  apps.rs               开始菜单应用枚举与搜索
  ai.rs                 OpenAI 兼容筛选客户端（curl CLI）
  http_api.rs           本地 HTTP 检索接口（std::net，127.0.0.1:18230）
  logging.rs            按天滚动文件日志
  config.rs             配置读写/迁移/备份
  bin/sync_diag.rs      索引同步诊断（逐根扫描结果）
  bin/import_diag.rs    配置 schema 校验（外部 JSON 注入前必跑）
tools/                  验证脚本（quiz-test.js / syntax-test.js）
scripts/gen-icon.mjs    图标生成（纯 Node 绘制 PNG）
.github/workflows/      构建与发版
screenshots/            README 运行截图（实机）
```

## 3. 本地环境

- 工具链：`rustup` stable `x86_64-pc-windows-gnu` + MSYS2 ucrt64
  - PATH：`/c/msys64/ucrt64/bin:$HOME/.cargo/bin`
  - `src-tauri/.cargo/config.toml` 指定 `x86_64-w64-mingw32-gcc.exe` 为 linker
- 前端零打包器：直接编辑 `ui/app.js|index.html|style.css`，改后必须 `node --check ui/app.js`
- **依赖红线**：网络/HTTP 一律走系统 `curl` CLI 或 `std::net`，不新增 reqwest 等 Rust 网络依赖；
  新增任何 crate 需在此文件说明理由
- **Windows 划词依赖**：直接依赖 `windows = "0.62"`，仅启用 COM、UI Automation、剪贴板、内存与键盘鼠标输入绑定；
  与 Tauri 已使用的 windows 0.62 统一，避免同时引入 0.59/0.62 两套绑定。该 crate 不提供网络能力。
- 内存/GUI 依赖：WebView2 运行时（Win10/11 自带）
- **磁盘约束**：C 盘长期紧张（≤2GB 空闲时 `cargo check` 会因 `os error 112 磁盘空间不足` 失败）。
  若 Z 盘已挂载，优先把构建缓存移出 C 盘：
  ```bash
  export CARGO_TARGET_DIR=/z/cargo-target/serachdock
  ```
  当前远程执行环境未挂载 `/z`，`F:` 也不是可靠独立卷（`Get-Volume` 查无对应卷）；
  可临时沿用 `src-tauri/target`，但须先确认剩余空间并监控构建增量。
  空间不足时再清理 `src-tauri/target`。**文档-only 提交可不做本地编译，以 CI 为准**（见 §4 step 5）。

## 4. 迭代闭环（每次改动完整走一遍，不得跳步）

```
1. 本地修改（ui/ 或 src-tauri/src/）
2. 静态检查：node --check ui/app.js；cargo check 零 error
3. 提交：git commit -m "vX.Y.Z: 一句话摘要"
4. 推送 staging（PAT 内嵌，见 §9 GCM 坑）：
   git push https://wakawakalululu:$GH_TOKEN@github.com/wakawakalululu/knowledge-dock-staging.git main
5. 等 CI 绿灯（GitHub API 轮询 runs/{id} → completed:success；红灯先 get_job_logs 看编译错）
6. 下载 artifact → dist/vX.Y.Z/ → 实机验证（见 §6）
7. 稳定版本：git tag -a vX.Y.Z -m "摘要" && 推送 tag → 自动 Release
```

任何一步失败：回到 1；**禁止"改了没验证就宣称完成"**。

Rust 云端构建闭环脚手架位于 `.claude/skills/rust-ci-dev-loop/`：

```bash
bash .claude/skills/rust-ci-dev-loop/scripts/loop_driver.sh precheck
# 提交后从 agent/* 分支推送 HEAD 到 staging:staging
bash .claude/skills/rust-ci-dev-loop/scripts/loop_driver.sh watch
bash .claude/skills/rust-ci-dev-loop/scripts/loop_driver.sh fetch_run knowledge-dock-windows-loop knowledge-dock-portable.exe 60
```

`precheck` 未返回 0 前禁止 push；每轮结果必须追加到 `.agent-loop/history.md`。
`build-loop.yml` 使用 `[profile.dev-release]` 保留符号/行号，产物仍只上传到 staging Actions。

## 5. 版本与发布规则

- 版本号**三处同步**（缺一不可）：
  - `src-tauri/Cargo.toml` → `version`
  - `src-tauri/tauri.conf.json` → `version`
  - `package.json` → `version`
- 锁文件同步：`src-tauri/Cargo.lock` 的 `knowledge-dock` 包版本，以及
  `package-lock.json` 顶层与 `packages[""].version` 必须与上面一致；提交前用
  `cargo check --locked` / `npm ci --package-lock-only --ignore-scripts` 验证。
- 小步迭代：每个可验证特性一个版本号，修复补丁也升版本（不复用旧号）
- Release 用 annotated tag（`v*`），CI 自动生成 notes + 附双产物
- staging 上所有版本必须可追溯：commit 信息即变更日志

## 6. 验证基线（分层，全部必须做）

| 层 | 方法 | 通过标准 |
|---|---|---|
| 编译 | `cargo check` | 零 error（存量 warning 不阻塞） |
| 前端语法 | `node --check ui/app.js` | 无语法错 |
| 前端划词回归 | `node tools/selection-ui-test.js`（完整 app.js + DOM/Tauri VM mock），另跑 `once`/`always` 监听失败模式 | 字面量参数、不写历史、竞态失效、启动监听顺序、关键监听失败时不发 ready、设置字段全部通过 |
| 前端冒烟 | 复制 `ui/` 到临时目录 + 注入 `window.__TAURI__` stub（mock invoke：get_config/search/search_apps/list_history/rescan 全覆盖）+ 本地 http-server | 关键 DOM 交互/渲染断言通过 |
| HTTP API | `node tools/quiz-test.js`、`node tools/syntax-test.js` | 全部用例通过（检索/过滤/语法/AI 降级）；任一断言失败必须返回非零退出码 |
| 后端诊断 | `cargo run --bin sync_diag`、`import_diag` | 逐根扫描正常；配置可解析 |
| 实机 | a11y 树读元素/值 + PowerShell `CopyFromScreen` 截图（透明窗 CUA 截图不可用） | 目标 UI 呈现/行为可见 |
| 配置落盘 | 直接读 `%APPDATA%/com.local.knowledgedock/config.json` 断言 | 值确实写入磁盘，非仅 UI 显示 |

## 7. 配置与日志

- 配置：`%APPDATA%/com.local.knowledgedock/config.json`；每次保存自动备份最近 5 份到 `config-backups/`
- **手工/脚本注入配置前必须**跑 `cargo run --bin import_diag -- <当前> <待导入>` 校验 schema 可解析
  （历史事故：`last_sync: null` 导致整份配置解析失败被回退覆盖）
- 所有新配置字段必须 `#[serde(default)]` 或 `Option`，保证旧配置可读；新增字段同时更新
  `import_diag` 覆盖的 diff 逻辑（如需展示）
- 日志：`logs/kd-YYYY-MM-DD.log`（按天滚动，单日 >10MB 换 `.old`，目录保留最近 7 份）
  - 入口：设置→通用→打开日志目录；`GET /api/logs?lines=N`
  - 关键路径必须 `kdlog!`（启动、引擎同步、git/webdav 失败、API 异常、配置异常）

## 8. 架构约束（防止回归）

- **单根失败不得中断全局**：`sync_roots`/`start_watchers` 对坏根跳过并记日志
- **新结果集必须失效前端缓存**：`ctxCache/ctxOpen/aiKeep/foldExpanded` 随 doSearch 重置
- **窗口形态变化只走 `update_view()`**（set_expanded + hidden + render），不得裸调 render
- **拖拽区**：`data-tauri-drag-region` 只放在专用拖拽元素（`.s-icon`），不得放父容器
- **HTTP handler 必须 catch_unwind**：单个 panic 只断连接，不得拖垮服务线程
- **phrase 查询**：PhraseQuery 要求 >1 term，单词短语必须退化为 TermQuery
- **可索引过滤条件**：`mtime` 等结构化条件必须在 `TopDocs` 截断前进入查询；不得只对前 N 条做后过滤并靠扩大 N 声称修复召回
- **划词安全边界**：功能默认关闭；UIA 只读优先；Ctrl+C 必须单独 opt-in，注入前拒绝任何非纯文本/自定义剪贴板格式；仅快照 `CF_TEXT`/`CF_OEMTEXT`/`CF_UNICODETEXT`/`CF_LOCALE`，快照与序列号必须在同一 clipboard guard 内读取，且 `CloseClipboard` 后仍须复核稳定序列号（系统可能延迟提交格式并递增序列号）；恢复必须由合法隐藏窗口持有剪贴板，禁止 `OpenClipboard(NULL)`；清空前预分配主副本和完整备份副本，首次重放失败时用备份完整重试；复制后同时复核序列号、`GetClipboardOwner` 与原前台窗口归属、全部格式，来源不明或并发更新时禁止恢复；序列号变化且确认归属后才恢复，绝不覆盖用户随后复制的新内容
- **划词触发与窗口**：忙碌采集只保留最新请求，快捷键线程不得等待上一轮采集；每次结果带单调 `request_id`，前端丢弃重复/乱流旧事件；成功与失败按到达顺序派发；窗口按显示器工作区最大重叠选择目标区域，同时约束宽高和位置，避免跨屏裁切
- **划词语义与事件**：选区走 `search_literal`，不解析高级语法、不写历史；`frontend_ready` 必须等待全部启动监听器注册，关键监听重试失败时不得发送 ready，3 秒兜底显窗不得放行划词事件；成功/失败事件均先显窗聚焦且不发 `ui-reset`，失败通过独立事件展示原因，并与排队成功结果共用同一有序队列；后端显窗不得预折叠窗口，形态统一由前端 `update_view()` 驱动
- **前端字符串**：HTML 属性内不得出现未转义引号（placeholder 事故）；含反引号/复杂串的文件操作用脚本文件

## 9. 已知环境坑（踩过即记，先查这里）

| 现象 | 原因 | 对策 |
|---|---|---|
| `git push` 挂起无输出，进程表见多个 `git.exe` + `git-credential-manager` 大内存 | GCM 凭据缓存失效，等终端交互 | 杀 GCM/git 进程；改用 PAT 内嵌 URL 推送（§4） |
| `cargo check` 报 `No space left on device` / `os error 112` / ld 失败（常连带 gcc 报错） | C 盘满 | `export CARGO_TARGET_DIR=/z/cargo-target/serachdock` 移出 C 盘；或清 `src-tauri/target`、`dist/`、`%TEMP%`；保留 ≥3GB |
| `cargo check \| tail -1 && git push` 在编译失败时仍推送 | 管道吞退出码 | 编译结论看 tail 输出本身，不链式 `&&` |
| bash 内联 `node -e` 中含反引号模板串，被当命令执行删内容 | shell 命令替换 | 含反引号/复杂字符串一律写脚本文件（Write 工具）再执行 |
| Git Bash 跑 `reg add ... /v ...` 报无效语法 | MSYS 路径转换吃掉 `/v` | `MSYS_NO_PATHCONV=1` 前缀（Rust Command 不受影响） |
| 透明窗 a11y 截图 `invalid_target` | WebView2 透明分层窗 | 用 PowerShell `CopyFromScreen`；a11y 元素顺序≠视觉顺序，按值锚定 |
| 程序化注入 Alt+Space 无效 | global-hotkey 过滤注入事件（LLKHF_INJECTED） | 热键行为需真实按键验证；显隐逻辑用 Esc/单实例唤回验证 |
| CUA 坐标点击需 raster 帧 | 透明窗无帧 | PowerShell `SetCursorPos+mouse_event+keybd_event` 系统级注入 |
| `raw.githubusercontent.com` 本地 SSL 失败 | 代理环境 | GitHub 内容验证一律走 `api.github.com` |
| 默认 GCM 弹窗中断自动化 | 交互式认证 | `GIT_TERMINAL_PROMPT=0` 仅用于诊断，不作为修复 |
| 远程 shell 执行 `git` / `node` / `cargo` 提示 `command not found` | WSL PATH 未包含 Windows 工具目录 | 用绝对路径调用 `C:\Program Files\Git\cmd\git.exe`、`C:\Program Files\nodejs\node.exe` 与已安装 toolchain 的 `cargo.exe` |
| Windows `git` / `node` 收到 `/mnt/c/...` 后找不到文件 | WSL 路径不会自动转换为 Windows 路径 | 参数统一经 `wslpath -w`；命令替换里的失败先赋值再使用，避免被外层 `echo` 掩盖 |
| Windows Node 在 WSL 后台管道写 stdout 报 `EISDIR` | Windows 进程把 WSL 管道误判为目录 | Node 先重定向到普通临时文件，再由 Bash 读取转发；不要直接接入后台管道 |
| MSYS2 `bash -lc` 丢失 WSL 传入的仓库路径变量 | Windows 登录 shell 未保留该环境变量 | 用 `printf %q` 编码 MSYS 路径后嵌入命令，不依赖跨进程自定义变量 |
| WSL 直接启动 Windows `cargo` 时 `collect2: ld returned 53` | Windows 子进程 PATH 被缩成 `C:\Windows\System32`，找不到 MSYS2 `ld/as` | 通过 `C:\msys64\usr\bin\bash.exe -lc` 进入 MSYS2，加入 `/c/msys64/ucrt64/bin` 后再执行 `cargo check` |
| 本地 `cargo check` 报 `src-tauri/icons/icon.ico` not found | 图标按约定不入库，CI 会动态生成 | 先 `npm ci`，再 `node scripts/gen-icon.mjs` 与 `node node_modules/@tauri-apps/cli/tauri.js icon scripts/app-icon.png -o src-tauri/icons` |
| 本地 `cargo check` / `cargo test` 报 `dlltool.exe`、`gcc.exe` 或 `windres` not found | Git Bash 登录环境未包含 MSYS2 ucrt64 与 Rust GNU 自包含链接工具 | 执行 `PATH="/c/msys64/ucrt64/bin:/c/Users/Admin/.rustup/toolchains/stable-x86_64-pc-windows-gnu/lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained:$HOME/.cargo/bin:$PATH" cargo ...`；`precheck` 在 Windows 分支也要显式设置该 PATH |
| `loop_driver.sh` 在原生 Git Bash 找不到 `/mnt/c/.../node.exe` | 驱动脚本原先只按 WSL 路径探测 Windows 工具 | 同时探测 `/mnt/c` 与 `/c`，`win_path` 优先 `wslpath`、其次 `cygpath`；MSYS2  manifest 路径按实际根路径归一化 |
| `cargo test` 编译成功后测试 exe 报 `STATUS_ENTRYPOINT_NOT_FOUND` | 测试可执行文件在主机运行时启动失败，尚未进入断言 | 用 `ldd` / `objdump` 排查 MSYS2 DLL；未解决前不得宣称单测通过，以 CI 构建结果作为独立验证 |
| `OpenClipboard(NULL)` 恢复文本时 `SetClipboardData` 失败 | `EmptyClipboard` 会把剪贴板 owner 置空 | 划词回退必须用本次采集期间存在的合法隐藏消息窗口打开/恢复剪贴板；复制后还需复核 owner 归属与全部格式，不能仅凭序列号变化 |
| `CloseClipboard` 后 `GetClipboardSequenceNumber` 递增 | Windows 可能在关窗时延迟提交 `CF_LOCALE` 等格式；关窗前读到的序列号会立即过期 | 快照和 Ctrl+C 结果都在关窗后复核稳定序列号，再用于并发检测/恢复；不要直接返回 guard 内的旧序列号 |
| `/api/health` 已通但 schema 重建期间查询全为 0 | 健康探针只证明 HTTP 就绪，不证明首个索引 commit 已可见 | 重建场景先轮询 `/api/stats` 的 `stats.total_docs > 0` 再跑回归；截图前用第二次实例触发单实例 `reveal` |
| `df` 显示 F 盘有空间，但 `mkdir` / Cargo 写 `F:/cargo-target` 报 `os error 2` | 当前 F 盘是 `scdfs` 只读挂载，不是可写构建盘 | 先用实际建目录验证可写性；不可写时复用现有 C 盘 target 做增量 `precheck`，需要重链接单测时先清理 C 盘空间 |

## 10. 提交与文档规范

- 提交信息：`vX.Y.Z: 一句话摘要`（中文，可附补充 commit：`vX.Y.Z 补充：…`）
- 文档与代码同 commit：功能变更同步更新 README（特性表/语法表/配置表）与 BASELINE（如涉及）
- 不提交：`target/`、`dist/`、二进制、日志、临时脚本
- 新经验（坑/教训）→ 追加到 §9；新约束 → 追加到 §8

## 11. 基线变更记录

| 日期 | 版本 | 变更 |
|---|---|---|
| 2026-11-25 | v0.8.40 | 基线首次建立；仓库迁移至 `C:\Users\Admin\Desktop\code\SerachDock`；清理历史误提交空文件（`7d`/`Nd`）；本地构建缓存改 Z 盘 |
| 2026-11-25 | v0.8.41 | mtime 写入 Tantivy 数值索引；时间窗在 TopDocs 前过滤；schema 升至 6 并自动重建旧索引 |
| 2026-11-25 | v0.8.42 | 接入可选划词自动搜索：独立快捷键、UIA 只读优先、受控 Ctrl+C 纯文本回退、字面量检索、不写历史、启动事件队列、连续触发合并、`request_id` 与竞态防护、工作区窗口约束；恢复增加稳定序列号复核、owner 校验和双副本重放；Windows 绑定统一为 0.62，补充前端 mock 回归 |
