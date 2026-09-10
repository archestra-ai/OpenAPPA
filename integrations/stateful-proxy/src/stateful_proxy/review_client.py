"""Authenticated client for the existing change-board /pending and /decide API."""
from __future__ import annotations

import http.cookiejar
import json
import stat
from pathlib import Path
from typing import Any
from urllib.request import HTTPCookieProcessor, Request, build_opener


class AuthenticatedReviewerClient:
    def __init__(self, base_url: str, cookie_jar: str | Path) -> None:
        path = Path(cookie_jar)
        if stat.S_IMODE(path.stat().st_mode) != 0o600:
            raise PermissionError(f"cookie jar must be mode 0600: {path}")
        jar = http.cookiejar.MozillaCookieJar(str(path))
        jar.load(ignore_discard=True, ignore_expires=True)
        self._opener = build_opener(HTTPCookieProcessor(jar))
        self.base_url = base_url.rstrip("/")

    def pending(self) -> list[dict[str, Any]]:
        return self._request("GET", "/pending")["pending"]

    def decide(self, review: dict[str, Any], ruling: str, reason: str | None = None) -> dict[str, Any]:
        value: dict[str, Any] = {"id": review["id"], "ruling": ruling}
        if reason is not None:
            value["reason"] = reason
        return self._request("POST", "/decide", value, {"X-HITL-CSRF": review["csrf_token"]})

    def _request(self, method: str, path: str, value: dict[str, Any] | None = None, headers: dict[str, str] | None = None) -> dict[str, Any]:
        data = None if value is None else json.dumps(value, separators=(",", ":")).encode()
        request = Request(self.base_url + path, data=data, method=method, headers={"Accept": "application/json", "Content-Type": "application/json", **(headers or {})})
        with self._opener.open(request, timeout=10) as response:
            return json.loads(response.read().decode())
