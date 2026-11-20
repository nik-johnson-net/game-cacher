#!/usr/bin/env bash
# Local dev stack without compose: one podman pod running game-cacher with S3
# storage (SeaweedFS) and Vault secrets (dev mode). The containers share the
# pod's network namespace, so they reach each other on 127.0.0.1. Works in
# nested/rootless podman where compose networking does not.
#
#   make docker                 # build game-cacher:dev first
#   dev/stack.sh up             # start (or restart) the stack
#   dev/stack.sh logs           # follow game-cacher logs
#   dev/stack.sh down           # stop and remove the pod (data volume kept)
#   dev/stack.sh down --volumes # also delete cached data
#
# Host ports (no root needed): UI/API 8080 (password: admin), DNS 5353,
# cache HTTP 8081, SNI 8443, S3 9000, Vault 8200.
#   dig @127.0.0.1 -p 5353 lancache.steamcontent.com
set -euo pipefail

POD=game-cacher-dev
VOLUME=game-cacher-dev-s3
IMAGE=${IMAGE:-localhost/game-cacher:dev}
podman=${PODMAN:-podman}

wait_for() { # name url
    for _ in $(seq 60); do
        curl -s -o /dev/null -m 2 "$2" && return 0
        sleep 1
    done
    echo "timed out waiting for $1 ($2)" >&2
    exit 1
}

up() {
    $podman image exists "$IMAGE" || { echo "image $IMAGE not found; run 'make docker' first" >&2; exit 1; }
    $podman pod rm -f "$POD" >/dev/null 2>&1 || true
    $podman volume exists "$VOLUME" || $podman volume create "$VOLUME" >/dev/null

    $podman pod create --name "$POD" \
        -p 8080:8080 -p 5353:53/udp -p 5353:53/tcp -p 8081:80 -p 8443:443 \
        -p 9000:9000 -p 8200:8200 >/dev/null

    # SeaweedFS's S3 gateway with auth disabled (any credentials are accepted).
    # Its volume server defaults to :8080, which game-cacher's admin port uses.
    $podman run -d --pod "$POD" --name "$POD-s3" -v "$VOLUME:/data" \
        docker.io/chrislusf/seaweedfs:latest \
        server -dir=/data -s3 -s3.port=9000 -volume.port=8090 -ip.bind=0.0.0.0 >/dev/null

    $podman run -d --pod "$POD" --name "$POD-vault" --cap-add IPC_LOCK \
        docker.io/hashicorp/vault:latest \
        server -dev -dev-root-token-id=dev-root -dev-listen-address=0.0.0.0:8200 >/dev/null

    wait_for vault http://127.0.0.1:8200/v1/sys/health
    echo "waiting for S3 and creating the bucket..."
    for _ in $(seq 60); do
        code=$(curl -s -o /dev/null -m 2 -w '%{http_code}' -X PUT http://127.0.0.1:9000/game-cache || true)
        [[ $code == 200 || $code == 409 ]] && break
        sleep 1
    done
    [[ $code == 200 || $code == 409 ]] || { echo "S3 did not come up (last status $code)" >&2; exit 1; }

    $podman run -d --pod "$POD" --name "$POD-app" \
        -e LOG=info \
        -e GC_ADMIN_PASSWORD=admin \
        -e GC_CACHE_IP=127.0.0.1 \
        -e GC_UPSTREAM_DNS=1.1.1.1,8.8.8.8 \
        -e GC_DNS_FORWARD=1.1.1.1 \
        -e GC_STORAGE=s3 \
        -e GC_S3_BUCKET=game-cache \
        -e GC_S3_ENDPOINT=http://127.0.0.1:9000 \
        -e GC_S3_PATH_STYLE=true \
        -e AWS_ACCESS_KEY_ID=any \
        -e AWS_SECRET_ACCESS_KEY=any \
        -e GC_SECRETS=vault \
        -e GC_VAULT_ADDR=http://127.0.0.1:8200 \
        -e VAULT_TOKEN=dev-root \
        -e GC_CACHE_MAX_SIZE=50GiB \
        "$IMAGE" >/dev/null

    wait_for game-cacher http://127.0.0.1:8080/healthz
    echo "game-cacher is up: http://127.0.0.1:8080 (password: admin)"
}

down() {
    $podman pod rm -f "$POD" >/dev/null 2>&1 || true
    if [[ ${1:-} == --volumes ]]; then
        $podman volume rm -f "$VOLUME" >/dev/null 2>&1 || true
    fi
    echo "stopped"
}

case ${1:-} in
    up) up ;;
    down) down "${2:-}" ;;
    logs) exec $podman logs -f "$POD-app" ;;
    status) $podman ps --pod --filter "pod=$POD" ;;
    *) echo "usage: $0 up|down [--volumes]|logs|status" >&2; exit 2 ;;
esac
