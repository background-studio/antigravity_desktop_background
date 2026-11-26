// 本文件是 Antigravity 渲染页注入脚本的唯一来源：src-tauri/build.rs 在编译期
// 抽取 BACKGROUND_CSS / REVIEW_SHADOW_CSS / 运行时模板（两空格缩进的 `return String.raw`）。
// payload.rs 会把 CSS 一起摘要进修订号，改 CSS 后热更新一定会重新注入。
import { createHash } from "node:crypto";

type MediaKind = "image" | "video";
interface DisplaySettings {
  fit: "cover" | "contain" | "fill" | "tile";
  positionX: number;
  positionY: number;
  opacity: number;
  blur: number;
  scale: number;
  overlayColor: string;
  overlayOpacity: number;
  homeIntensity: number;
  taskIntensity: number;
  sidebarOpacity: number;
  surfaceOpacity: number;
  cardOpacity: number;
  composerOpacity: number;
  menuOpacity: number;
  terminalOpacity: number;
  enabledOnHome: boolean;
  enabledOnTasks: boolean;
  videoMuted: boolean;
  videoPlaybackRate: number;
}

/*
 * Antigravity Desktop（Electron 44 / Hub UI）的配色令牌直接内联声明在 :root（html[style]）上：
 *   --background / --content / --sidebar / --shell / --card / --glass /
 *   --onyx-frame / --onyx-well / --onyx-raised / --onyx-selected / --onyx-key 等，
 *   Tailwind v4 通过 --color-background / --color-sidebar / --color-card 等映射到工具类。
 *
 * 关键约束：
 *   1. Antigravity 自带的 preload.js（installHubSurfaceThemeAutoSync）监听 :root 的 --background，
 *      并将其写回 app_storage.json（hubSurfaceBackgroundColor）。因此绝不修改 :root 上的原版令牌，
 *      只给 :root 写入 --agbg-* 自定义变量，所有覆盖均从 #root 与弹层节点向下生效；
 *      这样也让我们能直接从 getComputedStyle(document.documentElement) 读到当前深/浅主题的原始实色。
 *   2. 反色按钮（如 .bg-foreground.text-background）依赖 --background/--color-background 作为文字色，
 *      必须显式把 .text-background 锁定为 var(--agbg-solid-background)，防止按钮图标隐身。
 *   3. 布局外壳（.agy-shell / .agy-inset-shell / .agy-sheet）设透明，雾只在顶栏、侧栏、
 *      主画布（[data-main-content-body]）、右面板（[data-side-panel-body]）和输入区各着色一次，
 *      避免多层半透明叠加越叠越黑。
 *   4. 内置浏览器打开时会触发 body[data-has-browser-hole="true"]，动态用 clip-path 抠出挖洞区域，
 *      防止背景层挡住底层 WebContentsView。
 */
const BACKGROUND_CSS = String.raw`
html.antigravity-background-active,
html.antigravity-background-active body,
html.antigravity-background-active #root {
  background: transparent !important;
  background-color: transparent !important;
}

html.antigravity-background-active body {
  isolation: isolate;
}

/* 媒体层：固定铺满窗口，落在 body 隔离层叠上下文最底层，不干扰任何交互。 */
#antigravity-background-layer {
  position: fixed;
  inset: 0;
  z-index: -1;
  overflow: hidden;
  pointer-events: none;
  opacity: calc(var(--agbg-opacity) * var(--agbg-route-intensity));
  background-color: transparent;
  transition: opacity 220ms ease;
}

#antigravity-background-media,
#antigravity-background-tile {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  transform: scale(var(--agbg-scale));
  filter: blur(var(--agbg-blur));
  transform-origin: center center;
}

#antigravity-background-media {
  display: block;
  object-fit: var(--agbg-fit);
  object-position: var(--agbg-position-x) var(--agbg-position-y);
}

#antigravity-background-tile {
  display: none;
  background-image: var(--agbg-media-url);
  background-repeat: repeat;
  background-position: var(--agbg-position-x) var(--agbg-position-y);
  background-size: auto;
}

html.antigravity-background-fit-tile #antigravity-background-media { display: none; }
html.antigravity-background-fit-tile #antigravity-background-tile { display: block; }

#antigravity-background-overlay {
  position: absolute;
  inset: 0;
  background: var(--agbg-overlay-color);
  opacity: var(--agbg-overlay-opacity);
}

html.antigravity-background-home { --agbg-route-intensity: var(--agbg-home-intensity); }
html.antigravity-background-task { --agbg-route-intensity: var(--agbg-task-intensity); }
html.antigravity-background-home.antigravity-background-home-disabled,
html.antigravity-background-task.antigravity-background-task-disabled { --agbg-route-intensity: 0; }

/* 在 #root 作用域内将页面/外壳底色令牌透出，卡片与表面令牌按不透明度混合；绝不污染 :root。 */
html.antigravity-background-active #root {
  --background: transparent !important;
  --shell: transparent !important;
  --shell-canvas: transparent !important;
  --content: transparent !important;
  --color-background: transparent !important;
  --color-content: transparent !important;
  --card: color-mix(in srgb, var(--agbg-solid-card) calc(var(--agbg-card-opacity) * 100%), transparent) !important;
  --color-card: color-mix(in srgb, var(--agbg-solid-card) calc(var(--agbg-card-opacity) * 100%), transparent) !important;
  --glass: color-mix(in srgb, var(--agbg-solid-glass) calc(var(--agbg-card-opacity) * 100%), transparent) !important;
  --onyx-well: color-mix(in srgb, var(--agbg-solid-onyx-well) calc(var(--agbg-card-opacity) * 100%), transparent) !important;
  --onyx-raised: color-mix(in srgb, var(--agbg-solid-onyx-raised) calc(var(--agbg-card-opacity) * 100%), transparent) !important;
  --onyx-selected: color-mix(in srgb, var(--agbg-solid-onyx-selected) calc(var(--agbg-highlight-opacity) * 100%), transparent) !important;
  --onyx-key: color-mix(in srgb, var(--agbg-solid-onyx-key) calc(var(--agbg-highlight-opacity) * 100%), transparent) !important;
}

/* 反色文字（如语音按钮 .bg-foreground.text-background）保底用原主题实色，防止透明后隐身。 */
html.antigravity-background-active .text-background {
  color: var(--agbg-solid-background) !important;
}

/* 外壳层全部透出，只留微弱轮廓，不在外框重复叠底色。 */
html.antigravity-background-active .agy-shell,
html.antigravity-background-active .agy-inset-shell,
html.antigravity-background-active .agy-sheet {
  background-color: transparent !important;
  --shell-canvas: transparent !important;
}

/* 顶部标题栏：外层条着色一次，内部 title-menu-bar 透明避免双重叠加。 */
html.antigravity-background-active .agy-shell > .shrink-0.flex.bg-sidebar {
  background-color: color-mix(in srgb, var(--agbg-solid-sidebar) calc(var(--agbg-sidebar-opacity) * 100%), transparent) !important;
}
html.antigravity-background-active [data-testid="title-menu-bar"] {
  background-color: transparent !important;
}

/* 左侧边栏：只在 [role="navigation"] 着色一次，内部 sticky 分组头与项目按钮透明。 */
html.antigravity-background-active [role="navigation"].bg-sidebar,
html.antigravity-background-active [role="navigation"] {
  --sidebar: transparent !important;
  --color-sidebar: transparent !important;
  background-color: color-mix(in srgb, var(--agbg-solid-sidebar) calc(var(--agbg-sidebar-opacity) * 100%), transparent) !important;
}
html.antigravity-background-active [role="navigation"] .bg-sidebar,
html.antigravity-background-active [role="navigation"] [data-project-card="true"] {
  background-color: transparent !important;
}

/* 主画布与右侧辅助面板：单层着色，保留原生圆角与 ::after 1px 描边；内置浏览器挖洞宿主必须保持透明。 */
html.antigravity-background-active [data-main-content-body="true"]:not([data-has-browser-hole="true"]),
html.antigravity-background-active [data-side-panel-body="true"]:not([data-has-browser-hole="true"]) {
  background-color: color-mix(in srgb, var(--agbg-solid-content) calc(var(--agbg-surface-opacity) * 100%), transparent) !important;
}
html.antigravity-background-active body[data-has-browser-hole="true"] :is([data-main-content-body="true"], [data-side-panel-body="true"])[data-has-browser-hole="true"],
html.antigravity-background-active body[data-has-browser-hole="true"] [data-browser-hole]:not([data-browser-hole="false"]) {
  background-color: transparent !important;
  background-image: none !important;
}

/* 会话滚动区底部/顶部用 var(--background) 刷的渐隐条，在半透明背景上会变成突兀的黑带。 */
html.antigravity-background-active [data-main-content-body="true"] .absolute.pointer-events-none:is(.bottom-0, .top-0)[style*="gradient"] {
  background-image: none !important;
  background: none !important;
}

/* 主页顶部极光保留微弱光晕，不影响背景透出。 */
html.antigravity-background-active body[data-agy-home-glow] .agy-sheet:has(.agy-home) {
  background-color: transparent !important;
}

/* 输入框组合：外框去掉 4px 实色外圈，内层保留原生毛玻璃并按 composerOpacity 着色。 */
html.antigravity-background-active .surface-composer {
  background-color: color-mix(in srgb, var(--agbg-solid-onyx-frame) calc(var(--agbg-composer-opacity) * 65%), transparent) !important;
  box-shadow:
    inset 0 1px 2px rgb(0 0 0 / 0.16),
    0 0 0 1px var(--glass-border, rgb(255 255 255 / 0.085)),
    0 14px 32px -12px rgb(0 0 0 / 0.32) !important;
}
html.antigravity-background-active .surface-composer > .surface-composer-card {
  background-color: color-mix(in srgb, var(--agbg-solid-onyx-raised) calc(var(--agbg-composer-opacity) * 100%), transparent) !important;
}

/* Monaco 编辑器与终端：使用 terminalOpacity 保证代码与命令输出清晰易读。 */
html.antigravity-background-active #root :is(
  .monaco-editor,
  .monaco-editor-background,
  .monaco-editor .margin,
  .xterm,
  .xterm-viewport
) {
  background-color: color-mix(in srgb, var(--agbg-solid-content) calc(var(--agbg-terminal-opacity) * 100%), transparent) !important;
}

/*
 * Markdown 代码块：Antigravity 的 <pre class="notranslate"> 只是外层语义容器（无圆角且包裹 my-2 边距），
 * 真正带圆角与复制栏的卡片是里面的 div.rounded-xl（含 .code-block）。
 * 因此外层 <pre> 必须保持透明，仅对内层圆角卡片按 cardOpacity 单层着色，避免出现直角黑框与双层不透明底。
 */
html.antigravity-background-active #root pre:has(.code-block),
html.antigravity-background-active #root pre:has(> .rounded-xl) {
  background-color: transparent !important;
}
html.antigravity-background-active #root pre > :is(.rounded-xl, div:has(.code-block)),
html.antigravity-background-active #root pre:not(:has(.code-block)):not(:has(> .rounded-xl)) {
  background-color: color-mix(in srgb, var(--agbg-solid-card) calc(var(--agbg-card-opacity) * 100%), transparent) !important;
}

/*
 * 浮层 / 菜单 / 对话框 / 命令面板（Base UI Portal、listbox、surface-glass、fastpick）：
 * 使用 menuOpacity 着色，防止浮在正文上方时上下文字重叠难读。
 */
html.antigravity-background-active body > :not(#root):not(#antigravity-background-layer):not(#ag-ctx-card-portal):not(script):not(style),
html.antigravity-background-active #root :is(
  .surface-glass,
  .agy-fastpick,
  .quick-input-widget,
  [role="dialog"],
  [role="menu"]
) {
  --background: color-mix(in srgb, var(--agbg-solid-background) calc(var(--agbg-menu-opacity) * 100%), transparent) !important;
  --color-background: color-mix(in srgb, var(--agbg-solid-background) calc(var(--agbg-menu-opacity) * 100%), transparent) !important;
  --sidebar: color-mix(in srgb, var(--agbg-solid-sidebar) calc(var(--agbg-menu-opacity) * 100%), transparent) !important;
  --color-sidebar: color-mix(in srgb, var(--agbg-solid-sidebar) calc(var(--agbg-menu-opacity) * 100%), transparent) !important;
  --card: color-mix(in srgb, var(--agbg-solid-card) calc(var(--agbg-menu-opacity) * 100%), transparent) !important;
  --color-card: color-mix(in srgb, var(--agbg-solid-card) calc(var(--agbg-menu-opacity) * 100%), transparent) !important;
  --glass: color-mix(in srgb, var(--agbg-solid-glass) calc(var(--agbg-menu-opacity) * 100%), transparent) !important;
  --onyx-frame: color-mix(in srgb, var(--agbg-solid-onyx-frame) calc(var(--agbg-menu-opacity) * 100%), transparent) !important;
  --onyx-raised: color-mix(in srgb, var(--agbg-solid-onyx-raised) calc(var(--agbg-menu-opacity) * 100%), transparent) !important;
}

@media (prefers-reduced-motion: reduce) {
  #antigravity-background-layer { transition: none; }
}
`;

const REVIEW_SHADOW_STYLE_ID = "antigravity-background-review-shadow-style";
const REVIEW_SHADOW_CSS = String.raw`
/* Antigravity 主界面本身不使用 Shadow DOM（仅汉化插件的监控徽标用 Shadow DOM，不覆盖）。 */
:host { background-color: transparent !important; }
`;

export interface PayloadInput {
  mediaUrl: string;
  mediaKind: MediaKind;
  display: DisplaySettings;
  revision: string;
}

export function buildRendererPayload(input: PayloadInput) {
  const revision = createHash("sha256")
    .update(input.revision)
    .update(BACKGROUND_CSS)
    .update(REVIEW_SHADOW_CSS)
    .digest("hex");
  const serialized = JSON.stringify({ ...input, revision }).replace(/</g, "\\u003c");
  const css = JSON.stringify(BACKGROUND_CSS);
  const reviewShadowCss = JSON.stringify(REVIEW_SHADOW_CSS);
  const reviewShadowStyleId = JSON.stringify(REVIEW_SHADOW_STYLE_ID);
  return String.raw`((config, cssText, reviewShadowCssText, reviewShadowStyleId) => {
    const STATE = "__ANTIGRAVITY_BACKGROUND_STUDIO__";
    const STYLE_ID = "antigravity-background-style";
    const EARLY_STYLE_ID = "antigravity-background-early-transparency";
    const LAYER_ID = "antigravity-background-layer";
    const ROOT_CLASSES = [
      "antigravity-background-active", "antigravity-background-home", "antigravity-background-task",
      "antigravity-background-home-disabled", "antigravity-background-task-disabled",
      "antigravity-background-fit-tile"
    ];
    const SOLID_TOKENS = {
      "--agbg-solid-background": "--background",
      "--agbg-solid-content": "--content",
      "--agbg-solid-sidebar": "--sidebar",
      "--agbg-solid-shell": "--shell",
      "--agbg-solid-card": "--card",
      "--agbg-solid-glass": "--glass",
      "--agbg-solid-onyx-frame": "--onyx-frame",
      "--agbg-solid-onyx-well": "--onyx-well",
      "--agbg-solid-onyx-raised": "--onyx-raised",
      "--agbg-solid-onyx-selected": "--onyx-selected",
      "--agbg-solid-onyx-key": "--onyx-key"
    };
    const ROOT_PROPERTIES = [
      "--agbg-opacity", "--agbg-blur", "--agbg-scale", "--agbg-fit",
      "--agbg-position-x", "--agbg-position-y", "--agbg-overlay-color",
      "--agbg-overlay-opacity", "--agbg-home-intensity", "--agbg-task-intensity",
      "--agbg-sidebar-opacity", "--agbg-surface-opacity", "--agbg-card-opacity",
      "--agbg-highlight-opacity", "--agbg-composer-opacity", "--agbg-menu-opacity",
      "--agbg-terminal-opacity", "--agbg-media-url",
      ...Object.keys(SOLID_TOKENS)
    ];

    const previous = window[STATE];
    if (previous?.cleanup) previous.cleanup();
    void reviewShadowCssText;
    void reviewShadowStyleId;
    let scheduled = 0;

    const mediaUrl = (() => {
      const source = String(config.mediaUrl || "");
      const comma = source.indexOf(",");
      if (!source.startsWith("data:") || comma < 0) return source;
      const binary = atob(source.slice(comma + 1));
      const bytes = new Uint8Array(binary.length);
      for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
      const mime = /^data:([^;,]+)/.exec(source)?.[1] || "application/octet-stream";
      return URL.createObjectURL(new Blob([bytes], { type: mime }));
    })();
    if (!mediaUrl) throw new Error("背景媒体尚未就绪");

    const cleanup = () => {
      const state = window[STATE];
      state?.observer?.disconnect();
      state?.holeResizeObserver?.disconnect();
      if (state?.timer) clearInterval(state.timer);
      if (state?.onResize) window.removeEventListener("resize", state.onResize);
      if (state?.onPointerMove) window.removeEventListener("pointermove", state.onPointerMove);
      if (state?.onTransitionEnd) window.removeEventListener("transitionend", state.onTransitionEnd);
      if (state?.onScheme) state.scheme?.removeEventListener?.("change", state.onScheme);
      if (scheduled) cancelAnimationFrame(scheduled);
      scheduled = 0;
      for (const id of [LAYER_ID, STYLE_ID, EARLY_STYLE_ID]) document.getElementById(id)?.remove();
      document.documentElement?.classList.remove(...ROOT_CLASSES);
      for (const property of ROOT_PROPERTIES) document.documentElement?.style.removeProperty(property);
      if (state?.mediaUrl?.startsWith("blob:")) URL.revokeObjectURL(state.mediaUrl);
      delete window[STATE];
      return true;
    };

    const isDarkTheme = () => {
      const bodyClass = (document.body?.className || "").toLowerCase();
      if (bodyClass.includes("light")) return false;
      if (bodyClass.includes("dark")) return true;
      const scheme = (document.documentElement?.style?.colorScheme || "").toLowerCase();
      if (scheme.includes("light")) return false;
      if (scheme.includes("dark")) return true;
      try {
        return window.matchMedia("(prefers-color-scheme: dark)").matches;
      } catch {}
      return true;
    };

    // 原版令牌挂在 :root（html[style]）且绝不被我们修改，可直接实时读取当前主题原色。
    const readSolidColors = () => {
      const root = document.documentElement;
      if (!root) return null;
      const computed = getComputedStyle(root);
      const dark = isDarkTheme();
      const fallbackBg = computed.getPropertyValue("--background").trim() || (dark ? "#070708" : "#fafafa");
      const fallbackSidebar = computed.getPropertyValue("--sidebar").trim() || (dark ? "#111114" : "#f3f3f5");
      const fallbackCard = computed.getPropertyValue("--card").trim() || (dark ? "#131317" : "#ffffff");
      const colors = {};
      for (const [target, source] of Object.entries(SOLID_TOKENS)) {
        const value = computed.getPropertyValue(source).trim();
        if (value) {
          colors[target] = value;
        } else if (source === "--sidebar" || source === "--shell") {
          colors[target] = fallbackSidebar;
        } else if (source === "--background" || source === "--content") {
          colors[target] = fallbackBg;
        } else {
          colors[target] = fallbackCard;
        }
      }
      return colors;
    };

    let observedHoleNodes = [];
    let holeResizeObserver = null;

    // 内置浏览器 WebContentsView 挖洞激活时，用 clip-path 仅把浏览器视口区域从背景层抠除。
    const updateBrowserHoleClip = (layer) => {
      if (!layer) return;
      if (document.body?.getAttribute("data-has-browser-hole") !== "true") {
        if (layer.style.clipPath) layer.style.clipPath = "";
        if (holeResizeObserver && observedHoleNodes.length > 0) {
          holeResizeObserver.disconnect();
          observedHoleNodes = [];
        }
        return;
      }
      // 优先精确匹配 Antigravity 标记的 [data-browser-hole] 视口节点，退回最内层 [data-has-browser-hole="true"]。
      let holeNodes = [...document.querySelectorAll('[data-browser-hole]:not([data-browser-hole="false"])')].filter((el) => {
        const rect = el.getBoundingClientRect();
        return rect.width >= 40 && rect.height >= 40;
      });
      if (holeNodes.length === 0) {
        const candidates = [...document.querySelectorAll('[data-has-browser-hole="true"]')].filter(
          (el) => el !== document.body && !el.classList.contains("agy-inset-shell") && !el.classList.contains("agy-shell")
        );
        holeNodes = candidates.filter((el) => {
          if (el.querySelector('[data-has-browser-hole="true"]')) return false;
          const rect = el.getBoundingClientRect();
          return rect.width >= 40 && rect.height >= 40;
        });
      }
      if (holeResizeObserver) {
        const changed =
          holeNodes.length !== observedHoleNodes.length ||
          holeNodes.some((node, idx) => observedHoleNodes[idx] !== node);
        if (changed) {
          holeResizeObserver.disconnect();
          for (const node of holeNodes) holeResizeObserver.observe(node);
          observedHoleNodes = holeNodes;
        }
      }
      if (holeNodes.length === 0) {
        if (layer.style.clipPath) layer.style.clipPath = "";
        return;
      }
      const segments = ["0 0, 100% 0, 100% 100%, 0 100%, 0 0"];
      for (const node of holeNodes) {
        const rect = node.getBoundingClientRect();
        const x1 = Math.max(0, Math.floor(rect.left));
        const y1 = Math.max(0, Math.floor(rect.top));
        const x2 = Math.min(window.innerWidth, Math.ceil(rect.right));
        const y2 = Math.min(window.innerHeight, Math.ceil(rect.bottom));
        if (x2 - x1 < 40 || y2 - y1 < 40) continue;
        segments.push(
          x1 + "px " + y1 + "px, " +
          x2 + "px " + y1 + "px, " +
          x2 + "px " + y2 + "px, " +
          x1 + "px " + y2 + "px, " +
          x1 + "px " + y1 + "px"
        );
      }
      const nextClip = segments.length > 1 ? "polygon(evenodd, " + segments.join(", ") + ")" : "";
      if (layer.style.clipPath !== nextClip) layer.style.clipPath = nextClip;
    };

    const install = () => {
      const root = document.documentElement;
      if (!root) return false;
      const setClass = (name, on) => {
        if (root.classList.contains(name) !== on) root.classList.toggle(name, on);
      };
      const setProp = (name, value) => {
        if (root.style.getPropertyValue(name) !== value) root.style.setProperty(name, value);
      };

      const colors = readSolidColors();
      if (colors) for (const [name, value] of Object.entries(colors)) setProp(name, value);

      let style = document.getElementById(STYLE_ID);
      if (!style) {
        style = document.createElement("style");
        style.id = STYLE_ID;
        (document.head || root).appendChild(style);
      }
      if (style.dataset.agbgRevision !== config.revision) {
        style.textContent = cssText;
        style.dataset.agbgRevision = config.revision;
      }
      document.getElementById(EARLY_STYLE_ID)?.remove();

      let layer = document.getElementById(LAYER_ID);
      if (!layer && document.body) {
        layer = document.createElement("div");
        layer.id = LAYER_ID;
        layer.setAttribute("aria-hidden", "true");
        const media = document.createElement(config.mediaKind === "video" ? "video" : "img");
        media.id = "antigravity-background-media";
        media.setAttribute("aria-hidden", "true");
        if (config.mediaKind === "video") {
          media.autoplay = true;
          media.loop = true;
          media.muted = Boolean(config.display.videoMuted);
          media.defaultMuted = Boolean(config.display.videoMuted);
          media.playsInline = true;
        } else {
          media.alt = "";
          media.decoding = "async";
        }
        media.src = mediaUrl;
        const tile = document.createElement("div");
        tile.id = "antigravity-background-tile";
        const overlay = document.createElement("div");
        overlay.id = "antigravity-background-overlay";
        layer.append(media, tile, overlay);
        document.body.prepend(layer);
        if (config.mediaKind === "video") {
          media.playbackRate = Number(config.display.videoPlaybackRate) || 1;
          media.play().catch(() => undefined);
        }
      } else if (layer && document.body && layer.parentNode !== document.body) {
        document.body.prepend(layer);
      }

      const display = config.display;
      setClass("antigravity-background-active", true);
      setClass("antigravity-background-fit-tile", display.fit === "tile" && config.mediaKind === "image");
      setClass("antigravity-background-home-disabled", !display.enabledOnHome);
      setClass("antigravity-background-task-disabled", !display.enabledOnTasks);
      setProp("--agbg-opacity", String(display.opacity));
      setProp("--agbg-blur", display.blur + "px");
      setProp("--agbg-scale", String(display.scale));
      setProp("--agbg-fit", display.fit === "tile" ? "cover" : display.fit);
      setProp("--agbg-position-x", display.positionX + "%");
      setProp("--agbg-position-y", display.positionY + "%");
      setProp("--agbg-overlay-color", display.overlayColor);
      setProp("--agbg-overlay-opacity", String(display.overlayOpacity));
      setProp("--agbg-home-intensity", String(display.homeIntensity));
      setProp("--agbg-task-intensity", String(display.taskIntensity));
      setProp("--agbg-sidebar-opacity", String(display.sidebarOpacity));
      setProp("--agbg-surface-opacity", String(display.surfaceOpacity));
      setProp("--agbg-card-opacity", String(display.cardOpacity));
      setProp("--agbg-highlight-opacity", String(Math.min(1, display.cardOpacity + 0.25)));
      setProp("--agbg-composer-opacity", String(display.composerOpacity));
      setProp("--agbg-menu-opacity", String(display.menuOpacity));
      setProp("--agbg-terminal-opacity", String(display.terminalOpacity));
      setProp("--agbg-media-url", 'url("' + mediaUrl.replace(/["\\\n\r]/g, "") + '")');

      // 主页存在 .agy-home 且无会话步骤；进入会话/设置页时切换为会话页强度。
      const hasConversationStep = Boolean(document.querySelector('[data-testid="user-input-step"]'));
      const isHome = Boolean(document.querySelector(".agy-home")) && !hasConversationStep;
      setClass("antigravity-background-home", isHome);
      setClass("antigravity-background-task", !isHome);
      updateBrowserHoleClip(layer);
      return true;
    };

    const scheduleInstall = () => {
      if (scheduled) return;
      scheduled = requestAnimationFrame(() => { scheduled = 0; install(); });
    };
    const observer = new MutationObserver(scheduleInstall);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["style", "class", "data-theme"],
    });
    if (document.body) {
      observer.observe(document.body, {
        childList: true,
        attributes: true,
        attributeFilter: ["class", "style", "data-agy-theme", "data-has-browser-hole"],
      });
    }
    const onResize = () => scheduleInstall();
    window.addEventListener("resize", onResize);
    const onPointerMove = (event) => {
      if (event.buttons && document.body?.getAttribute("data-has-browser-hole") === "true") {
        scheduleInstall();
      }
    };
    const onTransitionEnd = () => {
      if (document.body?.getAttribute("data-has-browser-hole") === "true") {
        scheduleInstall();
      }
    };
    window.addEventListener("pointermove", onPointerMove, { passive: true });
    window.addEventListener("transitionend", onTransitionEnd, { passive: true });
    if (typeof ResizeObserver === "function") {
      holeResizeObserver = new ResizeObserver(scheduleInstall);
    }
    let scheme = null;
    try { scheme = window.matchMedia("(prefers-color-scheme: dark)"); } catch {}
    const onScheme = () => scheduleInstall();
    scheme?.addEventListener?.("change", onScheme);
    const timer = setInterval(install, 2500);
    window[STATE] = {
      revision: config.revision,
      cleanup,
      observer,
      holeResizeObserver,
      timer,
      onResize,
      onPointerMove,
      onTransitionEnd,
      scheme,
      onScheme,
      mediaUrl,
      isHealthy: () => Boolean(document.getElementById(LAYER_ID) && document.getElementById(STYLE_ID)),
    };
    install();
    return { installed: true, revision: config.revision, mediaKind: config.mediaKind };
  })(${serialized}, ${css}, ${reviewShadowCss}, ${reviewShadowStyleId})`;
}
