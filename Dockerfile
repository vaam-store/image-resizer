FROM rust:1 as base

ENV APP_NAME=emgr

WORKDIR /app

FROM base as builder

ENV CARGO_TERM_COLOR=always

RUN \
  --mount=type=bind,source=./Cargo.lock,target=/app/Cargo.lock \
  --mount=type=bind,source=./Cargo.toml,target=/app/Cargo.toml \
  --mount=type=bind,source=./packages,target=/app/packages \
  --mount=type=bind,source=./src,target=/app/src \
  --mount=type=cache,target=/app/target \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  cargo build --profile prod --locked \
  && cp ./target/release/$APP_NAME $APP_NAME

FROM gcr.io/distroless/static-debian12:nonroot

LABEL maintainer="stephane-segning <selastlambou@gmail.com>"
LABEL org.opencontainers.image.description="Resize images with this image"

ENV RUST_LOG=warn
ENV APP_NAME=emgr
ENV PORT=3000
ENV HOST=0.0.0.0

WORKDIR /app

COPY --from=builder /app/$APP_NAME /app/emgr

EXPOSE $PORT

ENTRYPOINT ["/app/emgr"]