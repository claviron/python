"""Type declarations for Gufi's runtime-installed native module.

Availability depends on the application's configured capabilities. This is not
a standalone Python implementation and supplies no legacy module alias.
"""

import builtins
from collections.abc import Awaitable
from logging import Handler
from typing import Any, Protocol

from . import channels as channels

def invoke(operation: str, payload: Any) -> Awaitable[Any]: ...

class Counter:
    @property
    def key(self) -> str: ...
    def get(self) -> int | None: ...
    def increment(self, by: int = 1) -> int: ...
    def decrement(self, by: int = 1) -> int: ...
    def reset(self) -> None: ...

def counter(key: str) -> Counter: ...

class Orbital:
    """One shared 64-bit value of the fleet: an int, a float or a bool.

    A handle, not a copy: every read and write goes to the fleet's shared
    memory. Creating one takes a slot for the life of the fleet's table and
    nothing releases it, so declare one for something that lasts, not one per
    request. Share it by passing its id.
    """

    @property
    def id(self) -> str:
        """`<kind>:<slot>:<epoch>`: what another worker opens."""
    def get(self) -> int | float | bool: ...
    def set(self, value: int | float | bool) -> None: ...
    def add(self, by: int | float) -> int | float:
        """Atomic on the value; the value after. An int wraps on overflow."""
    def wait(self, current: int | float | bool) -> int | float | bool:
        """Park until the value is no longer `current`; return what it is then.

        Returns at once when it already differs. The calling thread blocks
        with the GIL released, so call it from a worker thread, such as
        `await asyncio.to_thread(orbital.wait, seen)`, not on the event loop.
        Raises `RuntimeError` when the application is stopping. A float is
        compared by its bits.
        """

class _Orbitals(Protocol):
    def int(self, value: builtins.int) -> Orbital: ...
    def float(self, value: builtins.float) -> Orbital: ...
    def bool(self, value: builtins.bool) -> Orbital: ...
    def open(self, id: str) -> Orbital:
        """Open the value an id names. The id says which type it is."""

orbital: _Orbitals

class _Broadcast(Protocol):
    def __call__(
        self,
        channel: str | list[str],
        event: str,
        data: Any = None,
        *,
        application: str | None = None,
        socket_id: str | None = None,
    ) -> int: ...

broadcast: _Broadcast

class LogHandler(Handler):
    def __init__(self, channel: str, level: int | str = 0) -> None: ...
