export interface UploadOptions {
  /** Backend base URL, defaults to /api/v1. */
  baseUrl?: string;
}

export async function uploadFile(
  file: File,
  _options: UploadOptions = {}
): Promise<import('@/types').UploadResponse> {
  const formData = new FormData();
  formData.append('file', file);
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const headers: Record<string, string> = {
    Authorization: token ? `Bearer ${token}` : '',
  };
  if (tenantId) {
    headers['X-Tenant-ID'] = tenantId;
  }

  // Use a relative URL so the request goes through the vite dev server proxy,
  // which correctly forwards the Authorization header from the page's cookies/localStorage.
  // The proxy maps /api/v1/* -> http://localhost:3001/api/v1/*.
  const resp = await fetch('/api/v1/uploads/upload', {
    method: 'POST',
    headers,
    body: formData,
  });

  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(`上传失败: ${resp.status} ${text}`);
  }

  return resp.json();
}
