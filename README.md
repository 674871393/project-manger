# 前端项目管理器

本地桌面工具，把分散在各处的前端项目收进一张列表：一键启动 / 停止 / 重启，实时日志、运行地址、当前分支一目了然。

基于 Tauri 2 + React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui 构建，目标平台 Windows 11。

## 功能

- **项目管理**：添加 / 编辑 / 删除项目（仅移除记录，不动磁盘文件）；选择文件夹后自动读取 `package.json`，识别项目名、包管理器（按锁文件推断）、启动脚本、框架类型（vite / next / nuxt / webpack，以斜置水印展示）；支持扫描目录批量导入
- **进程控制**：一键和批量启停、重启，区分「进程已启动」和「服务已就绪」。支持依赖预检、安装、取消安装。
- **启动方案**：保存经常一起开发的项目组合，一键启停，下次可直接选中最近使用的方案。
- **实时日志**：抽屉式 stdout / stderr，支持跟随滚动、搜索、复制和清空。保留上次会话的尾部日志。
- **运行地址**：从本次开发日志或手动端口识别地址，校验监听进程归属并探测 TCP。保留 HTTPS 和路径，允许访问时显示局域网地址。
- **项目体检与端口总览**：检查项目路径、Node、包管理器、脚本和依赖。支持反查 TCP 监听端口、进程名和 PID。Git 分支可从仓库子目录和 worktree 读取。
- **本地发布**：检查 Git 工作区，使用指定 Node 子进程构建，验证本轮产物后写入 ZIP。产物目录可配置，默认 dist。最近 100 次发布及对应日志可在「发布历史」里查看。
- **系统集成**：托盘最小化保活（左键恢复，右键菜单）；正式版支持开机自启；应用退出自动清理所有项目进程，异常退出后下次启动检测并清理残留孤儿进程
- **搜索过滤**：按项目名 / 路径即时过滤

## 环境要求

- [Node.js](https://nodejs.org/) ≥ 20 + [pnpm](https://pnpm.io/)
- [Rust](https://www.rust-lang.org/)（含 MSVC 工具链）
- Windows 10/11（自带 WebView2）

## 常用命令

```bash
pnpm install        # 安装依赖
pnpm test           # 前端回归测试
cargo test --manifest-path src-tauri/Cargo.toml --locked  # Rust 回归测试
pnpm tauri:dev      # 开发调试（前端热更新 + Rust 增量编译）
pnpm tauri:build    # 完整打包：Release exe + NSIS/MSI 安装包
pnpm tauri:build:exe # 只编译 Release 绿色 exe，不生成安装包（更快）
```

打包产物：

| 产物 | 路径 |
|---|---|
| 绿色单文件 exe | `src-tauri/target/release/project-manager.exe` |
| NSIS 安装包 | `src-tauri/target/release/bundle/nsis/` |

## 项目结构

```
├── docs/PRD.md              # 产品需求文档
├── src/                     # 前端（React）
│   ├── components/          # 业务组件 + shadcn/ui
│   ├── stores/              # zustand：projects（持久化）/ runtime（进程状态与日志）
│   └── types.ts
├── src-tauri/               # 后端（Rust / Tauri 2）
│   ├── src/process_manager.rs  # 进程生命周期：spawn / 进程树 kill / 孤儿清理
│   ├── src/commands.rs      # invoke 命令：项目持久化、package.json 解析、分支读取
│   └── src/settings.rs      # 全局设置（托盘最小化、开机自启）
└── public/logo.png          # 应用图标源（经 tauri icon 生成全套图标）
```

数据全部保存在本地 `appDataDir`（`%APPDATA%/local.fpm/`）：`projects.json` 项目列表、`settings.json` 设置、`running-pids.json` 孤儿进程清理凭据。启动方案写入 `launch-plans.json`，发布记录和每次日志写入 `release-history/`。无任何网络上报。

## 开发说明

- 开机自启请在正式构建的应用里设置。`pnpm tauri:dev` 依赖本地开发服务器，开发模式下自启开关不可用。
- 前后端通信：`invoke` 命令（请求-响应）+ `emit`/`listen` 事件（日志流、进程退出推送）
- 修改 Rust 代码或图标后，`tauri dev` 会自动重编译；仅改图标偶尔不触发重编译，touch 一下 `src-tauri/src/` 下任意 `.rs` 文件即可
- 换应用图标：准备正方形 PNG 后执行 `pnpm tauri icon <图片路径>`

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
