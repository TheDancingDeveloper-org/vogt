# HTTP adapter decisions (WI-1061)

Decisions the Rust HTTP adapter (`engine/core/src/adapters/http/`) makes that
differ from, or interpret, the Python server. Each one names what would change
it.

## D1. `/docs` and `/redoc` are not served

The Python server serves FastAPI's Swagger UI at `/docs` and ReDoc at `/redoc`
(both return 200 today). The Rust adapter serves only `/openapi.json`.

- Why: both pages are FastAPI renderers that load their assets from a CDN. Nothing
  in the estate, the PWA or the CLI reads them, and `/openapi.json` is the contract.
- Consequence: a browser that opens `/docs` on the Rust server gets a 404.
- Revisit if: an operator needs an interactive API browser on the Rust server.
  Serving static Swagger or ReDoc assets would be a new, separate change.

## D2. The OpenAPI golden is the served Python document

`tests/parity/golden/openapi.json` is the document the Python server serves:
`vogt serve --no-auth --no-schedule`, then `GET /openapi.json`, against a freshly
`vogt init`ed data directory. `adapters::http::openapi` generates the same
document from the registry plus `openapi_hand.json`. Its test
`the_document_matches_the_python_golden` compares the two.

The comparison normalises two things that do not change what a client sees:

- **Parameter order.** FastAPI lists query parameters in model field order. The
  registry golden (`tests/parity/golden/registry.json`) has sorted keys, so field
  order is not recoverable from it. Parameters are compared by name.
- **Number form.** The Python document writes some bounds as floats (`99.0`) where
  the registry has integers (`99`). JSON numbers are compared by value, as
  Python's own equality does.

## D3. Hand-owned routes come from a fixture

Login, install, the health probes and `/mcp` are not registry operations, so their
paths and component schemas are not in the registry. They are copied from the
Python document into `engine/core/src/adapters/http/openapi_hand.json`. A change to
one of those Python models needs the fixture refreshed from the golden.

## D4. Trust and identity at the listener

- `X-Forwarded-Proto` is honoured only from a loopback peer, as uvicorn does with
  its default `forwarded_allow_ips`. Otherwise the listener's own scheme is used.
- A request with no `Host` header takes the listener's bind address, as Starlette
  takes `scope["server"]`.
- Revisit if: a front door on another host is placed in front of the core. It would
  need `forwarded_allow_ips` to be configured the same way on both sides.

## D5. `/metrics` is not in this slice

The opt-in Prometheus listener (`VOGT_METRICS_ADDR`, drift rows 319bb466 and
56001df) is not ported. It is a separate listener, not part of the HTTP surface
under test, and is tracked as a follow-up.

## D6. SSE `/api/events` is the engine's, not the core's

The Python core serves no `/api/events` route of its own. The engine serves the
PWA feed and follows the core through the `events.list` registry operation, so
the route is in the registry golden and nothing is missing from the core.
