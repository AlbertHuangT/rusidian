# Rusidian Context

Rusidian 是一个本地优先的原生 Markdown 桌面应用，以真实 Neovim 作为源码编辑器，并提供独立的渲染阅读体验。

## Language

**Vault**:
作为一个整体打开和索引的本地文件夹；它可以是 Obsidian vault，也可以是普通文件夹。
_Avoid_: Workspace, project

**阅读视图**:
Rusidian 渲染 Markdown 后提供的只读视图，拥有 Vim 式导航，但不是 Neovim 的 Normal 模式。
_Avoid_: Normal mode, preview mode

**源码视图**:
由真实 Neovim 驱动的文本视图，包含 Neovim 的 Normal、Insert、Visual 和命令行模式。
_Avoid_: Edit mode

**预览光标**:
阅读视图中只在可见字符和原子对象之间移动、并可映射回 Markdown 源码位置的逻辑光标。
_Avoid_: Neovim cursor

**原子对象**:
阅读视图中占据一个光标位置的图片或渲染图；对象内部不能放置预览光标。
_Avoid_: Character

**TikZ 块**:
语言标记为 `tikz`、内容包含完整 TikZ 环境、由 Rusidian 补齐外层 TeX 文档结构的围栏代码块。
_Avoid_: TeX document

**技术原型**:
用于验证单文件编辑、阅读视图、Neovim 嵌入和 TikZ 编译闭环的首个内部里程碑，不是可替代 Obsidian 的首个发行版。
_Avoid_: Version 1, MVP

**源码公开插件**:
源码必须公开可见、但许可证不受 Rusidian 限制的插件；它不等同于必须采用 OSI 认可许可证的开源插件。
_Avoid_: Open-source plugin
