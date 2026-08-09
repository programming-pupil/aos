import { describe, expect, it } from 'vitest';

import {
  MENU_CONFIGURABLE_PERMISSIONS,
  getRolePermissions,
  normalizeMenuPermissionsForUi,
} from './permissions';

describe('workspace menu permission', () => {
  it('is independently configurable and included by built-in roles', () => {
    expect(MENU_CONFIGURABLE_PERMISSIONS).toContain('workspace:read');
    expect(getRolePermissions('admin')).toContain('workspace:read');
    expect(getRolePermissions('developer')).toContain('workspace:read');
    expect(getRolePermissions('viewer')).toContain('workspace:read');
  });

  it('does not infer workspace access from a newly saved super-assistant permission', () => {
    expect(normalizeMenuPermissionsForUi(['super_assistant:read'])).toEqual([
      'super_assistant:read',
    ]);
  });
});

describe('task control permissions', () => {
  it('hides task command-center grants from menu configuration', () => {
    expect(MENU_CONFIGURABLE_PERMISSIONS).not.toContain('tasks:read');
    expect(MENU_CONFIGURABLE_PERMISSIONS).not.toContain('watchdog:read');
    expect(normalizeMenuPermissionsForUi(['watchdog:read', 'tasks:read'])).toEqual([]);
  });

  it('assigns control and admin capabilities by built-in role', () => {
    expect(getRolePermissions('viewer')).toContain('tasks:read');
    expect(getRolePermissions('viewer')).not.toContain('tasks:control');
    expect(getRolePermissions('developer')).toContain('tasks:control');
    expect(getRolePermissions('admin')).toContain('tasks:admin');
  });
});

describe('legacy implementation menu permissions', () => {
  it('does not expose hidden code execution surfaces for assignment', () => {
    expect(MENU_CONFIGURABLE_PERMISSIONS).not.toContain('rd_studio:read');
    expect(MENU_CONFIGURABLE_PERMISSIONS).not.toContain('agent:read');
    expect(MENU_CONFIGURABLE_PERMISSIONS).not.toContain('pipeline:read');
    expect(MENU_CONFIGURABLE_PERMISSIONS).not.toContain('rd_quality:read');
    expect(MENU_CONFIGURABLE_PERMISSIONS).not.toContain('rd_agents:read');
    expect(normalizeMenuPermissionsForUi([
      'rd_studio:read',
      'agent:read',
      'pipeline:read',
      'rd_quality:read',
      'rd_agents:read',
    ])).toEqual(['rd_specs:read']);
  });
});
