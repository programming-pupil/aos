import type { ComponentType } from 'react';
import { lazy, Suspense } from 'react';

type LazyCodeHighlighterProps = {
  code: string;
  language?: string;
  showLineNumbers?: boolean;
  className?: string;
  style?: React.CSSProperties;
  lineNumberStyle?: React.CSSProperties;
  codeTagStyle?: React.CSSProperties;
  wrapLongLines?: boolean;
};

function normalizeLanguage(language?: string): string {
  const normalized = (language ?? '').trim().toLowerCase();
  switch (normalized) {
    case 'js':
    case 'jsx':
      return 'javascript';
    case 'ts':
    case 'tsx':
      return 'typescript';
    case 'rs':
      return 'rust';
    case 'py':
      return 'python';
    case 'sh':
    case 'shell':
      return 'bash';
    case 'md':
      return 'markdown';
    case 'yml':
      return 'yaml';
    case 'html':
    case 'xml':
      return 'markup';
    case 'kt':
    case 'kts':
      return 'kotlin';
    case 'c++':
    case 'cxx':
    case 'cc':
    case 'hxx':
      return 'cpp';
    case 'c#':
    case 'cs':
      return 'csharp';
    case 'h':
    case 'hh':
      return 'c';
    case 'rb':
      return 'ruby';
    case 'gql':
      return 'graphql';
    case 'dockerfile':
      return 'docker';
    default:
      return normalized || 'text';
  }
}

const Highlighter = lazy(async () => {
  const [
    { default: SyntaxHighlighter },
    { oneDark },
    javascript,
    typescript,
    json,
    bash,
    rust,
    python,
    sql,
    diff,
    markdown,
    yaml,
    css,
    markup,
    java,
    kotlin,
    c,
    cpp,
    csharp,
    php,
    ruby,
    swift,
    graphql,
    docker,
  ] = await Promise.all([
    import('react-syntax-highlighter/dist/esm/prism-light'),
    import('react-syntax-highlighter/dist/esm/styles/prism'),
    import('react-syntax-highlighter/dist/esm/languages/prism/javascript'),
    import('react-syntax-highlighter/dist/esm/languages/prism/typescript'),
    import('react-syntax-highlighter/dist/esm/languages/prism/json'),
    import('react-syntax-highlighter/dist/esm/languages/prism/bash'),
    import('react-syntax-highlighter/dist/esm/languages/prism/rust'),
    import('react-syntax-highlighter/dist/esm/languages/prism/python'),
    import('react-syntax-highlighter/dist/esm/languages/prism/sql'),
    import('react-syntax-highlighter/dist/esm/languages/prism/diff'),
    import('react-syntax-highlighter/dist/esm/languages/prism/markdown'),
    import('react-syntax-highlighter/dist/esm/languages/prism/yaml'),
    import('react-syntax-highlighter/dist/esm/languages/prism/css'),
    import('react-syntax-highlighter/dist/esm/languages/prism/markup'),
    import('react-syntax-highlighter/dist/esm/languages/prism/java'),
    import('react-syntax-highlighter/dist/esm/languages/prism/kotlin'),
    import('react-syntax-highlighter/dist/esm/languages/prism/c'),
    import('react-syntax-highlighter/dist/esm/languages/prism/cpp'),
    import('react-syntax-highlighter/dist/esm/languages/prism/csharp'),
    import('react-syntax-highlighter/dist/esm/languages/prism/php'),
    import('react-syntax-highlighter/dist/esm/languages/prism/ruby'),
    import('react-syntax-highlighter/dist/esm/languages/prism/swift'),
    import('react-syntax-highlighter/dist/esm/languages/prism/graphql'),
    import('react-syntax-highlighter/dist/esm/languages/prism/docker'),
  ]);
  SyntaxHighlighter.registerLanguage('javascript', javascript.default);
  SyntaxHighlighter.registerLanguage('typescript', typescript.default);
  SyntaxHighlighter.registerLanguage('json', json.default);
  SyntaxHighlighter.registerLanguage('bash', bash.default);
  SyntaxHighlighter.registerLanguage('rust', rust.default);
  SyntaxHighlighter.registerLanguage('python', python.default);
  SyntaxHighlighter.registerLanguage('sql', sql.default);
  SyntaxHighlighter.registerLanguage('diff', diff.default);
  SyntaxHighlighter.registerLanguage('markdown', markdown.default);
  SyntaxHighlighter.registerLanguage('yaml', yaml.default);
  SyntaxHighlighter.registerLanguage('css', css.default);
  SyntaxHighlighter.registerLanguage('markup', markup.default);
  SyntaxHighlighter.registerLanguage('java', java.default);
  SyntaxHighlighter.registerLanguage('kotlin', kotlin.default);
  SyntaxHighlighter.registerLanguage('c', c.default);
  SyntaxHighlighter.registerLanguage('cpp', cpp.default);
  SyntaxHighlighter.registerLanguage('csharp', csharp.default);
  SyntaxHighlighter.registerLanguage('php', php.default);
  SyntaxHighlighter.registerLanguage('ruby', ruby.default);
  SyntaxHighlighter.registerLanguage('swift', swift.default);
  SyntaxHighlighter.registerLanguage('graphql', graphql.default);
  SyntaxHighlighter.registerLanguage('docker', docker.default);

  const Component = ({
    code,
    language,
    showLineNumbers,
    style,
    lineNumberStyle,
    codeTagStyle,
    wrapLongLines,
  }: LazyCodeHighlighterProps) => (
    <SyntaxHighlighter
      style={oneDark}
      language={normalizeLanguage(language)}
      showLineNumbers={showLineNumbers}
      customStyle={style}
      lineNumberStyle={lineNumberStyle}
      codeTagProps={{ style: codeTagStyle }}
      wrapLongLines={wrapLongLines ?? false}
    >
      {code}
    </SyntaxHighlighter>
  );
  return { default: Component as ComponentType<LazyCodeHighlighterProps> };
});

function PlainCodeBlock({
  code,
  className,
  style,
  codeTagStyle,
}: Pick<LazyCodeHighlighterProps, 'code' | 'className' | 'style' | 'codeTagStyle'>) {
  return (
    <pre
      className={className}
      style={{
        margin: 0,
        borderRadius: 8,
        fontSize: 13,
        background: '#1a1d23',
        color: '#e5e7eb',
        padding: 16,
        overflowX: 'auto',
        ...style,
      }}
    >
      <code style={codeTagStyle}>{code}</code>
    </pre>
  );
}

export function LazyCodeHighlighter(props: LazyCodeHighlighterProps) {
  return (
    <Suspense
      fallback={
        <PlainCodeBlock
          code={props.code}
          className={props.className}
          style={props.style}
          codeTagStyle={props.codeTagStyle}
        />
      }
    >
      <Highlighter {...props} />
    </Suspense>
  );
}

export function languageFromPath(language?: string | null, path?: string): string {
  const normalized = (language ?? '').toLowerCase();
  if (normalized) return normalizeLanguage(normalized);
  const ext = path?.split('.').pop()?.toLowerCase();
  return normalizeLanguage(ext);
}
