# knowledge-dock 知识悬浮搜索

uTools 风格的本地知识库检索工具：一条**常驻置顶**的极简搜索框，输入关键词即时全文检索你的 Markdown / TXT 笔记，`Enter` 直接打开源文件。全程离线，知识数据不出本机。

## 功能

- 极简形态：默认仅显示一条搜索框（圆角 + 阴影悬浮卡片），输入关键词时向下动态展开结果面板，清空即收起
- 全局快捷键：默认 `Alt+Space` 唤起/隐藏（可在设置中修改），无论焦点在哪个应用
- 智能隐藏：窗口失焦或再次 `Esc` 自动隐藏；「钉住模式」开启后始终显示且禁止拖动
- 本地全文检索（`.md` / `.markdown` / `.txt`），中文子串匹配 + 多词全含打分 + 标题加权，命中片段高亮
- `↑↓` 选择、`Enter` 打开文件、`Ctrl+Enter` 资源管理器定位、`Esc` 清空
- 系统托盘常驻，文件变更（notify 防抖）1 秒内自动重建索引
- 深浅色跟随系统；界面图标全部为内联 SVG
- 纯离线：检索全部在本机内存完成，零网络请求、零外部 API

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
本地开发 (cargo check 快速校验)
   → push 到 GitHub
   → Actions 流水线编译 (windows-latest / MSVC)
   → 下载 portable exe 本地运行测试
   → 发现问题回到本地开发迭代
   → 稳定后发 Release
```

> 本机 Rust 为 GNU 工具链，仅用于 `cargo check` 类型检查；真正的 exe 由
> GitHub Actions 的 MSVC 环境编译，保证产物一致性。

## 本地命令

```bash
npm install          # 安装 tauri cli
npm run tauri dev    # 本地调试（需 MSVC 工具链）
cargo check --manifest-path src-tauri/Cargo.toml   # 仅类型检查（GNU 可用）
cargo test  --manifest-path src-tauri/Cargo.toml   # 单元测试（需本机 sample-notes 数据）
npm run tauri build  # 本地打包（需 MSVC 工具链）
```

## CI 产物

每次 push 到 `main`，Actions 上传 artifact `knowledge-dock-windows`：

- `knowledge-dock-portable.exe` —— 免安装，双击即用
- `knowledge-dock_0.2.0_x64-setup.exe` —— NSIS 安装包

推送 `v*` 标签时自动创建 GitHub Release 并附以上产物；也可在 Actions 页手动触发并填版本号发版。

## 使用

1. 按 `Alt+Space`（或双击启动后的自动弹出）唤起搜索框
2. 首次使用：点击搜索条右侧「设置」→「选择文件夹…」指向你的笔记目录（如 Obsidian vault）
3. 输入关键词即搜，结果面板向下展开；清空即收起
4. `Esc` 清空输入，再按 `Esc`（或点击其他窗口）隐藏；`Alt+Space` 随时唤回

## 目录结构

```
ui/                  纯静态前端（withGlobalTauri，零打包器，图标全部内联 SVG）
src-tauri/src/
  main.rs            GUI 入口
  lib.rs             窗口形态/全局快捷键/托盘/IPC/文件监听/位置记忆
  indexer.rs         扫描 → 分块 → 打分检索（纯离线）
  config.rs          配置读写（%APPDATA%/com.local.knowledgedock/config.json）
  search_tests.rs    搜索引擎单元测试
scripts/gen-icon.mjs 图标生成（纯 Node 绘制 PNG）
.github/workflows/   GitHub Actions 编译流水线
```
