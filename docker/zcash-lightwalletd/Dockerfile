ARG ZCASHD_CONF_PATH=/etc/zcash.conf
ARG LWD_GRPC_PORT=9067
ARG LWD_HTTP_PORT=9068

##
## Build
##
FROM golang:1.17 AS build

# Create and change to the app directory.
WORKDIR /app

# Retrieve application dependencies.
# This allows the container build to reuse cached dependencies.
# Expecting to copy go.mod and if present go.sum.
COPY go.mod ./
COPY go.sum ./

# Do not use `go get` as it updates the requirements listed in your go.mod file.
# `go mod download` does not add new requirements or update existing requirements.
RUN go mod download

# Copy local code to the container image.
COPY . ./

# Build and install the binary.
RUN go build -v -o /opt/lightwalletd

ARG ZCASHD_CONF_PATH

RUN set -ex; \
  { \
    echo "rpcuser=zcashrpc"; \
    echo "rpcpassword=`head /dev/urandom | tr -dc A-Za-z0-9 | head -c 13 ; echo ''`" \
    echo "rpcbind=127.0.0.1"; \
    echo "rpcport=8232"; \
  } > "${ZCASHD_CONF_PATH}"

ENTRYPOINT ["/opt/lightwalletd"]
CMD ["--no-tls-very-insecure", "--grpc-bind-addr=0.0.0.0:9067",  "--http-bind-addr=0.0.0.0:9068", "--log-file=/dev/stdout", "--log-level=7"]

##
## Deploy
##
FROM debian:bookworm-slim as runtime

ARG ZCASHD_CONF_PATH
# Maintain backward compatibility with mainstream repo using this ARGs in docker-compose
ARG LWD_GRPC_PORT
ARG LWD_HTTP_PORT

WORKDIR /

COPY --from=build /opt/lightwalletd /usr/local/bin
COPY --from=build $ZCASHD_CONF_PATH ./

EXPOSE 9067
EXPOSE 9068

ENTRYPOINT ["lightwalletd"]
CMD ["--no-tls-very-insecure", "--grpc-bind-addr=0.0.0.0:9067",  "--http-bind-addr=0.0.0.0:9068", "--log-file=/dev/stdout", "--log-level=7"]
