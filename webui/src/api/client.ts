import axios from 'axios';
import type { InternalAxiosRequestConfig } from 'axios';
import { message } from 'antd';
import { getStoredAuthToken, useAuthStore } from '@/store/auth';
import { ApiError, getHttpErrorMessage } from './errors';
import { queryClient } from '@/queryClient';

export const client = axios.create({
  baseURL: '/api/v1',
  timeout: 300_000,
});

export const fastClient = axios.create({
  baseURL: '/api/v1',
  timeout: 15_000,
});

let lastAuthRedirectAt = 0;

function isSetupRequired(status: number, detail: unknown): boolean {
  if (status === 428) return true;
  if (!detail || typeof detail !== 'object') return false;
  const obj = detail as Record<string, unknown>;
  const errorCode = typeof obj.error === 'string' ? obj.error.toLowerCase() : '';
  const messageText = typeof obj.message === 'string' ? obj.message.toLowerCase() : '';
  return errorCode === 'setup_required' || messageText.includes('setup_required');
}

function redirectToSetup(): void {
  if (typeof window === 'undefined') return;
  if (window.location.pathname === '/setup') return;
  window.location.href = '/setup';
}

function redirectToLogin(messageText: string): void {
  const now = Date.now();
  if (now - lastAuthRedirectAt < 1_500) return;
  lastAuthRedirectAt = now;
  message.error(messageText || '登录已过期，请重新登录');
  queryClient.clear();
  useAuthStore.getState().logout();
  if (typeof window !== 'undefined' && window.location.pathname !== '/login') {
    window.location.href = '/login';
  }
}

function attachAuthHeaders(config: InternalAxiosRequestConfig) {
  const token = getStoredAuthToken();
  if (token) {
    config.headers.Authorization = `Bearer ${token}`;
  }
  const tenantId = localStorage.getItem('tenant_id');
  if (tenantId) {
    config.headers['X-Tenant-ID'] = tenantId;
  }
  return config;
}

function handleApiError(error: unknown): never {
  if (axios.isAxiosError(error)) {
    const status = error.response?.status ?? 0;
    const detail = error.response?.data;
    const messageText =
      error.response?.data?.message ??
      error.response?.data?.error ??
      getHttpErrorMessage(status, error.message);
    if (isSetupRequired(status, detail)) {
      redirectToSetup();
      throw new ApiError(messageText, status, error.code, detail);
    }
    if (status === 401) {
      redirectToLogin(messageText);
      throw new ApiError(messageText, status, error.code, detail);
    }
    if (status === 403) {
      message.error(messageText || '无权限访问');
      throw new ApiError(messageText, status, error.code, detail);
    }
    throw new ApiError(messageText, status, error.code, detail);
  }
  throw error;
}

fastClient.interceptors.request.use(attachAuthHeaders);
fastClient.interceptors.response.use((response) => response, handleApiError);

client.interceptors.request.use((config) => {
  attachAuthHeaders(config);
  if (typeof config.data === 'string' && config.data.startsWith('{"data":')) {
    try {
      const parsed = JSON.parse(config.data);
      if (
        parsed &&
        typeof parsed === 'object' &&
        !Array.isArray(parsed) &&
        'data' in parsed &&
        Object.keys(parsed).length === 1
      ) {
        config.data = JSON.stringify(parsed.data);
      }
    } catch {
      // Leave non-JSON payloads as-is.
    }
  }
  return config;
});
client.interceptors.response.use((response) => response, handleApiError);
