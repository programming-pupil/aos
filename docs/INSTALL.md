# AOS Installation Guide

For a beginner-friendly native and Docker workflow, first-run initialization,
MCP prerequisites, release packaging, backup, and reset procedures, see:

- [AOS 开源版从零部署与启动手册](./OPEN_SOURCE_DEPLOYMENT.zh-CN.md)
- [AOS 开源发布全功能测试手册](./OPEN_SOURCE_TEST_GUIDE.zh-CN.md)

AOS uses an embedded SQLite database for its own platform data. A normal
deployment needs one AOS server process, the Web UI, and a persistent local data
directory. MySQL is not required to install AOS; MySQL/TiDB remain supported as
external NL2SQL data sources configured after setup.

The recommended distribution is `aos-offline-<version>-<os>-<arch>`. It bundles
the pinned 384-dimensional multilingual local embedding model and ONNX Runtime.
It never downloads model files at release startup; the model is loaded lazily on
the first local semantic request. An optional Embedding API improves retrieval,
while API failures fall back to a physically separate local index.

## Prerequisites

| Requirement | Minimum | Recommended |
| --- | --- | --- |
| Docker | 24+ | 25+ |
| Docker Compose | v2 | latest |
| Node.js for native setup | 20.19+ or 22.12+ | 22 LTS |
| Rust for native setup | stable | latest stable |
| Memory | 4 GB | 8 GB+ |

## Quick Start With Docker

```bash
cd aos
./scripts/generate-env.sh
docker compose up --build
```

Open `http://localhost:3000`. The setup wizard creates the first tenant and
administrator. Before the API becomes ready, AOS creates `/data/aos.db` in the
`aos-server-data` volume and applies all embedded migrations.

Docker ports bind to `127.0.0.1` by default. For a remote deployment, set
`AOS_BIND_HOST=0.0.0.0` and put the Web service behind TLS, a firewall, and a
reverse proxy.

```bash
# Stop services and keep aos.db
docker compose down

# Delete all local AOS data and recreate an empty database on next start
docker compose down -v

# Tail backend logs
docker compose logs -f server
```

## Native Setup

For a source checkout, the supported one-process path builds the Web UI and
backend, prepares the pinned local model, and serves both UI and API:

```bash
./scripts/setup-environment.sh --check
./scripts/aos-start.sh
```

The first source start stores the verified model under
`.aos-runtime/models/fastembed`. That build/runtime cache is separate from
`.aos-data`, so resetting business data does not trigger another 257 MiB model
download. Extracted AOS Offline archives already contain the model and never
download it during startup.

## Environment Before First Start

Run `./scripts/generate-env.sh` once, then review `.env`. The generated
`JWT_SECRET`, 32-byte `ENCRYPTION_KEY`, and `TOKEN_ENCRYPTION_KEY` are required
and must be preserved with the data directory across upgrades. Changing an
encryption key makes already stored API or repository credentials unreadable.

Set `BASE_URL` to the real browser URL and keep `AOS_BIND_HOST=127.0.0.1` unless
the service is protected by a firewall, TLS, and a reverse proxy. Optionally set
`AOSD_GITHUB_TOKEN` for reliable Skill marketplace access. Invitation email
requires `SMTP_HOST`, `SMTP_PORT`, `SMTP_USE_TLS`, `SMTP_USERNAME`,
`SMTP_PASSWORD`, and an allowed `SMTP_FROM`; otherwise invitations remain usable
through their copyable link. Configure tenant model keys in the WebUI rather
than process-wide environment variables for production use.

For an extracted AOS Offline archive, run the same start script with no build
toolchain. On Windows x64 use:

```powershell
.\scripts\setup-environment.ps1
.\scripts\aos-start.ps1
```

The manual two-process development flow is below.

Generate secrets and load them into the shell:

```bash
./scripts/generate-env.sh
set -a
. ./.env
set +a
```

Start the backend. The data directory is created automatically and must be on a
local filesystem, not NFS, SMB, or another shared network mount.

```bash
cd rust
cargo build -p web-server --features full
./target/debug/web-server --addr 0.0.0.0:3001 --data-dir ../.aos-data
```

Start the Web UI in another terminal:

```bash
cd webui
npm install
npm run dev
```

Open the Vite URL, normally `http://localhost:5173`. It proxies `/api` and
`/ws` to `http://localhost:3001`.

## SQLite Operations

- Run exactly one AOS server process per data directory. AOS refuses a second
  process by locking `<data-dir>/aos.lock`.
- Back up the data directory only after stopping AOS, or use a SQLite-aware
  online backup that includes the WAL state.
- A clean shutdown checkpoints the WAL. After an unclean shutdown, AOS runs
  `PRAGMA quick_check` before starting workers.
- `AOS_SQLITE_MAX_CONNECTIONS` defaults to `8` and is limited to `8..16`.
- `AOS_SQLITE_CONTROL_MAX_CONNECTIONS` controls background/control workers,
  defaults to `8`, and is limited to `8..16` independently of interactive
  requests.
- `AOS_SQLITE_BUSY_TIMEOUT_MS` defaults to `30000` and is limited to
  `1000..60000`.
- To reset a pre-release native installation, stop AOS and remove its chosen
  data directory. This deletes all users, configuration, tasks, and history.

## In-place Offline Package Upgrade

Extract the new same-platform package beside the existing installation, then
run the upgrade script from the new package:

```bash
cd /path/to/aos-offline-NEW-<os>-<arch>
./scripts/aos-upgrade.sh --target /path/to/aos-offline-OLD-<os>-<arch> --port 3000
```

On Windows use `aos-upgrade.ps1 -Target <old-directory> -Port 3000`. The script
verifies the release manifest, stops AOS, backs up the complete data directory
and `.env`, swaps only release assets, starts the new server, and waits for
readiness. A failed start automatically restores both the previous release and
the pre-upgrade data snapshot. Backups remain under
`.aos-backups/upgrade-<timestamp>/` in the target installation.

## Production Notes

- Keep `JWT_SECRET`, `ENCRYPTION_KEY`, and `TOKEN_ENCRYPTION_KEY` private and
  unique. `ENCRYPTION_KEY` must be exactly 32 bytes.
- Persist and back up the complete `/data` volume.
- Use TLS in front of the Web UI and API.
- This release supports single-process, single-instance deployment only. It is
  multi-user and multi-tenant within that instance, but it is not a clustered
  deployment.
- Configure model providers from `System -> API Keys` after setup.
- NL2SQL works with the bundled local embedding profile when no Embedding API is
  configured. API and local vectors are never mixed.
- Configure SMTP only when invite email delivery is needed.
- Browser CORS defaults to `BASE_URL`. Set `CORS_ALLOWED_ORIGINS` only when a
  separate frontend origin needs direct API access.
- Keep `AOS_ALLOW_PUBLIC_REGISTRATION=false` for invite-only deployments.
