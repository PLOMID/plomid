# 
# © 2026 PLOMID Technology Solutions
#
# PLOMID
# Platform for Modern Intelligence and Data
#
# Author: Sainath Sapa
# GitHub: https://github.com/sainathsapa
#
# Licensed under the Apache License, Version 2.0;
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     https://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
# PLOMID server runtime image.
#
# This image contains ONLY the compiled server binary. There is deliberately no
# build stage, no toolchain, no package manager cache and no source code: the
# Linux executable is produced on the build host by `make linux-binary` (see the
# Makefile) and copied in as a single artifact. End users of the image can
# therefore not read the source, and the image cannot leak build inputs.
#
# Build via `make docker` so the artifact exists first.

FROM debian:bookworm-slim

# `TARGETARCH` (`arm64` / `amd64`) is set by BuildKit for the platform being
# built, so the same Dockerfile packages either architecture.
ARG TARGETARCH

LABEL org.opencontainers.image.title="PLOMID" \
      org.opencontainers.image.description="PLOMID database server (PostgreSQL wire protocol)" \
      org.opencontainers.image.licenses="Apache-2.0"

# Dedicated unprivileged account; the server never needs root.
RUN set -eux; \
    groupadd --system --gid 10001 plomid; \
    useradd --system --uid 10001 --gid plomid \
        --home-dir /var/lib/plomid --shell /usr/sbin/nologin plomid; \
    install -d -o plomid -g plomid -m 0750 /var/lib/plomid/data

# The one and only artifact taken from the outside world.
COPY dist/linux-${TARGETARCH}/plomid-server /usr/local/bin/plomid-server
RUN chmod 0755 /usr/local/bin/plomid-server

USER plomid:plomid
WORKDIR /var/lib/plomid/data

# A container is reachable only via a non-loopback bind, which the server
# refuses without an explicit opt-in. Opt the image in (the warning it logs on
# every start is intentional) and keep authentication enabled; the default
# credential pair is public, so pass PLOMID_PASSWORD when publishing the port.
ENV PLOMID_HOST=0.0.0.0 \
    PLOMID_PORT=5432 \
    PLOMID_DATA_DIR=/var/lib/plomid/data \
    PLOMID_ALLOW_INSECURE_REMOTE=true

# Crash-safe storage lives here; mount a volume so data outlives the container.
VOLUME ["/var/lib/plomid/data"]
EXPOSE 5432

# Liveness only: opens a TCP connection to the listener. No client library is
# shipped in the image, so this uses bash's /dev/tcp instead.
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/bin/bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${PLOMID_PORT:-5432}"]

ENTRYPOINT ["/usr/local/bin/plomid-server"]
