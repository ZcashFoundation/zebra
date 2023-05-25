FROM ghcr.io/actions/actions-runner:latest as runner

USER root

RUN apt-get update -y \
    && apt-get install -y --no-install-recommends \
    openssh-client \
    curl \
    jq \
    && rm -rf /var/lib/apt/lists/*

COPY --chown=runner:docker entrypoint.sh .
RUN chmod +x entrypoint.sh

USER runner

ENTRYPOINT ["./entrypoint.sh"]
