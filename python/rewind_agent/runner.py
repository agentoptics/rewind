"""Operator-friendly runner library for Rewind dispatch webhooks.

A *runner* is a long-lived agent process that exposes an HTTP
webhook endpoint. The Rewind server POSTs replay-job dispatches
directly to the configured webhook URL; the runner replies
``202 Accepted`` immediately and asynchronously runs the agent
under the supplied ``replay_context_id``. As the agent progresses,
the runner POSTs ``started`` / ``progress`` / ``completed`` /
``errored`` events back to ``POST /api/replay-jobs/{id}/events``.

This module ships:

- :class:`DispatchPayload` — decoded webhook body.
- :class:`ProgressReporter` — convenience wrapper around the
  events endpoint.
- :func:`asgi_handler` — parses the webhook body, creates a
  ``DispatchPayload`` and ``ProgressReporter``, and dispatches a
  coroutine to user code via the ``@handle_replay`` decorator.
- :func:`handle_replay` — decorator for user code that processes
  a dispatched job.

## Example

.. code-block:: python

    from fastapi import FastAPI, Request
    from rewind_agent import runner

    app = FastAPI()

    @runner.handle_replay
    async def my_replay_handler(payload, reporter):
        from rewind_agent import intercept
        from rewind_agent.explicit import ExplicitClient

        client = ExplicitClient(base_url=payload.base_url)
        client.attach_replay_context(
            session_id=payload.session_id,
            replay_context_id=payload.replay_context_id,
        )
        intercept.install()

        for i, step in enumerate(my_agent_run(), start=1):
            await reporter.progress(i)

        await reporter.completed()

    @app.post("/rewind-webhook")
    async def webhook(request: Request):
        body = await request.body()
        return await runner.asgi_handler(
            body_bytes=body,
            handler=my_replay_handler,
        )
"""

from __future__ import annotations

import asyncio
import dataclasses
import json
import logging
from typing import Any, Awaitable, Callable, Optional

logger = logging.getLogger(__name__)


# ──────────────────────────────────────────────────────────────────
# Dispatch payload
# ──────────────────────────────────────────────────────────────────


@dataclasses.dataclass(frozen=True)
class DispatchPayload:
    """Decoded body of a dispatch webhook from the Rewind server.

    Mirrors ``crates/rewind-web/src/runners.rs`` dispatch payload.

    ``replay_context_timeline_id`` is the fork the replay context is
    bound to — pass it to ``ExplicitClient.attach_replay_context`` so
    live cache misses (writes) record into the fork, not the source.

    ``source_timeline_id`` is the timeline that holds the user's edits.
    Runners use it to **read** the (potentially edited) step content at
    ``at_step``.  For ``CreateAndDispatch`` this differs from the fork;
    for ``ReuseContext`` both point to the same timeline.

    ``at_step`` is the original fork-point — the step number the user
    clicked "Run replay" at.  When ``at_step > 1`` runners reconstruct
    conversation history from steps 1..at_step-1 on the source timeline.
    """

    job_id: str
    session_id: str
    replay_context_id: str
    replay_context_timeline_id: str
    source_timeline_id: str
    base_url: str
    at_step: int
    dispatch_token: str

    @classmethod
    def from_json(cls, body: dict[str, Any]) -> "DispatchPayload":
        return cls(
            job_id=body["job_id"],
            session_id=body["session_id"],
            replay_context_id=body["replay_context_id"],
            replay_context_timeline_id=body["replay_context_timeline_id"],
            source_timeline_id=body["source_timeline_id"],
            base_url=body["base_url"],
            at_step=body["at_step"],
            dispatch_token=body["dispatch_token"],
        )


# ──────────────────────────────────────────────────────────────────
# Progress reporter
# ──────────────────────────────────────────────────────────────────


class ProgressReporter:
    """Thin wrapper around the events endpoint.

    Use this from inside a ``@handle_replay`` handler to emit
    ``started`` / ``progress`` / ``completed`` / ``errored`` events.
    Built on top of httpx (already required transitively by other
    rewind_agent modules); falls back to ``urllib`` if httpx is
    absent.
    """

    def __init__(self, job_id: str, base_url: str, dispatch_token: Optional[str] = None) -> None:
        self.job_id = job_id
        self._dispatch_token = dispatch_token
        url_root = base_url.rstrip("/")
        self._url = f"{url_root}/api/replay-jobs/{job_id}/events"

    async def started(self) -> None:
        await self._post({"event_type": "started"})

    async def progress(
        self,
        step_number: int,
        progress_total: Optional[int] = None,
        payload: Optional[dict[str, Any]] = None,
    ) -> None:
        body: dict[str, Any] = {
            "event_type": "progress",
            "step_number": step_number,
        }
        if progress_total is not None:
            body["progress_total"] = progress_total
        if payload is not None:
            body["payload"] = payload
        await self._post(body)

    async def completed(self) -> None:
        await self._post({"event_type": "completed"})

    async def errored(
        self,
        error_message: str,
        error_stage: str = "agent",
    ) -> None:
        await self._post(
            {
                "event_type": "errored",
                "error_message": error_message,
                "error_stage": error_stage,
            }
        )

    async def _post(self, body: dict[str, Any]) -> None:
        headers: dict[str, str] = {"Content-Type": "application/json"}
        if self._dispatch_token:
            headers["X-Rewind-Dispatch-Token"] = self._dispatch_token
        body_bytes = json.dumps(body).encode("utf-8")

        try:
            import httpx  # noqa: PLC0415
        except ImportError:
            await asyncio.to_thread(
                _urllib_post, self._url, headers, body_bytes
            )
            return

        try:
            async with httpx.AsyncClient(timeout=10.0) as client:
                resp = await client.post(self._url, headers=headers, content=body_bytes)
                if resp.status_code >= 400:
                    logger.warning(
                        "rewind runner: event POST %s returned %s: %s",
                        body.get("event_type"),
                        resp.status_code,
                        resp.text[:200],
                    )
        except Exception as e:  # noqa: BLE001
            logger.error("rewind runner: event POST failed: %s", e)


def _urllib_post(url: str, headers: dict[str, str], body: bytes) -> None:
    """Sync fallback when httpx isn't installed."""
    import urllib.error  # noqa: PLC0415
    import urllib.request  # noqa: PLC0415

    req = urllib.request.Request(url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            if resp.status >= 400:
                logger.warning("rewind runner: event POST returned %s", resp.status)
    except urllib.error.URLError as e:
        logger.error("rewind runner: event POST failed: %s", e)


# ──────────────────────────────────────────────────────────────────
# ASGI handler + decorator
# ──────────────────────────────────────────────────────────────────


HandlerFn = Callable[[DispatchPayload, ProgressReporter], Awaitable[None]]


def handle_replay(fn: HandlerFn) -> HandlerFn:
    """Marker decorator for user code that processes a dispatch.

    The decorator currently just returns the function unchanged —
    it exists so the docs and examples show a clean attribution
    point and so future versions can attach metadata or wrap with
    automatic error reporting.
    """
    return fn


async def asgi_handler(
    *,
    body_bytes: bytes,
    handler: HandlerFn,
    base_url: str = "http://127.0.0.1:4800",
    auto_emit_started: bool = True,
) -> tuple[int, dict[str, Any]]:
    """Parse the dispatch body, dispatch the handler, return ``(status, body)``.

    Plug this into your web framework. FastAPI example in the
    module docstring above; aiohttp / Starlette adapt the same way.

    No signature verification is performed — this relies on the runner
    being reachable only from the dispatching Rewind server (co-located
    sidecar / localhost). The dispatch payload includes a per-job
    ``dispatch_token`` that the :class:`ProgressReporter` echoes back
    via ``X-Rewind-Dispatch-Token`` when posting events.

    The handler runs as a background task — this function returns
    ``(202, {"job_id": ...})`` immediately so the Rewind server's
    timeout is satisfied.
    """
    try:
        body = json.loads(body_bytes)
        payload = DispatchPayload.from_json(body)
    except (ValueError, KeyError) as e:
        return 400, {"error": f"invalid dispatch body: {e}"}

    reporter = ProgressReporter(
        payload.job_id,
        base_url=payload.base_url or base_url,
        dispatch_token=payload.dispatch_token,
    )

    async def _run() -> None:
        if auto_emit_started:
            await reporter.started()
        try:
            await handler(payload, reporter)
        except Exception as e:  # noqa: BLE001
            logger.exception("rewind runner: handler raised")
            try:
                await reporter.errored(
                    error_message=f"handler raised: {e}",
                    error_stage="agent",
                )
            except Exception:
                logger.exception("rewind runner: errored event POST also failed")

    asyncio.create_task(_run())
    return 202, {"job_id": payload.job_id, "accepted": True}
