FROM debian:13-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home agentflare
USER agentflare
WORKDIR /home/agentflare

RUN curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/getappz/agentflare/master/install.sh | sh

ENV PATH="/home/agentflare/.local/bin:${PATH}"

ENTRYPOINT ["agentflare"]
