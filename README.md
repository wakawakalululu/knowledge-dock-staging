# knowledge-dock 知识悬浮搜索

uTools 风格的本地知识库检索工具：一个**常驻置顶**的小悬浮窗，输入关键词即时全文检索你的 Markdown / TXT 笔记，`Enter` 直接打开源文件。

![tech](https://img.shields.io/badge/Tauri-2-blue) ![platform](https://img.shields.io/badge/platform-Windows-lightgrey)

## 功能

- 无边框圆角悬浮窗，始终置顶、可拖动、重启后位置记忆
- 本地文件夹全文检索（`.md` / `.markdown` / `.txt`），中文子串匹配 + 多词全含打分，命中片段高亮
- `↑↓` 选择、`Enter` 打开文件、`Ctrl+Enter` 资源管理器定位、`Esc` 清空
- 系统托盘常驻：关闭窗口即隐藏，托盘可恢复/退出
- 文件监听（notify 防抖）：知识库变更 1 秒内自动重建索引
- 深浅色跟随系统；界面图标全部为内联 SVG

## 开发与推进路线

```
本地开发 (cargo check 快速校验)
   → push 到 GitHub
   → Actions 流水线编译 (windows-latest / MSVC)
   → 下载 portable exe 本地运行测试
   → 发现问题回到本地开发迭代
   → 稳定后打 tag v* 自动发 Release
```

> 本机 Rust 为 GNU 工具链，仅用于 `cargo check` 类型检查；真正的 exe 由
> GitHub Actions 的 MSVC 环境编译，保证产物一致性。

## 本地命令

```bash
npm install          # 安装 tauri cli
npm run tauri dev    # 本地调试（需 MSVC 工具链）
cargo check --manifest-path src-tauri/Cargo.toml   # 仅类型检查（GNU 可用）
npm run tauri build  # 本地打包（需 MSVC 工具链）
```

## CI 产物

每次 push 到 `main`，Actions 上传 artifact `knowledge-dock-windows`：

- `knowledge-dock-portable.exe` —— 免安装，双击即用
- `knowledge-dock_0.1.0_x64-setup.exe` —— NSIS 安装包

推送 `v*` 标签时自动创建 GitHub Release 并附以上产物。

## 使用

1. 启动后点击标题栏的「设置」按钮 → 「选择文件夹…」指向你的笔记目录（如 Obsidian vault）
2. 输入关键词即搜；首个扫描完成后文件变更自动同步
3. 关闭窗口 = 隐藏到托盘；托盘右键退出

## 目录结构

```
ui/                  纯静态前端（withGlobalTauri，零打包器）
src-tauri/src/
  main.rs            窗口/托盘/IPC/文件监听/位置记忆
  indexer.rs         扫描 → 分块 → 打分检索
  config.rs          配置读写（%APPDATA%/com.local.knowledgedock/config.json）
scripts/gen-icon.mjs 图标生成（纯 Node 绘制 PNG）
.github/workflows/   GitHub Actions 编译流水线
```
