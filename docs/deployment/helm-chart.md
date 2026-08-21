# Helm Chart Deployment

This repository ships **two** Helm charts, for two different deployment
models:

| Chart | Path | Workload kind | Use when |
|---|---|---|---|
| `emgr` | `helm/emgr/` | Standard Kubernetes `Deployment` (via the [bjw-s common library chart](https://bjw-s-labs.github.io/helm-charts/)) | You want a normal, always-on Deployment - fixed `replicaCount`, an optional HPA, a `PersistentVolumeClaim` for `local_fs` storage. |
| `emgr-serverless` | `helm/serverless/` | [Knative](https://knative.dev/) `Service` (via the Bitnami common library chart) | You run Knative and want scale-to-zero, and S3/MinIO storage (no local disk needed). |

Both charts install `emgr` itself; pick one, not both, for a given
deployment.

## Prerequisites

- A Kubernetes cluster and Helm 3.
- For `emgr-serverless`: [Knative Serving](https://knative.dev/docs/install/) installed on the cluster, and `cert-manager` if you enable `domain.tls.issuerRef`.

## `emgr` (Deployment chart)

### Chart location

`helm/emgr/`. `Chart.yaml` pins its `common` dependency to `bjw-s`'s
library chart `4.0.1` (see `helm/emgr/Chart.lock`) - most of the templating
(Deployment, Service, Ingress, ConfigMap, HPA, PVC) comes from that
library via `templates/common.yaml`'s single
`{{ include "bjw-s.common.loader.all" $ }}`. Two plain templates are
layered on top:

- `templates/poddisruptionbudget.yaml` - a `PodDisruptionBudget` with
  `minAvailable: 1`, added because the pinned `4.0.1` library predates
  that chart's own `podDisruptionBudget` values key. Without it, a
  voluntary disruption (node drain, cluster upgrade) could evict every
  `main` replica at once.
- The probes under `controllers.main.containers.app.probes` in
  `values.yaml` - the bjw-s common library does **not** synthesize
  liveness/readiness/startup probe defaults, so without this block
  traffic would route to pods before storage finishes initializing, and a
  wedged container would never be restarted.

### Probes

All three probes are plain Kubernetes HTTP probes against `/health` on
port `3000` (`values.yaml`, `controllers.main.containers.app.probes`):

| Probe | initialDelay | period | timeout | failureThreshold |
|---|---|---|---|---|
| liveness | 5s | 10s | 2s | 3 |
| readiness | 5s | 10s | 2s | 3 |
| startup | 0s | 5s | - | 30 (150s total before the container is killed for never finishing startup) |

### What the chart configures out of the box

`values.yaml`'s `configMaps.config.data` sets exactly three variables:

```yaml
LOCAL_FS_STORAGE_PATH: /tmp/data/images
CDN_BASE_URL: https://emgr.com
PERFORMANCE_PROFILE: high_throughput
```

...loaded via `envFrom: configMapRef` on the `app` container. A
`permission-fix` init container (running as root) `chown`s/`chmod`s the
mounted `ReadWriteMany` PVC to uid/gid `1001` before the app container
(which runs as that non-root user, `defaultPodOptions.securityContext`)
starts.

### What the chart does *not* configure - you must add it

**The chart as shipped does not set `SIGNING_KEY`/`SIGNING_SALT` (or
`ALLOW_UNSIGNED_REQUESTS`), and does not set `METRICS_AUTH_TOKEN` (or
`ALLOW_UNAUTHENTICATED_METRICS`).** `emgr` fails closed at startup without
these (see [Installation](../getting-started/installation.md#configure-the-two-startup-checks)),
so a pod deployed from this chart's defaults will crash-loop. Add them
yourself in a values override - the bjw-s common library supports either
plain values or a `Secret`-backed reference on the same container:

```yaml
# values-signing.yaml
controllers:
  main:
    containers:
      app:
        env:
          SIGNING_KEY:
            valueFrom:
              secretKeyRef:
                name: emgr-signing
                key: signing-key
          SIGNING_SALT:
            valueFrom:
              secretKeyRef:
                name: emgr-signing
                key: signing-salt
          METRICS_AUTH_TOKEN:
            valueFrom:
              secretKeyRef:
                name: emgr-signing
                key: metrics-auth-token
```

(with `emgr-signing` a `Secret` you create separately - the chart does not
create one for you) - or the equivalent inline `env` map if you don't want
a `Secret`. See the
[bjw-s common library's `env`/`envFrom` docs](https://bjw-s-labs.github.io/helm-charts/docs/common-library/values/#env-and-envfrom)
for the full schema. The image itself must also be an `otel`-enabled
build for `METRICS_AUTH_TOKEN` to matter at all - see the image tag note
below.

### Image tag

`values.yaml`'s `global.version` defaults to `fs-latest` - a **floating**
tag (moves on every push to `main`; `.github/workflows/build.yml` has no
release-tag trigger, so there's no immutable semver tag to default to
instead). `fs-latest` is a `local_fs`-only, non-`otel` build, so
`METRICS_AUTH_TOKEN` doesn't apply unless you override `global.version` to
one of the `*_otel-*` flavours (see [Docker deployment](docker.md) for the
full tag list). Pin `global.version` to a specific `fs-<sha>` (or
`fs_otel-<sha>`) tag for anything beyond a quick smoke test -
`pullPolicy: Always` on the container is a direct consequence of the
default floating tag, so different pods don't silently end up running
different builds under the same tag name.

### Deployment steps

```bash
# From the local chart directory (no chart repository is published today)
helm dependency build ./helm/emgr
helm install emgr ./helm/emgr \
  --namespace emgr --create-namespace \
  -f values-signing.yaml   # your own override, see above

kubectl get pods -n emgr
kubectl get svc -n emgr
```

Upgrade / uninstall:

```bash
helm upgrade emgr ./helm/emgr --namespace emgr -f values-signing.yaml
helm uninstall emgr --namespace emgr
```

## `emgr-serverless` (Knative chart)

### Chart location

`helm/serverless/`, depending on Bitnami's `common` library chart
(`Chart.yaml`; pulled from `oci://registry-1.docker.io/bitnamicharts`).
`templates/knative-service.yaml` renders a single
`serving.knative.dev/v1` `Service`; `templates/kserving-domain-mapping.yaml`
optionally adds a Knative `DomainMapping` (and a cert-manager
`Certificate`, if `domain.tls.issuerRef` is set) when `domain.enabled: true`.

### Probes

Unlike the Deployment chart, these are **Knative-native** probes (still
plain `httpGet` on `/health`, port `3000` from `ports.http1.port`) -
`readinessProbe`, `livenessProbe` and `startupProbe`, each with no
explicit timing overrides, so Knative's own probe defaults apply.

### What the chart configures out of the box

`values.yaml`'s `env` map is wired for **S3/MinIO storage**, with the
MinIO credentials already sourced from a `Secret` (name `minio`, keys
`access-key`/`secret-key`) via `secretKeyRef` - the same pattern you'd use
to add signing/metrics-auth secrets, see below:

```yaml
env:
  MINIO_ACCESS_KEY_ID: { secretKeyRef: { name: minio, key: access-key } }
  MINIO_SECRET_ACCESS_KEY: { secretKeyRef: { name: minio, key: secret-key } }
  MINIO_ENDPOINT_URL: http://minio:9000
  MINIO_BUCKET: emgr
  # ...
```

### What the chart does *not* configure - you must add it

Exactly like the `emgr` chart above, **no `SIGNING_KEY`/`SIGNING_SALT`
(or `ALLOW_UNSIGNED_REQUESTS`) and no `METRICS_AUTH_TOKEN` (or
`ALLOW_UNAUTHENTICATED_METRICS`) are set**, so a Knative revision deployed
from these defaults will fail its startup probe and never go Ready. Add
them to `values.yaml`'s `env` map using the same `secretKeyRef` shape
already used for `MINIO_ACCESS_KEY_ID` above, e.g.:

```yaml
env:
  SIGNING_KEY: { secretKeyRef: { name: emgr-signing, key: signing-key } }
  SIGNING_SALT: { secretKeyRef: { name: emgr-signing, key: signing-salt } }
  METRICS_AUTH_TOKEN: { secretKeyRef: { name: emgr-signing, key: metrics-auth-token } }
```

### Image tag - two confirmed problems, not just a stale default

`values.yaml`'s `image.tag` defaults to a plain `"latest"` - **this does
not correspond to any image `.github/workflows/build.yml` actually
publishes.** Every published tag is flavour-prefixed (`fs-latest`,
`fs_otel-latest`, `s3-latest`, `s3_otel-latest`, plus per-commit
`<flavor>-<sha>` - see [Docker deployment](docker.md)); there is no
bare `latest`. Given this chart's `env` defaults assume S3/MinIO storage
and it sends `LOG_LEVEL`/`OTLP_*` variables (meaningful only on an `otel`
build), you'd want `s3_otel-latest` or a pinned `s3_otel-<sha>` instead.

Overriding `image.tag` alone still won't produce a working image
reference, though - `helm template ./helm/serverless` (verified directly)
renders:

```yaml
- image: ghcr.io/ghcr.io/vaam-store/image-resizer:latest
```

`values.yaml` sets both `image.registry: ghcr.io` **and**
`image.repository: ghcr.io/vaam-store/image-resizer` (the repository
already includes the registry host), and the Bitnami `common.images.image`
helper this chart uses concatenates `registry`/`repository` unconditionally
- producing a doubled, non-existent `ghcr.io/ghcr.io/...` reference. Until
this is fixed in the chart, override `image.registry: ""` (empty) in your
values, alongside `image.tag`, or set `image.repository` to just
`vaam-store/image-resizer`.

Separately (also verified via `helm template`), `values.yaml`'s
`env.LOG_LEVEL: ${LOG_LEVEL:-info}` is Docker Compose's `${VAR:-default}`
substitution syntax, which Helm does not evaluate - it renders as the
**literal string** `${LOG_LEVEL:-info}` for the container's `LOG_LEVEL`
env var, not a resolved default. Override `env.LOG_LEVEL` to an actual
level (e.g. `info`) in your values file.

### Deployment steps

```bash
helm dependency build ./helm/serverless
helm install emgr-serverless ./helm/serverless \
  --namespace emgr --create-namespace \
  --set image.registry="" \
  --set image.tag=s3_otel-latest \
  --set env.LOG_LEVEL=info \
  -f values-signing.yaml

kubectl get ksvc -n emgr
```

Upgrade / uninstall:

```bash
helm upgrade emgr-serverless ./helm/serverless --namespace emgr -f values-signing.yaml
helm uninstall emgr-serverless --namespace emgr
```

## Notes

- Neither chart is published to a Helm repository today - both are
  installed from the local `helm/emgr`/`helm/serverless` directories in
  this checkout (`helm dependency build` first, to fetch the pinned
  `common` library chart into `charts/`).
- Configuration beyond what's listed above (performance tuning, the SSRF
  guard, resolution limits, presets) works the same way: add the env var
  from [Configuration](../getting-started/configuration.md) to whichever
  chart's env mechanism you're using.
