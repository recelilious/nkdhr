# 主题扩展令牌

> [English version / 英文版本](EXTENDING.md)

状态：声明式注册表与运行时链路已经交付；这里不是插件加载器，也不会从主题
profile 中加载可执行代码。

每个扩展拥有一个反向域名组，例如 `extension.com.example.widget`。宿主在创建
`ThemeRuntime` 前，把一组有界的 token 描述注册到 `ThemeExtensionRegistry`。
每条描述包含默认值、值类型，以及变更只需重绘还是必须重排。支持布尔、有界整数、
有界浮点数、有界字符串、颜色和封闭枚举选项。

profile 只保存稀疏数值，不保存 schema 或代码：

```json
{
  "overrides": {
    "extension": {
      "com.example.widget": {
        "gap": 12.5,
        "accent": "#aabbccff"
      }
    }
  }
}
```

对应的运行时路径是 `extension.com.example.widget.gap` 与
`extension.com.example.widget.accent`。缺省值来自注册表。未知组、未知 token、
错误 JSON 类型或越界值都会在发布前拒绝整个候选，旧的不可变 generation 会继续
可见。扩展组不能替换 `palette.*`、`spacing.*` 等内置令牌。

组件通过 `ThemeReadSet` 声明自己读取的命名空间路径，并从
`ThemeSnapshot::read_extension` 读取解析后的值。retained tree 会像处理内置
token 一样，按描述中的 paint/layout 分类精确失效。共享一个 runtime 的多个 root
仍只会在各自的本地活动边界同步；某个 root 可以跳过中间 generation，直接从自己
最后看到的快照计算差异。

`ThemeProfileLibrary` 与设置编辑器提供注册表感知入口，因此预览、校验、导入导出
使用同一组声明。所有负责校验持久化扩展值的进程都必须得到同一注册表。当前静态
`nkdhrd` 有意使用空扩展注册表，因此在未来插件加载里程碑把可信声明同时分发给
daemon、Settings 与 shell 之前，第三方组还不能通过 CTRL-5 持久化。

注册与解析均有资源上限：最多 256 个组、每组 256 个 token、每个枚举 256 个选项、
每个字符串 token 64 KiB；profile 仍受现有 1 MiB 上限约束。
