import { client } from './client';
import type {
  InviteUserResponse,
  LoginResponse,
  NotificationListResponse,
  SetupResponse,
  SetupStatusResponse,
  UserInfo,
  UserListResponse,
} from '@/types';

export const authApi = {
  login: (data: { email: string; password: string }) =>
    client.post<LoginResponse>('/auth/login', data).then((r) => r.data),

  register: (data: { email: string; password: string; name: string }) =>
    client.post<LoginResponse>('/auth/register', data).then((r) => r.data),

  me: () => client.get<UserInfo>('/auth/me').then((r) => r.data),

  logout: () => client.post('/auth/logout').then((r) => r.data),

  changePassword: (old_password: string, new_password: string) =>
    client.post('/auth/change-password', { old_password, new_password }).then((r) => r.data),

  acceptInvite: (data: { password: string; invite_token: string }) =>
    client.post('/auth/accept-invite', data).then((r) => r.data),
};

export const setupApi = {
  check: () =>
    client.get<SetupStatusResponse>('/setup/check', { headers: {} }).then((r) => r.data),

  init: (data: {
    tenant_name: string;
    tenant_slug: string;
    admin_email: string;
    admin_name: string;
    admin_password: string;
  }) =>
    client.post<SetupResponse>('/setup', data).then((r) => r.data),
};

export const usersApi = {
  list: (params?: { page?: number; per_page?: number }) =>
    client.get<UserListResponse>('/users', { params }).then((r) => r.data),

  me: () => client.get<UserInfo>('/users/me').then((r) => r.data),

  get: (id: string) =>
    client.get<UserInfo>(`/users/${encodeURIComponent(id)}`).then((r) => r.data),

  update: (id: string, data: {
    name?: string;
    role?: string;
    is_active?: boolean;
    permission_mode?: string;
    menu_permissions_inherited?: boolean;
    menu_permissions?: string[];
  }) =>
    client.patch<UserInfo>(`/users/${encodeURIComponent(id)}`, data).then((r) => r.data),

  delete: (id: string) =>
    client.delete(`/users/${encodeURIComponent(id)}`).then((r) => r.data),

  invite: (data: { email: string; name: string; role: string; invite: boolean; menu_permissions?: string[] }) =>
    client.post<InviteUserResponse>('/users', data).then((r) => r.data),

  sendResetEmail: (id: string) =>
    client.post<{
      success: boolean;
      reset_url?: string;
      email_configured: boolean;
      email_sent: boolean;
      email_error?: string | null;
    }>('/users/send-reset-email', { user_id: id }).then((r) => r.data),
};

export const notificationsApi = {
  list: (params?: { page?: number; per_page?: number; read?: string }) =>
    client.get<NotificationListResponse>('/notifications', { params }).then((r) => r.data),

  markRead: (id: string, read: boolean) =>
    client.patch<{ id: string; read: boolean }>(`/notifications/${encodeURIComponent(id)}/read`, { read }).then((r) => r.data),

  markAllRead: () =>
    client.patch<{ success: boolean }>('/notifications/mark-all-read', {}).then((r) => r.data),

  delete: (id: string) =>
    client.delete(`/notifications/${encodeURIComponent(id)}`).then((r) => r.data),
};
