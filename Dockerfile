# MUST stay on bookworm (Debian 12) to match the distroless/cc-debian12
# runtime below. The previous pin was a Debian 13/trixie image with glibc
# 2.41 while the runtime has 2.36, so the s3 build - whose native C
# dependency (aws-lc-sys) references GLIBC_2.38 symbols - produced an image
# that built cleanly and then died instantly at startup with
#   version 'GLIBC_2.38' not found
# CI never caught it: the pipeline builds images and never runs one.
# local_fs survived only by luck - it happens not to reference those
# symbols. If you bump this, bump the runtime base in lockstep and actually
# RUN the resulting image.
#
# Pinned by digest (GH #48) - "rust:1" is a floating tag that gets
# repointed on every new 1.x release, which lets replicas built on
# different days/nodes end up compiled with a different rustc. This is
# rustc 1.97.1 as of the pin below; bump deliberately with
# `docker pull rust:1 && docker inspect rust:1 --format='{{index .RepoDigests 0}}'`.
FROM rust@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 as builder

ENV APP_NAME=emgr

WORKDIR /app

ENV CARGO_TERM_COLOR=always

FROM builder as local_fs_builder

RUN \
  --mount=type=bind,source=./Cargo.lock,target=/app/Cargo.lock \
  --mount=type=bind,source=./Cargo.toml,target=/app/Cargo.toml \
  --mount=type=bind,source=./src,target=/app/src \
  --mount=type=bind,source=./benches,target=/app/benches \
  --mount=type=cache,target=/app/target \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  cargo build --profile perf --locked --bin emgr --features="local_fs" \
  && cp ./target/perf/$APP_NAME $APP_NAME

FROM builder as local_fs_otel_builder

RUN \
  --mount=type=bind,source=./Cargo.lock,target=/app/Cargo.lock \
  --mount=type=bind,source=./Cargo.toml,target=/app/Cargo.toml \
  --mount=type=bind,source=./src,target=/app/src \
  --mount=type=bind,source=./benches,target=/app/benches \
  --mount=type=cache,target=/app/target \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  cargo build --profile perf --locked --bin emgr --features="local_fs otel" \
  && cp ./target/perf/$APP_NAME $APP_NAME

FROM builder as s3_fs_builder

RUN \
  --mount=type=bind,source=./Cargo.lock,target=/app/Cargo.lock \
  --mount=type=bind,source=./Cargo.toml,target=/app/Cargo.toml \
  --mount=type=bind,source=./src,target=/app/src \
  --mount=type=bind,source=./benches,target=/app/benches \
  --mount=type=cache,target=/app/target \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  cargo build --profile perf --locked --bin emgr --features="s3" \
  && cp ./target/perf/$APP_NAME $APP_NAME

FROM builder as s3_fs_otel_builder

RUN \
  --mount=type=bind,source=./Cargo.lock,target=/app/Cargo.lock \
  --mount=type=bind,source=./Cargo.toml,target=/app/Cargo.toml \
  --mount=type=bind,source=./src,target=/app/src \
  --mount=type=bind,source=./benches,target=/app/benches \
  --mount=type=cache,target=/app/target \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  cargo build --profile perf --locked --bin emgr --features="s3 otel" \
  && cp ./target/perf/$APP_NAME $APP_NAME

FROM builder AS healthcheck_builder

RUN \
  --mount=type=bind,source=./Cargo.lock,target=/app/Cargo.lock \
  --mount=type=bind,source=./Cargo.toml,target=/app/Cargo.toml \
  --mount=type=bind,source=./src,target=/app/src \
  --mount=type=bind,source=./benches,target=/app/benches \
  --mount=type=cache,target=/app/target \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  cargo build --profile prod --locked --bin healthcheck \
  && cp ./target/prod/healthcheck healthcheck

# Pinned by digest (GH #48), same rationale as the builder image above -
# "nonroot" is also a floating tag. Bump deliberately with
# `docker pull gcr.io/distroless/cc-debian12:nonroot && docker inspect \
#   gcr.io/distroless/cc-debian12:nonroot --format='{{index .RepoDigests 0}}'`.
FROM gcr.io/distroless/cc-debian12@sha256:adcd20c7b4c988b73cbfbddb26d2eee574571e6d7c9ffea29b3821e0690efb77 as base_deploy

LABEL maintainer="vaam-store <vaam-store@ssegning.com>"
LABEL maintainer="stephane-segning <selastlambou@gmail.com>"
LABEL org.opencontainers.image.description="Resize images with this image"

ENV APP_NAME=emgr
ENV PORT=3000
ENV HOST=0.0.0.0

WORKDIR /app

EXPOSE $PORT

COPY --from=healthcheck_builder /app/healthcheck /app/healthcheck

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
  CMD ["/app/healthcheck"]

ENTRYPOINT ["/app/emgr"]
FROM base_deploy as fs_deploy


COPY --from=local_fs_builder /app/$APP_NAME /app/emgr

FROM base_deploy as fs_otel_deploy

COPY --from=local_fs_otel_builder /app/$APP_NAME /app/emgr

FROM base_deploy as s3_deploy

COPY --from=s3_fs_builder /app/$APP_NAME /app/emgr

FROM base_deploy as s3_otel_deploy

COPY --from=s3_fs_otel_builder /app/$APP_NAME /app/emgr
