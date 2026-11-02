# syntax=docker/dockerfile:1
FROM node:26-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci
COPY web/ ./
RUN npm run build

FROM rust:1.99-slim AS server
ARG KUROE_BUILD=dev
ENV KUROE_BUILD=$KUROE_BUILD
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src
COPY --from=web /web/dist ./web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && cp target/release/kuroe /kuroe

FROM debian:stable-slim
LABEL org.opencontainers.image.title="kuroe" \
      org.opencontainers.image.description="Self-hosted deployment server for Docker hosts" \
      org.opencontainers.image.source="https://github.com/SalahaldinBilal/kuroe" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
RUN apt-get update \
 && apt-get upgrade -y \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && install -m 0755 -d /etc/apt/keyrings \
 && curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
 && . /etc/os-release \
 && echo "deb [signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian $VERSION_CODENAME stable" > /etc/apt/sources.list.d/docker.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends docker-ce-cli docker-compose-plugin \
 && apt-get purge -y curl \
 && apt-get autoremove -y \
 && rm -rf /var/lib/apt/lists/*
COPY --from=server /kuroe /usr/local/bin/kuroe
ENV KUROE_DATA=/data KUROE_LISTEN=0.0.0.0:27863
VOLUME /data
EXPOSE 27863
ENTRYPOINT ["kuroe"]
CMD ["serve"]
