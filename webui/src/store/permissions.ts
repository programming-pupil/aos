import { create } from 'zustand';

/** All available permissions in the AOS system. */
export type Permission =
  | 'dashboard:read'
  | 'super_assistant:read'
  | 'workspace:read'
  | 'chat:read'
  | 'chat:write'
  | 'adversarial:read'
  | 'watchdog:read'
  | 'watchdog:write'
  | 'watchdog:admin'
  | 'tasks:read'
  | 'tasks:control'
  | 'tasks:admin'
  | 'rd_studio:read'
  | 'rd_specs:read'
  | 'rd_quality:read'
  | 'rd_agents:read'
  | 'agent:read'
  | 'agent:write'
  | 'projects:read'
  | 'projects:write'
  | 'projects:delete'
  | 'datasources:read'
  | 'datasources:write'
  | 'datasources:delete'
  | 'nl2sql_explore:read'
  | 'nl2sql_management:read'
  | 'nl2sql_analytics:read'
  | 'nl2sql:read'
  | 'nl2sql:write'
  | 'nl2sql:audit'
  | 'pipeline:read'
  | 'pipeline:write'
  | 'rd:admin'
  | 'operations_assistant:read'
  | 'operations_tasks:read'
  | 'operations_materials:read'
  | 'operations_governance:read'
  | 'operations_governance:write'
  | 'operations:read'
  | 'apikeys:read'
  | 'apikeys:write'
  | 'apikeys:delete'
  | 'mcp:read'
  | 'mcp:write'
  | 'mcp:delete'
  | 'skills:read'
  | 'skills:write'
  | 'skills:delete'
  | 'search_providers:read'
  | 'hooks:read'
  | 'hooks:write'
  | 'hooks:delete'
  | 'bot_agents:read'
  | 'bot_agents:write'
  | 'bot_agents:delete'
  | 'users:read'
  | 'users:write'
  | 'users:delete'
  | 'tenant:read'
  | 'tenant:write'
  | 'config:read';

export const MENU_READ_PERMISSIONS: Permission[] = [
  'dashboard:read',
  'super_assistant:read',
  'workspace:read',
  'chat:read',
  'adversarial:read',
  'tasks:read',
  'rd_studio:read',
  'rd_specs:read',
  'operations_assistant:read',
  'watchdog:read',
  'operations_tasks:read',
  'operations_materials:read',
  'operations_governance:read',
  'projects:read',
  'pipeline:read',
  'rd_quality:read',
  'rd_agents:read',
  'nl2sql_explore:read',
  'datasources:read',
  'nl2sql_management:read',
  'nl2sql_analytics:read',
  'mcp:read',
  'skills:read',
  'search_providers:read',
  'hooks:read',
  'bot_agents:read',
  'apikeys:read',
  'users:read',
  'config:read',
];

const HIDDEN_CONFIGURABLE_MENU_PERMISSIONS = new Set<Permission>([
  // Legacy entry points are now served by the unified Super Assistant menu.
  'chat:read',
  'adversarial:read',
  'operations_assistant:read',
  'watchdog:read',
  'tasks:read',
  'rd_studio:read',
  // Legacy implementation surfaces are hidden from navigation. Keep their
  // permission identifiers for old JWTs/API records, but never offer them as
  // assignable menu grants.
  'agent:read',
  'pipeline:read',
  'rd_quality:read',
  'rd_agents:read',
]);

export const MENU_CONFIGURABLE_PERMISSIONS: Permission[] = MENU_READ_PERMISSIONS.filter(
  (permission) => !HIDDEN_CONFIGURABLE_MENU_PERMISSIONS.has(permission),
);

/** Maps roles → sets of permissions. */
const ROLE_PERMISSIONS: Record<string, Permission[]> = {
  admin: [
    'dashboard:read',
    'super_assistant:read',
    'workspace:read',
    'chat:read', 'chat:write',
    'adversarial:read',
    'watchdog:read', 'watchdog:write', 'watchdog:admin',
    'tasks:read', 'tasks:control', 'tasks:admin',
    'rd_studio:read', 'rd_specs:read', 'rd_quality:read', 'rd_agents:read',
    'agent:read', 'agent:write',
    'projects:read', 'projects:write', 'projects:delete',
    'rd:admin',
    'datasources:read', 'datasources:write', 'datasources:delete',
    'nl2sql_explore:read', 'nl2sql_management:read', 'nl2sql_analytics:read',
    'nl2sql:read', 'nl2sql:write', 'nl2sql:audit',
    'pipeline:read', 'pipeline:write',
    'operations_assistant:read', 'operations_tasks:read', 'operations_materials:read', 'operations_governance:read', 'operations_governance:write',
    'operations:read',
    'apikeys:read', 'apikeys:write', 'apikeys:delete',
    'mcp:read', 'mcp:write', 'mcp:delete',
    'skills:read', 'skills:write', 'skills:delete',
    'search_providers:read',
    'hooks:read', 'hooks:write', 'hooks:delete',
    'bot_agents:read', 'bot_agents:write', 'bot_agents:delete',
    'users:read', 'users:write', 'users:delete',
    'tenant:read', 'tenant:write',
    'config:read',
  ],
  developer: [
    'dashboard:read',
    'super_assistant:read',
    'workspace:read',
    'chat:read', 'chat:write',
    'adversarial:read',
    'watchdog:read', 'watchdog:write',
    'tasks:read', 'tasks:control',
    'rd_studio:read', 'rd_specs:read', 'rd_quality:read',
    'agent:read', 'agent:write',
    'projects:read', 'projects:write',
    'datasources:read', 'datasources:write',
    'nl2sql_explore:read', 'nl2sql_management:read', 'nl2sql_analytics:read',
    'nl2sql:read',
    'pipeline:read',
    'operations_assistant:read', 'operations_tasks:read', 'operations_materials:read', 'operations_governance:read',
    'operations:read',
    'apikeys:read',
    'mcp:read',
    'skills:read',
    'search_providers:read',
    'hooks:read',
    'bot_agents:read',
    'config:read',
  ],
  viewer: [
    'dashboard:read',
    'super_assistant:read',
    'workspace:read',
    'chat:read',
    'adversarial:read',
    'watchdog:read',
    'tasks:read',
    'rd_studio:read', 'rd_specs:read', 'rd_quality:read',
    'agent:read',
    'operations_assistant:read', 'operations_tasks:read', 'operations_materials:read', 'operations_governance:read',
    'operations:read',
    'projects:read',
    'mcp:read',
    'skills:read',
    'config:read',
  ],
  superadmin: [
    'dashboard:read',
    'super_assistant:read',
    'workspace:read',
    'chat:read', 'chat:write',
    'adversarial:read',
    'watchdog:read', 'watchdog:write', 'watchdog:admin',
    'tasks:read', 'tasks:control', 'tasks:admin',
    'rd_studio:read', 'rd_specs:read', 'rd_quality:read', 'rd_agents:read',
    'agent:read', 'agent:write',
    'projects:read', 'projects:write', 'projects:delete',
    'rd:admin',
    'datasources:read', 'datasources:write', 'datasources:delete',
    'nl2sql_explore:read', 'nl2sql_management:read', 'nl2sql_analytics:read',
    'nl2sql:read', 'nl2sql:write', 'nl2sql:audit',
    'pipeline:read', 'pipeline:write',
    'operations_assistant:read', 'operations_tasks:read', 'operations_materials:read', 'operations_governance:read', 'operations_governance:write',
    'operations:read',
    'apikeys:read', 'apikeys:write', 'apikeys:delete',
    'mcp:read', 'mcp:write', 'mcp:delete',
    'skills:read', 'skills:write', 'skills:delete',
    'search_providers:read',
    'hooks:read', 'hooks:write', 'hooks:delete',
    'bot_agents:read', 'bot_agents:write', 'bot_agents:delete',
    'users:read', 'users:write', 'users:delete',
    'tenant:read', 'tenant:write',
    'config:read',
  ],
};

export function getRolePermissions(role: string): Permission[] {
  return ROLE_PERMISSIONS[role] ?? [];
}

const LEGACY_MENU_PERMISSION_EXPANSIONS: Record<string, Permission[]> = {
  'rd:read': ['rd_studio:read', 'rd_specs:read', 'projects:read', 'pipeline:read', 'rd_quality:read'],
  'agent:read': ['rd_studio:read', 'rd_specs:read', 'rd_quality:read'],
  'chat:read': ['super_assistant:read', 'chat:read'],
  'adversarial:read': ['super_assistant:read', 'adversarial:read'],
  'operations_assistant:read': ['super_assistant:read', 'operations_assistant:read'],
  'operations:read': [
    'super_assistant:read',
    'operations_assistant:read',
    'operations_tasks:read',
    'operations_materials:read',
    'operations_governance:read',
  ],
  'operations_governance:write': ['operations_governance:read', 'operations_governance:write'],
  'watchdog:read': ['tasks:read'],
  'watchdog:write': ['tasks:read', 'tasks:control'],
  'watchdog:admin': ['tasks:read', 'tasks:control', 'tasks:admin'],
  'nl2sql:read': ['nl2sql_explore:read', 'nl2sql_management:read', 'nl2sql_analytics:read'],
  'rd:admin': ['rd_agents:read'],
};

export function normalizeMenuPermissionsForUi(menuPermissions?: string[] | null): Permission[] {
  const normalized = new Set<Permission>();
  for (const permission of menuPermissions ?? []) {
    const value = permission.trim();
    if (!value) continue;
    const expanded = LEGACY_MENU_PERMISSION_EXPANSIONS[value] ?? [value as Permission];
    for (const expandedPermission of expanded) {
      if (MENU_CONFIGURABLE_PERMISSIONS.includes(expandedPermission)) {
        normalized.add(expandedPermission);
      }
    }
  }
  return Array.from(normalized);
}

interface PermissionsState {
  /** Cached permissions for the current user. */
  permissions: Set<Permission>;
  /** Update permissions when role changes. */
  setRole: (role: string) => void;
  /** Update permissions from a user payload and optional custom menu permissions. */
  setUserPermissions: (role: string, inherited: boolean, menuPermissions?: string[] | null) => void;
  /** Replace the current permission set directly. */
  setPermissions: (permissions: string[]) => void;
  /** Check if the current user has a specific permission. */
  hasPermission: (permission: Permission) => boolean;
  /** Check if the current user has any of the given permissions. */
  hasAnyPermission: (permissions: Permission[]) => boolean;
  /** Check if the current user has all of the given permissions. */
  hasAllPermissions: (permissions: Permission[]) => boolean;
  /** Whether the current user is an admin (or superadmin). */
  isAdmin: () => boolean;
}

export const usePermissions = create<PermissionsState>((set, get) => ({
  permissions: new Set(),

  setRole: (role: string) => {
    const perms = getRolePermissions(role);
    set({ permissions: new Set(perms) });
  },

  setUserPermissions: (role: string, inherited: boolean, menuPermissions?: string[] | null) => {
    const rolePerms = getRolePermissions(role);
    const normalizedMenuPermissions = normalizeMenuPermissionsForUi(menuPermissions);
    const perms = inherited
      ? rolePerms
      : [
          ...rolePerms.filter((permission) => !MENU_READ_PERMISSIONS.includes(permission)),
          ...normalizedMenuPermissions,
        ];
    set({ permissions: new Set(perms as Permission[]) });
  },

  setPermissions: (permissions: string[]) => {
    set({ permissions: new Set([
      ...(permissions as Permission[]),
      ...normalizeMenuPermissionsForUi(permissions),
    ]) });
  },

  hasPermission: (permission: Permission) => {
    return get().permissions.has(permission);
  },

  hasAnyPermission: (permissions: Permission[]) => {
    const perms = get().permissions;
    return permissions.some((p) => perms.has(p));
  },

  hasAllPermissions: (permissions: Permission[]) => {
    const perms = get().permissions;
    return permissions.every((p) => perms.has(p));
  },

  isAdmin: () => {
    return get().hasAnyPermission(['users:write', 'tenant:write']);
  },
}));

/**
 * Menu item visibility guard — used in Layout.tsx.
 * Return false to hide an item from non-admin users.
 */
export function canViewItem(
  permission: Permission | null,
  role: string,
): boolean {
  if (!permission) return true;
  const perms = ROLE_PERMISSIONS[role] ?? [];
  return perms.includes(permission);
}
