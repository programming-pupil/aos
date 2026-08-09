#!/usr/bin/env python3
"""Run one real PM Assistant background task and capture the full diagnostic trail.

This script is intentionally API-level: it does not mock the PM flow and it does
not call the model provider directly. It exercises the same backend endpoints
used by the Web UI:

  POST /api/v1/agent/sessions
  POST /api/v1/agent/sessions/{session_id}/pm-research-tasks
  GET  /api/v1/agent/pm-research-tasks/{task_id}/events
  GET  /api/v1/agent/pm-research-tasks/{task_id}
  GET  /api/v1/agent/pm-research-tasks/{task_id}/subtasks
  GET  /api/v1/agent/pm-research-tasks/{task_id}/subtasks/{subtask_id}/attempts
  GET  /api/v1/agent/sessions/{session_id}/history

Usage examples:

  AOS_PM_DIAG_PROMPT_FILE=/tmp/pm-prompt.txt \
  python3 scripts/pm-assistant-flow-diagnostic.py

  AOS_PM_DIAG_PROMPT='北京今天和明天天气预报，给来源' \
  AOS_PM_DIAG_MODEL=gpt-5.5 \
  python3 scripts/pm-assistant-flow-diagnostic.py

  AOS_PM_DIAG_DOCUMENT_PATHS='/tmp/report.csv,/tmp/context.md' \
  AOS_PM_DIAG_PROMPT='分析附件数据并给结论' \
  python3 scripts/pm-assistant-flow-diagnostic.py

  AOS_PM_DIAG_SESSION_ID='existing-pm-session-id' \
  AOS_PM_DIAG_PROMPT='北京今天和明天天气预报，给来源' \
  python3 scripts/pm-assistant-flow-diagnostic.py

Auth:
  - If AOS_PM_DIAG_TOKEN is set, it is used.
  - Otherwise, local dev mode reads the local aos.db and .env JWT_SECRET, selects an
    active user, and signs a short-lived HS256 JWT matching the server auth.
"""

from __future__ import annotations

import base64
import datetime as dt
import hmac
import hashlib
import json
import mimetypes
import os
import re
import socket
import sqlite3
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BASE_URL = "http://localhost:3001"
TERMINAL_STAGES = {"done", "failed", "cancelled"}
TERMINAL_STATUSES = {"completed", "failed", "cancelled"}
URL_RE = re.compile(r"https?://[^\s<>)\"']+", re.I)
TIMEOUT_RE = re.compile(r"(timeout|timed out|exceeded|超时|已超预算|524)", re.I)


@dataclass
class AuthContext:
    token: str
    tenant_id: str | None
    user_id: str | None
    email: str | None
    source: str


def load_dotenv(path: Path) -> dict[str, str]:
    env: dict[str, str] = {}
    if not path.exists():
        return env
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        value = value.strip().strip('"').strip("'")
        env[key.strip()] = value
    return env


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).decode("ascii").rstrip("=")


def sign_dev_jwt(secret: str, user: dict[str, Any]) -> str:
    now = int(time.time())
    header = {"alg": "HS256", "typ": "JWT"}
    payload = {
        "sub": user["id"],
        "email": user["email"],
        "role": user["role"],
        "tenant_id": user["tenant_id"],
        "iat": now,
        "exp": now + 24 * 60 * 60,
    }
    head = b64url(json.dumps(header, separators=(",", ":")).encode("utf-8"))
    body = b64url(json.dumps(payload, separators=(",", ":")).encode("utf-8"))
    signing_input = f"{head}.{body}".encode("ascii")
    sig = hmac.new(secret.encode("utf-8"), signing_input, hashlib.sha256).digest()
    return f"{head}.{body}.{b64url(sig)}"


def load_dev_user(dotenv: dict[str, str]) -> dict[str, Any]:
    database_path = load_database_path(dotenv)
    if not database_path.is_file():
        raise RuntimeError(f"local SQLite database does not exist: {database_path}")

    target_user_id = os.environ.get("AOS_PM_DIAG_USER_ID", "").strip()
    target_email = os.environ.get("AOS_PM_DIAG_USER_EMAIL", "").strip()
    where = "is_active = 1"
    params: list[Any] = []
    if target_user_id:
        where += " AND id = ?"
        params.append(target_user_id)
    if target_email:
        where += " AND email = ?"
        params.append(target_email)

    conn = sqlite3.connect(database_path)
    conn.row_factory = sqlite3.Row
    try:
        row = conn.execute(
            "SELECT id, email, role, tenant_id FROM users "
            f"WHERE {where} ORDER BY (role = 'admin') DESC, created_at ASC LIMIT 1",
            params,
        ).fetchone()
        if not row:
            raise RuntimeError("no active user found for local dev JWT auth")
        return dict(row)
    finally:
        conn.close()


def resolve_auth() -> AuthContext:
    token = os.environ.get("AOS_PM_DIAG_TOKEN", "").strip()
    if token:
        return AuthContext(
            token=token,
            tenant_id=os.environ.get("AOS_PM_DIAG_TENANT_ID") or None,
            user_id=os.environ.get("AOS_PM_DIAG_USER_ID") or None,
            email=os.environ.get("AOS_PM_DIAG_USER_EMAIL") or None,
            source="env-token",
        )

    dotenv = load_dotenv(ROOT / ".env")
    secret = os.environ.get("JWT_SECRET") or dotenv.get("JWT_SECRET")
    if not secret:
        raise RuntimeError("JWT_SECRET is required when AOS_PM_DIAG_TOKEN is not set")
    user = load_dev_user(dotenv)
    return AuthContext(
        token=sign_dev_jwt(secret, user),
        tenant_id=user.get("tenant_id"),
        user_id=user.get("id"),
        email=user.get("email"),
        source="local-dev-jwt",
    )


def load_prompt() -> str:
    prompt = os.environ.get("AOS_PM_DIAG_PROMPT", "")
    prompt_file = os.environ.get("AOS_PM_DIAG_PROMPT_FILE", "")
    if prompt_file:
        prompt = Path(prompt_file).read_text(encoding="utf-8")
    prompt = prompt.strip()
    if not prompt:
        raise RuntimeError("set AOS_PM_DIAG_PROMPT or AOS_PM_DIAG_PROMPT_FILE")
    return prompt


class Api:
    def __init__(self, base_url: str, auth: AuthContext) -> None:
        self.base_url = base_url.rstrip("/")
        self.auth = auth

    def _headers(self) -> dict[str, str]:
        headers = {
            "Authorization": f"Bearer {self.auth.token}",
            "Content-Type": "application/json",
            "Accept": "application/json",
            "User-Agent": "AOS PM Assistant Flow Diagnostic",
        }
        if self.auth.tenant_id:
            headers["X-Tenant-ID"] = self.auth.tenant_id
        return headers

    def request_json(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
        timeout: int = 60,
    ) -> Any:
        data = None if body is None else json.dumps(body, ensure_ascii=False).encode("utf-8")
        req = urllib.request.Request(
            f"{self.base_url}{path}",
            data=data,
            headers=self._headers(),
            method=method,
        )
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                raw = resp.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode("utf-8", "replace")
            raise RuntimeError(f"{method} {path} failed: HTTP {exc.code}: {raw[:1000]}") from exc
        if not raw:
            return None
        try:
            return json.loads(raw)
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"{method} {path} returned non-json: {raw[:1000]}") from exc

    def upload_file(self, path: Path, timeout: int = 120) -> dict[str, Any]:
        if not path.exists() or not path.is_file():
            raise RuntimeError(f"document path not found: {path}")
        boundary = f"----aosdiag{int(time.time() * 1000)}{os.getpid()}"
        media_type = mimetypes.guess_type(str(path))[0] or "application/octet-stream"
        body = (
            f"--{boundary}\r\n"
            f'Content-Disposition: form-data; name="file"; filename="{path.name}"\r\n'
            f"Content-Type: {media_type}\r\n\r\n"
        ).encode("utf-8")
        body += path.read_bytes()
        body += f"\r\n--{boundary}--\r\n".encode("utf-8")
        headers = {
            "Authorization": f"Bearer {self.auth.token}",
            "Accept": "application/json",
            "Content-Type": f"multipart/form-data; boundary={boundary}",
            "User-Agent": "AOS PM Assistant Flow Diagnostic",
        }
        if self.auth.tenant_id:
            headers["X-Tenant-ID"] = self.auth.tenant_id
        req = urllib.request.Request(
            f"{self.base_url}/api/v1/uploads/upload",
            data=body,
            headers=headers,
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                raw = resp.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode("utf-8", "replace")
            raise RuntimeError(f"POST /api/v1/uploads/upload failed: HTTP {exc.code}: {raw[:1000]}") from exc
        try:
            uploaded = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"POST /api/v1/uploads/upload returned non-json: {raw[:1000]}") from exc
        if not isinstance(uploaded, dict) or not uploaded.get("url"):
            raise RuntimeError(f"unexpected upload response for {path}: {uploaded!r}")
        return uploaded

    def stream_sse(
        self,
        path: str,
        deadline: float,
        event_sink: list[dict[str, Any]],
        socket_timeout: int = 30,
    ) -> None:
        req = urllib.request.Request(
            f"{self.base_url}{path}",
            headers={k: v for k, v in self._headers().items() if k != "Content-Type"},
            method="GET",
        )
        event_name = ""
        data_lines: list[str] = []

        def flush() -> None:
            nonlocal event_name, data_lines
            if not data_lines:
                event_name = ""
                return
            raw_data = "\n".join(data_lines)
            try:
                payload: Any = json.loads(raw_data)
            except json.JSONDecodeError:
                payload = {"raw": raw_data}
            event_sink.append(
                {
                    "receivedAt": dt.datetime.now(dt.timezone.utc).isoformat(),
                    "event": event_name or "message",
                    "payload": payload,
                }
            )
            event_name = ""
            data_lines = []

        try:
            with urllib.request.urlopen(req, timeout=socket_timeout) as resp:
                while time.time() < deadline:
                    try:
                        raw_line = resp.readline()
                    except socket.timeout:
                        event_sink.append(
                            {
                                "receivedAt": dt.datetime.now(dt.timezone.utc).isoformat(),
                                "event": "diagnostic_socket_timeout",
                                "payload": {"socketTimeoutSecs": socket_timeout},
                            }
                        )
                        continue
                    if raw_line == b"":
                        flush()
                        return
                    line = raw_line.decode("utf-8", "replace").rstrip("\r\n")
                    if line == "":
                        flush()
                    elif line.startswith("event:"):
                        event_name = line[6:].strip()
                    elif line.startswith("data:"):
                        data_lines.append(line[5:].lstrip())
        except urllib.error.HTTPError as exc:
            body = exc.read().decode("utf-8", "replace")
            event_sink.append(
                {
                    "receivedAt": dt.datetime.now(dt.timezone.utc).isoformat(),
                    "event": "diagnostic_stream_http_error",
                    "payload": {"status": exc.code, "body": body[:2000]},
                }
            )
        except Exception as exc:  # noqa: BLE001
            event_sink.append(
                {
                    "receivedAt": dt.datetime.now(dt.timezone.utc).isoformat(),
                    "event": "diagnostic_stream_error",
                    "payload": {"error": repr(exc)},
                }
            )


def load_database_path(dotenv: dict[str, str] | None = None) -> Path:
    dotenv = dotenv if dotenv is not None else load_dotenv(ROOT / ".env")
    configured = os.environ.get("AOS_PM_DIAG_DB") or dotenv.get("AOS_PM_DIAG_DB")
    if configured:
        return Path(configured).expanduser()
    data_dir = os.environ.get("AOS_DATA_DIR") or dotenv.get("AOS_DATA_DIR")
    return Path(data_dir).expanduser() / "aos.db" if data_dir else ROOT / ".aos-data" / "aos.db"


def load_document_paths() -> list[Path]:
    raw = os.environ.get("AOS_PM_DIAG_DOCUMENT_PATHS", "").strip()
    if not raw:
        return []
    paths: list[Path] = []
    for item in re.split(r"[,;\n]", raw):
        item = item.strip()
        if item:
            paths.append(Path(item).expanduser())
    return paths


def uploaded_documents_from_paths(api: Api, paths: list[Path]) -> list[dict[str, Any]]:
    documents: list[dict[str, Any]] = []
    for path in paths:
        uploaded = api.upload_file(path)
        documents.append(
            {
                "url": uploaded["url"],
                "name": uploaded.get("filename") or path.name,
                "mediaType": uploaded.get("mediaType") or uploaded.get("media_type"),
                "sizeBytes": uploaded.get("size"),
            }
        )
        print(
            "[pm-diag] uploaded "
            f"{path} -> {uploaded.get('url')} "
            f"media={uploaded.get('mediaType') or uploaded.get('media_type')}"
        )
    return documents


def capture_db_snapshot(task_id: str) -> dict[str, Any]:
    database_path = load_database_path()
    if not database_path.is_file():
        return {"available": False, "reason": f"SQLite database missing: {database_path}"}
    try:
        conn = sqlite3.connect(database_path)
        conn.row_factory = sqlite3.Row
    except Exception as exc:  # noqa: BLE001
        return {"available": False, "reason": f"connect failed: {exc}"}

    def query(cur: Any, sql: str, params: tuple[Any, ...] = ()) -> list[dict[str, Any]]:
        cur.execute(sql, params)
        return [dict(row) for row in cur.fetchall()]

    try:
        cur = conn.cursor()
        try:
            return {
                "available": True,
                "task": query(
                    cur,
                    """
                    SELECT task_id, status, stage, attempt, elapsed_ms, stage_elapsed_ms,
                           cancel_requested, lease_owner, lease_expires_at, heartbeat_at,
                           completed_at, updated_at
                    FROM pm_research_tasks WHERE task_id = ?
                    """,
                    (task_id,),
                ),
                "run": query(
                    cur,
                    """
                    SELECT run_id, status, current_stage, attempt, budget_profile,
                           pipeline_timeout_secs, total_elapsed_ms, error_code, error_message,
                           started_at, deadline_at, ended_at, updated_at
                    FROM pm_research_runs WHERE run_id = ?
                    """,
                    (task_id,),
                ),
                "stageAttempts": query(
                    cur,
                    """
                    SELECT stage, attempt_no, status, strategy, route_key, channel, variant,
                           timeout_secs, budget_secs, elapsed_ms, error_code, error_message,
                           substr(CAST(detail_json AS TEXT), 1, 4000) AS detail_preview,
                           started_at, ended_at, updated_at
                    FROM pm_research_stage_attempts
                    WHERE run_id = ?
                    ORDER BY id DESC LIMIT 80
                    """,
                    (task_id,),
                ),
                "sourceSlots": query(
                    cur,
                    """
                    SELECT slot_seq, route_key, channel, variant, source_key, source_url, status,
                           tool_call_count, elapsed_ms, error_code, error_message,
                           substr(CAST(detail_json AS TEXT), 1, 4000) AS detail_preview,
                           started_at, ended_at, updated_at
                    FROM pm_research_source_slots
                    WHERE run_id = ?
                    ORDER BY id DESC LIMIT 80
                    """,
                    (task_id,),
                ),
                "subtasks": query(
                    cur,
                    """
                    SELECT subtask_key, subtask_id, title, status, probe_candidate_count,
                           probe_completed_count, citation_count, domain_count, tool_call_count,
                           quality_score, error_code, error_message,
                           substr(CAST(detail_json AS TEXT), 1, 2000) AS detail_preview,
                           started_at, ended_at, updated_at
                    FROM pm_subtask_runs
                    WHERE run_id = ?
                    ORDER BY id DESC LIMIT 80
                    """,
                    (task_id,),
                ),
            }
        finally:
            cur.close()
    except Exception as exc:  # noqa: BLE001
        return {"available": False, "reason": f"snapshot query failed: {exc}"}
    finally:
        conn.close()


def is_terminal_status(obj: Any) -> bool:
    if not isinstance(obj, dict):
        return False
    stage = str(obj.get("stage") or "").lower()
    status = str(obj.get("status") or "").lower()
    if stage in TERMINAL_STAGES or status in {"failed", "cancelled"}:
        return True
    return status == "completed" and bool(obj.get("response"))


def walk(value: Any, path: str = ""):
    yield path, value
    if isinstance(value, dict):
        for key, item in value.items():
            child = f"{path}.{key}" if path else str(key)
            yield from walk(item, child)
    elif isinstance(value, list):
        for idx, item in enumerate(value):
            child = f"{path}[{idx}]"
            yield from walk(item, child)


def collect_urls(value: Any) -> list[str]:
    seen: set[str] = set()
    urls: list[str] = []
    ignored_path_prefixes = (
        "baseUrl",
        "auth",
        "createSessionResponse",
        "startTaskResponse",
    )
    for path, item in walk(value):
        if path.startswith(ignored_path_prefixes):
            continue
        if isinstance(item, str):
            for match in URL_RE.findall(item):
                clean = match.rstrip(".,;:，。；）)]}")
                host = domain_of(clean)
                if host.startswith("localhost") or host.startswith("127.0.0.1"):
                    continue
                if clean not in seen:
                    seen.add(clean)
                    urls.append(clean)
    return urls


def domain_of(url: str) -> str:
    try:
        return urllib.parse.urlparse(url).netloc.lower()
    except Exception:  # noqa: BLE001
        return ""


def collect_search_markers(value: Any) -> dict[str, Any]:
    markers: list[dict[str, Any]] = []
    layer_counts: dict[str, int] = {}
    for path, item in walk(value):
        low_path = path.lower()
        if isinstance(item, str):
            low_item = item.lower()
            if any(
                token in low_path or token in low_item
                for token in (
                    "native_model_search",
                    "responses_native",
                    "web_search",
                    "web.search",
                    "search_stage",
                    "used_layer",
                    "mcp",
                    "rag_local",
                    "configured_provider",
                    "search extension",
                    "search_ext",
                )
            ):
                markers.append({"path": path, "value": item[:500]})
                for name in (
                    "native_model_search",
                    "responses_native_web_search",
                    "configured_provider",
                    "mcp",
                    "rag_local",
                    "web.search.general",
                    "web_search",
                ):
                    if name in low_item:
                        layer_counts[name] = layer_counts.get(name, 0) + 1
        elif isinstance(item, (int, float, bool)) and any(
            token in low_path
            for token in (
                "native_attempt",
                "provider_attempt",
                "mcp_attempt",
                "rag_local_attempt",
                "tool_call_count",
                "citation_count",
                "domain_count",
            )
        ):
            markers.append({"path": path, "value": item})
    return {"layerCounts": layer_counts, "markers": markers[:300]}


def collect_timeouts(value: Any) -> list[dict[str, str]]:
    out: list[dict[str, str]] = []
    for path, item in walk(value):
        if isinstance(item, str) and TIMEOUT_RE.search(item):
            out.append({"path": path, "value": item[:800]})
    return out[:100]


def extract_response_text(status: Any, events: list[dict[str, Any]]) -> str:
    candidates: list[Any] = []
    if isinstance(status, dict):
        candidates.append(status.get("response"))
    for event in reversed(events):
        payload = event.get("payload")
        if isinstance(payload, dict):
            candidates.append(payload.get("response"))
    for candidate in candidates:
        if isinstance(candidate, dict):
            for key in ("text", "answer", "content", "markdown"):
                value = candidate.get(key)
                if isinstance(value, str) and value.strip():
                    return value.strip()
    return ""


def summarize_diagnostic(raw: dict[str, Any]) -> dict[str, Any]:
    events = raw.get("events") if isinstance(raw.get("events"), list) else []
    status = raw.get("status") if isinstance(raw.get("status"), dict) else {}
    history = raw.get("history") if isinstance(raw.get("history"), dict) else {}
    subtasks = raw.get("subtasks") if isinstance(raw.get("subtasks"), dict) else {}
    attempts = raw.get("attempts") if isinstance(raw.get("attempts"), dict) else {}
    response_text = extract_response_text(status, events)

    history_messages = history.get("messages") if isinstance(history.get("messages"), list) else []
    assistant_messages = [
        msg for msg in history_messages if isinstance(msg, dict) and msg.get("role") == "assistant"
    ]
    user_messages = [
        msg for msg in history_messages if isinstance(msg, dict) and msg.get("role") == "user"
    ]
    urls = collect_urls(raw)
    domains = sorted({domain_of(url) for url in urls if domain_of(url)})
    stages: list[dict[str, Any]] = []
    for event in events:
        payload = event.get("payload")
        if isinstance(payload, dict):
            stages.append(
                {
                    "event": event.get("event"),
                    "status": payload.get("status"),
                    "stage": payload.get("stage"),
                    "attempt": payload.get("attempt"),
                    "elapsedMs": payload.get("elapsed_ms") or payload.get("elapsedMs"),
                    "stageElapsedMs": payload.get("stage_elapsed_ms")
                    or payload.get("stageElapsedMs"),
                    "message": payload.get("message"),
                }
            )

    return {
        "taskId": raw.get("taskId"),
        "sessionId": raw.get("sessionId"),
        "authSource": raw.get("auth", {}).get("source") if isinstance(raw.get("auth"), dict) else None,
        "terminal": {
            "status": status.get("status"),
            "stage": status.get("stage"),
            "attempt": status.get("attempt"),
            "elapsedMs": status.get("elapsed_ms") or status.get("elapsedMs"),
            "stageElapsedMs": status.get("stage_elapsed_ms") or status.get("stageElapsedMs"),
            "error": status.get("error"),
            "cancelRequested": status.get("cancel_requested") or status.get("cancelRequested"),
        },
        "response": {
            "textChars": len(response_text),
            "hasNoCitableSourceNotice": "本轮未" in response_text and "来源" in response_text,
            "hasEllipsisMoreArtifacts": bool(re.search(r"\\.\\.\\.|\\+\\d+\\s*more", response_text, re.I)),
            "preview": response_text[:1200],
        },
        "history": {
            "messageCount": len(history_messages),
            "userMessageCount": len(user_messages),
            "assistantMessageCount": len(assistant_messages),
            "assistantPreviews": [
                json.dumps(msg, ensure_ascii=False)[:700] for msg in assistant_messages[-5:]
            ],
        },
        "events": {
            "count": len(events),
            "lastStages": stages[-30:],
        },
        "subtasks": {
            "count": subtasks.get("count") if isinstance(subtasks, dict) else None,
            "rows": len(subtasks.get("items") or []) if isinstance(subtasks, dict) else 0,
        },
        "attempts": {
            "subtaskKeys": sorted(attempts.keys()) if isinstance(attempts, dict) else [],
            "rowCount": sum(
                len(v.get("items") or []) for v in attempts.values() if isinstance(v, dict)
            )
            if isinstance(attempts, dict)
            else 0,
        },
        "urls": {
            "count": len(urls),
            "domainCount": len(domains),
            "domains": domains[:50],
            "samples": urls[:50],
        },
        "search": collect_search_markers(raw),
        "timeouts": collect_timeouts(raw),
    }


def make_output_paths(output_dir: Path, task_id: str) -> tuple[Path, Path]:
    output_dir.mkdir(parents=True, exist_ok=True)
    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    base = output_dir / f"pm-flow-{stamp}-{task_id}"
    return base.with_suffix(".raw.json"), base.with_suffix(".summary.json")


def write_diagnostic_output(
    output_dir: Path,
    raw: dict[str, Any],
    task_id: str,
) -> tuple[Path, Path, dict[str, Any]]:
    summary = summarize_diagnostic(raw)
    raw_path, summary_path = make_output_paths(output_dir, task_id)
    raw_path.write_text(json.dumps(raw, ensure_ascii=False, indent=2, default=str), encoding="utf-8")
    summary_path.write_text(
        json.dumps(summary, ensure_ascii=False, indent=2, default=str),
        encoding="utf-8",
    )
    return raw_path, summary_path, summary


def main() -> int:
    started = time.time()
    prompt = load_prompt()
    base_url = os.environ.get("AOS_PM_DIAG_BASE_URL", DEFAULT_BASE_URL).rstrip("/")
    model = os.environ.get("AOS_PM_DIAG_MODEL", "").strip()
    timeout_secs = int(os.environ.get("AOS_PM_DIAG_TIMEOUT_SECS", "1800"))
    poll_interval = float(os.environ.get("AOS_PM_DIAG_POLL_INTERVAL_SECS", "5"))
    output_dir = Path(os.environ.get("AOS_PM_DIAG_OUTPUT_DIR", "/tmp/aos-pm-diagnostics"))
    auth = resolve_auth()
    api = Api(base_url, auth)
    document_paths = load_document_paths()

    session_body: dict[str, Any] = {"source": "pm", "scenario": "pm", "locale": "zh-CN"}
    if model:
        session_body["model"] = model

    print(f"[pm-diag] base_url={base_url}")
    print(
        "[pm-diag] auth="
        f"{auth.source} user={auth.email or auth.user_id or 'unknown'} tenant={auth.tenant_id or 'unknown'}"
    )
    print(f"[pm-diag] prompt_chars={len(prompt)} timeout_secs={timeout_secs}")
    if document_paths:
        print(f"[pm-diag] document_paths={', '.join(str(path) for path in document_paths)}")

    reuse_session_id = os.environ.get("AOS_PM_DIAG_SESSION_ID", "").strip()
    if reuse_session_id:
        create_resp = {"reused": True, "sessionId": reuse_session_id}
        session_id = reuse_session_id
        print(f"[pm-diag] session_id={session_id} reused=true")
    else:
        create_resp = api.request_json("POST", "/api/v1/agent/sessions", session_body)
        session = create_resp.get("session") if isinstance(create_resp, dict) else None
        if not isinstance(session, dict):
            raise RuntimeError(f"unexpected create session response: {create_resp!r}")
        session_id = (
            session.get("session_id")
            or session.get("sessionId")
            or session.get("id")
            or session.get("session_id".replace("_", ""))
        )
        if not isinstance(session_id, str) or not session_id:
            raise RuntimeError(f"session id not found in response: {create_resp!r}")
        print(f"[pm-diag] session_id={session_id}")
    documents = uploaded_documents_from_paths(api, document_paths)

    start_resp = api.request_json(
        "POST",
        f"/api/v1/agent/sessions/{urllib.parse.quote(session_id)}/pm-research-tasks",
        {"message": prompt, "images": [], "documents": documents},
    )
    if not isinstance(start_resp, dict):
        raise RuntimeError(f"unexpected task start response: {start_resp!r}")
    task_id = start_resp.get("task_id") or start_resp.get("taskId")
    if not isinstance(task_id, str) or not task_id:
        raise RuntimeError(f"task id not found in response: {start_resp!r}")
    print(f"[pm-diag] task_id={task_id}")

    events: list[dict[str, Any]] = []
    status: Any = None
    subtasks: Any = None
    attempts: dict[str, Any] = {}
    history: Any = None
    deadline = time.time() + timeout_secs
    interrupted = False
    diagnostic_error: str | None = None
    try:
        api.stream_sse(
            f"/api/v1/agent/pm-research-tasks/{urllib.parse.quote(task_id)}/events",
            deadline,
            events,
            socket_timeout=int(os.environ.get("AOS_PM_DIAG_SOCKET_TIMEOUT_SECS", "30")),
        )

        while time.time() < deadline:
            status = api.request_json(
                "GET",
                f"/api/v1/agent/pm-research-tasks/{urllib.parse.quote(task_id)}",
                timeout=60,
            )
            if is_terminal_status(status):
                break
            print(
                "[pm-diag] waiting "
                f"status={status.get('status') if isinstance(status, dict) else None} "
                f"stage={status.get('stage') if isinstance(status, dict) else None} "
                f"elapsed={status.get('elapsed_ms') or status.get('elapsedMs') if isinstance(status, dict) else None}"
            )
            time.sleep(poll_interval)
    except KeyboardInterrupt:
        interrupted = True
        print("[pm-diag] interrupted; writing diagnostic snapshot", file=sys.stderr)
    except Exception as exc:  # noqa: BLE001
        diagnostic_error = repr(exc)
        print(f"[pm-diag] polling failed; writing diagnostic snapshot: {exc}", file=sys.stderr)
    finally:
        try:
            status = api.request_json(
                "GET",
                f"/api/v1/agent/pm-research-tasks/{urllib.parse.quote(task_id)}",
                timeout=60,
            )
        except Exception as exc:  # noqa: BLE001
            status = {"diagnosticError": f"status fetch failed: {exc}"}

        try:
            subtasks = api.request_json(
                "GET",
                f"/api/v1/agent/pm-research-tasks/{urllib.parse.quote(task_id)}/subtasks",
                timeout=60,
            )
        except Exception as exc:  # noqa: BLE001
            subtasks = {"diagnosticError": f"subtasks fetch failed: {exc}"}

        if isinstance(subtasks, dict):
            for row in subtasks.get("items") or []:
                if not isinstance(row, dict):
                    continue
                key = row.get("subtask_id") or row.get("subtask_key")
                if not isinstance(key, str) or not key.strip():
                    continue
                try:
                    attempts[key] = api.request_json(
                        "GET",
                        f"/api/v1/agent/pm-research-tasks/{urllib.parse.quote(task_id)}"
                        f"/subtasks/{urllib.parse.quote(key)}/attempts",
                        timeout=60,
                    )
                except Exception as exc:  # noqa: BLE001
                    attempts[key] = {"diagnosticError": f"attempt fetch failed: {exc}"}

        try:
            history = api.request_json(
                "GET",
                f"/api/v1/agent/sessions/{urllib.parse.quote(session_id)}/history",
                timeout=60,
            )
        except Exception as exc:  # noqa: BLE001
            history = {"diagnosticError": f"history fetch failed: {exc}"}

    raw = {
        "createdAt": dt.datetime.now(dt.timezone.utc).isoformat(),
        "elapsedSecs": round(time.time() - started, 3),
        "baseUrl": base_url,
        "model": model or None,
        "auth": {
            "source": auth.source,
            "tenantId": auth.tenant_id,
            "userId": auth.user_id,
            "email": auth.email,
        },
        "promptChars": len(prompt),
        "promptPreview": prompt[:2000],
        "documents": documents,
        "sessionId": session_id,
        "taskId": task_id,
        "createSessionResponse": create_resp,
        "startTaskResponse": start_resp,
        "events": events,
        "status": status,
        "subtasks": subtasks,
        "attempts": attempts,
        "history": history,
        "dbSnapshot": capture_db_snapshot(task_id),
        "interrupted": interrupted,
        "diagnosticError": diagnostic_error,
    }
    raw_path, summary_path, summary = write_diagnostic_output(output_dir, raw, task_id)
    print(f"[pm-diag] raw={raw_path}")
    print(f"[pm-diag] summary={summary_path}")
    print("[pm-diag] summary:")
    print(json.dumps(summary, ensure_ascii=False, indent=2)[:8000])
    if interrupted:
        return 130
    if diagnostic_error:
        return 2
    if not is_terminal_status(status):
        print("[pm-diag] task did not reach terminal status before diagnostic timeout", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print("[pm-diag] interrupted", file=sys.stderr)
        raise
    except Exception as exc:  # noqa: BLE001
        print(f"[pm-diag] failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
