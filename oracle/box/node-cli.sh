#!/bin/sh
# Runs cardano-cli with the arguments given in a new container of the image, user, environment and volumes of the node container NODE, a container the docker on PATH starts.
# Usage: NODE=container node-cli.sh <cardano-cli argument>...
set -eu
: "${NODE:?}"
image=$(docker inspect -f '{{.Config.Image}}' "$NODE")
user=$(docker inspect -f '{{.Config.User}}' "$NODE")
env_file=$(mktemp)
trap 'rm -f "$env_file"' EXIT
docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$NODE" >"$env_file"
docker run --rm --network none --volumes-from "$NODE" --env-file "$env_file" --user "${user:-0}" \
  --entrypoint cardano-cli "$image" "$@"
