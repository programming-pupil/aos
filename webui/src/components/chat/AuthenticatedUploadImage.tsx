import { useEffect, useState, type ComponentProps } from 'react';
import { Image } from 'antd';
import { client } from '@/api/client';

const PROTECTED_UPLOAD_PREFIX = '/api/v1/uploads/';

export function protectedUploadApiPath(
  source: string | undefined,
  pageOrigin = typeof window === 'undefined' ? undefined : window.location.origin,
): string | null {
  const value = source?.trim();
  if (!value) return null;
  if (value.startsWith(PROTECTED_UPLOAD_PREFIX)) {
    return value.slice('/api/v1'.length);
  }
  if (value.startsWith('/uploads/')) return value;
  if (!pageOrigin) return null;
  try {
    const parsed = new URL(value, pageOrigin);
    if (parsed.origin !== pageOrigin || !parsed.pathname.startsWith(PROTECTED_UPLOAD_PREFIX)) {
      return null;
    }
    return `${parsed.pathname.slice('/api/v1'.length)}${parsed.search}`;
  } catch {
    return null;
  }
}

export function useAuthenticatedUploadUrl(source?: string): string | undefined {
  const protectedPath = protectedUploadApiPath(source);
  const [resolved, setResolved] = useState<{ source?: string; url?: string }>(() => ({
    source,
    url: protectedPath ? undefined : source,
  }));

  useEffect(() => {
    const apiPath = protectedUploadApiPath(source);
    if (!apiPath) {
      setResolved({ source, url: source });
      return undefined;
    }

    let active = true;
    let objectUrl: string | undefined;
    setResolved({ source, url: undefined });
    void client
      .get<Blob>(apiPath, { responseType: 'blob' })
      .then((response) => {
        if (!active) return;
        objectUrl = URL.createObjectURL(response.data);
        setResolved({ source, url: objectUrl });
      })
      .catch(() => {
        if (active) setResolved({ source, url: undefined });
      });

    return () => {
      active = false;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [source]);

  if (resolved.source !== source) return protectedPath ? undefined : source;
  return resolved.url;
}

export function AuthenticatedUploadImage({ src, ...props }: ComponentProps<typeof Image>) {
  const resolvedSrc = useAuthenticatedUploadUrl(src);
  return <Image {...props} src={resolvedSrc} />;
}
