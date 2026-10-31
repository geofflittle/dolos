# Functions the oracle box scripts share, reading ROOT, SCRATCH, REV, MIN_FREE_G, HERE and TARGET.

PALLAS=${PALLAS:-scratch/pallas}
# The tree ids of the scratch trees the target dir's last build read.
BUILT="$ROOT/${TARGET:-target}/built-trees"
# The arms that plant a defect, each judged on the records plants/<arm>.records names.
PLANTED_ARMS="colret coin inflight rodrigo-reverted rob-reverted cost revalidate revalidate-drops subouts byron-attrs byron-signer byron-guard chaincode minfee-subtx emptyguards tag24 v1v2"

# Whether an arm plants its defect in pallas.
pallas_arm() {
  case $1 in
    rob-reverted | cost | byron-attrs | byron-signer | byron-guard | chaincode | minfee-subtx | emptyguards | tag24 | v1v2) return 0 ;;
    *) return 1 ;;
  esac
}

# The Dolos files whose change puts an arm in the quick tier, the pin files for a pallas arm.
plant_files() {
  if pallas_arm "$1"; then
    printf 'Cargo.toml\nCargo.lock\n'
    return
  fi
  case $1 in
    rodrigo-reverted) diff=rodrigo ;;
    *) diff=$1 ;;
  esac
  sed -n 's#^+++ b/##p' "$HERE/plants/$diff.diff"
}

# Exits when the build root has less than MIN_FREE_G gigabytes free.
require_free() {
  free=$(df -BG --output=avail "$ROOT" | tail -1 | tr -dc 0-9)
  if [ "$free" -lt "$MIN_FREE_G" ]; then
    echo "REFUSED free=${free}G below ${MIN_FREE_G}G at $ROOT"
    exit 2
  fi
}

# Puts the scratch tree at REV of the bare repository $ROOT/dolos.git with no planted change.
reset_scratch() {
  if [ ! -d "$ROOT/$SCRATCH/.git" ]; then
    git clone -q --no-checkout "$ROOT/dolos.git" "$ROOT/$SCRATCH"
  fi
  git -C "$ROOT/$SCRATCH" fetch -q "$ROOT/dolos.git" "$REV"
  git -C "$ROOT/$SCRATCH" checkout -q -f --detach FETCH_HEAD
  git -C "$ROOT/$SCRATCH" clean -q -fdx
}

# The tree id of a work tree's files, staged in an index of its own, or none when the tree is absent.
tree_of() {
  if [ ! -d "$1/.git" ]; then
    echo none
    return
  fi
  index="$1/.git/built-index"
  rm -f "$index"
  GIT_INDEX_FILE=$index git -C "$1" add -A
  GIT_INDEX_FILE=$index git -C "$1" write-tree
  rm -f "$index"
}

# The files that differ between an old and a new tree id of a work tree, each with a prefix, every file of the new tree when the old one is not in the repository.
changed_files() {
  if [ "$3" = none ]; then
    return
  fi
  if [ "$2" != none ] && git -C "$1" cat-file -e "$2^{tree}" 2>/dev/null; then
    git -C "$1" diff --name-only "$2" "$3"
  else
    git -C "$1" ls-tree -r --name-only "$3"
  fi | sed "s#^#$4#"
}

# The first dep-info file rustc wrote beside one of the output files of a unit, its outputs given comma separated.
dep_info() {
  echo "$1" | tr , '\n' | while read -r out; do
    out=${out#/w/}
    stem=$(basename "$out" | sed -E 's/^lib(.*)\.(rlib|rmeta|so|a)$/\1/')
    # A build script's output is a link named build-script-<name> in a dir named after the package and the unit hash.
    case $stem in
      build-script-*)
        dir=$(dirname "$out")
        stem="$(echo "$stem" | tr - _)-${dir##*-}"
        ;;
    esac
    if [ -f "$ROOT/$(dirname "$out")/$stem.d" ]; then
      echo "$ROOT/$(dirname "$out")/$stem.d"
      break
    fi
  done
}

# One line for each unit cargo reports fresh whose dep-info names a file of the list, or that has no dep-info.
fresh_reading() {
  grep '"reason":"compiler-artifact"' "$1" | grep '"fresh":true' |
    sed -n 's/.*"filenames":\[\([^]]*\)\].*/\1/p' | tr -d '"' |
    while read -r outs; do
      d=$(dep_info "$outs")
      if [ -z "$d" ]; then
        echo "STALE unit=$outs dep_info=missing"
        continue
      fi
      tr ' ' '\n' <"$d" | sed "s/:\$//; s#^/w/$SCRATCH/##" | grep -x -F -f "$2" | sort -u |
        sed "s#^#STALE unit=$outs file=#"
    done
}

# Runs cargo with the arguments in the scratch tree, cargo's messages in $1, and refuses when cargo reused a unit that reads a file changed since the target dir's last build.
build_checked() {
  json=$1
  shift
  dolos=$(tree_of "$ROOT/$SCRATCH")
  pallas=$(tree_of "$ROOT/$PALLAS")
  if [ -f "$BUILT" ]; then
    read -r built_dolos built_pallas <"$BUILT"
  else
    # With no record of the last build, every file gets a new time, so cargo rebuilds each unit that reads one.
    built_dolos=none built_pallas=none
    for tree in "$SCRATCH" "$PALLAS"; do
      if [ -d "$ROOT/$tree/.git" ]; then
        git -C "$ROOT/$tree" ls-files -z --cached --others --exclude-standard | (cd "$ROOT/$tree" && xargs -0 touch)
        # The index takes the new times too, so a later checkout of the same commit keeps each file and its time.
        git -C "$ROOT/$tree" update-index -q --refresh || true
      fi
    done
  fi
  # A build that fails or reuses a stale unit leaves units built from more than one tree, so it removes the record and the next build starts cold.
  if ! sh "$HERE/in-image.sh" "$SCRATCH" cargo "$@" --message-format=json >"$json" 2>"$json.err"; then
    rm -f "$BUILT"
    echo "BUILD_FAILED log=$json.err"
    return 1
  fi
  {
    changed_files "$ROOT/$SCRATCH" "$built_dolos" "$dolos" ""
    changed_files "$ROOT/$PALLAS" "$built_pallas" "$pallas" "/w/$PALLAS/"
  } >"$json.changed"
  fresh_reading "$json" "$json.changed" >"$json.stale"
  if [ -s "$json.stale" ]; then
    rm -f "$BUILT"
    head -4 "$json.stale"
    return 1
  fi
  echo "$dolos $pallas" >"$BUILT"
}

# Builds the test binary of the scratch tree's dolos library, cargo's messages in $1 and the rest of the arguments passed to cargo.
build_judge() {
  json=$1
  shift
  build_checked "$json" test --release -p dolos --lib --no-run "$@"
}

# The executable cargo built that a message file names, the test binary of a no run test build or the binary of a build.
executable() {
  sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' "$1" | tail -1
}
