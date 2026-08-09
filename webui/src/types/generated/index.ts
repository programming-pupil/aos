/**
 * Re-exports all backend-to-frontend type mappings.
 *
 * Import specific types from sub-modules for clarity:
 *   import type { BackendMcpServerInfo } from '@/types/generated/mcp';
 *
 * Import all types at once for convenience:
 *   import * as BackendTypes from '@/types/generated';
 */
export * from './mcp';
