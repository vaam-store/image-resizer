FROM rust:1 as base

ENV APP_NAME=emgr

WORKDIR /app

FROM base as builder

ENV CARGO_TERM_COLOR=always

FROM builder as local_fs_builder

RUN \
  --mount=type=bind,source=./Cargo.lock,target=/app/Cargo.lock \
  --mount=type=bind,source=./Cargo.toml,target=/app/Cargo.toml \
  --mount=type=bind,source=./packages,target=/app/packages \
  --mount=type=bind,source=./src,target=/app/src \
  --mount=type=cache,target=/app/target \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  cargo build --profile prod --locked --bin emgr --features="local_fs otel" \
  && cp ./target/prod/$APP_NAME $APP_NAME

FROM builder as s3_fs_builder

RUN \
  --mount=type=bind,source=./Cargo.lock,target=/app/Cargo.lock \
  --mount=type=bind,source=./Cargo.toml,target=/app/Cargo.toml \
  --mount=type=bind,source=./packages,target=/app/packages \
  --mount=type=bind,source=./src,target=/app/src \
  --mount=type=cache,target=/app/target \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  cargo build --profile prod --locked --bin emgr --features="s3 otel" \
  && cp ./target/prod/$APP_NAME $APP_NAME

FROM gcr.io/distroless/cc-debian12:nonroot as base_deploy

LABEL maintainer="stephane-segning <selastlambou@gmail.com>"
LABEL org.opencontainers.image.description="Resize images with this image"

#ENV RUST_LOG=warn
ENV APP_NAME=emgr
ENV PORT=3000
ENV HOST=0.0.0.0

WORKDIR /app

EXPOSE $PORT

HEALTHCHECK --interval=30s --timeout=10s --start-period=5s \
  CMD curl -f http://localhost:$PORT/health || exit 1

ENTRYPOINT ["/app/emgr"]

FROM base_deploy as fs_deploy

COPY --from=local_fs_builder /app/$APP_NAME /app/emgr

VOLUME ["/app/data/images"]


FROM base_deploy as s3_deploy

COPY --from=s3_fs_builder /app/$APP_NAME /app/emgr

VOLUME ["/app/data/images"]

