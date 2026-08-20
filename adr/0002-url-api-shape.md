# ADR 0002: URL API shape — path-based signed URLs vs. OpenAPI codegen

- Status: **Proposed** — recommendation pending owner approval (@stephane-segning)
- Issue: [#53](https://github.com/vaam-store/image-resizer/issues/53) — "DECISION: path-based
  signed URL API vs OpenAPI codegen" (parent: #17, P1-parity)
- Date: 2026-08-19

## Context

`imgproxy`'s interface is a signed URL path:

```
/{signature}/{processing_options}/{plain|base64 source}.{ext}
```

`emgr`'s is query parameters on `GET /api/images/resize` (`openapi.yaml:44-97`), and — this is the
part that forces the decision — **the entire HTTP surface is mechanically generated** from
`openapi.yaml` by `openapitools/openapi-generator-cli`, run unpinned
(no `:tag` on the image — `compose.yaml:167`) via `docker compose run` from `Makefile:23`
(`make init: rm -rf packages/gen-server && docker compose -p emgr run --rm
openapi-generator-cli $(c)`).

### The codegen is already costing real, verified money — not a hypothetical

- **A fresh clone cannot `cargo build`.** Verified directly: `packages/gen-server` is matched by
  the top-level `.gitignore:3` (`gen-server`) and is genuinely untracked —
  `git ls-files packages/gen-server` returns nothing, `git check-ignore -v
  packages/gen-server/src/apis/images.rs` confirms the match. `Cargo.toml:9` depends on it as a
  local path dependency (`gen-server = { path = "./packages/gen-server", ... }`) with no
  fallback. A clone that hasn't run `make init` (which needs Docker) cannot compile at all — this
  is issue #45.
- **The generator is unpinned, so its output drifts silently.** `packages/gen-server/.openapi-
  generator/VERSION` currently records `7.24.0`, but nothing pins that version in `compose.yaml` —
  the next `make init` on a clean machine pulls whatever `openapitools/openapi-generator-cli` is
  tagged `latest` at that moment. This is exactly how the app ended up needing
  `headers::Host` (via `axum_extra::extract::Host` under `axum-extra 0.12`) when an earlier
  generator run had targeted `axum-extra 0.10`'s different extractor shape — a full dependency
  migration forced by a regeneration, not a deliberate app change.
- **The generated response enums constrain which HTTP statuses can be returned**, which is why
  error handling was routed around them. `src/modules/api/handler.rs:88-101` documents this
  directly:

  > "Turns an `AppError`... into a real HTTP response with the correct status code, bypassing the
  > generated `DownloadResponse`/`ResizeResponse` enums entirely. This is the generated router's
  > own extension point (`handle_error`)... so no OpenAPI regeneration is needed to add error
  > status codes (#41, #25)."

  Concretely: `openapi.yaml` was later extended to declare `400`/`502`/`503` for `resize` and
  `404`/`502` for `download` (`openapi.yaml:58-97`, `104-142`), and `packages/gen-server/src/apis/
  images.rs` now generates matching `Status400_...`/`Status404_...`/etc. enum variants for those.
  But the actual error path in `src/modules/api/resize.rs:31,59` still returns a plain `AppError`
  and lets `ErrorHandler::handle_error` build the `Response` by hand, bypassing those generated
  variants entirely. **The generated enums exist and technically match the spec, but the
  real error-handling logic already lives outside them** — the codegen's main remaining
  contribution on the error path is ceremony, not behavior.

## How much of the generated surface is actually load-bearing?

Read `packages/gen-server/src/` in full to answer this concretely rather than guess:

| File | Lines | What it is |
|---|---:|---|
| `types.rs` | 790 | `ByteArray`, `Nullable<T>`, XSS-check helpers (`check_xss_string` etc., used because the OpenAPI-generic template assumes HTML-facing string fields — irrelevant to an image proxy) |
| `models.rs` | 453 | `ResizeQueryParams`/`DownloadPathParams` plus **newtype wrappers per scalar param** (`BlurSigma(pub f32)`, `Grayscale(pub bool)`) — each with a hand-generated `Deref`/`DerefMut`/`From`/`validator::Validate` impl that adds no behavior beyond the field it wraps (`models.rs:121-193`) |
| `header.rs` | 170 | `IntoHeaderValue` conversion boilerplate |
| `server/mod.rs` | 424 | Per-endpoint axum handlers: extract → validate → call trait method → match every response-enum variant → build `Response` |
| `apis/images.rs` | 120 | `DownloadResponse`/`ResizeResponse` enums + the `Images` trait |
| `apis/mod.rs` | 24 | `ErrorHandler` trait |
| **Total** | **2011** | |

Against that, the app's own hand-written glue is already thin:
- `src/modules/router/router.rs` (51 lines) — mounts `gen_server::server::new(api_service)`
  and adds `/health`, `/metrics`, root redirect.
- `src/modules/api/resize.rs`'s `Images` trait impl (~50 lines of real logic across `download`
  and `resize`) — the entire business-logic surface touching the generated types.
- `src/models/params.rs` — a **hand-written, hand-maintained mirror** of `ResizeQueryParams`
  (`ResizeQuery`, built via `o2o`'s `#[from_owned(ResizeQueryParams)]`) that the app converts to
  immediately after extraction, because the generated struct doesn't (and structurally can't,
  without a spec edit + regen) carry app-only fields like `enlarge` — hence the
  `#[ghost(false)]` workaround documented right there in the source (`src/models/params.rs:22-30`,
  citing #36). **The app already treats the generated struct as a disposable wire-format shim**,
  not as its real query type.
- `src/services/storage/key_validation.rs` (229 lines) — a **second, independent implementation**
  of the same key-shape check the generated `Key::validate` (`packages/gen-server/src/
  models.rs:248-295`) already performs, because (per that file's own doc comment) the storage
  layer can't trust path/S3-key safety to validation that happens one layer up and might be
  skipped or drift. The codegen's validation and the app's real validation are **two separate
  implementations of the same rule today**, which is itself a drift risk the codegen was
  supposed to prevent.

Net picture: of ~2011 generated lines, the load-bearing part for two GET endpoints with six total
parameters is genuinely small — request extraction, five range/enum checks, and response
serialization. The rest is templated ceremony (XSS helpers for an image API, per-scalar newtype
wrappers, an error-response path the app doesn't actually use).

## Options

### Option A — keep codegen, express the signed path within OpenAPI

Would mean modeling `/{signature}/{processing_options}/{source}.{ext}` as an OpenAPI path with a
`{processing_options}` path segment encoding an arbitrary, extensible mini-grammar
(`w:300/h:200/blur:5` etc., imgproxy-style) — something OpenAPI path templating does not
represent naturally (it's built for named path params, not colon-delimited sub-grammars inside
one segment). Every new processing option would still require a spec edit + Docker regen, and
the regen-drift risk demonstrated above (`axum-extra` 0.10 → 0.12, the unpinned generator image)
does not go away — it gets exercised more often, since a URL-shape epic implies frequent
iteration on exactly this part of the spec.

### Option B — drop codegen, hand-write the axum router

Gains, weighed honestly against the cost:
- **Fixes #45 outright** — no Docker dependency to get a fresh clone building.
- **No generator-version drift** — the router only changes when a developer changes it.
- **Free hand over status codes** — `handle_error` already does the real work; dropping the
  generated enums removes a redundant, only-partially-used abstraction layer, not a working
  contract.
- **Drop-in imgproxy URL compatibility becomes possible.** This is the strongest argument in
  favor, independent of the codegen question: a competing image proxy whose URLs are a drop-in
  replacement for imgproxy's has a materially shorter adoption path for anyone currently on
  imgproxy — flip a base URL, done. OpenAPI's parameter model cannot express that path shape
  cleanly even in principle (see Option A), so this argument only fully cashes out under Option B.

Cost, estimated concretely from the table above rather than assumed:
- Hand-write request parsing/validation for 2 routes, ~6 parameters, replacing
  `server/mod.rs` (424 lines) + the relevant slices of `models.rs` (453) and `types.rs` (790) —
  realistically **150-300 lines** of new axum extractors + validation, given the app already has
  working validation logic to lift from `src/services/storage/key_validation.rs` and the
  range/enum checks currently expressed as `validator::Validate` impls.
- Hand-write response building for 5-8 status codes — small, since `AppError::into_response()`
  (referenced in `src/modules/api/handler.rs:101`) already does this for the error paths; only
  the success-path response construction (currently the generated `Status200_.../Status301_...`
  match arms) needs a hand-written equivalent, maybe **30-50 lines**.
- `packages/gen-server` (2011 lines, the Cargo dependency, the `openapi-generator-cli` Docker
  service in `compose.yaml:167-180`, `.openapi-generator-ignore`) is deleted outright.
- `openapi.yaml` is either deleted or kept purely as **documentation** (hand-maintained, not
  code-generating) if API docs generation (`mkdocs.yml`) still wants it as a source.
- New work not present today: parsing imgproxy's `{processing_options}` mini-grammar and a
  signature-verification step (HMAC, matching imgproxy's scheme) — genuinely new functionality,
  not a codegen-avoidance cost, and the actual size of *that* work is out of scope for this ADR
  (it belongs to whatever issue implements the chosen URL shape).

## Decision (recommendation — pending owner approval)

**Recommend Option B: drop the OpenAPI codegen, hand-write the axum router, and adopt an
imgproxy-compatible signed path URL shape as the target.**

Reasoning:
1. The measured evidence above shows the codegen is *already* net-negative today, before any
   URL-shape work even starts: it breaks fresh clones (#45), it silently drifted the app's own
   dependency versions once already, and its main remaining differentiator (structured error
   responses) is bypassed by hand-written code that exists specifically because the generated
   version wasn't good enough (#41, #25).
2. The generated surface is mostly ceremony for this API's actual size (2 routes, 6 params) — the
   app has, in practice, already reimplemented or wrapped every piece of it that matters
   (`ResizeQuery`, `key_validation.rs`, `handle_error`). Hand-writing the router formalizes what
   the codebase is already doing implicitly, rather than adding new complexity.
3. Drop-in imgproxy URL compatibility is a real, concrete migration argument (issue #53's own
   framing: "very likely correct if the goal is genuine competition") and is only fully available
   once the OpenAPI constraint is gone.
4. This does **not** depend on ADR 0001's outcome — the format the pipeline encodes to (JPEG /
   lossy WebP / AVIF) is orthogonal to how the URL that requests it is parsed and signed. Land
   either ADR first.

## Migration sequence

1. **Design the URL grammar** as its own short spec (not OpenAPI) — `/{signature}/{options}/
   {source}.{ext}` with an explicit option-key list (`w`, `h`, `blur`, `gray`, `format`/`ext` —
   the same six params `ResizeQueryParams` has today) and an HMAC signature scheme. Decide
   base64 vs. plain source-URL encoding (imgproxy supports both).
2. **Hand-write the axum router** for the two existing routes first, functionally matching
   today's behavior exactly (same params, same statuses), with `packages/gen-server` still
   present but unused — this isolates "codegen removal" from "URL shape change" as two
   independently verifiable steps.
3. **Delete `packages/gen-server`, its `Cargo.toml` dependency, the `openapi-generator-cli`
   service in `compose.yaml`, and `Makefile`'s `init` target's Docker dependency.** Fresh-clone
   `cargo build` now works with no Docker step — closes #45.
4. **Add the new path-based route(s)** alongside (or replacing) the query-param route, parsing
   the imgproxy-style `{options}` segment and verifying the signature. `src/models/params.rs`'s
   `ResizeQuery` becomes the single query struct fed by both the legacy query-param path (if kept
   for compatibility) and the new signed-path parser, instead of being an `o2o` conversion off a
   generated type.
5. **Decide the legacy `/api/images/resize?...` route's fate** — deprecate immediately, keep as
   an alias, or drop. Not decided by this ADR; flag as an open question for the owner alongside
   this decision.
6. **Update dependent issues** per #53's own "Done when": #45 (fresh-clone build) and #27 close
   or get re-scoped once step 3 lands; anything referencing `openapi.yaml`-driven behavior
   (`mkdocs.yml`'s API docs generation, if any) gets re-pointed at hand-maintained docs.

## Consequences

- `Cargo.toml` loses the `gen-server` path dependency (and, transitively, `ammonia`, `frunk*`,
  `validator`, `serde_html_form` if nothing else in the app needs them — worth an audit at
  removal time). Owned by another workstream in this epic; not edited here.
- `compose.yaml` loses the `openapi-generator-cli` service; `Makefile`'s `init` target no longer
  needs Docker.
- `openapi.yaml` either goes away or becomes documentation-only, hand-edited without a generation
  step downstream of it.
- The team takes on ongoing maintenance of hand-written request validation that codegen
  previously produced automatically — mitigated by the fact that the app already hand-maintains
  the equivalent logic today in `key_validation.rs` and `params.rs`.
- Once the signed-path shape lands, `emgr` gains a genuine "swap your imgproxy base URL" adoption
  story, which is the strongest lever issue #53 identifies for competing with imgproxy directly.

## What would change this recommendation

- If OpenAPI-based client SDK generation (for some consumer not evidenced in this repo) turns out
  to be a hard requirement, Option A's cost changes — worth confirming no external consumer
  depends on generated clients before deleting `openapi.yaml`'s code-generating role.
- If pinning the generator image and fixing today's error-handling gap (routing more statuses
  through the generated enums instead of `handle_error`) turns out to be cheap, Option A's
  "unpinned/drifting" and "constrains status codes" objections both weaken — but neither
  addresses the URL-shape limitation (OpenAPI can't express imgproxy's path grammar), which is
  the deciding factor independent of the codegen-quality objections.
