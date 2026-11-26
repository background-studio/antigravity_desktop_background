# Antigravity 插件图标素材

- `background-original.png`：用户提供的 106×106 方形原图，未裁切、未调色。
- `background-source.png`：原图副本。
- `background-40.png`：原图缩放后套公共圆角模板，alpha 乘 0.4；RGB 不调色。
- `antigravity-official.svg`：Antigravity 客户端官方几何路径。
- `prepare-mark.mjs`：保留官方几何路径，采用冰白渐变与品牌蓝细描边生成 `mark-generated.png`。
- `frame.svg`：品牌蓝色圆角边框。

```powershell
npm install
node prepare-mark.mjs
npm run prepare-background -- "<方形底图路径>"
npm run compose -- ../../../background-studio
npm run verify -- ../../../background-studio
```
