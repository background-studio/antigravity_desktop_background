# Changelog

## 0.1.1

- 修复会话窗口 Markdown 代码块（含顶部语言栏与复制按钮）外层出现直角黑框与高不透明底色的问题：外层 `<pre>` 改为透明，仅内层圆角代码卡片跟随「卡片不透明度」单层着色。
- 修复 `build.rs` 在 Windows CI（CRLF 检出）下读取 `src/main/payload.ts` 时换行符不一致导致单测失败与修订哈希漂移的问题。

## 0.1.0

- 初始版本：适配 Antigravity Desktop（Windows / Electron 44 / Hub UI）。
- 实现 Background Studio 协议 2 纯 Rust 无界面 worker，支持图片与视频（64 MB 分块 Blob 上传）、热更新、自动托管与多窗口独立背景（`perWindowMedia`）。
- 进程扫描与重启自动排除 `sidecar_sdk`（`RunAsNode`）进程；页面过滤通过本机回环 URL 与 Hub DOM/bridge 探针双重校验。
- 样式注入保留 `:root` 原版变量，支持深浅主题实时跟随、主页/会话页独立强度及内置浏览器挖洞避让。
