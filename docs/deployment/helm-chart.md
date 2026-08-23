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

### Signing / metrics-auth secrets (GH #84)

`emgr` fails closed at startup without `SIGNING_KEY`/`SIGNING_SALT` (or
`ALLOW_UNSIGNED_REQUESTS`), and without `METRICS_AUTH_TOKEN` (or
`ALLOW_UNAUTHENTICATED_METRICS`) on an `otel`-enabled image (see
[Installation](../getting-started/installation.md#configure-the-two-startup-checks)).

This is a real cluster, not a local `compose.yaml`, so the chart does
**not** default `ALLOW_UNSIGNED_REQUESTS`/`ALLOW_UNAUTHENTICATED_METRICS`
to `true` the way `compose.yaml` does for local development - that would
silently trade security for convenience. Instead `values.yaml`'s
`controllers.main.containers.app.env` already wires all three variables to
a `Secret` named `emgr-signing`:

```yaml
env:
  SIGNING_KEY:
    valueFrom:
      secretKeyRef: { name: emgr-signing, key: signing-key }
  SIGNING_SALT:
    valueFrom:
      secretKeyRef: { name: emgr-signing, key: signing-salt }
  METRICS_AUTH_TOKEN:
    valueFrom:
      secretKeyRef: { name: emgr-signing, key: metrics-auth-token }
```

You must create that `Secret` yourself - the chart deliberately does not,
so the key/salt/token never live in `values.yaml` or a values override:

```bash
kubectl create secret generic emgr-signing --namespace emgr \
  --from-literal=signing-key="$(openssl rand -hex 32)" \
  --from-literal=signing-salt="$(openssl rand -hex 16)" \
  --from-literal=metrics-auth-token="$(openssl rand -hex 32)"
```

If the `Secret` is missing, the pod fails loudly
(`CreateContainerConfigError`, visible in `kubectl describe pod`) instead
of starting insecurely. To point at a differently-named `Secret`, override
the `name:` fields above in your own values file. See the
[bjw-s common library's `env`/`envFrom` docs](https://bjw-s-labs.github.io/helm-charts/docs/common-library/values/#env-and-envfrom)
for the full schema. `METRICS_AUTH_TOKEN` only matters once the image
itself is an `otel`-enabled build - see the image tag note below.

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

# Create the emgr-signing Secret first (see above) - the chart's values.yaml
# already points at it, no values override needed unless you want to
# rename the Secret or its keys.
helm install emgr ./helm/emgr --namespace emgr --create-namespace

kubectl get pods -n emgr
kubectl get svc -n emgr
```

Upgrade / uninstall:

```bash
helm upgrade emgr ./helm/emgr --namespace emgr
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

### Signing / metrics-auth secrets (GH #84)

Exactly like the `emgr` chart above, `values.yaml`'s `env` map already
wires `SIGNING_KEY`/`SIGNING_SALT`/`METRICS_AUTH_TOKEN` to a `Secret`
named `emgr-signing`, using the same `secretKeyRef` shape as
`MINIO_ACCESS_KEY_ID` above:

```yaml
env:
  SIGNING_KEY: { secretKeyRef: { name: emgr-signing, key: signing-key } }
  SIGNING_SALT: { secretKeyRef: { name: emgr-signing, key: signing-salt } }
  METRICS_AUTH_TOKEN: { secretKeyRef: { name: emgr-signing, key: metrics-auth-token } }
```

Create that `Secret` yourself before installing (see the `kubectl create
secret` command in the `emgr` chart section above - the same Secret works
for both charts). Without it, the Knative revision fails its startup probe
and never goes Ready, instead of serving unsigned requests or an open
`/metrics`.

### Image tag and registry (GH #85, fixed)

`values.yaml`'s `image.registry` is empty and `image.repository` carries
the full `ghcr.io/vaam-store/image-resizer` path - `helm template` used to
render a doubled `ghcr.io/ghcr.io/...` reference when both fields set a
registry host; only `repository` does now. `image.tag` defaults to
`s3_otel-latest`, matching this chart's `env` defaults (S3/MinIO storage,
`OTLP_*` variables meaningful only on an `otel` build) and an image
flavour `.github/workflows/build.yml` actually publishes (see
[Docker deployment](docker.md) for the full tag list). Like
`helm/emgr`'s `global.version`, this is still a **floating** tag - pin to
a specific `s3_otel-<sha>` for anything beyond a quick smoke test.

`values.yaml`'s `env.LOG_LEVEL` is a plain `info` - it used to be Docker
Compose's `${LOG_LEVEL:-info}` shell-substitution syntax, which Helm never
evaluates and rendered as that literal string. Override `env.LOG_LEVEL`
directly (`--set env.LOG_LEVEL=debug`) if you want something else.

### Deployment steps

```bash
helm dependency build ./helm/serverless

# Create the emgr-signing Secret first (see above).
helm install emgr-serverless ./helm/serverless --namespace emgr --create-namespace

kubectl get ksvc -n emgr
```

Upgrade / uninstall:

```bash
helm upgrade emgr-serverless ./helm/serverless --namespace emgr
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
- `.github/workflows/ci.yml`'s `helm-verify` job runs `helm dependency
  build`, `helm lint` and `helm template` for both charts on every PR (GH
  #84 / #85) - a rendering defect like the ones described above now fails
  a build instead of reaching a user.
