/**
 * Standardized API error wrapper.
 * All API errors thrown by the client are wrapped in this type.
 */
export class ApiError extends Error {
  constructor(
    message: string,
    public readonly status: number,
    public readonly code?: string,
    public readonly detail?: unknown
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

/** Maps HTTP status codes to user-friendly messages */
export function getHttpErrorMessage(status: number, defaultMsg: string): string {
  switch (status) {
    case 400: return '请求参数错误';
    case 401: return '未登录或登录已过期，请重新登录';
    case 403: return '没有权限执行此操作';
    case 404: return '请求的资源不存在';
    case 422: return '请求格式正确但语义错误';
    case 429: return '请求过于频繁，请稍后再试';
    case 500: return '服务器内部错误';
    case 502: return '网关错误，服务暂时不可用';
    case 503: return '服务暂时不可用';
    default: return defaultMsg;
  }
}
