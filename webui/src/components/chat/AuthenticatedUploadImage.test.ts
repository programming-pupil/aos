import { describe, expect, it } from 'vitest';
import { protectedUploadApiPath } from './AuthenticatedUploadImage';

describe('protectedUploadApiPath', () => {
  it('maps protected relative and same-origin upload URLs to the authenticated API client path', () => {
    expect(protectedUploadApiPath('/api/v1/uploads/user-a/image.png', 'https://aos.example')).toBe(
      '/uploads/user-a/image.png',
    );
    expect(
      protectedUploadApiPath(
        'https://aos.example/api/v1/uploads/user-a/image.png?version=2',
        'https://aos.example',
      ),
    ).toBe('/uploads/user-a/image.png?version=2');
  });

  it('does not attach credentials to public or cross-origin images', () => {
    expect(protectedUploadApiPath('https://cdn.example/image.png', 'https://aos.example')).toBeNull();
    expect(protectedUploadApiPath('data:image/png;base64,abc', 'https://aos.example')).toBeNull();
    expect(protectedUploadApiPath('blob:https://aos.example/id', 'https://aos.example')).toBeNull();
  });
});
