# Rusidian Product Decisions

> 状态：截至 2026-09-11 的已确认决定。与历史计划冲突时以本文件为准；未列为“已确认”的事项不得当作决定。

## Product direction

- Rusidian 是独立的、本地优先的原生 Markdown 桌面应用，不是 Electron 应用，也不只是 Neovim 的 TikZ 渲染插件。
- Rust 与 GPUI 已确定；GPUI 负责应用生命周期、窗口、控件和文档绘制。
- 源码视图运行用户本机的真实 Neovim，完整目标是加载用户的 `init.lua` 或 `init.vim`；传统 `.vimrc` 只承诺 Neovim 本身能够兼容的部分。
- 阅读视图负责渲染 Markdown；同一窗口不会同时显示源码和阅读视图。长期允许同一文档在多个窗口中分别显示源码和阅读视图。
- macOS 首发，之后支持 Linux；Windows 是明确的非目标。
- 第一验收对象是用户自己的真实 vault 和 Neovim 配置。项目可以开源，但首版不承诺普遍兼容所有用户环境。

## Interaction model

- 阅读视图属于 Rusidian 应用层，不是 Neovim Normal 模式。
- 阅读视图中：`i` 进入源码 Insert，`Enter` 进入源码 Normal。
- 源码 Normal 中：默认用 `Esc` 返回阅读视图。
- Rusidian 必须启动真实 Neovim 后检查全局映射及 Markdown buffer 的局部映射；发生冲突时让用户修改 Rusidian 快捷键，或展示由用户自行修改的 Neovim 配置。不得静态猜测或自动改写用户配置。
- 阅读视图采用字符级逻辑光标。隐藏的 Markdown 标记没有光标位置；可见字符精确映射回对应源码字符。
- 图片是原子对象；独占一段的图片作为图片块，行内图片作为段落中的一个原子位置。
- `j/k` 按源码物理行移动，`gj/gk` 按屏幕折行移动。
- 阅读视图的原型指令范围：`h/j/k/l`、`w/b/e`、`0/^/$`、`gg/G`、`Ctrl-d/u/f/b`、数字次数、`/?/n/N/*/#`、`f/F/t/T`、字符 Visual、整行 Visual 和 `y`。不支持修改，也不支持矩形 Visual。
- `gf` 打开内部笔记或附件；`gx` 打开外部链接。`Enter` 始终进入源码 Normal。
- 阅读视图中的复制只写系统剪贴板，不写 Neovim 寄存器。纯文字复制渲染后的文字；单独图片复制图片；文字和图片混合选择写入富文本，并提供纯文本备用内容。
- 预览直接读取当前 Neovim buffer，包括未保存修改。切换阅读视图不会保存文件，保存继续使用 Neovim 的正常机制。
- 长期多窗口共享同一个内存 buffer；阅读窗口实时看到编辑窗口尚未保存的修改。

## Markdown contract

- 长期目标是解析并无损保存 CommonMark、GFM 和 Obsidian Flavored Markdown，并渲染已明确支持的语义。
- 原始 HTML，包括 HTML 注释，在阅读视图中全部按代码样式显示；不执行 HTML 语义。
- Mermaid 代码块无损保存，但只显示源码；Rusidian 不提供 Mermaid 渲染。
- 正式版本支持行内 `$...$` 和块级 `$$...$$` 数学公式；技术原型不实现公式。
- 默认关闭“严格换行”，单个源码换行会显示。打开 Obsidian vault 时，读取并遵循其严格换行设置。
- 所有文本文件都可以交给 Neovim 编辑，但只有 Markdown 文件拥有阅读视图。
- 因 HTML 和 Mermaid 的显示策略，Rusidian 承诺文档数据兼容，不承诺与 Obsidian 完全一致的显示效果。

## TikZ and TeX

- `tikz` 围栏中写完整 TikZ 环境，例如 `tikzpicture` 或 `tikzcd`；不接受完整 TeX 文档。Rusidian 负责补齐文档类型、宏包和 `document` 外层。
- 技术原型只使用 Tectonic，因此只承诺 Tectonic 能处理的 TikZ；完整 TeX Live 兼容不是原型承诺。
- 安装时让用户选择预下载 Tectonic 离线资源包，或保持联网并在缺少资源时按需下载。
- 用户可配置全局及每个 vault 的 TeX 前导内容；这些设置保存在系统目录，不写入 vault。
- 完整 TikZ 块在闭合围栏时触发后台编译；编辑已有完整块时，在光标离开该块后触发编译。
- Markdown 立即显示。首次编译和重新编译期间，TikZ 位置显示占位符；编译失败只在该块显示错误卡片，其他内容继续渲染。
- Tectonic 生成 PDF。磁盘只保存具有容量上限的 PDF 缓存；显示位图主要驻留内存。
- TeX 默认在沙箱内运行。vault 可被单独标记为可信，但外部命令、读取 vault 外文件等能力仍应逐项授权。

## Vault and local data

- “文件 → 打开文件夹”可以打开任意文件夹；存在 Obsidian 配置时将其作为 Obsidian vault 处理。
- Rusidian 只读 `.obsidian/` 中影响文件语义的设置，例如链接格式、附件目录、排除路径和严格换行；忽略主题、插件、Obsidian 快捷键与 workspace，不执行其中的代码。
- Rusidian 不在 vault 中创建 `.rusidian/` 或生成缓存文件。索引、缓存和每个 vault 的 Rusidian 设置均保存在系统目录。
- 长期数据兼容范围包括 Markdown、wikilink、嵌入、附件、properties、tags、链接重命名更新和反向链接。
- 不内置同步，也不兼容 Obsidian Sync；直接与 Git、Syncthing、iCloud 等文件夹同步方案共存。
- 远程图片默认不加载。用户触发时可选择仅加载一次，或允许该 vault 自动加载。远程访问、TeX 外部命令和 vault 外文件访问是彼此独立的权限。

## Appearance and launch

- 阅读视图默认跟随系统明暗模式，并允许强制亮色或强制暗色；源码视图使用用户的 Neovim 配色。
- TikZ 默认显示在白色背景卡片中。
- 技术原型同时支持命令行打开、菜单“文件 → 打开文件”，以及系统右键“打开方式”。

## GUI architecture

- 使用 GPUI，不使用 WebView，也不维护 AppKit 或 SwiftUI Adapter。GPUI 内部的平台实现不属于 Rusidian 自有 Adapter。
- GPUI 拥有应用生命周期、窗口和全部界面；Liquid Glass 仅是视觉参考，不是产品承诺。
- 技术原型只使用原始 GPUI，不引入 `gpui-component`，也不复用或复制 Zed 的内部 UI 控件。
- 依赖锁定到经过构建验证的 Zed Git 提交；不跟随浮动主分支，只在需要修复时人工升级。
- GUI 依赖必须允许商业使用和再分发；优先宽松许可证，必要时可以接受 LGPL。GPUI 当前采用 Apache-2.0。
- Apple Silicon macOS 首发；Linux x86-64 后续支持。Intel Mac 和 Windows 不承诺。
- macOS 通过签名、公证安装包和 Homebrew 分发，不以 Mac App Store 为目标。
- 技术原型必须验证中文输入法、中文字体回退、Emoji、组合字符和高分屏。无障碍不作为当前选型门槛。
- 应用包不超过 50 MB，不含 Tectonic 离线资源；在 `nvim --clean` 下，Rusidian 与 Neovim 合计空闲内存不超过 150 MB；冷启动到可编辑不超过 1 秒；交互不得出现明显输入延迟。
- GPUI 未通过中文输入、稳定性或性能硬门槛时，先进行一次有明确上限的定位；仍不满足则重新选型，优先重新评估 Iced，而不是放宽要求或长期维护分支。

## Technical prototype acceptance

- 单个 Markdown 文件、单个窗口；不包含文件树、多窗口、搜索、反向链接和数学公式。
- 先用 `nvim --clean` 验证嵌入，再加载用户真实配置；真实配置通过后才算原型完成。
- 使用标准 Markdown 解析器；原型只渲染段落、标题、粗体、斜体、普通代码块、本地图片和 TikZ，其他语法显示源码。
- 实现已确认的源码/阅读视图切换、只读 Vim 导航、Visual、复制、后台 Tectonic 编译、错误隔离和 PDF 缓存闭环。

## After the prototype

第一优先级是打开整个 vault、文件树、多文件标签页和多窗口。随后再补齐完整 Markdown/数学渲染，以及索引、全文搜索、wikilink、反向链接、tags 和 properties 等知识库能力。

## Plugins and themes

- 插件系统不属于技术原型，具体能力、界面位置、运行方式和权限模型暂不决定。
- 若以后加入插件，源码必须公开可见；Rusidian 不限制其许可证。这里使用“源码公开插件”，不将其误称为必须采用开放许可证的“开源插件”。
- 当前倾向先采用声明式扩展；只有真实需求证明不足时，才考虑在隔离环境中加入 WebAssembly。此方向不是已锁定架构。
- 不承诺兼容 Obsidian 社区插件或 Obsidian 主题。Rusidian 自己的主题插件方向待定。

## Privacy and diagnostics

- 笔记内容始终在本地处理，不上传。
- 默认不上传遥测或诊断信息。
- 用户可以主动提交崩溃报告或匿名诊断；提交前应明确展示将发送的数据范围。

## Deferred decisions

- Rusidian 自有插件系统与主题插件的具体设计。
- 自动检查更新是否可由用户选择开启。
- 技术原型之后各里程碑的精确验收标准。
