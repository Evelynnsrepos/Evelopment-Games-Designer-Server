# Build: docker build -t egd-server -f packaging/Dockerfile .
FROM rust:1-slim AS build
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY server server
COPY admin-ui admin-ui
RUN cd server && cargo build --release --locked

FROM debian:stable-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data egd && mkdir /data && chown egd /data
COPY --from=build /src/server/target/release/egd-server /usr/local/bin/egd-server
USER egd
VOLUME /data
ENV EGD_DATA=/data \
    EGD_SYNC_ADDR=0.0.0.0:8474 \
    EGD_ADMIN_ADDR=0.0.0.0:8475
EXPOSE 8474 8475
# Publish the admin port on 127.0.0.1 only: docker run -p 127.0.0.1:8475:8475 ...
CMD ["egd-server", "--no-browser", "--headless"]
