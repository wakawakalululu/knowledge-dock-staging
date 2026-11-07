# knowledge-dock 知识悬浮搜索

uTools 风格的本地知识库检索工具：一条**常驻置顶**的极简搜索框，输入关键词即时检索你的本地知识（法律法规 / 官方文档 / 问答库 / 笔记），`Enter` 直接打开源文件。全程离线，知识数据不出本机。

## 检索引擎（v0.3.0，无 AI、纯离线）

- **差异化切块**：法规按`第X条`正则切分并跟踪`编/章/节`层级；Markdown 按标题树切块（pulldown-cmark，正确处理代码围栏）；问答按`问:/答:`成对识别；纯文本按空行分段。统一收敛为 `DocumentChunk { doc_type, breadcrumbs, text, line }`
- **Tantivy BM25**：词频/逆文档频率打分，低频关键词（如"辞退""赔偿"）权重指数级放大，把真正的答案顶到第一位；标题权重 2.0、面包屑 1.5
- **jieba 中文分词**：自定义 Tokenizer 接入 Tantivy，索引与查询同源分词
- **同义词查询改写**：内嵌静态词典（`src-tauri/assets/synonyms.json`，可在配置目录 `synonyms.json` 覆盖扩展）。输入"辞职"自动扩展"离职 OR 辞退 OR 解雇"，零网络、零模型
- **上下文就地展开**：结果卡片上的展开按钮，直接读取源文件前 10 行 + 后 20 行，不开源文件即可阅读完整上下文
- **持久化索引**：Tantivy 索引落盘 `%APPDATA%/com.local.knowledgedock/index`，启动 mmap 秒开；文件变更按 mtime 差异增量更新（delete_by_term + add），不整库重建

## 功能

- 极简形态：默认仅显示一条搜索框，输入关键词时向下动态展开结果面板，清空即收起
- 全局快捷键：默认 `Alt+Space` 唤起/隐藏（可在设置中修改）
- 智能隐藏：窗口失焦或再次 `Esc` 自动隐藏；「钉住模式」开启后始终显示且禁止拖动
- 结果展示：类型徽标（法律/文档/问答/笔记）+ 面包屑导航（如 劳动法 > 第四章 > 第四十四条）+ 命中高亮（含同义词命中）
- `↑↓` 选择、`Enter` 打开文件、`Ctrl+Enter` 资源管理器定位、`Esc` 清空
- 系统托盘常驻；深浅色跟随系统；界面图标全部为内联 SVG

## 配置项（config.json，设置面板可视化修改）

| 字段 | 默认 | 说明 |
|---|---|---|
| `knowledge_dir` | 空 | 知识库根目录 |
| `x` / `y` | 居中 | 窗口位置（拖动自动记忆） |
| `width` | 600 | 悬浮窗宽度（320-1600） |
| `pin_mode` | false | 钉住模式：始终显示、禁用拖动、Esc 不隐藏 |
| `shortcut` | Alt+Space | 全局快捷键 |

## 开发与推进路线

```
本地开发 (cargo check 快速校验 + tooling/search-verify 检索验证)
   → push 到 GitHub
   → Actions 流水线编译 (windows-latest / MSVC)
   → 下载 portable exe 本地运行测试
   → 发现问题回到本地开发迭代
   → 稳定后发 Release
```

> 本机 Rust 为 GNU 工具链，仅用于类型检查与单元验证；真正的 exe 由
> GitHub Actions 的 MSVC 环境编译，保证产物一致性。

## 本地命令

```bash
npm install          # 安装 tauri cli
npm run tauri dev    # 本地调试（需 MSVC 工具链）
cargo check --manifest-path src-tauri/Cargo.toml   # 类型检查（GNU 可用）
cargo test  --manifest-path src-tauri/Cargo.toml   # 单元测试（自建临时语料，CI 可跑）
cd tooling/search-verify && cargo run              # 检索引擎端到端验证（对 sample-notes）
```

> 本地无 C 编译器时的构建依赖说明：jieba 词典压缩需要 zstd，已通过
> `.cargo/zstd-pkg`（MSYS2 预编译库）+ 本地 pkg-config 应答器解决，
> 编译时需设置 `ZSTD_SYS_USE_PKG_CONFIG=1`。

## CI 产物

每次 push 到 `main`，Actions 上传 artifact `knowledge-dock-windows`：

- `knowledge-dock-portable.exe` —— 免安装，双击即用
- `knowledge-dock_0.3.0_x64-setup.exe` —— NSIS 安装包

推送 `v*` 标签时自动创建 GitHub Release 并附以上产物；也可在 Actions 页手动触发并填版本号发版。

## 使用

1. 按 `Alt+Space` 唤起搜索框
2. 首次使用：点击搜索条右侧「设置」→「选择文件夹…」指向你的知识库（法规 TXT、Markdown 手册、问答库均可混放）
3. 输入关键词即搜，BM25 把最像"答案"的知识块排最前；点卡片右下的展开按钮阅读上下文
4. `Esc` 清空输入，再按 `Esc`（或点击其他窗口）隐藏；`Alt+Space` 随时唤回

## 目录结构

```
ui/                     纯静态前端（withGlobalTauri，零打包器，图标全部内联 SVG）
src-tauri/src/
  main.rs               GUI 入口
  lib.rs                窗口形态/全局快捷键/托盘/IPC/文件监听/位置记忆
  indexer.rs            差异化切块（法规/手册/问答/笔记 → DocumentChunk）
  engine.rs             Tantivy BM25 + jieba + 持久化索引 + 同义词改写
  config.rs             配置读写
  search_tests.rs       切块与检索测试（临时语料，CI 可跑）
src-tauri/assets/       内嵌同义词词典
scripts/gen-icon.mjs    图标生成（纯 Node 绘制 PNG）
tooling/search-verify   检索引擎端到端验证工具
.github/workflows/      GitHub Actions 编译流水线
```
