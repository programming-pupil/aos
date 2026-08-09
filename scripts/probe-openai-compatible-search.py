#!/usr/bin/env python3
"""Probe OpenAI-compatible chat/search endpoints without logging secrets.

Usage:
  AOS_PROBE_BASE_URL=https://example.com/v1 \
  AOS_PROBE_MODEL=gpt-5.5 \
  AOS_PROBE_API_KEY=sk-... \
  python3 scripts/probe-openai-compatible-search.py
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request
from typing import Any


BASE_URL = os.environ.get("AOS_PROBE_BASE_URL", "").rstrip("/")
MODEL = os.environ.get("AOS_PROBE_MODEL", "")
API_KEY = os.environ.get("AOS_PROBE_API_KEY", "")
QUESTION = os.environ.get(
    "AOS_PROBE_QUESTION",
    "请联网查询北京今天和明天的天气预报，用中文简短回答，并给出来源。",
)


def require_env() -> None:
    missing = [
        name
        for name, value in [
            ("AOS_PROBE_BASE_URL", BASE_URL),
            ("AOS_PROBE_MODEL", MODEL),
            ("AOS_PROBE_API_KEY", API_KEY),
        ]
        if not value
    ]
    if missing:
        print(f"Missing env: {', '.join(missing)}", file=sys.stderr)
        sys.exit(2)


def post_json(path: str, payload: dict[str, Any], timeout: int = 90) -> tuple[int | None, str, float, str | None]:
    headers = {
        "Authorization": f"Bearer {API_KEY}",
        "Content-Type": "application/json",
        "Accept": "application/json",
        "User-Agent": "AOS/0.1 OpenAI-Compatible Search Probe",
    }
    request = urllib.request.Request(
        f"{BASE_URL}{path}",
        data=json.dumps(payload, ensure_ascii=False).encode("utf-8"),
        headers=headers,
        method="POST",
    )
    started = time.time()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return (
                response.status,
                response.read().decode("utf-8", "replace"),
                time.time() - started,
                None,
            )
    except urllib.error.HTTPError as exc:
        return (
            exc.code,
            exc.read().decode("utf-8", "replace"),
            time.time() - started,
            None,
        )
    except Exception as exc:  # noqa: BLE001 - diagnostic script
        return None, "", time.time() - started, repr(exc)


def extract_text(payload: Any) -> str:
    if not isinstance(payload, dict):
        return ""
    chunks: list[str] = []
    output_text = payload.get("output_text")
    if isinstance(output_text, str):
        chunks.append(output_text)
    for item in payload.get("output") or []:
        if not isinstance(item, dict):
            continue
        content = item.get("content")
        if isinstance(content, list):
            for part in content:
                if isinstance(part, dict) and isinstance(part.get("text"), str):
                    chunks.append(part["text"])
        elif isinstance(content, str):
            chunks.append(content)
    for choice in payload.get("choices") or []:
        if not isinstance(choice, dict):
            continue
        message = choice.get("message")
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if isinstance(content, str):
            chunks.append(content)
        elif isinstance(content, list):
            for part in content:
                if isinstance(part, dict) and isinstance(part.get("text"), str):
                    chunks.append(part["text"])
    return "\n".join(chunk for chunk in chunks if chunk).strip()


def summarize(name: str, status: int | None, body: str, elapsed: float, error: str | None) -> None:
    print(f"\n## {name}")
    print(f"status={status} elapsed={elapsed:.2f}s error={error or ''}")
    if not body:
        return
    try:
        payload = json.loads(body)
    except json.JSONDecodeError:
        print(f"body_non_json={body[:500]}")
        return

    if isinstance(payload, dict):
        print(f"top_keys={list(payload.keys())[:12]}")
        if "error" in payload:
            print(f"error_json={json.dumps(payload['error'], ensure_ascii=False)[:800]}")
        if "code" in payload or "message" in payload:
            print(
                "code_message="
                + json.dumps(
                    {key: payload.get(key) for key in ("code", "message") if key in payload},
                    ensure_ascii=False,
                )[:800]
            )
        output = payload.get("output")
        if isinstance(output, list):
            print(
                "output_types="
                + json.dumps(
                    [item.get("type") for item in output[:10] if isinstance(item, dict)],
                    ensure_ascii=False,
                )
            )
        choices = payload.get("choices")
        if isinstance(choices, list) and choices:
            choice = choices[0] if isinstance(choices[0], dict) else {}
            message = choice.get("message") if isinstance(choice, dict) else {}
            print(f"finish_reason={choice.get('finish_reason') if isinstance(choice, dict) else None}")
            print(f"message_keys={list(message.keys()) if isinstance(message, dict) else []}")
            if isinstance(message, dict) and message.get("tool_calls"):
                print(f"tool_calls={json.dumps(message['tool_calls'], ensure_ascii=False)[:1000]}")

    text = extract_text(payload)
    print(f"text_chars={len(text)}")
    if text:
        print(f"text_sample={text[:1000]}")


def main() -> None:
    require_env()
    message_array_input = [
        {
            "role": "system",
            "content": [{"type": "input_text", "text": "Use current sources."}],
        },
        {
            "role": "user",
            "content": [{"type": "input_text", "text": QUESTION}],
        },
    ]
    cases: list[tuple[str, str, dict[str, Any]]] = [
        (
            "responses_web_search_plain_string_minimal",
            "/responses",
            {
                "model": MODEL,
                "input": QUESTION,
                "tools": [{"type": "web_search"}],
                "tool_choice": "auto",
                "max_output_tokens": 500,
            },
        ),
        (
            "responses_web_search_plain_string_with_options",
            "/responses",
            {
                "model": MODEL,
                "input": QUESTION,
                "tools": [{"type": "web_search"}],
                "tool_choice": "auto",
                "max_output_tokens": 500,
                "web_search_options": {"search_context_size": "medium"},
            },
        ),
        (
            "responses_web_search_message_array_minimal",
            "/responses",
            {
                "model": MODEL,
                "input": message_array_input,
                "tools": [{"type": "web_search"}],
                "tool_choice": "auto",
                "max_output_tokens": 500,
            },
        ),
        (
            "responses_web_search_message_array_with_options",
            "/responses",
            {
                "model": MODEL,
                "input": message_array_input,
                "tools": [{"type": "web_search"}],
                "tool_choice": "auto",
                "max_output_tokens": 500,
                "web_search_options": {"search_context_size": "medium"},
            },
        ),
        (
            "responses_web_search_preview",
            "/responses",
            {
                "model": MODEL,
                "input": QUESTION,
                "tools": [{"type": "web_search_preview"}],
                "tool_choice": "auto",
                "max_output_tokens": 500,
            },
        ),
        (
            "chat_completions_web_search_preview",
            "/chat/completions",
            {
                "model": MODEL,
                "messages": [{"role": "user", "content": QUESTION}],
                "tools": [{"type": "web_search_preview"}],
                "tool_choice": "auto",
                "max_tokens": 500,
                "stream": False,
            },
        ),
        (
            "plain_chat_completions",
            "/chat/completions",
            {
                "model": MODEL,
                "messages": [{"role": "user", "content": "请只回复：OK"}],
                "max_tokens": 20,
                "stream": False,
            },
        ),
    ]
    for name, path, payload in cases:
        summarize(name, *post_json(path, payload))


if __name__ == "__main__":
    main()
