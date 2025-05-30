FROM rust:1 as base

ENV APP_NAME=backend

WORKDIR /app

FROM base as builder

ENV CARGO_TERM_COLOR=always

RUN \
  --mount=type=bind,source=./Cargo.lock,target=/app/Cargo.lock \
  --mount=type=bind,source=./Cargo.toml,target=/app/Cargo.toml \
  --mount=type=bind,source=./migrations,target=/app/migrations \
  --mount=type=bind,source=./packages,target=/app/packages \
  --mount=type=bind,source=./src,target=/app/src \
  --mount=type=cache,target=/app/target \
  --mount=type=cache,target=/usr/local/cargo/registry/cache \
  --mount=type=cache,target=/usr/local/cargo/registry/index \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  cargo build --release --locked \ 
  && cp ./target/release/$APP_NAME $APP_NAME

FROM debian:12 as dep

RUN rm -f /etc/apt/apt.conf.d/docker-clean; echo 'Binary::apt::APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/keep-cache

RUN \
  --mount=type=cache,target=/var/cache/apt,sharing=locked \
  --mount=type=cache,target=/var/lib/apt,sharing=locked \
  apt-get update \
  && apt-get install -y libpq5 --no-install-recommends

# Dependencies for libpq (used by diesel)
RUN \
  --mount=type=cache,target=/usr/lib/*-linux-gnu \
  mkdir /deps && \
  cp /usr/lib/*-linux-gnu/libpq.so* /deps && \
  cp /usr/lib/*-linux-gnu/libgssapi_*.so* /deps && \
  cp /usr/lib/*-linux-gnu/libunistring.so* /deps && \
  cp /usr/lib/*-linux-gnu/libidn*.so* /deps && \
  cp /usr/lib/*-linux-gnu/libkeyutils.so* /deps && \
  cp /usr/lib/*-linux-gnu/libtasn1.so* /deps && \
  cp /usr/lib/*-linux-gnu/libnettle.so* /deps && \
  cp /usr/lib/*-linux-gnu/libhogweed.so* /deps && \
  cp /usr/lib/*-linux-gnu/libgmp.so* /deps && \
  cp /usr/lib/*-linux-gnu/libffi.so* /deps && \
  cp /usr/lib/*-linux-gnu/libp11-kit.so* /deps && \
  cp /usr/lib/*-linux-gnu/libkrb*.so* /deps && \
  cp /usr/lib/*-linux-gnu/libcom_err.so* /deps && \
  cp /usr/lib/*-linux-gnu/libk5crypto.so* /deps && \
  cp /usr/lib/*-linux-gnu/libsasl2.so* /deps && \
  cp /usr/lib/*-linux-gnu/libgnutls.so* /deps && \
  cp /usr/lib/*-linux-gnu/liblber-*.so* /deps && \
  cp /usr/lib/*-linux-gnu/libldap-*.so* /deps && \
  cp /usr/lib/*-linux-gnu/libgcc_s.so* /deps

FROM gcr.io/distroless/base-debian12:nonroot

LABEL maintainer="stephane-segning <selastlambou@gmail.com>"
LABEL org.opencontainers.image.description="backend for Adorsys GIS Lessons App"

ENV RUST_LOG=warn
ENV APP_NAME=backend
ENV PORT=3000
ENV HOST=0.0.0.0

WORKDIR /app

COPY --from=builder /app/$APP_NAME /app/backend
COPY --from=dep /deps /usr/lib/

EXPOSE $PORT

ENTRYPOINT ["/app/backend"]