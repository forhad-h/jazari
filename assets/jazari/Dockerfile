# Worker image for jazari: one container per product, with the repo mounted at its host path.
# Common toolchains are included, so workers can build and test without asking anyone. Anything
# else they install in user space (pip --user, a venv, npm in the project).
FROM node:22-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      git ca-certificates curl jq ripgrep make build-essential pkg-config sqlite3 shellcheck \
      python3 python3-venv python3-pip python-is-python3 \
 && rm -rf /var/lib/apt/lists/*
ARG CLAUDE_CODE_VERSION=2.1.332
RUN npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} && npm cache clean --force
USER node
ENV PATH=/home/node/.local/bin:$PATH \
    PIP_BREAK_SYSTEM_PACKAGES=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1
WORKDIR /home/node
ENTRYPOINT ["sleep", "infinity"]
