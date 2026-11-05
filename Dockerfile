# syntax=docker/dockerfile:1.7
FROM --platform=$BUILDPLATFORM alpine:3.23 AS web-build
ARG BUILDARCH
RUN --mount=type=cache,id=ntfy-apk-${BUILDARCH},target=/var/cache/apk,sharing=locked \
    apk --cache-dir /var/cache/apk --timeout 60 add nodejs npm
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build && cp build/index.html build/app.html && rm -f build/config.js && find build -name '*.map' -delete

FROM alpine:3.23 AS rust-build
ARG TARGETARCH
RUN --mount=type=cache,id=ntfy-apk-${TARGETARCH},target=/var/cache/apk,sharing=locked \
    apk --cache-dir /var/cache/apk --timeout 60 add rust cargo musl-dev gcc openssl-dev pkgconf
ENV CARGO_HOME=/usr/local/cargo
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY tests/ tests/
RUN --mount=type=cache,id=ntfy-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=ntfy-rust-${TARGETARCH},target=/build/target \
    cargo build --locked --release --features container && cp target/release/ntfy /usr/local/bin/ntfy

FROM rust-build AS test
RUN --mount=type=cache,id=ntfy-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=ntfy-rust-${TARGETARCH},target=/build/target \
    cargo test --locked --features container

FROM alpine:3.23 AS runtime
LABEL org.opencontainers.image.title="ntfy-rs" \
      org.opencontainers.image.description="Rust ntfy server with accounts, Web Push, SMTP, Matrix and UnifiedPush support" \
      org.opencontainers.image.source="https://github.com/utoptop/ntfy" \
      org.opencontainers.image.licenses="Apache-2.0"
RUN apk add --no-cache libgcc libssl3 ca-certificates \
    && addgroup -g 10001 ntfy && adduser -D -H -u 10001 -G ntfy ntfy \
    && mkdir -p /var/lib/ntfy /etc/ntfy /usr/share/ntfy/web \
    && chown ntfy:ntfy /var/lib/ntfy
COPY --from=rust-build /usr/local/bin/ntfy /usr/local/bin/ntfy
COPY --from=web-build /web/build/ /usr/share/ntfy/web/
COPY LICENSE NOTICE /usr/share/licenses/ntfy/
USER 10001:10001
EXPOSE 8080
VOLUME ["/var/lib/ntfy"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 CMD ["ntfy", "healthcheck"]
ENTRYPOINT ["ntfy"]
CMD ["serve"]
