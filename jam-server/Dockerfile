# The jam server alone: the app, egui and librespot are not compiled here.
# Build from the repository root:
#   docker build -f jam-server/Dockerfile -t spotizgeg-jam .
FROM rust:1.98-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p jam-server

# Newer than or equal to the build image's Debian, so its glibc suffices.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends openssl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home-dir /data jam \
    && mkdir /data \
    && chown jam /data
COPY --from=build /src/target/release/jam-server /usr/local/bin/jam-server
COPY jam-server/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
USER jam
VOLUME /data
EXPOSE 4070
ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["serve"]
