#!/usr/bin/env python3
"""
Dump a PM research run's end-to-end trace from the AOS SQLite database.

Examples:
  python rust/scripts/pm_run_trace_dump.py \
    --run-id pm-research-task-xxxx \
    --out /tmp/run_trace.json
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple
from collections import Counter


def default_database_path() -> Path:
    data_dir = os.getenv("AOS_DATA_DIR")
    if data_dir:
        return Path(data_dir).expanduser() / "aos.db"
    home = Path.home()
    if sys.platform == "darwin":
        return home / "Library" / "Application Support" / "com.aos.enterprise" / "aos.db"
    if os.name == "nt":
        roaming = Path(os.getenv("APPDATA", home / "AppData" / "Roaming"))
        return roaming / "aos" / "enterprise" / "data" / "aos.db"
    xdg_data = Path(os.getenv("XDG_DATA_HOME", home / ".local" / "share"))
    return xdg_data / "enterprise" / "aos.db"


class DictCursor:
    def __init__(self, connection: sqlite3.Connection):
        self._cursor = connection.cursor()

    def __enter__(self) -> "DictCursor":
        return self

    def __exit__(self, *_: object) -> None:
        self._cursor.close()

    def execute(self, query: str, parameters: Tuple[Any, ...] = ()) -> "DictCursor":
        self._cursor.execute(query.replace("%s", "?"), parameters)
        return self

    def fetchone(self) -> Optional[Dict[str, Any]]:
        row = self._cursor.fetchone()
        return dict(row) if row is not None else None

    def fetchall(self) -> List[Dict[str, Any]]:
        return [dict(row) for row in self._cursor.fetchall()]


class DictConnection:
    def __init__(self, database_path: Path):
        uri = f"file:{database_path.resolve()}?mode=ro"
        self._connection = sqlite3.connect(uri, uri=True)
        self._connection.row_factory = sqlite3.Row

    def cursor(self) -> DictCursor:
        return DictCursor(self._connection)

    def close(self) -> None:
        self._connection.close()


def parse_json(value: Any) -> Any:
    if value is None:
        return None
    if isinstance(value, (dict, list)):
        return value
    if isinstance(value, (bytes, bytearray)):
        value = value.decode("utf-8", errors="replace")
    if isinstance(value, str):
        text = value.strip()
        if not text:
            return None
        try:
            return json.loads(text)
        except Exception:
            return value
    return value


def json_extract_query(blob: Any) -> Optional[str]:
    obj = parse_json(blob)
    if not isinstance(obj, dict):
        return None
    q = obj.get("query")
    if isinstance(q, str) and q.strip():
        return q.strip()
    sq = obj.get("search_query")
    if isinstance(sq, list) and sq and isinstance(sq[0], dict):
        q2 = sq[0].get("q")
        if isinstance(q2, str) and q2.strip():
            return q2.strip()
    return None


def extract_query_from_text(text: Optional[str]) -> Optional[str]:
    if not isinstance(text, str):
        return None
    match = re.search(r'"query"\s*:\s*"([^"]+)"', text)
    if match:
        return match.group(1)
    return None


def dedupe_keep_order(items: List[str]) -> List[str]:
    seen = set()
    out = []
    for item in items:
        if item in seen:
            continue
        seen.add(item)
        out.append(item)
    return out


def normalize_text(s: Optional[str]) -> str:
    if not isinstance(s, str):
        return ""
    return " ".join(s.strip().split()).lower()


def classify_url_quality(
    hit: Dict[str, Any],
    min_content_chars: int = 1200,
) -> Dict[str, Any]:
    url = hit.get("url") or ""
    title = normalize_text(hit.get("title"))
    snippet = normalize_text(hit.get("snippet"))
    content_chars = int(hit.get("contentChars") or 0)
    source = (hit.get("contentSource") or "none").strip() or "none"
    domain = (hit.get("domain") or "").strip().lower()

    nav_signals = [
        "login",
        "sign in",
        "signup",
        "register",
        "privacy",
        "terms",
        "newsroom",
        "/news",
        "/tag/",
        "/category/",
    ]
    url_l = (url or "").lower()
    title_l = title.lower()
    snippet_l = snippet.lower()
    nav_like = any(
        token in url_l or token in title_l or token in snippet_l for token in nav_signals
    )

    zero_content = content_chars <= 0
    low_content = 0 < content_chars < min_content_chars
    extracted = source in {"readability", "jina_plain_text", "html_to_text"}
    has_snippet = len(snippet.strip()) >= 40

    if zero_content:
        label = "bad"
    elif nav_like and content_chars < max(2200, min_content_chars):
        label = "suspect"
    elif low_content:
        label = "suspect"
    elif not extracted:
        label = "suspect"
    elif not has_snippet:
        label = "suspect"
    else:
        label = "good"

    return {
        "url": url,
        "domain": domain,
        "title": hit.get("title"),
        "snippet": hit.get("snippet"),
        "contentChars": content_chars,
        "contentSource": source,
        "relevanceScore": hit.get("relevanceScore"),
        "zeroContent": zero_content,
        "lowContent": low_content,
        "navLike": nav_like,
        "hasSnippet": has_snippet,
        "qualityLabel": label,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Dump PM research run trace from DB")
    parser.add_argument("--run-id", required=True, help="pm_research_runs.run_id")
    parser.add_argument(
        "--database",
        type=Path,
        default=None,
        help="path to aos.db (default: AOS_DATA_DIR/aos.db or the native AOS data directory)",
    )
    parser.add_argument(
        "--out",
        default=None,
        help="output json path (default: /tmp/run_trace_<run_id>.json)",
    )
    parser.add_argument(
        "--include-raw-io",
        action="store_true",
        help="include tool input_raw/output_raw in output",
    )
    args = parser.parse_args()

    run_id = args.run_id.strip()
    if not run_id:
        raise SystemExit("--run-id cannot be empty")

    out_path = args.out or f"/tmp/run_trace_{run_id}.json"
    database_path = (args.database or default_database_path()).expanduser()
    if not database_path.is_file():
        raise SystemExit(f"AOS SQLite database not found: {database_path}")
    conn = DictConnection(database_path)

    with conn.cursor() as c:
        c.execute(
            """
            SELECT run_id, task_id, session_id, source, status, current_stage, attempt,
                   budget_profile, pipeline_timeout_secs, max_attempts,
                   source_slot_search_secs, source_slot_browser_secs, source_slot_api_fetch_secs,
                   retrieve_max_tool_calls, max_calls_per_source, user_message, total_elapsed_ms,
                   error_code, error_message, final_quality_score, metadata_json,
                   started_at, deadline_at, ended_at, created_at, updated_at
            FROM pm_research_runs
            WHERE run_id = %s
            LIMIT 1
            """,
            (run_id,),
        )
        run = c.fetchone()
    if not run:
        raise SystemExit(f"run not found: {run_id}")

    task_id = run.get("task_id")

    with conn.cursor() as c:
        c.execute(
            """
            SELECT id, stage, attempt_no, status, strategy, route_key, channel, variant,
                   timeout_secs, budget_secs, elapsed_ms, detail_json, repair_scope_json, result_json,
                   error_code, error_message, started_at, ended_at, created_at, updated_at
            FROM pm_research_stage_attempts
            WHERE run_id = %s
            ORDER BY attempt_no ASC, id ASC
            """,
            (run_id,),
        )
        stage_rows = c.fetchall()

        c.execute(
            """
            SELECT id, stage_attempt_id, slot_seq, route_key, channel, variant, source_key, source_url,
                   status, tool_call_count, elapsed_ms, error_code, error_message, detail_json,
                   started_at, ended_at, created_at, updated_at
            FROM pm_research_source_slots
            WHERE run_id = %s
            ORDER BY slot_seq ASC, id ASC
            """,
            (run_id,),
        )
        source_slots = c.fetchall()

        c.execute(
            """
            SELECT id, stage_attempt_id, source_slot_id, call_seq, tool_name, tool_use_id,
                   input_preview, output_preview, input_raw, output_raw, is_error,
                   error_code, error_message, http_status, latency_ms, route_key, channel,
                   provider, provider_trace, url, domain, created_at
            FROM pm_research_tool_call_ledger
            WHERE run_id = %s
            ORDER BY call_seq ASC, id ASC
            """,
            (run_id,),
        )
        tool_rows = c.fetchall()

        c.execute(
            """
            SELECT id, subtask_key, subtask_id, title, goal, deliverable, required_evidence_type,
                   priority, status, probe_candidate_count, probe_completed_count, citation_count,
                   domain_count, tool_call_count, quality_score, error_code, error_message, detail_json,
                   started_at, ended_at, created_at, updated_at
            FROM pm_subtask_runs
            WHERE run_id = %s
            ORDER BY id ASC
            """,
            (run_id,),
        )
        subtask_runs = c.fetchall()

        c.execute(
            """
            SELECT id, subtask_run_id, subtask_key, attempt_no, attempt_key, variant,
                   route_key, route_channel, status, elapsed_ms, citation_count, domain_count,
                   tool_call_count, quality_score, error_code, error_message, detail_json,
                   started_at, ended_at, created_at, updated_at
            FROM pm_subtask_attempts
            WHERE run_id = %s
            ORDER BY subtask_run_id ASC, attempt_no ASC, id ASC
            """,
            (run_id,),
        )
        subtask_attempts = c.fetchall()

        c.execute(
            """
            SELECT event_type, severity, message, payload_json, created_at
            FROM pm_audit_trails
            WHERE run_id = %s
            ORDER BY id ASC
            """,
            (run_id,),
        )
        audits = c.fetchall()

        c.execute(
            """
            SELECT prompt_key, prompt_version, prompt_hash, stage, run_count, metadata_json,
                   last_used_at, updated_at
            FROM pm_prompt_registry
            WHERE last_run_id = %s
            ORDER BY updated_at DESC, id DESC
            """,
            (run_id,),
        )
        prompts = c.fetchall()

        event_rows: List[Dict[str, Any]] = []
        if task_id:
            c.execute(
                """
                SELECT seq, status, stage, attempt, message, elapsed_ms, stage_elapsed_ms,
                       detail_json, response_json, error_message, created_at
                FROM pm_research_task_events
                WHERE task_id = %s
                ORDER BY seq ASC
                """,
                (task_id,),
            )
            event_rows = c.fetchall()

    # Parse stage details for planned task graph / query variants and executed query strings.
    planned_query_variants: List[str] = []
    planned_subtasks: List[Dict[str, Any]] = []
    retrieve_query_samples: List[str] = []
    retrieve_selected_variants: List[str] = []
    retrieve_urls_from_stage: List[str] = []

    normalized_stages: List[Dict[str, Any]] = []
    for row in stage_rows:
        detail = parse_json(row.get("detail_json"))
        repair_scope = parse_json(row.get("repair_scope_json"))
        result = parse_json(row.get("result_json"))

        if isinstance(detail, dict):
            if not planned_query_variants and isinstance(detail.get("queryVariants"), list):
                planned_query_variants = [str(x) for x in detail["queryVariants"]]
            if not planned_subtasks:
                task_graph = detail.get("taskGraph")
                if isinstance(task_graph, dict) and isinstance(task_graph.get("subtasks"), list):
                    planned_subtasks = task_graph["subtasks"]

            if row.get("stage") == "retrieve":
                sv = detail.get("selectedVariant")
                if isinstance(sv, str):
                    retrieve_selected_variants.append(sv)

                tool_summary = detail.get("toolSummary")
                if isinstance(tool_summary, dict):
                    urls = tool_summary.get("urls")
                    if isinstance(urls, list):
                        retrieve_urls_from_stage.extend([str(u) for u in urls])
                    samples = tool_summary.get("samples")
                    if isinstance(samples, list):
                        for sample in samples:
                            if not isinstance(sample, dict):
                                continue
                            q = json_extract_query(sample.get("input")) or extract_query_from_text(
                                sample.get("output")
                            )
                            if q:
                                retrieve_query_samples.append(q)

        normalized_stages.append(
            {
                "id": row.get("id"),
                "stage": row.get("stage"),
                "attemptNo": row.get("attempt_no"),
                "status": row.get("status"),
                "strategy": row.get("strategy"),
                "routeKey": row.get("route_key"),
                "channel": row.get("channel"),
                "variant": row.get("variant"),
                "timeoutSecs": row.get("timeout_secs"),
                "budgetSecs": row.get("budget_secs"),
                "elapsedMs": row.get("elapsed_ms"),
                "detail": detail,
                "repairScope": repair_scope,
                "result": result,
                "errorCode": row.get("error_code"),
                "errorMessage": row.get("error_message"),
                "startedAt": row.get("started_at"),
                "endedAt": row.get("ended_at"),
                "createdAt": row.get("created_at"),
                "updatedAt": row.get("updated_at"),
            }
        )

    # Tool ledger normalization + URL/content extraction.
    tool_calls: List[Dict[str, Any]] = []
    tool_queries_from_input: List[str] = []
    websearch_hits: List[Dict[str, Any]] = []
    unique_urls: List[str] = []
    unique_url_seen = set()

    for row in tool_rows:
        input_raw = parse_json(row.get("input_raw"))
        output_raw = parse_json(row.get("output_raw"))
        q = json_extract_query(input_raw)
        if q:
            tool_queries_from_input.append(q)

        url = row.get("url")
        if isinstance(url, str) and url and url not in unique_url_seen:
            unique_url_seen.add(url)
            unique_urls.append(url)

        tool_calls.append(
            {
                "id": row.get("id"),
                "stageAttemptId": row.get("stage_attempt_id"),
                "sourceSlotId": row.get("source_slot_id"),
                "callSeq": row.get("call_seq"),
                "toolName": row.get("tool_name"),
                "toolUseId": row.get("tool_use_id"),
                "isError": bool(row.get("is_error")),
                "errorCode": row.get("error_code"),
                "errorMessage": row.get("error_message"),
                "httpStatus": row.get("http_status"),
                "latencyMs": row.get("latency_ms"),
                "routeKey": row.get("route_key"),
                "channel": row.get("channel"),
                "provider": row.get("provider"),
                "providerTrace": row.get("provider_trace"),
                "url": row.get("url"),
                "domain": row.get("domain"),
                "inputPreview": row.get("input_preview"),
                "outputPreview": row.get("output_preview"),
                "inputRaw": input_raw if args.include_raw_io else None,
                "outputRaw": output_raw if args.include_raw_io else None,
                "parsedQuery": q,
                "createdAt": row.get("created_at"),
            }
        )

        if row.get("tool_name") == "WebSearch.hit":
            title = None
            snippet = None
            content_chars = None
            relevance = None
            content_source = None
            if isinstance(output_raw, dict):
                title = output_raw.get("title")
                snippet = output_raw.get("snippet")
                content_chars = output_raw.get("contentChars")
                relevance = output_raw.get("relevanceScore")
                content_source = output_raw.get("contentSource")
            websearch_hits.append(
                {
                    "id": row.get("id"),
                    "callSeq": row.get("call_seq"),
                    "url": row.get("url"),
                    "domain": row.get("domain"),
                    "title": title,
                    "snippet": snippet,
                    "contentChars": content_chars,
                    "relevanceScore": relevance,
                    "contentSource": content_source,
                    "isError": bool(row.get("is_error")),
                    "errorCode": row.get("error_code"),
                }
            )

    # Task events and final response payload.
    task_events: List[Dict[str, Any]] = []
    final_response: Optional[Dict[str, Any]] = None
    for row in event_rows:
        detail = parse_json(row.get("detail_json"))
        response = parse_json(row.get("response_json"))
        event = {
            "seq": row.get("seq"),
            "status": row.get("status"),
            "stage": row.get("stage"),
            "attempt": row.get("attempt"),
            "message": row.get("message"),
            "elapsedMs": row.get("elapsed_ms"),
            "stageElapsedMs": row.get("stage_elapsed_ms"),
            "detail": detail,
            "response": response,
            "errorMessage": row.get("error_message"),
            "createdAt": row.get("created_at"),
        }
        task_events.append(event)
        if isinstance(response, dict) and row.get("status") in {"completed", "failed"}:
            final_response = response

    # Derived query perspectives.
    executed_query_candidates = dedupe_keep_order(
        [*tool_queries_from_input, *retrieve_query_samples, *retrieve_selected_variants]
    )
    planned_query_variants = dedupe_keep_order([str(x) for x in planned_query_variants])
    retrieve_urls_from_stage = dedupe_keep_order(retrieve_urls_from_stage)
    unique_urls = dedupe_keep_order([*unique_urls, *retrieve_urls_from_stage])

    # Derived quality diagnostics for URL hits.
    unique_hit_map: Dict[str, Dict[str, Any]] = {}
    for hit in websearch_hits:
        url = hit.get("url")
        if not isinstance(url, str) or not url:
            continue
        prev = unique_hit_map.get(url)
        prev_chars = int(prev.get("contentChars") or 0) if prev else -1
        cur_chars = int(hit.get("contentChars") or 0)
        if prev is None or cur_chars > prev_chars:
            unique_hit_map[url] = hit
    unique_hits = list(unique_hit_map.values())

    quality_rows = [classify_url_quality(hit) for hit in unique_hits]
    quality_counts = Counter(row["qualityLabel"] for row in quality_rows)
    source_counts = Counter((row.get("contentSource") or "none") for row in quality_rows)

    pm_quality = None
    if isinstance(final_response, dict):
        maybe_quality = final_response.get("pm_quality")
        if isinstance(maybe_quality, dict):
            pm_quality = maybe_quality
    cited_urls = pm_quality.get("citations", []) if isinstance(pm_quality, dict) else []
    citations_with_zero_or_missing_content: List[str] = []
    for url in cited_urls:
        hit = unique_hit_map.get(url)
        if not hit or int(hit.get("contentChars") or 0) <= 0:
            citations_with_zero_or_missing_content.append(url)

    max_calls_per_source = int(run.get("max_calls_per_source") or 0)
    retrieve_attempts = [s for s in normalized_stages if s.get("stage") == "retrieve"]
    retrieve_probe_only_count = sum(
        1
        for s in retrieve_attempts
        if isinstance(s.get("detail"), dict)
        and bool(s["detail"].get("probeOnlyForRouting"))
    )

    trace = {
        "runId": run_id,
        "run": {
            **run,
            "metadata_json": parse_json(run.get("metadata_json")),
        },
        "planned": {
            "queryVariantsCount": len(planned_query_variants),
            "queryVariants": planned_query_variants,
            "subtaskCount": len(planned_subtasks),
            "subtasks": planned_subtasks,
        },
        "executed": {
            "stageAttemptsCount": len(normalized_stages),
            "stageAttempts": normalized_stages,
            "sourceSlotsCount": len(source_slots),
            "sourceSlots": [
                {
                    **slot,
                    "detail_json": parse_json(slot.get("detail_json")),
                }
                for slot in source_slots
            ],
            "subtaskRunsCount": len(subtask_runs),
            "subtaskRuns": [
                {**r, "detail_json": parse_json(r.get("detail_json"))} for r in subtask_runs
            ],
            "subtaskAttemptsCount": len(subtask_attempts),
            "subtaskAttempts": [
                {**r, "detail_json": parse_json(r.get("detail_json"))} for r in subtask_attempts
            ],
            "toolCallCount": len(tool_calls),
            "toolCalls": tool_calls,
            "webSearchHitCount": len(websearch_hits),
            "webSearchHits": websearch_hits,
            "executedQueryCandidatesCount": len(executed_query_candidates),
            "executedQueryCandidates": executed_query_candidates,
            "uniqueUrlCount": len(unique_urls),
            "uniqueUrls": unique_urls,
        },
        "llm": {
            "promptUsageCount": len(prompts),
            "promptUsage": [
                {
                    **p,
                    "metadata_json": parse_json(p.get("metadata_json")),
                }
                for p in prompts
            ],
            "taskEventCount": len(task_events),
            "taskEvents": task_events,
            "finalResponse": final_response,
            "note": (
                "Exact per-attempt full prompt text is generally not persisted. "
                "Use stage detail + promptUsage hash/version + taskEvents as nearest reconstruction."
            ),
        },
        "diagnostics": {
            "coverage": {
                "plannedSubtasks": len(planned_subtasks),
                "executedSubtaskAttempts": len(subtask_attempts),
                "plannedQueryVariants": len(planned_query_variants),
                "executedQueryCandidates": len(executed_query_candidates),
                "retrieveAttempts": len(retrieve_attempts),
                "retrieveProbeOnlyCount": retrieve_probe_only_count,
            },
            "quota": {
                "maxCallsPerSource": max_calls_per_source,
                "sourceRouteCount": len(
                    {
                        (slot.get("route_key") or "").strip().lower()
                        for slot in source_slots
                        if slot.get("route_key")
                    }
                ),
                "note": (
                    "If retrieveProbeOnlyCount is high while maxCallsPerSource is small, "
                    "coverage can end early before all subtasks run."
                ),
            },
            "urlQuality": {
                "uniqueHitUrlCount": len(unique_hits),
                "qualityCounts": dict(quality_counts),
                "contentSourceCounts": dict(source_counts),
                "zeroContentCount": sum(1 for row in quality_rows if row["zeroContent"]),
                "lowContentCount": sum(1 for row in quality_rows if row["lowContent"]),
                "navLikeCount": sum(1 for row in quality_rows if row["navLike"]),
                "minContentCharsRecommended": 1200,
            },
            "citationGrounding": {
                "citationCount": len(cited_urls),
                "citationsWithZeroOrMissingContentCount": len(
                    citations_with_zero_or_missing_content
                ),
                "citationsWithZeroOrMissingContent": citations_with_zero_or_missing_content,
            },
            "llmUsage": (
                final_response.get("usage", {})
                if isinstance(final_response, dict)
                else {}
            ),
        },
        "audit": [
            {
                **a,
                "payload_json": parse_json(a.get("payload_json")),
            }
            for a in audits
        ],
    }

    out_file = Path(out_path)
    out_file.parent.mkdir(parents=True, exist_ok=True)
    out_file.write_text(json.dumps(trace, ensure_ascii=False, indent=2, default=str), encoding="utf-8")
    conn.close()

    summary = {
        "runId": run_id,
        "taskId": run.get("task_id"),
        "status": run.get("status"),
        "stageAttempts": len(normalized_stages),
        "plannedSubtasks": len(planned_subtasks),
        "executedSubtaskRuns": len(subtask_runs),
        "executedSubtaskAttempts": len(subtask_attempts),
        "plannedQueryVariants": len(planned_query_variants),
        "executedQueryCandidates": len(executed_query_candidates),
        "toolCalls": len(tool_calls),
        "webSearchHits": len(websearch_hits),
        "uniqueUrls": len(unique_urls),
        "qualityGoodUrls": quality_counts.get("good", 0),
        "qualitySuspectUrls": quality_counts.get("suspect", 0),
        "qualityBadUrls": quality_counts.get("bad", 0),
        "citationWithZeroOrMissingContent": len(citations_with_zero_or_missing_content),
        "taskEvents": len(task_events),
        "promptUsage": len(prompts),
        "output": str(out_file),
    }
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
