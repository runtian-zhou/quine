FROM rust:1.90-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        gh \
        git \
        jq \
        less \
        procps \
        ripgrep \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

ENV CARGO_HOME=/usr/local/cargo
ENV HOME=/root
ENV XDG_RUNTIME_DIR=/tmp/xdg-runtime
ENV XDG_STATE_HOME=/root/.quine

RUN rustup component add clippy rustfmt

RUN mkdir -p /tmp/xdg-runtime /root/.quine/state

ENTRYPOINT ["cargo", "run", "--bin", "quine", "--"]
CMD ["chat"]
