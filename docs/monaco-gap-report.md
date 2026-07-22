# Monaco Editor Gap Report — w3cos ESM→Rust 编译里程碑

> **里程碑状态（2026-07-22）：已完成。** Headless DOM、窗口渲染和真实键盘输入均已通过；同一次 GUI 运行中窗口显示 `X// Monaco...`，AI Bridge 也读取到 model 的 `X` 前缀。最终输入修复包括 RegExp `lastIndex`、DOM pointer/focus/input 桥、数组与调用参数 spread，以及 `Symbol.iterator`/LinkedList 迭代。本文档继续作为兼容性缺口台账。
> 生成于 2026-07-18。目标：把 `monaco-editor@0.52.2` 的编辑器核心（`esm/vs/editor/editor.api`）经 w3cos ESM 编译管线编译为原生 w3cos 应用。

## 1. 规模实测

- monaco-editor 0.52.2 ESM 全量：986 个 JS 模块，98 个 CSS 文件。
- `editor.api` 实际可达依赖图（`cargo test -p w3cos-compiler --test monaco_graph -- --ignored` 实测，0.65s）：**528 个模块、2702 条 import 边、2292 个导出、3655 个 bundle 符号；未解析绑定 211 条 —— 全部为 `import * as ns` 命名空间导入，无其它解析缺口**。
- 入口示例：`examples/monaco-editor/app.ts`（`import * as monaco from "monaco-editor/esm/vs/editor/editor.api"`）。

## 2. 语法特性用量（esm/vs 全量 grep 统计）

| 特性 | 次数 | w3cos 现状 |
|------|------|-----------|
| class（总数） | 1551 | ⚠️ 仅 struct 骨架，无继承/构造参数/字段 |
| class extends | 843 | ❌ 不支持 |
| super() 构造调用 | 921 | ❌ |
| super.method() | 307 | ❌ |
| getter（类内） | 954 | ⚠️ 仅对象字面量 `__w3cos_getter_` 约定 |
| setter（类内） | 146 | ❌ |
| static 方法 | 805 | ❌ |
| static {} 初始化块 | 714 | ❌ |
| #私有字段 | 849 | ❌ |
| 类表达式 | 616 | ❌ |
| try/catch | 329 | ❌ catch 被丢弃，throw → panic! |
| new Promise | 140 | ❌ 无 Promise 对象/微任务 |
| 动态 import() | 105 | ❌（多为语言服务懒加载，核心可后置） |
| CSS import | 110 | ⚠️ resolver 跳过，不进样式表 |
| 模板字符串 | 9969 | ✅ |
| 展开 ... | 1878 | ⚠️ 部分 |
| typeof | 1839 | ✅ |
| ?. / ?? | 1436 / 897 | ⚠️ 部分 |
| switch | 1068 | ✅（ESM 路径） |
| Symbol.* | 577 | ❌ 无 Symbol |
| Reflect.* | 340 | ❌ |
| BigInt | 254 | ❌（多在语言服务，核心可后置） |
| Object.defineProperty | 218 | ⚠️ JsObject 有 define_property，语义简化 |
| WeakMap / WeakRef | 55 / 15 | ❌ |
| function* / yield | 22 / 38 | ❌（用量小，可后置） |
| 解构导出 `export const {a,b} = ...` | ≥1（dom.js 关键模块） | ✅ 本次已支持 |

## 3. 浏览器/DOM API 用量 vs 平台现状

已实现（够用）：createElement、body/head、createTextNode、createDocumentFragment、createRange、classList、addEventListener 三阶段、PointerEvent、KeyboardEvent、CompositionEvent、CustomEvent、Selection/Range、Canvas2D、Clipboard（navigator.clipboard 15 处）、ResizeObserver(38)/MutationObserver(20)/IntersectionObserver(35)、matchMedia(11)、rAF、localStorage、indexedDB、TextEncoder/TextDecoder、WebSocket、performance 基础。

| API | 用量 | 现状 |
|-----|------|------|
| document.activeElement / hasFocus / documentElement / defaultView / location | 各 1-3 | ⚠️ 需补齐 getter |
| document.execCommand / queryCommandSupported（复制路径） | 3 / 5 | ❌（可 shim 到 Clipboard API） |
| createElementNS（SVG） | 7 | ❌ |
| window.setTimeout/setInterval/clearTimeout | 5/3/3 | ⚠️ Rust 侧有 timers.rs，无 JS 全局 |
| window.innerWidth/innerHeight/devicePixelRatio/screen | ~9 | ❌ 需补 |
| window.performance.mark/measure/now | 18 | ❌（可 no-op 起步） |
| Intl.Collator/Locale/Segmenter/DateTimeFormat 等 | ~50 | ❌（可降级 shim） |
| queueMicrotask | 17 | ❌ |
| structuredClone | 8 | ❌（可 JSON 降级） |
| AbortController | 20 | ❌ |
| requestIdleCallback | 12 | ❌（可 setTimeout 降级） |
| DragEvent / ClipboardEvent | 36 / 24 | ❌ 事件类型缺 |
| getComputedStyle | 21 | ⚠️ 仅回显 inline style，无级联 |
| URLSearchParams / URL | 39 | ❌ |
| atob / btoa | 16 | ❌ |
| EditContext | 3 个文件 | ❌（monaco 有特性检测，可降级到 textarea 路径） |
| new Worker | 3 | ⏸️ editor.api 范围内可避免 |

**关键架构缺口**：编译产物里的 `document` 全局当前是 `w3cos_core::builtins::document` **假对象**（返回 no-op Value::object），不连真实 w3cos-dom Document。Monaco 要渲染必须建「Value 级 DOM 桥」：编译代码的 DOM 调用落到真实 Document 节点上。

## 4. 解析器现状（本次会话修复，全部带单测）

已修复并验证：
- dotted-basename 子路径解析（`editor.api` → `editor.api.js`）。
- 路径词法规范化（`a/b/../c.js` ≡ `a/c.js`）——修复前依赖图爆炸到 38443 个伪模块。
- resolve 缓存 + 模块/符号索引——全图扫描从 >20min 降到 0.65s。
- `export * from` 收集与转发。
- 解构导出（`export const { a, b } = ...`，monaco dom.js 关键模式）。
- `export default <expr>`（ExportDefaultExpr）收集。

仍缺（阻塞项）：
- **`import * as ns` 命名空间导入**（211 条未解析绑定的全部）：app.ts 及 monaco 内部大量使用；需要 bundle 记录命名空间导入 + codegen 发命名空间对象（resolver 侧 `all_exports()` API 已备好）。

## 5. 编译器实施清单（Stage 2，按依赖排序）

1. **类系统**（进行中）：extends/super()/super.method/instanceof/构造字段/static 方法+块/getter/setter/#私有字段/类表达式 → w3cos-core JsObject 原型链 + call slot 方案。
2. **命名空间导入**：`import * as ns` → 命名空间对象（配合 `all_exports()`）。
3. **try/catch/finally/throw**：panic_any(Value) + catch_unwind；首版限同步。
4. **JsPromise**：三态 + then/catch/finally 链 + Promise.all/race + 微任务队列（挂 timers::tick）+ await 桥接。
5. **JS 全局**：setTimeout/setInterval/clearTimeout/clearInterval → timers.rs；JSON.parse/stringify → serde_json；queueMicrotask；performance.now/mark/measure（可 no-op）；atob/btoa；URL/URLSearchParams；structuredClone（JSON 降级）。
6. **CSS import**：esm_resolver 收集 .css 边 → css_parser 解析 → 编译期合并进应用样式表（Monaco 110 个 css import）。

## 6. DOM/运行时实施清单（Stage 3）

- Value 级 DOM 桥（替换 builtins 假 document）：编译代码的 createElement/appendChild/addEventListener/style 写入落到真实 w3cos-dom Document。
- getComputedStyle 最小级联（inline + 样式表规则）。
- document.activeElement/hasFocus/documentElement/defaultView/location、window.inner\*/devicePixelRatio/screen/performance。
- createElementNS（SVG，monaco 图标用）。
- execCommand/queryCommandSupported → Clipboard shim。
- DragEvent/ClipboardEvent 事件类型。
- requestIdleCallback（setTimeout 降级）、AbortController。
- 动态 `<style>` 注入 → Document 样式表注册（monaco 主题路径）。

## 7. 明确排除（本里程碑不做）

- Worker 加载（editor.main 的 language services）、ts/css/html language workers、EditContext（走降级路径）、动态 import()（105 处多为懒加载，先返回 rejected Promise shim）、BigInt、WeakMap/WeakRef 真语义、function* 生成器、Service Worker。

## 8. 生成工程 cargo check 清零（Stage 2 收尾，本次会话）

`cargo check --manifest-path /tmp/w3cos-build/Cargo.toml` 从 **856 个错误降到 0**（exit 0），
`cargo build` 全量链接通过，产出 75.7 MB 可执行文件。修复全部落在 lowering
（`esm_lowering.rs` / `esm_codegen.rs` / `scope_analysis.rs`），未手改任何生成文件：

- **E0384 ×207**：dynamic 模式下所有 JS let/const/var 一律发 `let mut`（JS 绑定均可重赋值）。
- **E0382 ×313 / E0507 ×41**：clone-on-use —— 对象字面量 shorthand 改走 `resolve_value`（局部
  Value `.clone()`、fn-item/跨模块 fn 包 `Value::function`、类/命名空间走访问器调用）；
  `this` 在值位置发 `__this.clone()`；可选链调用参数走 `lower_argument`；参数默认值
  用当前 ctx（带 known_values/classes/namespaces/boxed）lowering。
- **E0594 ×256 / E0596 ×3**：闭包按 JS 引用捕获实现 —— `scope_analysis::analyze_fn_body`
  计算「被闭包捕获 且 在任意处被赋值」的名字，声明处发 `Rc<RefCell<Value>>`，读
  `(*x.borrow()).clone()`、写 `*x.borrow_mut() = v`；捕获 prologue 的 `x.clone()` 克隆 Rc
  即共享同一 cell（live-binding 语义正确）。分析覆盖 fn 声明（`fn_scopes_capture`）、
  class 成员、switch/try/do-while/labeled、可选链、解构赋值目标；名字按 sanitize 后对齐。
- **E0061 ×23 / E0425 ×20**：参数绑定改两阶段 —— 先按 `__args` 原样绑定全部参数，再按
  声明序以 `if x.is_undefined() { x = default; }` 追加默认值（默认值可引用/闭包捕获后面的
  参数）；默认值 ctx 补齐 namespace/class 解析。
- **E0695 ×7 / E0696 ×135**：循环发 Rust 标签（`'__lpN`），switch 发独立 `'__swN` 块标签，
  `break`/`continue` 按内层 breakable/loop 标签栈发射；JS 带标号语句映射到对应 Rust 标签。
  经典 for 与 do-while 用 first-iteration 标志位把 update/test 移到循环头部，使
  `continue` 语义与 JS 完全一致（for 的 continue 会跑 update，do-while 的 continue 会跑 test）。
- **E0618 ×2**：fn 形式调用（`f(vec![..])`）仅在名字未被局部 Value 遮蔽时使用，否则走
  `Value::call`。 **E0277 ×2**：同 shorthand 修复。 **E0599/E0308**：`Array`/`Object`
  作为值改发 callable facade（`Array.from`、`Object.keys/values/is` 映射到 core builtins，
  其余静态成员降级为 Undefined）。

### 记录在案的 JS 语义折衷

- 参数默认值若引用**更靠后**的参数：生成的两阶段绑定会正常求值（JS 属 TDZ 运行时错误），
  语义略放宽，编译可用。
- 被 box 的循环变量在「迭代它 且 循环体内又给它赋值」时，运行期可能触发 RefCell
  already-borrowed panic（编译不受影响；该写法在 monaco 中未出现）。
- `for (x of xs)`（无声明）改写为对外部绑定的写穿（比 JS 语义更准确）；`var` 循环头写穿到
  函数作用域提升槽（单一绑定，符合 JS）；`let`/`const` 循环头为每轮迭代新绑定。
- forEach 回调体内的 `return` 仍会从外层 Rust fn 返回（既有折衷；JS 中应只返回回调）。
- 闭包内 box 名按名字近似（shadowing 同名绑定会一并 box）—— 语义健全，仅多付 Rc 开销。
- `arguments` 以数组值近似；`await`/动态 `import()` 仍为既有降级路径（见 §7）。
