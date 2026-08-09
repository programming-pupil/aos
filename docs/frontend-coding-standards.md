# React 前端代码规范

本文档定义 `webui/` 的代码规范，确保代码质量达到顶级开源水准。所有提交到 `webui/` 的代码均须遵循。

---

## 1. 组件规范

### 1.1 函数组件 + Hooks

所有组件必须为函数组件，禁止使用 class 组件（新代码）。

```tsx
// 推荐
export function UserAvatar({ userId, size }: { userId: string; size?: number }) {
  const { data } = useUser(userId);
  return <Avatar src={data?.avatar} size={size ?? 32} />;
}

// 禁止
class UserAvatar extends Component { ... }
```

### 1.2 单一职责

单个组件不超过 300 行。超过则应拆分为子组件或抽取自定义 Hook。

### 1.3 组件拆分原则

- 按功能域拆分（`components/chat/`, `components/nl2sql/`, `components/layout/`）
- 按原子级别：原子组件 → 分子组件 → 页面组件
- 页面组件只负责布局和数据组装，核心逻辑委托给子组件

---

## 2. 类型规范

### 2.1 TypeScript 严格模式

所有文件必须开启严格模式：

```json
// tsconfig.json
{
  "compilerOptions": {
    "strict": true,
    "noImplicitAny": true,
    "strictNullChecks": true
  }
}
```

### 2.2 接口 vs 类型别名

- `interface` 用于公开 API（props、API 响应）：
  ```tsx
  interface UserProfileProps { userId: string; onClose: () => void; }
  interface ApiResponse<T> { data: T; error?: string; }
  ```
- `type` 用于联合类型、工具类型、映射类型：
  ```tsx
  type Status = 'idle' | 'loading' | 'success' | 'error';
  type Nullable<T> = T | null;
  ```

### 2.3 禁止 `any`

禁止使用 `any` 类型。无法确定类型时使用 `unknown` 并配合类型守卫。

```tsx
// 禁止
function handleData(data: any) { ... }

// 推荐
function handleData(data: unknown) {
  if (typeof data === 'string') { ... }
  else if (isUser(data)) { ... }
}
```

---

## 3. 状态管理

### 3.1 状态分层

| 状态类型 | 工具 | 说明 |
|----------|------|------|
| 本地 UI 状态 | `useState` / `useReducer` | 仅本组件使用 |
| 跨组件共享 | `zustand` store | 多组件共享的 UI 状态 |
| 服务端状态 | `@tanstack/react-query` | 服务端数据获取、缓存、同步 |

禁止混用三者：用 `useState` 存服务端数据，或用 `zustand` 做数据获取。

### 3.2 React Query 规范

```tsx
// 推荐：集中 API 调用
const { data, isLoading, error } = useQuery({
  queryKey: queryKeys.dataSources.list(),
  queryFn: () => dataSourcesApi.list(),
  staleTime: 60_000, // 1分钟内不重新获取
});

// 推荐：变更后刷新
await qc.invalidateQueries({ queryKey: queryKeys.nl2sql.history() });
```

---

## 4. API 层

### 4.1 统一 API 入口

所有 API 调用走 `src/api/index.ts`，禁止在组件内直接 `fetch`。

```tsx
// 推荐：API 模块化
export const nl2sqlApi = {
  query: (req: QueryRequest) => apiClient.post<QueryResponse>('/nl2sql/query', req),
  execute: (req: ExecuteRequest) => apiClient.post<ExecuteResponse>('/nl2sql/execute', req),
  history: () => apiClient.get<PaginatedResponse<QueryHistoryItem>>('/nl2sql/history'),
};

// 禁止：在组件内直接 fetch
fetch('/api/nl2sql/query', { method: 'POST', ... });
```

### 4.2 错误处理

每个 async 操作必须有错误处理：

```tsx
// 推荐：try/catch
try {
  const result = await nl2sqlApi.query(req);
  setData(result);
} catch (err) {
  message.error(`查询失败: ${(err as Error).message}`);
}

// 推荐：React Query error 状态
const { error } = useQuery({ queryFn: fetchData });
if (error) return <ErrorView error={error} />;
```

---

## 5. Hooks 抽取

可复用的组件内逻辑必须抽取为自定义 Hook：

```
src/hooks/
├── useDebounce.ts
├── useScrollToBottom.ts
├── useDataSourceSelection.ts
└── ...
```

```tsx
// 推荐：抽取可复用逻辑
export function useDebounce<T>(value: T, delay: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), delay);
    return () => clearTimeout(t);
  }, [value, delay]);
  return debounced;
}
```

---

## 6. 样式规范

### 6.1 CSS Variables + Ant Design

样式统一使用 CSS Variables（已有 Deep Space 主题系统）配合 Ant Design 组件。

```tsx
// 推荐：CSS Variables
<div style={{ background: 'var(--bg-surface)', color: 'var(--text-primary)' }}>

// 禁止：内联硬编码颜色（动态值除外）
<div style={{ background: '#1a1a2e' }}>  // 禁止

// 推荐：Ant Design 组件
<Button type="primary" onClick={handleSubmit}>提交</Button>

// 禁止：内联 style 替代 Ant Design 组件
<div onClick={handleClick} style={{ ...buttonStyle }}>提交</div>
```

### 6.2 动态样式

动态计算的样式（基于 props / state）允许内联：

```tsx
<div style={{ opacity: isDisabled ? 0.5 : 1, width: `${progress}%` }}>
```

### 6.3 禁止内联像素值

禁止在 JSX 中使用硬编码像素值（动态计算例外）：

```tsx
// 禁止
<div style={{ padding: '16px 20px', fontSize: 13 }}>

// 推荐：CSS 变量
<div style={{ padding: 'var(--space-md)', fontSize: 'var(--text-md)' }}>
```

---

## 7. 国际化（i18n）

所有用户可见文本必须走 `react-i18next`，禁止硬编码字符串。

```tsx
// 推荐：i18n key
<Text>{t('nl2sql.welcomeTitle')}</Text>

// 禁止：硬编码中文
<Text>欢迎使用 NL2SQL</Text>
```

翻译文件结构：

```
src/locales/
├── zh-CN.json
└── en-US.json
```

---

## 8. 文件与目录组织

```
src/
├── api/              # API 调用层
│   ├── index.ts      # API 客户端封装
│   ├── nl2sql.ts
│   ├── dataSources.ts
│   └── queryKeys.ts  # React Query key 管理
├── components/       # UI 组件
│   ├── chat/
│   ├── nl2sql/
│   ├── layout/
│   └── ui/           # 原子 UI 组件
├── hooks/            # 自定义 Hooks
├── pages/            # 页面组件
├── stores/           # Zustand stores
├── types/            # 全局类型定义
├── utils/            # 工具函数
├── locales/          # i18n 翻译文件
├── App.tsx
└── main.tsx
```

---

## 9. 注释规范

### 9.1 禁止冗余注释

```tsx
// 禁止：显而易见的注释
// 点击按钮时调用 handleSend
onClick={handleSend}

// 推荐：解释为什么/意图
// 800ms 防抖 — 用户停止输入后才调用后端，避免频繁请求
useEffect(() => {
  const timer = setTimeout(() => suggestSource(input), 800);
  return () => clearTimeout(timer);
}, [input]);
```

### 9.2 JSDoc

复杂的工具函数和类型定义使用 JSDoc：

```tsx
/**
 * Formats a timestamp as a relative time string (e.g., "2 minutes ago").
 * @param ts - ISO 8601 timestamp or Unix epoch in milliseconds
 * @returns Human-readable relative time
 */
function formatRelativeTime(ts: string | number): string { ... }
```

---

## 10. 测试

### 10.1 单元测试

关键业务逻辑必须配 Vitest 单元测试：

- 自定义 Hooks（`useDebounce`, `useScrollToBottom`）
- 工具函数（日期格式化、数据转换）
- 状态管理逻辑（Zustand actions）

### 10.2 测试文件位置

```
src/
├── utils/
│   ├── formatTime.ts
│   └── formatTime.test.ts
├── hooks/
│   ├── useDebounce.ts
│   └── useDebounce.test.ts
```

### 10.3 组件测试

使用 `@testing-library/react`，测试行为而非实现细节：

```tsx
test('shows error message when query fails', async () => {
  render(<QueryInput onSubmit={mockSubmit} />);
  // ...
});
```
