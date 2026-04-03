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
        python-is-python3 \
        python3 \
        ripgrep \
    && rm -rf /var/lib/apt/lists/*

ENV CARGO_HOME=/usr/local/cargo
ENV CARGO_TARGET_DIR=/opt/quine-target
ENV HOME=/root
ENV XDG_RUNTIME_DIR=/tmp/xdg-runtime
ENV XDG_STATE_HOME=/root/.quine

RUN rustup component add clippy rustfmt

RUN mkdir -p /tmp/xdg-runtime /root/.quine/state /opt/quine-target

WORKDIR /opt/quine-src

COPY . .

ARG GIT_COMMIT_HASH=unknown
ENV GIT_COMMIT_HASH=${GIT_COMMIT_HASH}

RUN cargo build --bin quine

WORKDIR /workspace

ENTRYPOINT ["/bin/bash"]
CMD ["-l"]
