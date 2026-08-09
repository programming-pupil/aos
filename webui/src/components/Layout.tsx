import { Suspense, useState, useEffect, useCallback, useMemo } from "react";
import type { ReactNode } from "react";
import { useNavigate, useLocation } from "@/router";
import {
  Avatar,
  Dropdown,
  Typography,
  Space,
  Tooltip,
  Badge,
  message,
  Modal,
  Form,
  Input,
  Button,
} from "antd";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  DashboardOutlined,
  RobotOutlined,
  FolderOpenOutlined,
  AppstoreOutlined,
  KeyOutlined,
  LogoutOutlined,
  UserOutlined,
  TeamOutlined,
  SwapOutlined,
  GlobalOutlined,
  DatabaseOutlined,
  ThunderboltOutlined,
  LockOutlined,
  BarChartOutlined,
  SettingOutlined,
  SafetyCertificateOutlined,
  SearchOutlined,
  SolutionOutlined,
  FundProjectionScreenOutlined,
  ReadOutlined,
  ControlOutlined,
  StarOutlined,
  DeploymentUnitOutlined,
  ApiOutlined,
  CodeOutlined,
  DesktopOutlined,
  LineChartOutlined,
  MessageOutlined,
} from "@ant-design/icons";
import { useAuthStore } from "@/store/auth";
import { tenantsApi, authApi } from "@/api";
import { queryKeys } from "@/api/queryKeys";
import { usePermissions, type Permission } from "@/store/permissions";
import { CommandPalette } from "@/components/CommandPalette";
import { LanguageSwitcher } from "@/components/LanguageSwitcher";
import {
  NotificationBell,
  NotificationsDrawer,
} from "@/components/Notifications";
import { notificationsApi } from "@/api";
import { useSystemEvents } from "@/api/systemEvents";
import { useTranslation } from "react-i18next";
import type { MenuProps } from "antd";
import type { TenantInfo } from "@/types";

const { Text } = Typography;

function ContentLoading({ label }: { label: string }) {
  return (
    <div
      style={{
        minHeight: "100%",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
        background:
          "radial-gradient(circle at 30% 20%, rgba(124, 58, 237, 0.10), transparent 28%), var(--bg-void)",
      }}
    >
      <div
        style={{
          padding: "14px 18px",
          borderRadius: 12,
          border: "1px solid var(--border-subtle)",
          background: "var(--bg-surface)",
          color: "var(--text-secondary)",
          boxShadow: "var(--shadow-card)",
          fontSize: 13,
        }}
      >
        {label}
      </div>
    </div>
  );
}

type MenuItem = {
  key: string;
  icon: React.ReactNode;
  labelKey: string;
  groupKey: string;
  permission: Permission | null;
  children?: MenuItem[];
};

const ALL_MENU_ITEMS: MenuItem[] = [
  // Unified super-assistant entry — merges AI Chat, Ops Assistant and Data
  // Attribution into a single entry point (Req 1.1). Chat-question capability is
  // lifted up here; the four main-domain menus keep only base-config items (Req 1.4).
  {
    key: "/dashboard",
    icon: <DashboardOutlined />,
    labelKey: "nav.dashboard",
    groupKey: "nav.groups.workspace",
    permission: "dashboard:read",
  },
  {
    key: "/super-assistant",
    icon: <RobotOutlined />,
    labelKey: "nav.superAssistant",
    groupKey: "nav.groups.aiChat",
    permission: "super_assistant:read",
  },
  {
    key: "/agent",
    icon: <CodeOutlined />,
    labelKey: "nav.agent",
    groupKey: "nav.groups.dev",
    permission: "rd_studio:read",
  },
  {
    key: "/rd/specs",
    icon: <SolutionOutlined />,
    labelKey: "nav.rdSpecs",
    groupKey: "nav.groups.dev",
    permission: "rd_specs:read",
  },
  {
    key: "/operations/tasks",
    icon: <GlobalOutlined />,
    labelKey: "nav.operationsTasks",
    groupKey: "nav.groups.operations",
    permission: "operations_tasks:read",
  },
  {
    key: "/operations/materials",
    icon: <AppstoreOutlined />,
    labelKey: "nav.operationsMaterials",
    groupKey: "nav.groups.operations",
    permission: "operations_materials:read",
  },
  {
    key: "/operations/governance",
    icon: <LockOutlined />,
    labelKey: "nav.operationsGovernance",
    groupKey: "nav.groups.operations",
    permission: "operations_governance:read",
  },
  {
    key: "/projects",
    icon: <FolderOpenOutlined />,
    labelKey: "nav.projects",
    groupKey: "nav.groups.dev",
    permission: "projects:read",
  },
  {
    key: "/pipeline",
    icon: <ThunderboltOutlined />,
    labelKey: "nav.pipeline",
    groupKey: "nav.groups.dev",
    permission: "pipeline:read",
  },
  {
    key: "/rd/quality",
    icon: <BarChartOutlined />,
    labelKey: "nav.rdQuality",
    groupKey: "nav.groups.dev",
    permission: "rd_quality:read",
  },
  {
    key: "/rd/agents",
    icon: <SafetyCertificateOutlined />,
    labelKey: "nav.rdAgents",
    groupKey: "nav.groups.dev",
    permission: "rd_agents:read",
  },
  {
    key: "/nl2sql",
    icon: <FundProjectionScreenOutlined />,
    labelKey: "nav.nl2sql",
    groupKey: "nav.groups.dataAnalysis",
    permission: "nl2sql_explore:read",
  },
  {
    key: "/datasources",
    icon: <DatabaseOutlined />,
    labelKey: "nav.datasources",
    groupKey: "nav.groups.dataAnalysis",
    permission: "datasources:read",
  },
  {
    key: "/nl2sql/sql-knowledge",
    icon: <ReadOutlined />,
    labelKey: "nav.sqlKnowledge",
    groupKey: "nav.groups.dataAnalysis",
    permission: "nl2sql_management:read",
  },
  {
    key: "/nl2sql/management",
    icon: <ControlOutlined />,
    labelKey: "nav.nl2sqlManagement",
    groupKey: "nav.groups.dataAnalysis",
    permission: "nl2sql_management:read",
  },
  {
    key: "/nl2sql/analytics",
    icon: <LineChartOutlined />,
    labelKey: "nav.nl2sqlAnalytics",
    groupKey: "nav.groups.dataAnalysis",
    permission: "nl2sql_analytics:read",
  },
  {
    key: "/hooks",
    icon: <ApiOutlined />,
    labelKey: "nav.hooks",
    groupKey: "nav.groups.extensions",
    permission: "hooks:read",
  },
  {
    key: "/skills",
    icon: <StarOutlined />,
    labelKey: "nav.skills",
    groupKey: "nav.groups.extensions",
    permission: "skills:read",
  },
  {
    key: "/search-providers",
    icon: <SearchOutlined />,
    labelKey: "nav.searchProviders",
    groupKey: "nav.groups.extensions",
    permission: "search_providers:read",
  },
  {
    key: "/mcp",
    icon: <DeploymentUnitOutlined />,
    labelKey: "nav.mcp",
    groupKey: "nav.groups.extensions",
    permission: "mcp:read",
  },
  {
    key: "/workspace",
    icon: <DesktopOutlined />,
    labelKey: "nav.workspace",
    groupKey: "nav.groups.system",
    permission: "workspace:read",
  },
  {
    key: "/keys",
    icon: <KeyOutlined />,
    labelKey: "nav.apikeys",
    groupKey: "nav.groups.system",
    permission: "apikeys:read",
  },
  {
    key: "/config/management",
    icon: <SettingOutlined />,
    labelKey: "nav.configManagement",
    groupKey: "nav.groups.system",
    permission: "config:read",
  },
  {
    key: "/bot-agents",
    icon: <MessageOutlined />,
    labelKey: "nav.botAgents",
    groupKey: "nav.groups.system",
    permission: "bot_agents:read",
  },
  {
    key: "/users",
    icon: <TeamOutlined />,
    labelKey: "nav.users",
    groupKey: "nav.groups.system",
    permission: "users:read",
  },
];

const GROUP_ORDER_KEYS = [
  "nav.groups.workspace",
  "nav.groups.aiChat",
  "nav.groups.operations",
  "nav.groups.dev",
  "nav.groups.dataAnalysis",
  "nav.groups.extensions",
  "nav.groups.system",
];

export default function Layout({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const location = useLocation();
  const queryClient = useQueryClient();
  const [collapsed, setCollapsed] = useState(false);
  const [commandOpen, setCommandOpen] = useState(false);
  const [notifOpen, setNotifOpen] = useState(false);
  const [tenantSwitchModalOpen, setTenantSwitchModalOpen] = useState(false);
  const [switchTargetTenant, setSwitchTargetTenant] =
    useState<TenantInfo | null>(null);
  const [form] = Form.useForm();
  const { user, logout, tenantId, login } = useAuthStore();
  const permissions = usePermissions((state) => state.permissions);
  const hasPermission = useCallback(
    (permission: Permission) => permissions.has(permission),
    [permissions],
  );

  useSystemEvents({
    autoConnect: true,
    onMcpUpdated: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.list() });
      queryClient.invalidateQueries({ queryKey: queryKeys.mcp.stats() });
      queryClient.invalidateQueries({ queryKey: queryKeys.chatSessions.all });
    },
    onSkillsUpdated: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.skills.all });
      queryClient.invalidateQueries({ queryKey: queryKeys.commands.all });
      queryClient.invalidateQueries({ queryKey: queryKeys.chatSessions.all });
    },
    onSearchProvidersUpdated: () => {
      queryClient.invalidateQueries({
        queryKey: queryKeys.pm.searchProviders(),
      });
      queryClient.invalidateQueries({ queryKey: queryKeys.pm.searchDoctor() });
      queryClient.invalidateQueries({ queryKey: queryKeys.chatSessions.all });
    },
    onHooksUpdated: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.hooks.all });
    },
  });

  // Tenant list query (only for users who can read tenants)
  const { data: tenantsData, isLoading: tenantsLoading } = useQuery({
    queryKey: queryKeys.tenants.list(),
    queryFn: () => tenantsApi.list({ per_page: 100 }),
    enabled: hasPermission("tenant:read"),
    retry: false,
  });

  // Current tenant name
  const currentTenant = tenantsData?.tenants.find((t) => t.id === tenantId);
  const tenantName = currentTenant?.name ?? t("tenants.defaultName");

  // Re-login mutation for tenant switching
  const reloginMutation = useMutation({
    mutationFn: (data: { email: string; password: string }) =>
      authApi.login(data),
    onSuccess: (data) => {
      queryClient.clear();
      login(data.token, data.user);
      message.success(
        t("tenants.switchedTo", { name: switchTargetTenant?.name }),
      );
      setTenantSwitchModalOpen(false);
      setSwitchTargetTenant(null);
      form.resetFields();
    },
    onError: () => {
      message.error(t("auth.loginFailed"));
    },
  });

  // Open tenant switch: show re-login modal instead of blindly reloading
  const handleTenantSwitch = (tenant: TenantInfo) => {
    if (tenant.id === tenantId) return;
    setSwitchTargetTenant(tenant);
    setTenantSwitchModalOpen(true);
  };

  // Filter menu items based on current permissions (keep parent if any child is visible)
  const visibleMenuItems = useMemo(() => {
    const hiddenProductSurface = new Set(["/agent", "/pipeline", "/rd/quality"]);
    const filterItems = (items: MenuItem[]): MenuItem[] => {
      const result: MenuItem[] = [];
      items.forEach((item) => {
        if (hiddenProductSurface.has(item.key)) return;
        const visibleBySelf =
          !item.permission || hasPermission(item.permission);
        const visibleChildren = item.children
          ? filterItems(item.children)
          : undefined;
        const hasVisibleChildren = !!visibleChildren?.length;
        if (visibleBySelf || hasVisibleChildren) {
          result.push({
            ...item,
            children: visibleChildren,
          });
        }
      });
      return result;
    };
    return filterItems(ALL_MENU_ITEMS);
  }, [hasPermission]);

  // Unread notification count
  const { data: notifData } = useQuery({
    queryKey: queryKeys.notifications.list({ per_page: 1 }),
    queryFn: () => notificationsApi.list({ per_page: 1 }),
    refetchInterval: 30_000,
    retry: false,
  });

  const handleKeyDown = useCallback((e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "k") {
      e.preventDefault();
      setCommandOpen((o) => !o);
    }
  }, []);

  useEffect(() => {
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [handleKeyDown]);

  const userMenu: MenuProps["items"] = [
    {
      key: "profile",
      icon: <UserOutlined />,
      label: user?.name ?? user?.email,
    },
    { type: "divider" },
    {
      key: "logout",
      icon: <LogoutOutlined />,
      label: t("auth.logout"),
      danger: true,
    },
  ];

  const handleUserMenuClick: MenuProps["onClick"] = ({ key }) => {
    if (key === "logout") {
      queryClient.clear();
      logout();
      navigate("/login");
    }
  };

  const refreshQueriesForRoute = useCallback(
    (targetKey: string) => {
      let prefixes: readonly ReadonlyArray<unknown>[] = [];
      if (targetKey.startsWith("/dashboard")) {
        prefixes = [queryKeys.dashboard.all];
      } else if (targetKey.startsWith("/chat")) {
        prefixes = [
          queryKeys.chatSessions.all,
          queryKeys.agentSessions.all,
          queryKeys.commands.all,
        ];
      } else if (targetKey.startsWith("/adversarial")) {
        prefixes = [queryKeys.chatAdversarial.all, queryKeys.apiKeys.all];
      } else if (targetKey.startsWith("/tasks")) {
        prefixes = [queryKeys.tasks.all];
      } else if (
        targetKey.startsWith("/agent-ops") ||
        targetKey.startsWith("/watchdog")
      ) {
        prefixes = [queryKeys.agentOps.all];
      } else if (targetKey.startsWith("/config")) {
        prefixes = [queryKeys.config.all];
      } else if (
        targetKey.startsWith("/agent") ||
        targetKey.startsWith("/projects") ||
        targetKey.startsWith("/pipeline") ||
        targetKey.startsWith("/rd")
      ) {
        prefixes = [
          queryKeys.rd.all,
          queryKeys.projects.all,
          queryKeys.agentSessions.all,
        ];
      } else if (targetKey.startsWith("/operations")) {
        prefixes = [queryKeys.agentSessions.all, queryKeys.dashboard.all];
      } else if (
        targetKey.startsWith("/nl2sql") ||
        targetKey.startsWith("/datasources")
      ) {
        prefixes = [queryKeys.nl2sql.all, queryKeys.dataSources.all()];
      } else if (targetKey.startsWith("/hooks")) {
        prefixes = [queryKeys.hooks.all];
      } else if (targetKey.startsWith("/skills")) {
        prefixes = [queryKeys.skills.all];
      } else if (targetKey.startsWith("/mcp")) {
        prefixes = [queryKeys.mcp.all];
      } else if (targetKey.startsWith("/keys")) {
        prefixes = [queryKeys.apiKeys.all];
      } else if (targetKey.startsWith("/workspace")) {
        prefixes = [queryKeys.agentSessions.all];
      } else if (targetKey.startsWith("/users")) {
        prefixes = [queryKeys.users.all];
      } else if (targetKey.startsWith("/bot-agents")) {
        prefixes = [queryKeys.botAgents.all];
      }

      queryClient.refetchQueries({ type: "active" });
      prefixes.forEach((queryKey) => {
        queryClient.invalidateQueries({ queryKey });
        queryClient.refetchQueries({ queryKey, type: "active" });
      });
      window.dispatchEvent(
        new CustomEvent("aos-route-refresh", { detail: { path: targetKey } }),
      );
    },
    [queryClient],
  );

  const handleNavNavigate = (targetKey: string) => {
    refreshQueriesForRoute(targetKey);
    navigate(targetKey);
  };

  const siderWidth = collapsed ? 56 : 220;

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "row",
        height: "100vh",
        overflow: "hidden",
      }}
    >
      {/* Sidebar */}
      <aside
        style={{
          width: siderWidth,
          height: "100vh",
          background: "var(--bg-surface)",
          borderRight: "1px solid var(--border-subtle)",
          display: "flex",
          flexDirection: "column",
          flexShrink: 0,
          transition: "width var(--transition-base)",
          overflow: "hidden",
          zIndex: 10,
        }}
      >
        {/* Logo */}
        <div
          style={{
            height: "var(--topbar-height)",
            display: "flex",
            alignItems: "center",
            padding: "0 14px",
            borderBottom: "1px solid var(--border-subtle)",
            flexShrink: 0,
            gap: 8,
            cursor: "pointer",
            overflow: "hidden",
          }}
          onClick={() => setCollapsed(!collapsed)}
        >
          <div
            style={{
              width: 28,
              height: 28,
              borderRadius: 6,
              background:
                "linear-gradient(135deg, var(--accent-ai) 0%, #a855f7 100%)",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              flexShrink: 0,
              boxShadow: "var(--glow-purple-sm)",
            }}
          >
            <span
              style={{
                fontSize: 14,
                fontWeight: 800,
                color: "#fff",
                fontFamily: "var(--font-code)",
              }}
            >
              A
            </span>
          </div>
          {!collapsed && (
            <Text
              strong
              style={{
                fontSize: 15,
                fontWeight: 700,
                color: "var(--text-primary)",
                letterSpacing: 0.5,
                fontFamily: "var(--font-code)",
                whiteSpace: "nowrap",
              }}
            >
              {t("layout.brandName")}
            </Text>
          )}
        </div>

        {/* Nav */}
        <nav
          style={{
            flex: 1,
            overflowY: "auto",
            overflowX: "hidden",
            padding: "4px 0",
          }}
        >
          {GROUP_ORDER_KEYS.map((groupKey) => {
            const items = visibleMenuItems.filter(
              (item) => item.groupKey === groupKey,
            );
            if (!items.length) return null;
            return (
              <div key={groupKey}>
                {!collapsed && (
                  <div
                    style={{
                      padding: "10px 16px 4px",
                      fontSize: 10,
                      color: "var(--text-muted)",
                      fontWeight: 600,
                      textTransform: "uppercase" as const,
                      letterSpacing: "0.08em",
                    }}
                  >
                    {t(groupKey)}
                  </div>
                )}
                {items.map(({ key, icon, labelKey, children }) => {
                  const childActive = !!children?.some(
                    (child) => location.pathname === child.key,
                  );
                  const active = location.pathname === key || childActive;
                  return (
                    <div key={key}>
                      <div
                        onClick={() => handleNavNavigate(key)}
                        style={{
                          display: "flex",
                          alignItems: "center",
                          gap: 10,
                          padding: collapsed ? "10px 0" : "8px 16px",
                          justifyContent: collapsed ? "center" : "flex-start",
                          cursor: "pointer",
                          color: active
                            ? "var(--text-primary)"
                            : "var(--text-secondary)",
                          background: active
                            ? "var(--accent-ai-muted)"
                            : "transparent",
                          borderLeft: active
                            ? "2px solid var(--accent-ai)"
                            : "2px solid transparent",
                          transition: "all var(--transition-fast)",
                          fontSize: 13,
                        }}
                        onMouseEnter={(e) => {
                          if (!active) {
                            e.currentTarget.style.background =
                              "var(--bg-hover)";
                          }
                        }}
                        onMouseLeave={(e) => {
                          if (!active) {
                            e.currentTarget.style.background = "transparent";
                          }
                        }}
                      >
                        <span style={{ fontSize: 14, flexShrink: 0 }}>
                          {icon}
                        </span>
                        {!collapsed && (
                          <span
                            style={{
                              whiteSpace: "nowrap" as const,
                              overflow: "hidden",
                              textOverflow: "ellipsis",
                            }}
                          >
                            {t(labelKey)}
                          </span>
                        )}
                      </div>
                      {!collapsed && !!children?.length && (
                        <div style={{ paddingBottom: 2 }}>
                          {children.map((child) => {
                            const childIsActive =
                              location.pathname === child.key;
                            return (
                              <div
                                key={child.key}
                                onClick={() => handleNavNavigate(child.key)}
                                style={{
                                  display: "flex",
                                  alignItems: "center",
                                  gap: 8,
                                  padding: "6px 16px 6px 40px",
                                  cursor: "pointer",
                                  color: childIsActive
                                    ? "var(--text-primary)"
                                    : "var(--text-muted)",
                                  background: childIsActive
                                    ? "var(--bg-hover)"
                                    : "transparent",
                                  borderLeft: childIsActive
                                    ? "2px solid var(--accent-ai)"
                                    : "2px solid transparent",
                                  fontSize: 12,
                                  transition: "all var(--transition-fast)",
                                }}
                                onMouseEnter={(e) => {
                                  if (!childIsActive) {
                                    e.currentTarget.style.background =
                                      "var(--bg-hover)";
                                  }
                                }}
                                onMouseLeave={(e) => {
                                  if (!childIsActive) {
                                    e.currentTarget.style.background =
                                      "transparent";
                                  }
                                }}
                              >
                                <span style={{ fontSize: 12, flexShrink: 0 }}>
                                  {child.icon}
                                </span>
                                <span
                                  style={{
                                    whiteSpace: "nowrap" as const,
                                    overflow: "hidden",
                                    textOverflow: "ellipsis",
                                  }}
                                >
                                  {t(child.labelKey)}
                                </span>
                              </div>
                            );
                          })}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            );
          })}
        </nav>

        {/* Collapse toggle */}
        <div
          onClick={() => setCollapsed(!collapsed)}
          style={{
            padding: "10px 0",
            borderTop: "1px solid var(--border-subtle)",
            textAlign: "center",
            cursor: "pointer",
            color: "var(--text-muted)",
            fontSize: 14,
            flexShrink: 0,
          }}
        >
          <span
            style={{
              transition: "transform var(--transition-base)",
              display: "inline-block",
              transform: collapsed ? "rotate(180deg)" : "none",
            }}
          >
            ▶
          </span>
        </div>
      </aside>

      {/* Main */}
      <div
        style={{
          flex: 1,
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
          minWidth: 0,
        }}
      >
        {/* TopBar */}
        <header
          style={{
            height: "var(--topbar-height)",
            background: "var(--bg-surface)",
            borderBottom: "1px solid var(--border-subtle)",
            padding: "0 16px",
            display: "flex",
            alignItems: "center",
            flexShrink: 0,
            zIndex: 9,
            justifyContent: "flex-end",
          }}
        >
          <Space size={16}>
            {/* Tenant Switcher */}
            {hasPermission("tenant:read") && (
              <Dropdown
                menu={{
                  items:
                    tenantsData?.tenants.map((tenant) => ({
                      key: tenant.id,
                      icon: <GlobalOutlined />,
                      label: (
                        <Space>
                          <span>{tenant.name}</span>
                          {tenant.id === tenantId && <Badge status="success" />}
                        </Space>
                      ),
                      onClick: () => handleTenantSwitch(tenant),
                    })) ?? [],
                }}
                placement="bottomRight"
                trigger={["click"]}
              >
                <div
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 6,
                    cursor: "pointer",
                    padding: "4px 8px",
                    borderRadius: 6,
                    transition: "background var(--transition-fast)",
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.background = "var(--bg-hover)";
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.background = "transparent";
                  }}
                >
                  <GlobalOutlined
                    style={{ fontSize: 13, color: "var(--text-secondary)" }}
                  />
                  <Text
                    style={{
                      fontSize: 13,
                      color: "var(--text-secondary)",
                      maxWidth: 120,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    {tenantName}
                  </Text>
                  <SwapOutlined
                    style={{ fontSize: 10, color: "var(--text-muted)" }}
                  />
                </div>
              </Dropdown>
            )}
            <LanguageSwitcher />
            <NotificationBell
              onClick={() => setNotifOpen(true)}
              unreadCount={notifData?.unread_count ?? 0}
            />
            <Dropdown
              menu={{ items: userMenu, onClick: handleUserMenuClick }}
              placement="bottomRight"
            >
              <div
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  cursor: "pointer",
                }}
              >
                <Avatar
                  icon={<UserOutlined />}
                  size={28}
                  style={{ background: "var(--accent-ai)", flexShrink: 0 }}
                />
                <Text
                  style={{
                    fontSize: 13,
                    color: "var(--text-secondary)",
                    maxWidth: 120,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                >
                  {user?.name ?? user?.email ?? "User"}
                </Text>
              </div>
            </Dropdown>
          </Space>
        </header>

        {/* Content */}
        <main
          style={{
            flex: 1,
            overflow: "auto",
            background: "var(--bg-void)",
            height: "100%",
          }}
        >
          <Suspense fallback={<ContentLoading label={t("common.loading")} />}>
            {children}
          </Suspense>
        </main>
      </div>

      {/* Command Palette */}
      <CommandPalette
        open={commandOpen}
        onClose={() => setCommandOpen(false)}
      />

      {/* Notifications Drawer */}
      <NotificationsDrawer
        open={notifOpen}
        onClose={() => setNotifOpen(false)}
      />

      {/* Tenant Switch: re-login modal */}
      <Modal
        title={t("tenants.switchTo", { name: switchTargetTenant?.name })}
        open={tenantSwitchModalOpen}
        onCancel={() => {
          setTenantSwitchModalOpen(false);
          setSwitchTargetTenant(null);
          form.resetFields();
        }}
        footer={null}
        width={400}
      >
        <div
          style={{
            marginBottom: 16,
            color: "var(--text-secondary)",
            fontSize: 13,
          }}
        >
          {t("tenants.switchHint")}
        </div>
        <Form
          form={form}
          layout="vertical"
          onFinish={(values) => reloginMutation.mutate(values)}
        >
          <Form.Item
            name="email"
            label={t("auth.email")}
            rules={[{ required: true, type: "email" }]}
          >
            <Input prefix={<UserOutlined />} placeholder={t("auth.email")} />
          </Form.Item>
          <Form.Item
            name="password"
            label={t("auth.password")}
            rules={[{ required: true, min: 8 }]}
          >
            <Input.Password
              prefix={<LockOutlined />}
              placeholder={t("auth.password")}
            />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0, marginTop: 8 }}>
            <Space>
              <Button
                onClick={() => {
                  setTenantSwitchModalOpen(false);
                  setSwitchTargetTenant(null);
                  form.resetFields();
                }}
              >
                {t("common.cancel")}
              </Button>
              <Button
                type="primary"
                htmlType="submit"
                loading={reloginMutation.isPending}
              >
                {t("tenants.switchConfirm")}
              </Button>
            </Space>
          </Form.Item>
        </Form>
      </Modal>
    </div>
  );
}
