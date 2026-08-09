import { describe, expect, it } from 'vitest';
import {
  initialSessionRenameUiState,
  sessionRenameUiReducer,
} from './SessionList';

describe('session rename UI state', () => {
  it('waits for the dropdown to close before mounting the rename input', () => {
    const opened = sessionRenameUiReducer(initialSessionRenameUiState, {
      type: 'menu',
      open: true,
    });
    const selected = sessionRenameUiReducer(opened, { type: 'selectRename' });

    expect(selected).toEqual({
      menuOpen: false,
      renamePending: true,
      editing: false,
    });

    expect(sessionRenameUiReducer(selected, { type: 'activateRename' })).toEqual({
      menuOpen: false,
      renamePending: false,
      editing: true,
    });
  });

  it('cancels a pending rename if the dropdown reopens', () => {
    const pending = sessionRenameUiReducer(initialSessionRenameUiState, {
      type: 'selectRename',
    });
    const reopened = sessionRenameUiReducer(pending, { type: 'menu', open: true });

    expect(reopened.renamePending).toBe(false);
    expect(sessionRenameUiReducer(reopened, { type: 'activateRename' }).editing).toBe(false);
  });
});
