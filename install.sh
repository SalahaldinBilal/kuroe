#!/bin/sh
set -eu

IMAGE="${KUROE_IMAGE:-ghcr.io/salahaldinbilal/kuroe:latest}"
PORT="${KUROE_PORT:-27863}"
BINARY_URL="${KUROE_BINARY_URL:-}"
RELEASES="https://github.com/SalahaldinBilal/kuroe/releases/latest/download"
MODE=container

for arg in "$@"; do
  case "$arg" in
    --systemd) MODE=systemd ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

show_setup_code() {
  tries=0
  while [ "$tries" -lt 30 ]; do
    if CODE=$("$@" 2>/dev/null); then
      if [ -n "$CODE" ]; then
        echo
        echo "Setup code: $CODE"
        echo "Creating the owner account needs it. To see it again, run:"
        echo "  $*"
      fi
      return 0
    fi
    tries=$((tries + 1))
    sleep 1
  done
  echo "kuroe isn't answering yet. Get the setup code later with: $*" >&2
}

command -v docker >/dev/null || { echo "docker is not installed" >&2; exit 1; }
docker info >/dev/null 2>&1 || { echo "cannot reach the docker daemon" >&2; exit 1; }

if [ "$MODE" = container ]; then
  docker pull "$IMAGE"
  docker volume create kuroe_data >/dev/null
  docker network inspect kuroe_proxy >/dev/null 2>&1 || docker network create --label kuroe.managed=true kuroe_proxy >/dev/null
  docker rm -f kuroe >/dev/null 2>&1 || true
  docker run -d --name kuroe --restart unless-stopped \
    --network kuroe_proxy \
    -p "$PORT:27863" \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v kuroe_data:/data \
    "$IMAGE" serve
  echo "kuroe is running on http://localhost:$PORT"
  show_setup_code docker exec kuroe kuroe admin setup-code
  exit 0
fi

[ "$(uname -s)" = Linux ] || { echo "--systemd is Linux only" >&2; exit 1; }
[ "$(id -u)" = 0 ] || { echo "--systemd needs root" >&2; exit 1; }
command -v curl >/dev/null || { echo "curl is not installed" >&2; exit 1; }

if [ -z "$BINARY_URL" ]; then
  case "$(uname -m)" in
    x86_64 | amd64) ARCH=amd64 ;;
    aarch64 | arm64) ARCH=arm64 ;;
    *) echo "there is no release binary for $(uname -m)" >&2; exit 1 ;;
  esac
  BINARY_URL="$RELEASES/kuroe-linux-$ARCH"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
curl -fsSL "$BINARY_URL" -o "$TMP/kuroe"
EXPECTED=$(curl -fsSL "$BINARY_URL.sha256" | cut -d ' ' -f 1)
ACTUAL=$(sha256sum "$TMP/kuroe" | cut -d ' ' -f 1)
[ "$EXPECTED" = "$ACTUAL" ] || { echo "the download doesn't match $BINARY_URL.sha256" >&2; exit 1; }

install -m 755 "$TMP/kuroe" /usr/local/bin/kuroe
curl -fsSL "${BINARY_URL%/*}/kuroe.service" -o /etc/systemd/system/kuroe.service
mkdir -p /var/lib/kuroe
systemctl daemon-reload
systemctl enable kuroe
systemctl restart kuroe
echo "kuroe is running on http://localhost:27863"
show_setup_code /usr/local/bin/kuroe --data-dir /var/lib/kuroe admin setup-code
