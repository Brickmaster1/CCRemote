FROM --platform=$TARGETPLATFORM rust:alpine3.22 AS builder

ARG TARGETPLATFORM

RUN apk add --no-cache git musl-dev openssl-dev pkgconfig build-base
	
WORKDIR /usr/src
	
RUN git clone --filter=blob:none --sparse https://github.com/cyb0124/CCRemote.git \
    && cd CCRemote \
    && git sparse-checkout set server
	
WORKDIR /usr/src/CCRemote/server

RUN cargo build --release


FROM --platform=$TARGETPLATFORM alpine:3.22
RUN apk add --no-cache ca-certificates

COPY --from=builder /usr/src/CCRemote/server/target/release/cc-remote /usr/local/bin/cc-remote

EXPOSE 1847
ENTRYPOINT ["/usr/local/bin/cc-remote"]