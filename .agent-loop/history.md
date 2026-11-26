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

### Iteration #2 最终审计 - 2026-11-25T01:36:49Z

- **审计提交**: `3ac5fe90ad44e7f22bd3572bd23658c1924da0e1`（仅文档补充，代码与 `13b71a9` 相同）。
- **CI 状态**: [run 45518007621](https://github.com/wakawakalululu/knowledge-dock-staging/actions/runs/45518007621) `completed/success`；artifact `knowledge-dock-windows-loop` ZIP SHA-256 `fc1ce4e8a7faad2a515604defddd32e13c01e1a9f2ae40d8fe98b23fdf6a74c7`，与本地文件一致。
- **本地门禁**: `loop_driver.sh precheck` 退出码 0，输出 `PRECHECK_OK 3ac5fe90ad44e7f22bd3572bd23658c1924da0e1`。
- **可复现实机结果**: 本次 portable 日志 `.local-logs/audit_20261125T092144Z.stderr.log` 记录同步完成 `docs=1124 blocks=83513`；`.local-logs/audit_20261125T092144Z.syntax.log` 为 `10/10 通过`，`.local-logs/audit_20261125T092144Z.quiz.log` 为 `9/9 通过`；`.local-logs/audit_20261125T092144Z.mtime.json` 显示全局时间窗命中 2 条、越界 0 条。
- **UI 与清理**: 单实例唤回后窗口句柄非零，600×76 搜索框截图 `.local-logs/audit_20261125T092144Z.ui.png` 已人工查看；PID 75528 已停止，18230 无 LISTEN，无残留 portable 进程。
- **单测阻塞**: 现有测试 exe 包含目标用例，但 Windows 退出码 `-1073741511 (0xC0000139)`；补充 DLL 搜索路径与同目录 `WebView2Loader.dll` 均未修复。`cargo test` 重建进一步被 C 盘 `os error 112` 阻塞；F 盘虽显示 20GB 空闲，但当前 `scdfs` 挂载不可写。单测未通过，不计为验证成功。
- **下一步行动**: 产品层收官完成；单测环境需在获得可写构建盘或清理 C 盘后继续排查，本轮不再推送第 4 次构建。

### Iteration #3 - 2026-11-25T06:57:00Z

- **Commit**: `9710558ba3214ff1dbf976b25c1a93159dec012d`（v0.8.42，基线 `7551f15`）。
- **改动要点**: 接入可选划词自动搜索；UIA 只读优先、Ctrl+C 纯文本快照/恢复回退、字面量检索、不写历史、独立快捷键与失败回滚；成功/失败共用有序事件队列，失败也显窗提示；前端补 IME、chips 保留、竞态与启动监听回归；Windows 绑定统一为 0.62；驱动脚本兼容原生 Git Bash 与 WSL 路径。
- **CI 状态**: [run 45619168747](https://github.com/wakawakalululu/knowledge-dock-staging/actions/runs/45619168747) `success`；产物 `knowledge-dock-windows-loop` 已下载并审计。
- **本地门禁**: `loop_driver.sh precheck` 退出码 0，输出 `PRECHECK_OK 7551f15d28e6f0f6a0cbfdf41b4d1403e27b8d92`；六处版本一致、前端语法、`SELECTION_UI_OK`、`cargo check --locked` 均通过。`cargo check --tests --locked` 通过，仅保留两个存量 warning。
- **配置诊断**: `import_diag` 成功解析旧配置与含划词字段的待导入配置，输出划词开关/快捷键/回退差异及 Ctrl+C 风险警告；未改写用户当前配置。
- **UI 证据**: 本机 Chrome 无头 + Tauri mock 输出 `UI_SMOKE_OK 默认关闭/设置保存/字面量/不写历史/失败提示`；默认折叠截图 `.local-logs/ui-selection-default.png` 无额外覆盖层，通用设置截图 `.local-logs/ui-selection-settings.png` 已人工查看。
- **单测阻塞**: Rust 测试 exe 仍为 `0xc0000139 / STATUS_ENTRYPOINT_NOT_FOUND`，未进入断言，不计为单测通过。
- **下一步行动**: 提交 v0.8.42，仅推 staging；等待 CI 后下载产物，执行真实 UIA/剪贴板/配置落盘与检索回归。

### Iteration #3 本地安全复核 - 2026-11-25T12:06:30Z

- **Commit**: `9710558ba3214ff1dbf976b25c1a93159dec012d`（工作树在基线 `7551f15` 之上，已提交）。
- **改动要点**: 剪贴板恢复改为清空前准备主副本与完整备份，并在首次重放失败时完整重试；快照与 Ctrl+C 结果均等待 `CloseClipboard` 后的稳定序列号，避免系统延迟提交格式造成旧序列号误判；连续触发合并为最新请求，结果带单调 `request_id`，窗口按显示器工作区约束完整显示。人工复核另修正序列号持续变化时的等待节流，以及 2000 字符边界可能留下的尾随空格。
- **本地门禁**: 删除临时探针源文件后，`loop_driver.sh precheck` 退出码 0（6 处版本、三种前端回归、`cargo check --locked`）；`cargo check --tests --locked` 通过，仅有两个存量 warning。
- **实机安全证据**: 临时 Rust 探针实际执行并通过归一化/2000 字符/UTF-16/格式白名单/恢复告警断言；当前 `selection.rs` 的真实快照恢复路径通过，原始纯文本逐字恢复；自定义格式、图片、文件、富文本均在 Ctrl+C 注入前拒绝且原内容保留，测试 finally 恢复原始剪贴板。
- **序列号诊断**: 实测 `CloseClipboard` 后序列号从 12555 增至 12558，证明必须使用关窗后的稳定值；修复后直接恢复测试通过。
- **实机缺口**: 当前工具桌面下原生 `EDIT` 即使获得前台/焦点，`EM_GETSEL` 仍为 `0,0`，UIA 提供方不暴露 TextPattern，真实键盘与直接 `EM_SETSEL` 均未形成选区；真实 Notepad 复测同样为 `NOTEPAD_SELECTION 0,0`、焦点匹配但 UIA 类型为 `Pane` 且无 TextPattern。因此 UIA 正向读取与 Ctrl+C 正向复制链路仍未验证通过，不能计为成功。
- **单测状态**: 测试目标仅完成编译；测试 exe 启动错误 `0xc0000139` 未重新证明解决，未报告 `cargo test` 全套通过。
- **下一步行动**: 完成独立代码审查后提交，仅推 staging；推送仍须以最新工作树 precheck 退出码 0 为前提。

### Iteration #3 最终实机审计 - 2026-11-25T14:54:00Z

- **审计提交**: `9710558ba3214ff1dbf976b25c1a93159dec012d`（v0.8.42）。
- **CI 状态**: [run 45619168747](https://github.com/wakawakalululu/knowledge-dock-staging/actions/runs/45619168747) `completed/success`；artifact `knowledge-dock-windows-loop` 已下载。
- **产物证据**: ZIP 19,085,366 字节，SHA-256 `3f6c0ebf89ddbe39633ffc7594cb2a6a5a6dd20b30ec9f20276d1bddb18ccb11`；portable 21,699,584 字节，SHA-256 `12bafb482d5f0107a8b3169fdde3540ebe01667f849326a74e5a17ae10ee5f5a`；NSIS 10,539,411 字节，SHA-256 `faa2cf629af0e55fb8425426ae9d5af877f74d5f4eeab4ac10c93076e78cf252`；两个 exe 的 File/Product Version 均为 0.8.42。
- **可复现实机结果**: portable 启动日志显示同步完成 `docs=1124 blocks=83513`；真实 HTTP API 答题回归 9/9、高级语法回归 10/10。默认配置直接读取为 `selection_search_enabled=false`、`selection_shortcut=Alt+Shift+Space`、`selection_clipboard_fallback=false`。
- **配置落盘**: 通过真实设置 UI 启用划词后直接读取 `config.json`，开关为 true、快捷键为 `Alt+Shift+Space`、回退为 false；回退复测前按仓库规范先执行 `cargo run --bin import_diag`，确认仅回退开关变化及风险警告后再注入；测试结束后再次诊断并恢复为开关/回退均 false，直接读盘断言通过。
- **UIA 正向链路**: 本机 Chrome 真实 textarea 选区经临时探针验证 `method=uia` 且文本逐字一致；v0.8.42 portable 的真实 `Alt+Shift+Space` 链路将完整字面量（含 `+must -exclude title: root:` 与中文库名）写入搜索框并渲染结果，搜索历史 6→6；截图 `.local-logs/ui-selection-production-shortcut.png` 已人工查看。
- **Ctrl+C 正向链路**: 标准 WinForms TextBox 临时探针通过真实 Ctrl+C，捕获完整字面量并逐字恢复原剪贴板；最终 portable 在无 TextPattern、复制时由原前台子窗口合法持有剪贴板的测试控件上完成完整回退，`FocusId=q`、查询逐字一致、`ClipboardRestored=true`、历史 6→6；截图 `.local-logs/ui-selection-production-fallback.png` 已人工查看。
- **安全拒绝链路**: 自定义格式、图片、文件、富文本均在 Ctrl+C 注入前拒绝且原内容保留；Chrome 并发剪贴板更新触发“检测到剪贴板被其他程序更新，已停止划词搜索并保留最新内容”，证明安全门不会覆盖新内容，测试 finally 恢复了原始剪贴板。
- **单测状态**: 补齐 MSYS2/Rust GNU 工具路径后，定向 `cargo test --locked --lib selection::tests -- --nocapture` 完成编译，但测试 exe 仍在启动阶段退出 `0xc0000139 / STATUS_ENTRYPOINT_NOT_FOUND`，未进入断言，因此不计为单测通过。
- **清理**: 最终 portable 进程数 0，18230 LISTEN 数 0；用户配置保持划词与回退默认关闭，搜索历史保持 6 条。
- **下一步行动**: 产品层收官完成；无未解决的产品缺陷。Rust 单测启动环境问题仍作为已知阻塞保留，不计入本次成功项。

### Iteration #4 - 2026-11-26T16:44:03Z

- **Commit**: `863cacc0504ad5c8ae46021d4ee117fa2707f7d1`（v0.8.43，基线 `101202a`）。
- **改动要点**: 新增统一 `safe_path` 边界校验；WebDAV 集合路径按完整段判断，origin 在解码前校验原始路径边界，下载/子目录请求逐段重编码，拒绝穿越、反斜杠、盘符、绝对路径、大小写别名及保留设备名（含 COM¹/²/³），下载与删除共用安全拼接；Git/WebDAV 来源名覆盖配置加载、保存、Profile、导入、健康检查、同步与删除；非法旧镜像元数据只告警跳过；Git 切分支使用来源命名空间外的唯一临时目录并在网络操作后复核边界；新增/同步/移除统一先锁后读配置；新增 `path_security` 定向测试并加入两个 Windows CI 工作流。
- **独立复核**: 多轮只读审查修复 origin 编码边界、add 候选配置原子性、WebDAV path 大小写、Git 临时目录碰撞、rename 失败保留、URL 二次解码、reparse 复核与同名 add/remove/sync 竞态后，最终复核 `PASS`。
- **本地门禁**: `node --check ui/app.js`、`VERSION_OK 0.8.43 (6 locations)`、npm 锁文件校验、三种划词前端回归、`cargo check --tests --locked` 与 `loop_driver.sh precheck` 均通过；仅保留两个存量 warning。
- **定向测试**: `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib path_security -- --nocapture` 编译成功，但测试 exe 仍在启动阶段退出 `0xc0000139 / STATUS_ENTRYPOINT_NOT_FOUND`，未进入断言，因此不计为本地测试通过。
- **下一步行动**: 以 Windows CI 的定向测试执行结果、产物审计和隔离 mock WebDAV 实机回归作为本轮闭环证据。

### Iteration #4 最终实机审计 - 2026-11-26T20:49:00Z

- **代码提交**: `863cacc0504ad5c8ae46021d4ee117fa2707f7d1`；仅推送 `staging`，未推 `origin`，未 amend/force push。
- **CI 状态**: [run 45875616869](https://github.com/wakawakalululu/knowledge-dock-staging/actions/runs/45875616869) `completed/success`，head SHA 与代码提交完全一致；`Run path security tests` 实际执行并完成，结果为 `18 passed; 0 failed; 0 ignored`。
- **产物证据**: artifact ZIP 19,128,995 字节，SHA-256 `551a332fc37d5ca5553e862f34f8510a8212f4a8e4088d486e4ce1aaeabc5b57`；portable 21,764,608 字节，SHA-256 `08c2d3e61c499856c97d027ad571285697302f5eea2fea7d3387a2c237151271`；NSIS 10,558,651 字节，SHA-256 `ecab556ea8fa6decd6745dda09a20dd94b5c7d1097e57b25a1a3a5838707aea2`；两个 exe 的 File/Product Version 均为 0.8.43。
- **WebDAV 实机**: 本地 mock 返回正常 `docs/ok.md` 时下载、元数据、知识根注册与检索通过；`..\`、编码 `..\`、盘符路径、集合前缀兄弟路径均在 GET 前拒绝，目录外哨兵未被创建、覆盖或删除；删除测试源只移除自身镜像与元数据。
- **配置与导入实机**: `import_diag` 对非法来源非零退出、对合法候选零退出；UI 非法导入明确报错且真实 `config.json` SHA-256 不变；临时隔离标识的 debug 探针直接调用 `save_config`，Git/WebDAV 非法名称均被拒绝，`STATE_UNCHANGED=true` 且配置哈希不变；合法 UI 导入后直接读盘并核对镜像、元数据与检索，再恢复导入前配置。
- **回归实机**: 答题 9/9、高级语法 10/10；真实 Chrome UIA 选区与无 TextPattern 的 Ctrl+C 回退均将完整字面量写入搜索框，`FocusId=q`、历史 6→6，回退测试 `ClipboardRestored=true`。
- **清理**: 最终 portable/mock/测试宿主进程数 0，18230/18765 LISTEN 数 0；临时语法根、导入夹具、测试镜像与隔离应用数据已删除；用户配置恢复为原始 SHA-256 `C23723E31910B6739260D3D1D206A7E7CF132D216D505B9C7DDF0D25E8DC29EE`，划词与回退保持默认关闭。
- **单测状态**: 本地 GNU 测试 exe 的 `0xc0000139 / STATUS_ENTRYPOINT_NOT_FOUND` 仍作为独立环境阻塞保留；本轮测试通过证据来自 Windows CI 的独立执行。
- **下一步行动**: 产品层闭环，无未解决的产品缺陷；补充本文档后以新的文档提交推 staging，不修改已验证代码提交。
