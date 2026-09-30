# The container image ghcr.io/baotista/licguard: the released static (musl)
# binary on a distroless base, which ships the CA certificates the registry
# origin needs for HTTPS.
#
# The build context holds one binary per platform, at
# <os>/<arch>/licguard (e.g. linux/amd64/licguard); the publish-container
# workflow lays it out from the release archives.
#
#   docker run --rm -v "$PWD:/work" ghcr.io/baotista/licguard check

FROM gcr.io/distroless/static-debian12
ARG TARGETPLATFORM
COPY ${TARGETPLATFORM}/licguard /usr/local/bin/licguard
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/licguard"]
