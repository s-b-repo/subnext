"""Stable identifiers and a logical clock.

Everything in DCR that is addressable gets a content-derived id, so the same
material ingested twice lands on the same span and the same fact instead of
silently duplicating. Time is a logical counter rather than wall-clock: node
timestamps only need a total order for staleness comparisons, and a logical
clock keeps runs reproducible.
"""

from __future__ import annotations

import hashlib
import itertools


def digest(*parts: object, length: int = 12) -> str:
    h = hashlib.sha1()
    for p in parts:
        h.update(str(p).encode("utf-8"))
        h.update(b"\x1f")
    return h.hexdigest()[:length]


def make_id(prefix: str, *parts: object, length: int = 12) -> str:
    return f"{prefix}_{digest(*parts, length=length)}"


class Clock:
    """Monotonic logical clock. `tick()` is the timestamp stamped on nodes."""

    def __init__(self, start: int = 0) -> None:
        self._counter = itertools.count(start + 1)
        self._now = start

    def tick(self) -> int:
        self._now = next(self._counter)
        return self._now

    def now(self) -> int:
        return self._now

    def restore(self, value: int) -> None:
        """Resume a clock after loading a persisted store."""
        self._now = value
        self._counter = itertools.count(value + 1)
