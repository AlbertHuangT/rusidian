# 数学渲染选型与验收（2026-09-15）

## 已采用

RaTeX 0.1.14（发布源码提交 `08cae05377938391117913ca4f278e6a3ffb6a8a`）负责 LaTeX 数学解析与排版。`parse → layout → to_display_list` 输出 em 单位的字形、直线、矩形和路径，包含基线以上的 height 和以下的 depth。GPUI 直接绘制；Tectonic 仅保留 TikZ。

依据：[parser 发布源码](https://docs.rs/crate/ratex-parser/0.1.14/source/src/lib.rs)、[layout 发布源码](https://docs.rs/crate/ratex-layout/0.1.14/source/src/to_display.rs)、[DisplayList 数据结构](https://docs.rs/crate/ratex-types/0.1.14/source/src/display_item.rs)。

选型原因是可直接消费排版坐标，已发布可锁定的 Rust crates，并有自包含数学字体；不把上游宣传的语法覆盖率视为本项目验收结果。ReX 也是数学排版库，但本次没有完成其构建或兼容性验证，不采用未经验证的性能/维护状况比较。[ReX 项目](https://github.com/ReTeX/ReX)

没有采用 `ratex-gpui` 0.4.0：它依赖 `gpui-pre` 0.3，其渲染入口生成图片；直接引入会改变本项目锁定的 GPUI 依赖，也不符合本次原生绘制目标。[依赖](https://docs.rs/crate/ratex-gpui/0.4.0/source/Cargo.toml)、[渲染源码](https://docs.rs/crate/ratex-gpui/0.4.0/source/src/render.rs)

## 接入中验证的约束

- GPUI 当前 macOS 字体加载代码拒绝缺少 `m` 的字体；KaTeX_Size2 等专用字体符合这一条件。首次真实窗口检查发现积分和可伸缩括号回退为普通字体，因此所有 KaTeX 字形通过 `ttf-parser` 读取原始轮廓，再交给 GPUI PathBuilder 绘制。CJK/Emoji 继续使用 GPUI 的系统字体回退。[GPUI 固定提交源码](https://github.com/zed-industries/zed/blob/d9e1c024f393832765a03f4de204d6c8cd9abcb2/crates/gpui_macos/src/text_system.rs)
- 缓存单位是当前文档中的 `(源码, 行内或块级样式)`，包括失败结果。正文编辑保留相同公式，不缓存 PNG；单独复制时才生成透明 PNG。离开文档的布局被移除。
- 输入限制 16 KiB；输出限制 8192 项、65536 条轮廓指令和有限几何范围。上游另有解析深度与宏展开次数限制；这不是完整 LaTeX 沙箱。普通公式路径不执行命令或读取 TeX 文件。[解析器深度限制](https://docs.rs/crate/ratex-parser/0.1.14/source/src/stack_safety.rs)、[宏展开限制](https://docs.rs/crate/ratex-parser/0.1.14/source/src/macro_expander.rs)
- 不支持任意宏包及用户 TeX 前导内容，不自动回退 Tectonic。段落中的公式与正文共用基线；同段落所有折行目前共用最高公式的行高，逐行紧凑布局暂缓。复杂 Unicode 文本的字距取决于上游估算和系统字体，尚未承诺完整 TeX 文本排版质量。

## 字体与许可证

RaTeX 代码采用 MIT；KaTeX 字体采用 SIL OFL 1.1，不能把字体视为 MIT。发布 crate 内全部 TTF 合计 513664 字节，无需运行时下载。[RaTeX 许可证](https://github.com/erweixin/RaTeX/blob/08cae05377938391117913ca4f278e6a3ffb6a8a/LICENSE)、[字体 crate](https://docs.rs/crate/ratex-katex-fonts/0.1.14/source/README.md)

已保存 [RaTeX 许可](licenses/RATEX.txt) 和包含原字体版权元数据的 [KaTeX 字体许可](licenses/KATEX-FONTS.txt)。发布应用包时必须一同分发这些许可；本次未制作正式安装包。

## 验收证据

- `cargo test`：21 项通过，2 项外部工具集成测试默认忽略。
- `cargo test tikz::tests::compiles_simple_tikz -- --ignored --nocapture`：通过，保留的 Tectonic → PDF → PNG 路径可用。
- `cargo build`、`cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`git diff --check`：通过，GPUI Git revision 未改变。
- 数学回归覆盖分数、根号、括号、积分、求和、矩阵、中文、无效命令、深层嵌套，以及专用数学字体的轮廓尺寸。
- 应用回归覆盖重复公式去重、正文编辑后的 Arc 复用、行内/块级缓存区分、过期布局移除和基线计算。
- 16 次布局与轮廓生成在一次本机 debug 测试中耗时约 15 ms；含首次字体初始化，不是 release 性能承诺，也没有与 Tectonic 做同条件基准比较。
- 初版真实窗口已检查 `examples/math.md`，发现并修复专用数学字体回退。修正后窗口复查暂被 Mac 锁屏阻止，视觉验收尚未完成。未据此宣布整体验收或包体积、空闲内存、启动性能门槛通过。

人工复查样例：[examples/math.md](../examples/math.md)。模块拥有者的无 AI 设计说明仍需本人完成；本文件只是选型资料和工具验收记录。
