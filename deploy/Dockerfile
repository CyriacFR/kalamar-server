# Builds kalamar-service and kalamar-relay into one small image.
#   docker build -f deploy/Dockerfile -t kalamar .
#   docker run -d --name kalamar -v kalamar-data:/data -v $PWD/kalamar.toml:/etc/kalamar/kalamar.toml:ro \
#       -p 127.0.0.1:8080:8080 kalamar                           # the API, behind your HTTPS reverse proxy
#   docker run -d --name kalamar-relay -p 47313:47313/udp -v kalamar-data:/data:ro kalamar kalamar-relay --bind 0.0.0.0 --room-secret-file /data/relay.key
# With [embedded_relay] (the preset friends) one container runs both: add -p 47313:47313/udp to the first and skip the
# second. The relay is bound to the service by a secret: it refuses to start without --room-secret-file (or --open, for
# a relay meant to be open to anyone); the secret made once by `docker run --rm -v kalamar-data:/data kalamar
# kalamar-relay --new-room-secret /data/relay.key` (mode 0600, the image's account).
# The base images below are named by tag, which moves: pin them by digest (image@sha256:...) for a reproducible build.
# In the configuration: bind = "0.0.0.0:8080", database = "/data/kalamar.sqlite3", secret_file = "/data/secret.key",
# and trusted_proxies = the address the proxy reaches the container from (e.g. "172.16.0.0/12").

FROM rust:1.97-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p kalamar-service -p kalamar-relay

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /data kalamar && mkdir -p /data && chown kalamar /data
COPY --from=build /src/target/release/kalamar-service /usr/local/bin/
COPY --from=build /src/target/release/kalamar-relay /usr/local/bin/
USER kalamar
WORKDIR /data
VOLUME /data
EXPOSE 8080/tcp 47313/udp
ENTRYPOINT []
CMD ["kalamar-service", "--config", "/etc/kalamar/kalamar.toml", "--log-format", "json", "serve"]
