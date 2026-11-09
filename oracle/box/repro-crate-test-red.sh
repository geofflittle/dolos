#!/bin/sh
# Runs the box function test with the control check that wants exactly one passed crate test.
# Usage: repro-crate-test-red.sh <empty work dir>
# Expected verdict lines: "LIB-TEST crate_passed FAIL", "LIB-TEST crate_unrun FAIL" and "LIB-TEST RESULT FAIL".
set -u
sh "$(dirname "$0")/lib_test.sh" "$1" 2>/dev/null | grep -E 'crate_|RESULT'
