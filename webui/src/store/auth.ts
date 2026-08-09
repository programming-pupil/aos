import { create } from 'zustand';
import type { UserInfo } from '@/types';
import { usePermissions } from './permissions';

interface AuthState {
  token: string | null;
  tenantId: string | null;
  user: UserInfo | null;
  isAuthenticated: boolean;
  login: (token: string, user: UserInfo) => void;
  logout: () => void;
  switchTenant: (token: string, user: UserInfo) => void;
}

function extractTenantId(token: string): string | null {
  try {
    const decoded = decodeJwtPayload(token);
    return typeof decoded.tenant_id === 'string' ? decoded.tenant_id : null;
  } catch {
    return null;
  }
}

function decodeJwtPayload(token: string): Record<string, unknown> {
  const payload = token.split('.')[1];
  if (!payload) {
    throw new Error('JWT payload missing');
  }
  const normalized = payload.replace(/-/g, '+').replace(/_/g, '/');
  const padded = normalized.padEnd(Math.ceil(normalized.length / 4) * 4, '=');
  return JSON.parse(atob(padded));
}

export function isTokenExpired(token: string, clockSkewSeconds = 30): boolean {
  try {
    const decoded = decodeJwtPayload(token);
    const rawExp = decoded.exp;
    const exp = typeof rawExp === 'number' ? rawExp : Number(rawExp);
    if (!Number.isFinite(exp)) return true;
    return exp <= Math.floor(Date.now() / 1000) + clockSkewSeconds;
  } catch {
    return true;
  }
}

function clearStoredAuth(): void {
  localStorage.removeItem('token');
  localStorage.removeItem('user');
  localStorage.removeItem('tenant_id');
}

export function getStoredAuthToken(): string | null {
  const token = localStorage.getItem('token');
  if (!token) return null;
  if (isTokenExpired(token)) {
    clearStoredAuth();
    return null;
  }
  return token;
}

export const useAuthStore = create<AuthState>((set) => {
  const storedToken = getStoredAuthToken();
  const storedTenantId = storedToken ? extractTenantId(storedToken) : null;
  const storedUser = storedToken
    ? (JSON.parse(localStorage.getItem('user') || 'null') as UserInfo | null)
    : null;

  // Sync permissions on startup if already logged in
  if (storedUser?.role) {
    usePermissions
      .getState()
      .setUserPermissions(
        storedUser.role,
        storedUser.menu_permissions_inherited ?? true,
        storedUser.menu_permissions ?? []
      );
  }

  return {
    token: storedToken,
    tenantId: storedTenantId,
    user: storedUser,
    isAuthenticated: !!storedToken,

    login: (token, user) => {
      const tenantId = extractTenantId(token);
      localStorage.setItem('token', token);
      localStorage.setItem('user', JSON.stringify(user));
      localStorage.setItem('tenant_id', tenantId ?? '');
      usePermissions
        .getState()
        .setUserPermissions(
          user.role,
          user.menu_permissions_inherited ?? true,
          user.menu_permissions ?? []
        );
      set({ token, tenantId, user, isAuthenticated: true });
    },

    logout: () => {
      clearStoredAuth();
      usePermissions.getState().setPermissions([]);
      set({ token: null, tenantId: null, user: null, isAuthenticated: false });
    },

    switchTenant: (token, user) => {
      const tenantId = extractTenantId(token);
      localStorage.setItem('token', token);
      localStorage.setItem('user', JSON.stringify(user));
      localStorage.setItem('tenant_id', tenantId ?? '');
      usePermissions
        .getState()
        .setUserPermissions(
          user.role,
          user.menu_permissions_inherited ?? true,
          user.menu_permissions ?? []
        );
      set({ token, tenantId, user, isAuthenticated: true });
    },
  };
});
