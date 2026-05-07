FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p mail-canvas-cli

FROM node:22-bookworm-slim

WORKDIR /app
COPY --from=builder /app/target/release/mail-canvas /app/target/release/mail-canvas
COPY examples ./examples
COPY fixtures ./fixtures

ENV MAIL_CANVAS_RENDERER=/app/target/release/mail-canvas
ENV MAIL_CANVAS_HOST=0.0.0.0
ENV MAIL_CANVAS_PORT=8787
ENV MAIL_CANVAS_FONT_DIR=/app/fixtures/fonts
ENV MAIL_CANVAS_MAX_BODY_BYTES=1048576

EXPOSE 8787
CMD ["node", "examples/http-service.mjs"]
