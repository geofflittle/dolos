FROM haskell:9.6.7@sha256:27cf8fb8cf74d3a24313f6d4a100f9c335336c41a9f0197eb53f3ea3502b4ce8

RUN printf '%s\n' \
      'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20261024T120000Z bullseye main' \
      'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20261024T120000Z bullseye-updates main' \
      >/etc/apt/sources.list \
    && apt-get update && apt-get install -y --no-install-recommends \
      pkg-config zlib1g-dev libtool autoconf automake \
      build-essential git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN git init -q /tmp/sodium && cd /tmp/sodium \
    && git fetch -q --depth 1 https://github.com/IntersectMBO/libsodium 8a355ef81ed0de09f1fbc4eec76a3f7cf716f202 \
    && git checkout -q FETCH_HEAD && autoreconf -fi \
    && ./configure --prefix=/usr/local && make -j4 && make install && rm -rf /tmp/sodium

RUN git init -q /tmp/secp && cd /tmp/secp \
    && git fetch -q --depth 1 https://github.com/bitcoin-core/secp256k1 acf5c55ae6a94e5ca847e07def40427547876101 \
    && git checkout -q FETCH_HEAD && ./autogen.sh \
    && ./configure --prefix=/usr/local --enable-module-schnorrsig --enable-experimental \
    && make -j4 && make install && rm -rf /tmp/secp

RUN git init -q /tmp/blst && cd /tmp/blst \
    && git fetch -q --depth 1 https://github.com/supranational/blst 8c7db7fe8d2ce6e76dc398ebd4d475c0ec564355 \
    && git checkout -q FETCH_HEAD && ./build.sh \
    && mkdir -p /usr/local/include /usr/local/lib/pkgconfig \
    && cp bindings/blst_aux.h bindings/blst.h bindings/blst.hpp /usr/local/include/ \
    && cp libblst.a /usr/local/lib/ \
    && printf 'prefix=/usr/local\nexec_prefix=${prefix}\nlibdir=${exec_prefix}/lib\nincludedir=${prefix}/include\n\nName: libblst\nDescription: Multilingual BLS12-381 signature library\nURL: https://github.com/supranational/blst\nVersion: 0.3.14\nCflags: -I${includedir}\nLibs: -L${libdir} -lblst\n' > /usr/local/lib/pkgconfig/libblst.pc \
    && rm -rf /tmp/blst

ENV PKG_CONFIG_PATH=/usr/local/lib/pkgconfig
ENV LD_LIBRARY_PATH=/usr/local/lib
RUN ldconfig
