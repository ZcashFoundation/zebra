FROM ghcr.io/actions/actions-runner:latest as runner

COPY --chown=runner:docker entrypoint.sh .
RUN chmod +x entrypoint.sh

ENTRYPOINT ["home/runner/entrypoint.sh"]
