# 模型能力档案与推理参数

AOS 不把 `low`、`high`、`xhigh` 等参数写死在 API Key 上。模型能力以租户级、版本化的
Model Capability Profile 保存，API Key 只引用档案并保留少量人工覆盖。这样更换 Key
不会重建能力档案，更换服务商地址或模型时才重新解析和关联。

## 为什么不能只调用 `/models`

OpenAI、Anthropic、DeepSeek、Kimi、智谱 GLM、Google Gemini、xAI Grok 和各类 OpenAI-compatible
网关没有统一的“参数说明”接口。`/models` 通常只返回模型 ID，少数服务会额外返回上下文
窗口、输入模态或支持的参数。
因此 AOS 使用分层协商，而不把缺失字段误判为“不支持”：

1. 真实验证：用户主动执行低成本探测，可信度最高；
2. 服务商元数据：仅采用服务商明确返回的字段；
3. 内置注册表：随 AOS 版本发布并带 registry version；
4. 保守回退：未知自定义模型不自动开启推理、工具或结构化输出；
5. API Key 人工覆盖：仅覆盖该 Key，优先于档案，适合私有网关。

低可信来源不会覆盖已真实验证的结果。内置注册表版本更新时，同来源的旧 inferred 档案
会刷新；真实验证档案过期后会提示重新验证，但仍可继续使用，不会在请求链路中阻塞用户。

Kimi、GLM、Gemini 和 Grok 在 Setup 与 API Keys 页面提供官方地址预设，也允许直接改成兼容的
三方中转地址。它们复用 OpenAI-compatible 传输，但使用独立的 canonical provider 身份，
避免不同服务商的能力档案相互污染。

## 联网能力边界

“模型可以联网”和“服务商原生搜索”不是同一件事。AOS 将两条链路分开：

- 模型原生搜索依赖模型、接口协议和中转站同时支持对应 search 请求；
- AOS 内置 `WebSearch` 是零配置运行时工具，通过 DuckDuckGo、Brave、Wikipedia、
  Hacker News Algolia 和 Stack Exchange 等公共来源检索；需要能访问互联网，但不需要搜索 API Key；
- Brave API、Tavily、Serper、Exa、SearXNG、Generic HTTP 和搜索 MCP 是可选增强源，
  用于提高覆盖、稳定性或接入私有搜索；它们不是联网能力的使用前提；
- `WebFetch` 和上述搜索工具只要求聊天模型可靠支持工具调用，与模型 API 的域名无关。

内置 provider 不会自动打开未经验证的原生搜索。配置三方中转站也不会被 AOS 按域名阻止，
但中转站是否完整兼容原生 Responses/search 协议仍需真实验证。运行顺序为：健康的 Search
扩展 -> AOS 内置搜索 -> 模型原生搜索 -> 搜索 MCP -> 本地/RAG。单个 Search 扩展的健康测试
采用严格模式，扩展故障不会被内置搜索伪装成测试成功。

## WebUI 使用流程

进入“系统 -> API 密钥”：

1. 选择服务商并填写 API Key；
2. 点击模型输入框右侧的刷新按钮，从服务商获取模型列表；
3. 选择模型，AOS 显示协议、上下文、最大输出、推理取值、工具调用和结构化输出能力；
4. 对未知模型或私有网关点击“验证能力”；
5. 推理策略通常保持“自动”，只有明确成本或质量要求时才固定为其他档位；
6. 服务商无法列出模型时，可以直接手工输入模型 ID，再使用内置档案或执行验证。

模型列表发现失败不影响手工配置。能力验证会产生少量真实模型调用和费用，不会执行用户
工具，也不会发送业务上下文。

## 推理策略

界面提供统一的内部策略，运行时再映射为服务商参数：

| AOS 策略 | 行为 |
| --- | --- |
| 自动 | 按当前任务的 Fast、Standard、Deep 内部预算选择 |
| 快速 | 始终使用 profile 的 fast 映射 |
| 标准 | 始终使用 standard 映射 |
| 深度 | 始终使用 deep 映射 |
| 最大 | 使用该 profile 声明的最高受支持值 |

示例映射：

| 模型族 | 传输方式 | 支持值/映射 |
| --- | --- | --- |
| OpenAI GPT-5 | `reasoning_effort` | `low` / `medium` / `high` / `xhigh` |
| DeepSeek V4 | `reasoning_effort` | `low` / `high` / `max` |
| Anthropic thinking | `thinking` 对象 | AOS 档位映射为 token budget |
| Qwen thinking | `enable_thinking` | `disabled` / `enabled` |

“最大”不等于默认。深度研究等流程仍可选择内部预算；固定“最大”会增加耗时和费用，应由
用户明确启用。

## 运行时兼容与降级

- 只有服务商以 `400/422` 明确返回 unknown/unsupported/not allowed parameter 时，AOS 才
  学习该端点、模型和 Key 不支持对应可选参数；
- `401`、`429`、超时和 `5xx` 不会被当作能力不支持；
- `max_tokens` 与 `max_completion_tokens` 被明确拒绝时会互换并重试一次；
- `reasoning_effort`、`include_reasoning`、`temperature`、`top_p` 以及
  `thinking`/`thinking_config`/`enable_thinking` 被明确拒绝时会移除并重试一次；
- Anthropic thinking 的 budget 始终小于 `max_tokens`，并为可见回答保留输出空间，不会突破
  profile 的最大输出上限；
- 未识别的自定义模型默认不发送实验性参数，避免因猜测导致全部请求失败。

运行时降级只处理可选参数，不会把认证失败、余额不足或限流隐藏成“兼容成功”。

## 数据模型

SQLite 表 `model_capability_profiles` 按以下身份唯一：

```text
tenant + canonical provider + normalized base URL + protocol + model + model type
```

档案包含 schema version、registry version、来源、可信度、探测状态、能力 JSON、探测观察、
最后错误、探测时间和过期时间。`api_keys.model_profile_id` 关联当前档案。

只有聊天模型使用该档案。Embedding 的 provider、维度、向量签名和双索引生命周期由独立的
NL2SQL embedding profile 管理，两类 profile 不混用。

## 安全边界

- 模型列表和真实探测要求 `apikeys:write`；读取档案要求 `apikeys:read`；
- Key 明文只在后端内存中解密，不进入模型档案、响应或日志；
- 探测响应只保留截断错误摘要，不保存服务商完整返回正文；
- 选择或探测模型只持久化 profile，不会在编辑表单尚未保存时提前改绑 API Key；
- 只有 API Key 创建、保存修改或兼容回填流程负责关联 profile；
- 人工 `extraBody` 不能覆盖 `model`、`messages`、`tools`、`stream` 和 token 上限等核心字段。

## 验收清单

1. OpenAI、DeepSeek、Kimi、GLM、Gemini、Grok、Anthropic 和自定义地址均可手工输入模型并保存；
2. 支持 `/models` 的服务可搜索和选择模型，不支持时有清晰错误且可手工继续；
3. “发现 -> 选择 -> 保存 -> 重开编辑”后 profile 来源、协议和取值一致；
4. 只轮换 API Key 时 profile ID 不变；修改模型或 Base URL 后关联新 identity；
5. 已验证 profile 不被内置注册表或服务商元数据降级覆盖；
6. 429、超时、401、5xx 不会把能力标记为不支持；
7. 明确 unsupported parameter 时只额外重试一次，且第二次请求移除该参数；
8. 普通只读用户不能执行发现、探测、创建、修改或删除；
9. 中英文界面无缺失 key，窄屏下推理策略和能力说明不溢出；
10. SQLite 从空库执行全部 migration 后，profile 表和 API Key 外键列存在。

## 故障排查

- 获取模型失败：确认 Base URL 指向服务商根路径或 `/v1`，并检查 Key 是否有列模型权限；
- 验证失败但普通调用成功：查看具体检查项，工具/结构化输出探测可能被网关策略禁止，保留
  自动或人工覆盖即可；
- 参数仍被拒绝：执行一次真实验证，或在高级覆盖中关闭该能力；
- 修改 Key 后行为变化：确认同时没有修改 provider、Base URL 或模型；单独换 Key 不会切换
  profile，但服务商可能对不同账号开放不同能力，此时应重新验证。
