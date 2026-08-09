<p align="center">
  <img src="docs/assets/aos-hero.svg" alt="AOS - Agent Operating System" width="100%">
</p>

<p align="center">
  <a href="./README.zh-CN.md">中文</a> ·
  <a href="./docs/INSTALL.md">Documentation</a> ·
  <a href="./LICENSE">Apache-2.0 License</a>
</p>

<p align="center">
  <a href="https://github.com/programming-pupil/aos/actions/workflows/rust-ci.yml"><img src="https://github.com/programming-pupil/aos/actions/workflows/rust-ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/npm-v0.1.0-CB3837?logo=npm&logoColor=white" alt="npm version 0.1.0">
  <img src="https://img.shields.io/badge/Node.js-%3E%3D20.19%20%7C%7C%20%3E%3D22.12-339933?logo=nodedotjs&logoColor=white" alt="Node.js >=20.19 or >=22.12">
  <img src="https://img.shields.io/badge/License-Apache--2.0-0b8f55?logo=opensourceinitiative&logoColor=white" alt="Apache-2.0 License">
  <img src="https://img.shields.io/badge/Discord-community%20coming%20soon-5865F2?logo=discord&logoColor=white" alt="Discord community coming soon">
</p>

# AOS

AOS means **Agent Operating System**: a Web-first, multi-tenant workspace for building useful Agent workflows around one recoverable session. It brings conversation, evidence-based research, data questions, memory, files, Skills, MCP, task recovery, and external Bot channels into one product surface.

> **Project status:** AOS is an active source release under regression testing. This repository contains the source tree, documentation, tests, and packaging scripts. Prebuilt AOS Offline archives are intentionally kept out of Git history and will be attached to GitHub Releases only when a build is ready. Some integrations require provider credentials and external services.

## What AOS is for

- **One recoverable Agent session** for normal questions, tool use, long-running work, and follow-up context.
- **Deep research** that gathers and checks evidence with bounded execution and a usable fallback answer.
- **Data work** with datasource-aware NL2SQL, semantic retrieval, SQL knowledge, and attribution workflows.
- **Mobile Agent access** through Bot Gateway adapters for supported chat platforms and notification delivery.
- **Progressive engineering design** that turns a product request into a reviewable core design, then an editable implementation plan and task breakdown for an external coding Agent.
- **Skills and MCP** as optional extensions that can be installed, inspected, and governed.

The project is designed to make Agent work inspectable and recoverable. It does not claim that every provider adapter or every workflow is production-ready on every platform yet.

## Try the source tree

The shortest supported path for contributors is Docker:

    ./scripts/generate-env.sh
    docker compose up --build

Open http://localhost:3000 and complete setup. Add an enabled chat-scoped API key in System -> API Keys before asking the model to do provider-backed work.

For native development:

    ./scripts/setup-environment.sh --check
    ./scripts/aos-start.sh

Read the deployment guide before exposing AOS beyond localhost:

- [Installation](./docs/INSTALL.md)
- [Open-source deployment](./docs/OPEN_SOURCE_DEPLOYMENT.zh-CN.md)
- [Complete test guide](./docs/OPEN_SOURCE_TEST_GUIDE.zh-CN.md)

## Repository map

- rust/ - Rust workspace, API, runtime, Agent orchestration, and WebServer.
- webui/ - React and Vite application.
- docs/ - architecture, deployment, security, integration, and test documentation.
- eval/ and examples/ - deterministic fixtures and small integration examples.
- scripts/ - setup, start/stop, upgrade, and packaging helpers.

## Development checks

    cd rust
    cargo fmt --all
    cargo check -p web-server
    cargo test --workspace --all-features

    cd ../webui
    npm ci
    npm run typecheck
    npm run build:ci

Targeted test guides cover the Bot Gateway, data attribution, NL2SQL configuration, and the progressive engineering design workflow.

## Documentation

- [Architecture](./docs/ARCHITECTURE.md)
- [Model capability profiles](./docs/MODEL_CAPABILITY_PROFILES.md)
- [Long-running tasks and mobile operations design](./docs/AGENTOPS_WATCHDOG_DESIGN.md)
- [Bot capability contract](./docs/BOT_CAPABILITY_CONTRACT.md)
- [Bot Gateway enterprise setup](./docs/BOT_GATEWAY_ENTERPRISE.md)
- [NL2SQL design](./docs/DESIGN_DATASOURCES_NL2SQL.md)
- [Engineering design center](./docs/AOS_ENGINEERING_DESIGN_CENTER.zh-CN.md)
- [Security policy](./SECURITY.md)
- [Contributing](./CONTRIBUTING.md)

## Release artifacts

AOS Offline is a separately built distribution for macOS, Linux, and Windows x64. Release archives are not committed to this repository. Maintainers build and validate an archive, then publish it under GitHub Releases with its checksum and upgrade notes. Until that happens, use the source workflow above.

## License

AOS is licensed under the [Apache License 2.0](./LICENSE). Third-party and upstream attribution notices are in [NOTICE.md](./NOTICE.md).
