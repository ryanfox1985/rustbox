# syntax=docker/dockerfile:1

FROM rust:1-slim-trixie AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked \
    && cp target/release/rustbox /usr/local/bin/rustbox

FROM debian:trixie-slim
RUN useradd --system --no-create-home app \
    && mkdir /data && chown app /data
COPY --from=builder /usr/local/bin/rustbox /usr/local/bin/rustbox
USER app
ENV PORT=3000
ENV UI_PORT=8080
ENV DATABASE_URL=sqlite:/data/rustbox.db
ENV MATTER_DIR=/data/matter
EXPOSE 3000 8080
CMD ["rustbox"]
