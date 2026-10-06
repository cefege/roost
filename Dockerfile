# The Roost coordinator image: `roost coord` plus the web bundle it serves.
#
# Stateless when ROOST_COORDINATOR_DATABASE_URL names a Postgres server: the
# container then writes nothing durable and runs on a read-only root
# filesystem. Without it the coordinator keeps its SQLite file under
# /var/lib/roost, which must then be a volume. Workers are not in this image;
# they run on real hosts and enroll with `roost add-machine`.
#
#   docker build -t roost-coordinator .
#   docker run --rm -p 4113:4113 \
#     -e ROOST_COORDINATOR_DATABASE_URL=postgres://… roost-coordinator
#   docker exec <container> roost add-browser

FROM rust:1.98.1-bookworm AS build
ARG ROOST_BUILD_VERSION=dev
ARG ROOST_BUILD_SHA=unknown
WORKDIR /src
# rust-toolchain.toml pins the toolchain, its components and the wasm target;
# copying it first lets this layer cache across source edits.
COPY rust-toolchain.toml ./
RUN rustup show active-toolchain \
 && cargo install dioxus-cli --version 0.7.10 --locked
COPY . .
ENV ROOST_BUILD_VERSION=$ROOST_BUILD_VERSION ROOST_BUILD_SHA=$ROOST_BUILD_SHA
# One RUN, so the cache-mounted target directory never has to be an image
# layer: the two artifacts are copied out to /out before the mount goes away.
# dx never prunes old hashed files from public/, hence the rm.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p roost-cli \
 && rm -rf target/dx/roost-web/release/web/public \
 && dx build --release --profile wasm-release -p roost-web --platform web \
 && test -f target/dx/roost-web/release/web/public/index.html \
 && if grep -rl '__smoke' target/dx/roost-web/release/web/public; then \
      echo "the web bundle carries __smoke; this is not a production bundle" >&2; exit 1; \
    fi \
 && mkdir -p /out \
 && cp target/release/roost /out/roost \
 && cp -R target/dx/roost-web/release/web/public /out/web

# glibc, libgcc_s and libm are everything the binary links (SQLite and OpenSSL
# are compiled in); nonroot is uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/roost /usr/local/bin/roost
COPY --from=build /out/web /srv/roost/web
# 0.0.0.0 inside the container; the published port decides who reaches it.
ENV ROOST_COORDINATOR_BIND=0.0.0.0:4113 \
    ROOST_WEB_DIST_PATH=/srv/roost/web \
    ROOST_COORD_DATA_DIR=/var/lib/roost \
    ROOST_COORD_LOG_DIR=/tmp
EXPOSE 4113
ENTRYPOINT ["/usr/local/bin/roost"]
CMD ["coord"]
