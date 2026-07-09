# ADR-0002: 会话分组的拖拽用 dnd-kit + PointerSensor

状态：已接受（2026-06-04）
关联：PRD_会话分组.md 模块 3；ADR-0001（分组用 tag 不用真文件夹）

## 背景

「会话分组」需要三类拖拽：
1. 组内会话上下拖动调顺序；
2. 会话跨组移动（从 A 组拖到 B 组）；
3. 组之间上下拖动调顺序。

两个现实约束：
- **Tauri WKWebView 默认 `dragDropEnabled: true`，会拦截 WebView 里所有 HTML5 drag 事件**（`dragstart`/`dragover`/`drop`）。证据见 `src/lib/drag-state.ts` 顶部注释。因此任何依赖 HTML5 拖拽 API 的方案在本客户端里直接失效。
- 项目现有的文件树拖拽是**自写**的 `mousedown`/`mousemove`/`mouseup` 状态机（`drag-state.ts`），为「单一文件 → 拖进某文件夹 / 拖进聊天框」这一种场景设计：单拖拽源 + 用 `data-*` 属性 + `elementFromPoint` 检测落点 + ghost 元素。它没有列表排序、占位、跨容器移动的概念。

## 决策

引入 **`@dnd-kit/core` + `@dnd-kit/sortable` + `@dnd-kit/utilities`**，传感器用 **`PointerSensor`**。

- `PointerSensor` 基于 **pointer 事件**（`pointerdown`/`pointermove`/`pointerup`），**不是 HTML5 DnD**。Tauri 的 `dragDropEnabled` 拦截的是 HTML5 drag 事件，不碰 pointer 事件，所以 dnd-kit 的拖拽不受拦截——无需关闭 `dragDropEnabled`（关掉会破坏现有文件拖入功能）。
- `@dnd-kit/sortable` 原生支持列表排序与跨容器（组）拖动，正好覆盖上述三类拖拽。

## 被否决的方案

1. **扩展现有 `drag-state.ts`**：它为「单源 → 文件夹」设计，要改造成「列表排序 + 组内/组间区分 + 跨容器移动 + 占位动画」需大量新逻辑，排序拖拽的占位计算、边界处理自写极易出 bug。否决。
2. **HTML5 原生 DnD**：被 Tauri `dragDropEnabled` 拦截，本客户端不可用。否决。
3. **关闭 `dragDropEnabled` 改用 HTML5 DnD**：会破坏现有「拖文件进聊天框 / 文件树移动」功能（依赖 Tauri 原生 drag-drop 事件）。否决。

## 后果

- 新增三个前端依赖（dnd-kit 系列），无 Rust 侧改动。
- **拖拽逻辑的测试策略**：把「dnd-kit 的 `onDragEnd` 事件 → 账本（groupStore）action」的映射抽成纯函数单测（node 环境即可断言：某次拖拽该调哪个 action、参数是什么），避开 dnd-kit 在 jsdom 里模拟 pointer 拖拽的已知困难。三层渲染、折叠、未归类区等纯展示，按 PRD 走 Phase 5 `functional-test` 人工验。
- **遗留验证项**：`PointerSensor` 在本 WKWebView 里不被拦截，是基于「pointer 事件 ≠ HTML5 drag 事件」的设计推断 + Tauri 社区通行做法；须在 `functional-test` 阶段在真客户端里实拖一次确认。
- 与现有自写文件树拖拽并存：两套机制互不干扰（一个 pointer-based 管会话/组，一个 mouse-based 管文件树）。`isTreeDragActive()` 的既有逻辑不受影响。
