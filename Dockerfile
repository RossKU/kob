# kob-executor for Linux operators (docs/ops/executor-quickstart.md, "Docker"). One image serves one network, the
# deployment build of NETWORK (mainnet or testnet-10, docs/ops/release.md):
#
#   docker build --build-arg NETWORK=mainnet -t kob-executor:mainnet .
#   install -d -o 10001 -g 10001 -m 0700 /srv/kob
#   docker run -it --rm --network host -v /srv/kob:/data kob-executor:mainnet setup --no-start
#   docker run -d --name kob --restart unless-stopped --network host -v /srv/kob:/data kob-executor:mainnet
#
# The volume /data holds the configuration, the operator's key file (mode 0600, owned by uid 10001) and the data directory.
# KOB_EXECUTOR_DIR=/data makes it the directory of `setup` and of the start without arguments (the default command), which
# runs the configuration `setup` wrote there, whatever role it chose. `--network host` reaches the node's loopback wRPC
# (127.0.0.1:18110 / 18210) and keeps the read API on the host's loopback. No image is published: build it from the commit you
# verified. Base images are pinned by exact version tag.

ARG NETWORK=mainnet

FROM rust:1.94.0-slim-trixie AS build
ARG NETWORK
# the commit the image is built from, reported by /v1/health (docs/ops/release.md, 7): --build-arg KOB_GIT_COMMIT=$(git rev-parse HEAD)
ARG KOB_GIT_COMMIT=
# the image's toolchain is the one rust-toolchain.toml pins; this keeps rustup from fetching the file's wasm target and
# components, which the executor does not need
ENV RUSTUP_TOOLCHAIN=1.94.0 CARGO_INCREMENTAL=0
WORKDIR /src
COPY . .
RUN case "$NETWORK" in \
      mainnet) f=deploy-mainnet ;; \
      testnet-10) f=deploy-tn10 ;; \
      *) echo "NETWORK must be mainnet or testnet-10, not $NETWORK" >&2; exit 1 ;; \
    esac \
 && cargo build --release --locked -p kob-executor --features "kob-executor/$f" \
 && install -m 0755 target/release/kob-executor /usr/local/bin/kob-executor \
 && kob-executor --version

FROM debian:trixie-20261020-slim
ARG NETWORK
LABEL org.opencontainers.image.title="kob-executor" \
      org.opencontainers.image.description="KOB executor (${NETWORK} deployment build): indexer, matcher, keepers" \
      org.opencontainers.image.licenses="ISC"
# nothing is installed: wss:// node endpoints use the root certificates compiled into the binary (webpki-roots)
RUN useradd --system --uid 10001 --user-group --home-dir /data --no-create-home --shell /usr/sbin/nologin kob \
 && install -d -o kob -g kob -m 0700 /data
COPY --from=build /usr/local/bin/kob-executor /usr/local/bin/kob-executor
ENV KOB_EXECUTOR_DIR=/data
USER 10001:10001
WORKDIR /data
VOLUME ["/data"]
ENTRYPOINT ["kob-executor"]
