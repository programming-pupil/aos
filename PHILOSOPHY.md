# AOS Philosophy

AOS is built around a simple idea: teams should be able to turn operational intent into reliable AI-assisted workflows without forcing every user to understand prompts, provider wiring, tools, or infrastructure internals.

## 1. Web-First Operations

The primary surface is the Web workspace. Users configure capabilities, data, models, skills, hooks, bots, and governance in the UI, then use those capabilities through chat, scheduled tasks, Bot Gateway channels, and workflow pages.

## 2. Capability Binding Over One-Off Prompts

AOS treats AI features as reusable capabilities rather than isolated prompt boxes:

- Operations assistant for market, product, and growth research.
- Data exploration for NL2SQL and datasource analysis.
- Materials studio for copy, image, music, and future media workflows.
- Skills and MCP for extensible tools and domain knowledge.
- Bot Gateway for external chat platforms and proactive notifications.

## 3. Effect First, Cost Guarded

The system should prefer quality and completeness, while still making cost, token usage, route choices, and evidence coverage observable. Guardrails should prevent waste and silent degradation, not weaken answers.

## 4. Open Integration Surface

AOS should be easy to extend without changing core code:

- API keys are table-driven and scene-aware.
- Hooks can intercept lifecycle stages.
- Skills can be installed from repositories or marketplace search.
- Bot Gateway channels adapt external platforms to AOS capabilities.
- MCP servers can be managed and hot-reloaded from the UI.

## 5. Observable By Default

Every major workflow should leave an audit trail: requests, stages, tool calls, selected routes, generated artifacts, notifications, hook execution, token usage, and errors. If a result is weak, operators should be able to see why.

## 6. Practical Open Source

AOS should be understandable from a clean clone: current docs, reproducible setup, clear module boundaries, and no stale CLI instructions in public entry points. Historical compatibility code may remain where it still protects runtime behavior, but public product documentation should describe the current Web-first system.
