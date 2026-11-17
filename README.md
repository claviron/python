# claviron-python

CPython embedded in a Rust host. This package owns PyO3 and the interpreter
boundary: one CPython 3.15 per process, its venv and the application's
environment. With `asgi` it runs ASGI applications: lifespan, streaming HTTP
and message sessions. It does not start a runner, HTTP listener or process
supervisor.

The `gufi` runner uses this engine over Orbit. Hosts that own routing and
lifecycle can depend on the engine directly. It succeeds `claviron-asgi`, which
gets no further releases, and is not published yet.

```toml
[dependencies]
claviron-python = { version = "0.2", features = ["asgi"] }
```

| Feature | Adds |
| --- | --- |
| (none) | `interpreter`: start CPython with its installation or a venv, and prepare it for an application |
| `asgi` | ASGI applications and the `claviron` module they import |
| `log`, `broadcast`, `django` | `claviron.LogHandler`, `claviron.broadcast`, the Django Channels layer |
| `websocket` | WebSockets answered by the application, as ASGI `websocket` sessions |
| `backend`, `edge` | `python:` routes and the one application `python.yaml` names |
| `webtransport` | WebTransport streams on an Edge's HTTP/3 listener, each an ASGI WebSocket connection |

## Ownership

`asgi::PythonApplication` declares the `module:attribute` target, module paths
and optional virtual environment. `boot()` starts the application-loop thread
and returns its `RunningPythonApplication` owner only after lifespan startup
completes. Its cloneable handle accepts streaming HTTP and message sessions.

The host retains the running owner while requests are admitted. It stops its
ingress and drains accepted work before calling `shutdown()`, which waits for
accepted exchanges, ASGI lifespan shutdown and the loop thread. Drop uses the
same shutdown path. Boot and shutdown may block; async hosts must run these
ownership transitions on their blocking executor. No implicit grace deadline
or forced Python-task cancellation is introduced.

`routing::PythonService` is a Tower-compatible HTTP adapter. Its shared handle
cell follows the host-managed application generation; an empty cell returns
503. `routing::request_scope` preserves Claviron's accepted connection and
trusted-proxy context when adapting message handshakes.

Initialization owns one process-wide CPython 3.15 interpreter and its selected
environment. This package provides no independent virtual-environment or
subinterpreter isolation inside a process. Application stop/restart runs ASGI
lifespan without finalizing CPython or evicting imported modules.

## In a host

With `edge` enabled, the registration call installs the Python application
and the `python:` route factory on Edge.

```rust
claviron_python::register(state)?;
```

Hosts that supply their own site registry can select `backend` and call
`claviron_python::backend::register(site)` alongside runtime registration.

```yaml
# python.yaml
name: app                             # its name in the application manager
target: app:app
module_path: apps/python
venv: .venv
env:
  APP_SECRET_KEY: ${APP_SECRET_KEY}   # expanded from the host's environment and env files
  APP_DEBUG: "0"
# env_file: config/app.env            # further values, with deployment variants
# frameworks: [django]                # shown by management clients
# icon: django                        # a symbolic icon code, never a path
```

```yaml
# a host in vhost.yaml
routes:
  /api:
    python: {}
```

`python.yaml` names one application, because a process has one interpreter and
one environment. A second application runs in its own process and joins over
Orbit, which is what the `gufi` runner does. An interpreter does not change its
environment, so a changed `python.yaml` takes a restart. Each worker boots its
own interpreter; a master boots none.

Where the host registers Claviron's application manager, the application is
listed and controlled as `python:<name>`, with request mode `asgi`. Stopping
it runs ASGI lifespan shutdown, and its routes answer 503 until it starts
again; starting it boots it from the `python.yaml` published at that moment.
Its modules stay imported in the interpreter, so a restart runs a new lifespan
with the code already loaded: changed code takes a new worker.

The application's `os.environ` is its own: the values `env` and `env_file`
declare, beside only the runtime essentials the host keeps, such as `PATH` and
`HOME`. `${NAME}` expands from the host's environment and its `.env` files,
but the host's variables themselves are not passed on. The values are written
into the interpreter's mapping, never into the process environment, so other
runtimes in the process do not see them and a child process started without
an explicit `env` does not inherit them. A C library that reads its settings with
`getenv()` does not see them either; pass such settings to it from Python.

The path reaches the application as the client sent it. An HTTP/1.1 WebSocket
goes to the application when the host's transport allows `h1.upgrade`, and is
refused with 403 otherwise. WebSocket over HTTP/2 or HTTP/3 is not served from
a route yet.

When the process begins to go away, or the application is stopped, every open
WebSocket and WebTransport stream is retired: the application receives
`websocket.disconnect` with code 1001, and when it returns without closing,
the client is sent a 1001 Going Away close. Nothing is cancelled; a stream
ends because both of its sides were told, not because a timeout ran out.

With the `webtransport` feature, a route names the paths a WebTransport session
may open on, under its mount, and the host's transport allows `h3.webtransport`:

```yaml
routes:
  /:
    python:
      webtransport: [/ws/echo]
```

Each reliable bidirectional stream of a session is one ASGI WebSocket
connection, with `scope["transport"]` set to `"webtransport"`. The stream
carries one record per message, as a stream relayed over Orbit does:

```text
u8 kind (1 text, 2 binary, 8 close) | u32 length (little endian) | payload
```

The application decides the first stream before the session is answered, so a
refusal stays an HTTP status; a later stream it refuses is reset. Without the
feature, a route that names paths is refused when it is prepared.

## Native Python capabilities

The native Python namespace is `claviron`, including invocation, counters
and Orbitals, plus optional broadcast, logging and Django Channels
capabilities. The
`broadcast`, `log` and `django` features select their native adapters; the host
supplies the matching services. They do not install a Python compatibility
package or change the ASGI protocol.

`claviron.orbital` declares and opens Orbitals: one shared 64-bit value each,
an `int`, a `float` or a `bool`, in the fleet's shared memory. The object is a
handle, so every worker that opens the id reads and writes the same value.

```python
import asyncio
import claviron

progress = claviron.orbital.int(0)            # or .float(0.0), .bool(False)
progress.add(1)                               # atomic; the value after
same = claviron.orbital.open(progress.id)     # in any worker of the fleet
seen = await asyncio.to_thread(same.wait, 1)  # parks until it is not 1
```

Creating an Orbital takes a slot for the life of the fleet's table. Nothing is
released, so declare one for something that lasts, not one per request, and
share it by passing its id, `<kind>:<slot>:<epoch>` as text. `wait(current)`
blocks its thread with the GIL released until the value differs from
`current`; run it off the event loop. It has no timeout: a write ends it, or
the application stopping does, with `RuntimeError`. An unsigned (`u64:`) id is
refused.

The module exists only inside the host process. Add this repository's `stubs/`
directory to your editor's stub search path; it holds type declarations only.

## Building

Select CPython 3.15 with `PYO3_PYTHON` or PyO3 configuration; this package
validates the selected interpreter and its ABI. A host binary linking a shared
libpython finds it at run time through the rpath its own build script adds, with
PyO3's standard tool:

```toml
[build-dependencies]
pyo3-build-config = { version = "0.30", features = ["resolve-config"] }
```

```rust
// build.rs
fn main() {
    pyo3_build_config::add_libpython_rpath_link_args();
}
```

It reads the same `PYO3_PYTHON` / `PYO3_CONFIG_FILE` selection, so the binary
loads the libpython it was built against wherever it is started, without
`LD_LIBRARY_PATH`. A static libpython needs nothing.

Existing engine tests live under `tests/`. The Python driver checks can also
run directly, without booting a Rust host:

```sh
python3.15 -B tests/python/test_asgi_driver.py
python3.15 -B tests/python/test_native_module.py
```
