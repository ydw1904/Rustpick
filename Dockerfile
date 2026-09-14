# Headless miner image. Data (settings, login, claims, status) lives in the /data volume.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p rustpick

FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/rustpick /usr/local/bin/rustpick
ENV RUSTPICK_DIR=/data
VOLUME /data
# unhealthy when the miner hasn't refreshed status.json for 10 minutes
HEALTHCHECK --interval=1m --start-period=2m CMD ["rustpick", "status", "--check"]
ENTRYPOINT ["rustpick"]
CMD ["run"]
