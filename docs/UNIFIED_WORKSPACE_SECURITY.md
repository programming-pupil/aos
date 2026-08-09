# Unified Super Assistant Workspace Security

The unified workspace is an authenticated virtual projection. It is not a
server filesystem browser. Every operation derives `tenant_id`, `user_id`, and
`session_id` from server-side authentication claims; tool arguments cannot
override that identity.

## Required migrations

Run migrations 165, 166, and 169 through 173 before enabling the unified parent
loop. The server performs read-only schema health checks at startup and never
creates workspace tables from request handlers.

Migration 173 adds authorization-first keyset indexes for uploads, exact
history, generated artifacts, and tenant-shared projections. These indexes are
required for large workspaces; omitting them does not weaken SQL predicates but
can make complete path and keyword scans impractical.

## Rollout modes

`tenant_agent_features` supports `off`, `shadow`, and `on` for:

- `unified_parent_loop`
- `unified_workspace`
- `adaptive_completion_gate`

`shadow` still produces one unified answer. It records a distinct execution
mode for comparison and never runs a second answer-producing path.

## Read isolation

Private uploads, exact archives, artifacts, and project roots are filtered by
tenant and owner before recall and re-authorized before reads. Tenant SQL
knowledge retains its existing explicit tenant/datasource scope. `/shared`
contains only administrator-published projections with entry-scoped grants.

Workspace cursors include tenant, user, workspace, query, and ACL fingerprints.
Grant revocation or a shared-entry version change invalidates old cursors.
Denied requests return a not-found-or-denied result without exposing another
user's filename, path, identifier, content, or grant metadata.

## Isolated command execution

`workspace_execute` is registered only when all of the following are true:

1. The server runs on Linux.
2. `bwrap` (Bubblewrap) and `prlimit` are installed.
3. The startup probe confirms that the host root and another-user probe root are
   invisible.
4. The generated mount is the only writable workspace mount.
5. The network namespace has no external route.
6. CPU, address-space, file-size, process-count, and wall-time limits are active.

Example packages:

```bash
# Debian/Ubuntu
apt-get install bubblewrap util-linux ripgrep

# Fedora/RHEL derivatives
dnf install bubblewrap util-linux ripgrep
```

The worker materializes only the current authenticated user's authorized
virtual resources into a `0700` temporary snapshot. System binaries are mounted
read-only, the snapshot is mounted read-only, `/workspace/generated` is mounted
writable, and networking is disabled. Output and generated files are persisted
as user/session-scoped artifacts before the snapshot is deleted.

On macOS, Windows, hosts without `bwrap`, or hosts where the isolation probe
fails, `workspace_execute` is absent. AOS never falls back to a host shell.
Live public facts continue to use the configured WebSearch/WebFetch tools.

## Verification

Normal unit tests verify unsupported-host behavior and virtual path properties.
Database-backed A/B isolation and event-sequence tests now create a disposable
SQLite database automatically and run in the normal workspace test suite:

```bash
cargo test -p agent-gateway workspace::tests --lib
cargo test -p web-server --features bot-agents,nl2sql --lib
```

Linux CI should install `bubblewrap` and run the agent-gateway sandbox tests.
The tests verify host-path and symlink isolation, network isolation, writable
mount policy, resource limits, and wall-time termination.
