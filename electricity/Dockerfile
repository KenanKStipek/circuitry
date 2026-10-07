# Multi-arch image for the electricity binary (preview). Built from
# pre-built static musl binaries placed at dist/linux/$TARGETARCH/electricity
# by the CI workflow — this Dockerfile has no RUN steps, so no emulation is
# needed to build the arm64 image on an amd64 runner.
#
# distroless/static includes CA certificates and an unprivileged user; it
# ships its own multi-arch manifest, so FROM resolves per target platform.
FROM gcr.io/distroless/static-debian12:nonroot
ARG TARGETARCH
COPY dist/linux/${TARGETARCH}/electricity /usr/local/bin/electricity
ENTRYPOINT ["/usr/local/bin/electricity"]
