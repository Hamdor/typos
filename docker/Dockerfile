FROM ubuntu:20.04
ARG VERSION=1.0.4
ENV VERSION=${VERSION}
RUN apt-get update && apt-get install -y wget
RUN wget https://github.com/crate-ci/typos/releases/download/v${VERSION}/typos-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz && \
    tar -xzvf typos-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz && \
    mv typos /usr/local/bin
COPY entrypoint.sh /entrypoint.sh
WORKDIR /github/workspace
ENTRYPOINT ["/bin/bash", "/entrypoint.sh"]
