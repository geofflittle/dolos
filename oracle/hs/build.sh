#!/bin/sh
# Compiles one module of this directory against a prebuilt cardano-ledger
# package store, mounted read only.
# Usage: build.sh <module> <store dir> <work dir>
set -eu
M=$1
STORE=$2
WORK=$3
mkdir -p "$WORK/bin" "$WORK/obj" "$WORK/out"
cp "$(dirname "$0")"/*.hs "$WORK/"
docker run --rm -v "$STORE":/work:ro -v "$WORK":/probe -w /probe conformance-gen-hs:2 \
  ghc -O0 -hide-all-packages -package-env - \
    -package-db /work/cabal-dir/store/ghc-9.6.7/package.db \
    -package-db /work/cardano-ledger/dist-newstyle/packagedb/ghc-9.6.7 \
    -package base -package containers -package data-default -package microlens \
    -package cardano-strict-containers -package cardano-data -package cardano-slotting \
    -package cardano-ledger-core -package cardano-ledger-shelley -package cardano-ledger-mary \
    -package cardano-ledger-dijkstra -package cardano-ledger-binary \
    -package-id cardano-ledger-core-1.23.0.0-inplace-testlib \
    -package small-steps -package transformers -package bytestring -package base16-bytestring \
    -package hspec -package ImpSpec -package aeson -package cardano-ledger-alonzo \
    -package plutus-ledger-api -package mtl \
    -package cardano-ledger-byron -package cardano-crypto-wrapper -package cardano-crypto-class \
    -package primitive -package cborg \
    -package-id cardano-ledger-dijkstra-0.5.0.0-inplace-testlib \
    -package-id cardano-ledger-shelley-1.21.0.0-inplace-testlib \
    -package-id cardano-ledger-conway-1.25.0.0-inplace-testlib \
    -package-id cardano-ledger-alonzo-1.18.0.0-inplace-testlib \
    -package-id cardano-ledger-babbage-1.15.0.1-inplace-testlib \
    -package-id cardano-ledger-mary-1.12.0.0-inplace-testlib \
    -package-id cardano-ledger-allegra-1.10.2.1-inplace-testlib \
    -outputdir "/probe/obj/$M" -o "/probe/bin/$M" "$M.hs"
