# Container Workflow

The root `Containerfile` is a multi-stage build file with two runtime targets:

- `server-runtime` - builds and runs the Rust `web-server` binary.
- `web-runtime` - builds the React Web UI and serves it with Nginx, proxying `/api` and `/ws` to the backend service.

## Run The Full Stack

```bash
./scripts/generate-env.sh
docker compose up --build
```

Open `http://localhost:3000`.

Ports bind to `127.0.0.1` by default. Set `AOS_BIND_HOST=0.0.0.0` only for a deliberate remote deployment behind TLS and a firewall.

Services:

- `server` - AOS WebServer on port `3001`, with `/data/aos.db` created and migrated at startup.
- `web` - Nginx Web UI on port `3000`.

## Build Individual Images

```bash
# Backend runtime image
docker build -f Containerfile --target server-runtime -t aos-server .

# Frontend runtime image
docker build -f Containerfile --target web-runtime -t aos-web .
```

## Reset Local Docker Data

```bash
docker compose down -v
docker compose up --build
```

Use this when you want to recreate AOS from a clean SQLite data volume.

## Development Without Docker

For local non-container development, use [`../USAGE.md`](../USAGE.md) and [`../rust/README.md`](../rust/README.md).
