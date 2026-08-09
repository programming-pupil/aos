# Model Capability Profiles and Reasoning Parameters

AOS stores model behavior in tenant-scoped, versioned capability profiles instead of hard-coding
provider parameters on an API key. Rotating a key keeps the profile; changing the provider endpoint
or model resolves a new profile identity.

## Detection model

There is no portable API that returns every model parameter. OpenAI, Anthropic, DeepSeek, Kimi,
Zhipu GLM, Google Gemini, xAI Grok, and OpenAI-compatible gateways expose different metadata. Most `/models`
endpoints return only IDs, while a few providers include context limits, modalities, or supported
parameters. AOS therefore uses this precedence order:

1. Explicit live verification;
2. Provider metadata;
3. The versioned registry bundled with AOS;
4. A conservative profile for unknown custom models;
5. Per-key manual overrides, which take precedence at runtime.

A lower-confidence source cannot overwrite a verified profile. Registry profiles refresh when the
bundled registry version changes. Expired verification results remain usable but are marked for
reverification so model traffic is never blocked by capability maintenance.

Kimi, GLM, Gemini, and Grok have official endpoint presets in Setup and API Keys, while still allowing a
compatible proxy URL. They share the OpenAI-compatible transport but retain distinct canonical
provider identities, so their capability profiles cannot contaminate one another.

## Web access boundary

Model web access and provider-native search are separate capabilities. AOS keeps two paths:

- Native search requires the selected model, wire protocol, and proxy to support the search request.
- AOS built-in `WebSearch` is a zero-configuration runtime tool backed by independent live public
  sources including DuckDuckGo, Brave, Wikipedia, Hacker News Algolia, and Stack Exchange. It
  requires internet access but no search API key.
- Brave API, Tavily, Serper, Exa, SearXNG, Generic HTTP, and search MCP are optional enhancements
  for coverage, reliability, or private sources. They are not prerequisites for web access.
- `WebFetch` and these search tools are independent of the model API hostname and only require
  reliable tool calling from the chat model.

Bundled providers do not automatically enable unverified native search. A custom proxy hostname is
not blocked by AOS, but complete compatibility with a provider's native Responses/search protocol
still requires live verification. Runtime order is healthy Search Extensions, AOS built-in search,
model-native search, search MCP, then local/RAG. Individual Search Extension health tests are strict,
so built-in fallback cannot hide a broken extension.

## User flow

In **System -> API Keys**:

1. Select a provider and enter the key.
2. Fetch and search the provider's model list, or enter a model ID manually.
3. Review protocol, context/output limits, reasoning values, tool calling, and structured output.
4. Run live verification for an unknown model or private gateway.
5. Keep the reasoning policy on **Auto** unless a fixed cost/quality policy is required.

Discovery failure never blocks manual model entry. Live verification makes a few small, billable
requests without business context and without executing user tools.

## Reasoning policy mapping

| AOS policy | Runtime behavior |
| --- | --- |
| Auto | Uses the current task's Fast, Standard, or Deep budget |
| Fast | Always uses the profile's fast mapping |
| Standard | Always uses its standard mapping |
| Deep | Always uses its deep mapping |
| Maximum | Uses the highest value declared by that profile |

Built-in transports include OpenAI/DeepSeek `reasoning_effort`, Anthropic `thinking` token budgets,
and Qwen `enable_thinking`. Maximum is an explicit policy, not the default.

## Runtime fallback

- Only an explicit `400/422` unknown/unsupported/not-allowed parameter response downgrades an
  optional parameter.
- Authentication errors, rate limits, timeouts, and `5xx` responses never change capabilities.
- AOS can switch between `max_tokens` and `max_completion_tokens` after explicit rejection.
- It removes a rejected `reasoning_effort`, reasoning-output flag, sampling parameter, `thinking`,
  `thinking_config`, or `enable_thinking`, then retries once.
- Anthropic thinking budgets remain below `max_tokens` and reserve output space for visible text.
- Unknown custom models do not receive experimental parameters by default.

Fallback does not conceal invalid credentials, insufficient balance, or rate limiting.

## Persistence and security

`model_capability_profiles` is unique by tenant, canonical provider, normalized Base URL, protocol,
model, and model type. It stores schema/registry versions, source, confidence, status, capabilities,
probe observations, errors, and expiry. `api_keys.model_profile_id` links the active chat profile.
Embedding profiles remain isolated in the NL2SQL dual-index subsystem.

Model discovery and verification require `apikeys:write`; profile reads require `apikeys:read`. Keys
are decrypted only in backend memory and are never stored in capability profiles or returned to the
browser. Discovery and verification persist the profile only; an unsaved edit cannot relink an API
key. Core request fields cannot be overridden through manual `extraBody`.

## Release verification

- Test manual model entry for OpenAI, DeepSeek, Kimi, GLM, Gemini, Grok, Anthropic, and a custom endpoint.
- Test discovery success and a provider without `/models` support.
- Verify the discovery-to-selection-to-save profile JSON round trip.
- Confirm key rotation preserves profile identity and endpoint/model changes create a new identity.
- Confirm verified profiles cannot be downgraded by metadata or registry inference.
- Confirm `401`, `429`, timeouts, and `5xx` do not learn unsupported capabilities.
- Confirm explicit parameter rejection causes exactly one adjusted retry.
- Confirm read-only users cannot discover, probe, create, update, or delete keys.
- Run SQLite migrations from an empty database and verify the profile table/link column.
- Run Rust tests, WebUI type checking, i18n checking, linting, and a production build.
