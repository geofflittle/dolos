ARG TOOLCHAIN

FROM ${TOOLCHAIN} AS dependencies
ARG LEDGER
WORKDIR /work/cardano-ledger
RUN git init -q . \
    && git fetch -q --depth 1 https://github.com/IntersectMBO/cardano-ledger "$LEDGER" \
    && git checkout -q -f FETCH_HEAD
COPY reference-writer.cabal /work/writer/
COPY pins/${LEDGER}.projec[t] /work/pins/
RUN echo 'packages: /work/writer' > cabal.project.local \
    && if [ -f "/work/pins/$LEDGER.project" ]; then cat "/work/pins/$LEDGER.project" >> cabal.project.local; fi \
    && cabal update \
    && cabal build -j4 --only-dependencies reference-writer

FROM dependencies AS writer
ARG LEDGER
ARG NAME
COPY . /work/writer/
RUN printf '%s\n' \
      'module Build (pin, name) where' \
      'pin :: String' \
      "pin = \"$LEDGER\"" \
      'name :: String' \
      "name = \"$NAME\"" \
      > /work/writer/Build.hs \
    && cabal build -j4 reference-writer \
    && cp "$(cabal list-bin reference-writer)" /work/reference-writer

FROM ${TOOLCHAIN}
COPY --from=writer /work/reference-writer /usr/local/bin/reference-writer
ENTRYPOINT ["reference-writer"]
