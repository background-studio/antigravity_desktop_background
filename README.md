# Antigravity Background Studio

给 [Antigravity](https://antigravity.google) Windows 桌面应用（Electron 44 / Hub UI）加背景图与视频的 Background Studio 插件。

纯 Rust 无界面 worker，只由 Background Studio 壳启动；通过本机回环 CDP 注入 Hub 渲染页，**不修改** `Antigravity.exe`、`app.asar` 或任何安装文件，与代理补丁（`antigravity-proxy`）及汉化补丁（`antigravity-desktop-cn`）零冲突。

- 插件 id：`antigravity`，协议 `pluginProtocol: 2`，Pipe `\\.\pipe\background-studio-antigravity`
- 目标：`%LOCALAPPDATA%\Programs\Antigravity\Antigravity.exe`（已在 Antigravity 2.35.1 上验证）
- 运行状态：`%LOCALAPPDATA%\AntigravityBackgroundStudio\runtime.json`
- 能力：支持图片与视频（上限 64 MB，分块 Blob 注入）、热更新、自动接管、**多窗口独立背景（`perWindowMedia: true`）**

## 工作方式

1. 壳 `configure` 后，worker 等待 Antigravity；已在运行的普通 Antigravity 不会被静默关闭，点「立即接管」（`apply`）才会重启它。
2. 接管时只结束目标目录下的 Electron 浏览器进程树（自动跳过 `ELECTRON_RUN_AS_NODE` 跑的 `sidecar_sdk` 进程），以 `--remote-debugging-address=127.0.0.1 --remote-debugging-port=<9257 起>` 重新启动。
3. 注入 `src/main/payload.ts`：
   - 绝不修改 `:root` 上的原生 `--background` 等变量（避免触发自带 `preload.js` 的 `installHubSurfaceThemeAutoSync` 污染 `app_storage.json`），直接实时读取当前深/浅主题原色。
   - 外壳（`.agy-shell` / `.agy-inset-shell` / `.agy-sheet`）透明，顶栏、侧栏、主画布（`[data-main-content-body]`）、右面板（`[data-side-panel-body]`）与输入区各自单层着色，避免半透明层层叠加。
   - 保护 `.text-background` 反色文字、弹层/菜单（Base UI portal / listbox / `.surface-glass`）、Monaco 编辑器与终端可读性，并在内置浏览器挖洞（`data-has-browser-hole="true"`）时动态用 `clip-path` 避让。
4. `pause` 完整移除注入；`restore` 移除注入，并在 Antigravity 以调试端口运行时以官方普通参数重启它。

## 构建

```powershell
cd src-tauri
cargo fmt --all -- --check
cargo test
cargo build --release
```

产物 `src-tauri/target/release/antigravity-background-studio.exe`，安装到壳时改名为 `Antigravity Background Studio.exe`，与 `plugin.json` 放在 `<壳 dataRoot>\plugins\antigravity\<版本>\`。
