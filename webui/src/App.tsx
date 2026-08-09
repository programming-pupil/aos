import { Redirect, Route, Switch, useLocation } from '@/router';
import { Suspense, lazy, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { setupApi, usersApi } from '@/api';
import { useAuthStore } from '@/store/auth';
import { usePermissions, type Permission } from '@/store/permissions';
import Layout from '@/components/Layout';

const Login = lazy(() => import('@/pages/Login'));
const Setup = lazy(() => import('@/pages/Setup'));
const SharePreview = lazy(() => import('@/pages/SharePreview'));
const Dashboard = lazy(() => import('@/pages/Dashboard'));
const Hooks = lazy(() => import('@/pages/Hooks'));
const McpServers = lazy(() => import('@/pages/McpServers'));
const Skills = lazy(() => import('@/pages/Skills'));
const SearchProviders = lazy(() => import('@/pages/SearchProviders'));
const ApiKeys = lazy(() => import('@/pages/ApiKeys'));
const ConfigManagement = lazy(() => import('@/pages/ConfigManagement'));
const BotAgents = lazy(() => import('@/pages/BotAgents'));
const SuperAssistant = lazy(() => import('@/pages/SuperAssistant'));
const SuperAdversarial = lazy(() => import('@/pages/SuperAdversarial'));
const WatchDog = lazy(() => import('@/pages/WatchDog'));
const TaskCommandCenter = lazy(() => import('@/pages/TaskCommandCenter'));
const RdStudio = lazy(() => import('@/pages/RdStudio'));
const OperationsTasks = lazy(() => import('@/pages/OperationsTasks'));
const OperationsMaterials = lazy(() => import('@/pages/OperationsMaterials'));
const OperationsGovernance = lazy(() => import('@/pages/OperationsGovernance'));
const Pipeline = lazy(() => import('@/pages/Pipeline'));
const Projects = lazy(() => import('@/pages/Projects'));
const RdSpecs = lazy(() => import('@/pages/RdSpecs'));
const RdQuality = lazy(() => import('@/pages/RdQuality'));
const RdAgents = lazy(() => import('@/pages/RdAgents'));
const Nl2sql = lazy(() => import('@/pages/Nl2sql'));
const Nl2sqlManagement = lazy(() => import('@/pages/Nl2sqlManagement'));
const Nl2sqlSchemaChanges = lazy(() => import('@/pages/Nl2sqlSchemaChanges'));
const Nl2sqlAnalytics = lazy(() => import('@/pages/Nl2sqlAnalytics'));
const SqlKnowledgeBase = lazy(() => import('@/pages/SqlKnowledgeBase'));
const DataSources = lazy(() => import('@/pages/DataSources'));
const Users = lazy(() => import('@/pages/Users'));
const Tenants = lazy(() => import('@/pages/Tenants'));
const InvitePage = lazy(() => import('@/pages/InvitePage'));
const Workspace = lazy(() => import('@/pages/Workspace'));

function PageLoading() {
  return (
    <div
      style={{
        minHeight: '100vh',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        background: 'var(--bg-void)',
        color: 'var(--text-muted)',
        fontSize: 14,
      }}
    >
      Loading...
    </div>
  );
}

function NoPermission() {
  const { t } = useTranslation();
  return (
    <div
      style={{
        minHeight: '60vh',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        color: 'var(--text-muted)',
        fontSize: 14,
      }}
    >
      {t('common.noPermission')}
    </div>
  );
}

function RequireAuth({ children }: { children: ReactNode }) {
  const { token, isAuthenticated, login } = useAuthStore();
  const isDevMode = typeof window !== 'undefined' && new URLSearchParams(window.location.search).get('dev') === '1';
  useEffect(() => {
    if (isDevMode) {
      login('dev-token', { id: 'dev', email: 'dev@aos.ai', name: 'Developer', role: 'admin', tenant_id: 'dev-tenant' });
    }
  }, [isDevMode, login]);

  useEffect(() => {
    if (isDevMode || !isAuthenticated || !token) return;

    let cancelled = false;
    usersApi
      .me()
      .then((freshUser) => {
        if (!cancelled) {
          login(token, freshUser);
        }
      })
      .catch(() => {
        // Keep the current session on transient network errors; auth failures are
        // handled by the API interceptor elsewhere.
      });

    return () => {
      cancelled = true;
    };
  }, [isDevMode, isAuthenticated, token, login]);

  if (isDevMode || isAuthenticated) return <>{children}</>;
  return <Redirect to="/login" replace />;
}

function RequirePermission({
  permission,
  children,
}: {
  permission: Permission;
  children: ReactNode;
}) {
  const permitted = usePermissions((state) => state.permissions.has(permission));
  if (!permitted) {
    return <NoPermission />;
  }
  return <>{children}</>;
}

function withPermission(permission: Permission, node: ReactNode) {
  return <RequirePermission permission={permission}>{node}</RequirePermission>;
}

function SetupGate({ children }: { children: ReactNode }) {
  const { isAuthenticated } = useAuthStore();
  const location = useLocation();
  const [status, setStatus] = useState<'checking' | 'initialized' | 'uninitialized'>('checking');
  const [revalidating, setRevalidating] = useState(false);
  const initialCheckDoneRef = useRef(false);
  const isDevMode =
    typeof window !== 'undefined' &&
    new URLSearchParams(window.location.search).get('dev') === '1';

  useEffect(() => {
    let cancelled = false;
    const isInitialCheck = !initialCheckDoneRef.current;
    if (isDevMode) {
      setStatus('initialized');
      setRevalidating(false);
      initialCheckDoneRef.current = true;
      return () => {
        cancelled = true;
      };
    }

    if (status === 'uninitialized') {
      setRevalidating(true);
    }

    setupApi
      .check()
      .then((res) => {
        if (cancelled) return;
        setStatus(res.initialized ? 'initialized' : 'uninitialized');
        setRevalidating(false);
        initialCheckDoneRef.current = true;
      })
      .catch(() => {
        if (cancelled) return;
        // Fail-open only for the very first check to avoid a permanent blank page
        // on transient network issues.
        if (isInitialCheck) {
          setStatus('initialized');
          initialCheckDoneRef.current = true;
        }
        setRevalidating(false);
      });

    return () => {
      cancelled = true;
    };
  }, [isDevMode, location.pathname]);

  useEffect(() => {
    const handleSetupComplete = () => {
      setStatus('initialized');
      setRevalidating(false);
      initialCheckDoneRef.current = true;
    };
    window.addEventListener('aos-setup-complete', handleSetupComplete);
    return () => window.removeEventListener('aos-setup-complete', handleSetupComplete);
  }, []);

  if (status === 'checking') {
    return <PageLoading />;
  }

  if (status === 'uninitialized') {
    if (location.pathname === '/setup') {
      return <>{children}</>;
    }
    if (revalidating) {
      return <PageLoading />;
    }
    return <Redirect to="/setup" replace />;
  }

  if (location.pathname === '/setup') {
    return <Redirect to={isAuthenticated || isDevMode ? '/dashboard' : '/login'} replace />;
  }

  return <>{children}</>;
}

function ProtectedApp() {
  return (
    <RequireAuth>
      <Layout>
        <Switch>
          <Route path="/"><Redirect to="/dashboard" replace /></Route>
          <Route path="/dashboard">{withPermission('dashboard:read', <Dashboard />)}</Route>
          <Route path="/hooks">{withPermission('hooks:read', <Hooks />)}</Route>
          <Route path="/mcp">{withPermission('mcp:read', <McpServers />)}</Route>
          <Route path="/skills">{withPermission('skills:read', <Skills />)}</Route>
          <Route path="/search-providers">{withPermission('search_providers:read', <SearchProviders />)}</Route>
          <Route path="/keys">{withPermission('apikeys:read', <ApiKeys />)}</Route>
          <Route path="/config/management">{withPermission('config:read', <ConfigManagement />)}</Route>
          <Route path="/bot-agents">{withPermission('bot_agents:read', <BotAgents />)}</Route>
          <Route path="/super-assistant">{withPermission('super_assistant:read', <SuperAssistant />)}</Route>
          <Route path="/workspace">{withPermission('workspace:read', <Workspace />)}</Route>
          <Route path="/chat"><Redirect to="/super-assistant" replace /></Route>
          <Route path="/adversarial">{withPermission('adversarial:read', <SuperAdversarial />)}</Route>
          <Route path="/tasks">{withPermission('tasks:read', <TaskCommandCenter />)}</Route>
          <Route path="/watchdog"><Redirect to="/bot-agents" replace /></Route>
          <Route path="/agent-ops">{withPermission('tasks:admin', <WatchDog />)}</Route>
          <Route path="/agent">{withPermission('rd_studio:read', <RdStudio />)}</Route>
          <Route path="/operations"><Redirect to="/super-assistant" replace /></Route>
          <Route path="/operations/assistant"><Redirect to="/super-assistant" replace /></Route>
          <Route path="/operations/copilot"><Redirect to="/super-assistant" replace /></Route>
          <Route path="/operations/tasks">{withPermission('operations_tasks:read', <OperationsTasks />)}</Route>
          <Route path="/operations/collection"><Redirect to="/operations/tasks" replace /></Route>
          <Route path="/operations/materials">{withPermission('operations_materials:read', <OperationsMaterials />)}</Route>
          <Route path="/operations/governance">{withPermission('operations_governance:read', <OperationsGovernance />)}</Route>
          <Route path="/operations/insights"><Redirect to="/super-assistant" replace /></Route>
          <Route path="/operations/workshop"><Redirect to="/super-assistant" replace /></Route>
          <Route path="/operations/settings"><Redirect to="/super-assistant" replace /></Route>
          <Route path="/pipeline">{withPermission('pipeline:read', <Pipeline />)}</Route>
          <Route path="/projects">{withPermission('projects:read', <Projects />)}</Route>
          <Route path="/rd/specs">{withPermission('rd_specs:read', <RdSpecs />)}</Route>
          <Route path="/rd/quality">{withPermission('rd_quality:read', <RdQuality />)}</Route>
          <Route path="/rd/agents">{withPermission('rd_agents:read', <RdAgents />)}</Route>
          <Route path="/nl2sql">{withPermission('nl2sql_explore:read', <Nl2sql />)}</Route>
          <Route path="/nl2sql/attribution"><Redirect to="/super-assistant" replace /></Route>
          <Route path="/nl2sql/sql-knowledge">{withPermission('nl2sql_management:read', <SqlKnowledgeBase />)}</Route>
          <Route path="/nl2sql/management">{withPermission('nl2sql_management:read', <Nl2sqlManagement />)}</Route>
          <Route path="/nl2sql/schema-changes">{withPermission('nl2sql_management:read', <Nl2sqlSchemaChanges />)}</Route>
          <Route path="/nl2sql/analytics">{withPermission('nl2sql_analytics:read', <Nl2sqlAnalytics />)}</Route>
          <Route path="/datasources">{withPermission('datasources:read', <DataSources />)}</Route>
          <Route path="/users">{withPermission('users:read', <Users />)}</Route>
          <Route path="/tenants">{withPermission('tenant:read', <Tenants />)}</Route>
          <Route path="/*"><Redirect to="/dashboard" replace /></Route>
        </Switch>
      </Layout>
    </RequireAuth>
  );
}

export default function App() {
  return (
    <SetupGate>
      <Suspense fallback={<PageLoading />}>
        <Switch>
          <Route path="/login"><Login /></Route>
          <Route path="/setup"><Setup /></Route>
          <Route path="/invite"><InvitePage /></Route>
          <Route path="/preview/share"><SharePreview /></Route>
          <Route path="/*"><ProtectedApp /></Route>
        </Switch>
      </Suspense>
    </SetupGate>
  );
}
