#!/bin/sh

set -e

DIR="$( cd "$( dirname "$0" )" && pwd )"
REPO_ROOT="$(git rev-parse --show-toplevel)"
PLATFORM="linux/amd64"
OCI_OUTPUT="$REPO_ROOT/build/oci"
DOCKERFILE="$REPO_ROOT/docker/Dockerfile"
NAME=zebra

export DOCKER_BUILDKIT=1
export SOURCE_DATE_EPOCH=1

echo $DOCKERFILE
mkdir -p $OCI_OUTPUT

# Build runtime image for docker run
echo "Building runtime image..."
docker build -f "$DOCKERFILE" "$REPO_ROOT" \
	--platform "$PLATFORM" \
	--target runtime \
	--output type=oci,rewrite-timestamp=true,force-compression=true,dest=$OCI_OUTPUT/zebra.tar,name=zebra \
	"$@"

# Extract binary locally from export stage
echo "Extracting binary..."
docker build -f "$DOCKERFILE" "$REPO_ROOT" --quiet \
	--platform "$PLATFORM" \
	--target export \
	--output type=local,dest="$REPO_ROOT/build" \
	"$@"

