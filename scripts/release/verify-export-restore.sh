#!/usr/bin/env bash
# verify-export-restore — ONE COMMAND for the candidate-level export and
# backup/restore proof (go-public plan §3 GP04; runbook:
# docs/validation-and-release.md, "Export and restore candidate proof").
#
# The export-parity (PR #77) and backup/restore (M4.4) behaviors are
# proven by unit/integration suites and e2e specs against development
# builds; this script produces the missing evidence: the same behaviors
# driven against a PACKAGED RELEASE CANDIDATE, bound to the candidate's
# SHA-256 digests, as a dated record that can be produced on any
# authorized host.
#
# Three legs (per-leg logs, skip flags for iteration but NEVER for a
# certification run — a skipped leg is reported as SKIPPED and never
# counts as green):
#
#   export  the export spec's parity section (export count == search
#           total, ids in search order, all formats) and the
#           cross-page selection section (every checked id from beyond
#           page one is exported), driven against the candidate binary
#           through the e2e harness's MAILCHAT_E2E_BINARY override
#           inside the existing e2e container
#   restore the backup/restore journey (a seeded vault with known
#           messages AND files backed up, restored, opened; the
#           return-to-previous-vault journey and the honest
#           prior-recovery status lines) plus the restart spec, against
#           the same candidate binary
#   suites  the M4.4 backup suite (interrupted-switch boundaries) and
#           the export v2 suite rerun at the candidate commit, itemized
#           in the record rather than buried in a full workspace run
#
# Artifacts:
#   - default (docker route): build the Linux candidate through
#     scripts/release/build-linux.sh (the pinned 22.04 toolchain), then
#     digest the AppImage + deb exactly the way the SHA256SUMS process
#     does, and extract the candidate binary from the AppImage
#     (--appimage-extract — no FUSE needed) for the e2e legs;
#   - --bundles DIR: like default but with already-built bundles
#     (skips the build);
#   - --binary PATH: host route — digest and drive an explicit candidate
#     binary (any platform's unpacked candidate) with the legs running
#     on the host itself; needs the host webkit/tauri-driver stack for
#     the e2e legs (the Linux-without-docker route — on macOS/Windows
#     the WebDriver e2e legs are not the validated route and stay a
#     per-platform concern, see the runbook).
#
# Every run writes a dated evidence record (per-check pass/fail,
# artifact digests, commit, environment, and the counts the specs emit
# as [evidence] lines: messages, media objects, export rows, the backup
# container digest) into the log dir, and copies it under
# docs/evidence/ so a green (or honestly failed) run can be committed.
#
# Usage:
#   scripts/release/verify-export-restore.sh [--logs DIR]
#                                           [--bundles DIR | --binary PATH]
#                                           [--skip-export | --skip-restore |
#                                            --skip-suites | --skip-build]
#                                           [--no-evidence-copy] [--plan]
#
#   --plan  print the legs and the artifact digests this run would bind
#           without building or executing anything (acceptance check:
#           the binding must be inspectable before it is produced).
#
# Requires: docker (default route), git, sha256sum (or shasum -a 256).
# Exit status is 0 only when every requested leg passed.
set -uo pipefail

here="$(cd "$(dirname "$BASH_SOURCE")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
LOGS="$repo/target/release/export-restore-logs/$stamp"
BUNDLES=""
BINARY=""
SKIP_EXPORT=0
SKIP_RESTORE=0
SKIP_SUITES=0
SKIP_BUILD=0
PLAN=0
EVIDENCE_COPY=1

usage() {
  echo "usage: $0 [--logs DIR] [--bundles DIR | --binary PATH] [--skip-export]" >&2
  echo "          [--skip-restore] [--skip-suites] [--skip-build] [--no-evidence-copy] [--plan]" >&2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --logs)           LOGS="$2"; shift 2 ;;
    --bundles)        BUNDLES="$2"; shift 2 ;;
    --binary)         BINARY="$2"; shift 2 ;;
    --skip-export)    SKIP_EXPORT=1; shift ;;
    --skip-restore)   SKIP_RESTORE=1; shift ;;
    --skip-suites)    SKIP_SUITES=1; shift ;;
    --skip-build)     SKIP_BUILD=1; shift ;;
    --no-evidence-copy) EVIDENCE_COPY=0; shift ;;
    --plan)           PLAN=1; shift ;;
    *) echo "unknown option: $1" >&2; usage; exit 2 ;;
  esac
done

if [[ -n "$BINARY" && -n "$BUNDLES" ]]; then
  echo "error: --binary and --bundles are mutually exclusive (one artifact source per run)" >&2
  exit 2
fi

hash256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

digest_of() { hash256 "$1" | awk '{print $1}'; }

commit="$(git -C "$repo" rev-parse HEAD)"
short="$(git -C "$repo" rev-parse --short HEAD)"
branch="$(git -C "$repo" branch --show-current 2>/dev/null || echo detached)"

# The candidate binary the e2e harness drives (MAILCHAT_E2E_BINARY).
# Docker route: extracted from the AppImage into a repo-local work dir
# (under target/, so the matrix-style /src mount carries it into the
# containers). Host route: the --binary path as given.
WORK_DIR="$repo/target/release/export-restore-work/$stamp"
candidate_bin=""

overall=0
declare -a summary=()
declare -A item_status
record() { summary+=("$(printf '%-10s %s' "$1" "$2")"); }

# ----------------------------------------------------- artifact resolution

find_bundle() { # find_bundle <root> <pattern> — newest match or empty
  find "$1" -maxdepth 2 -name "$2" 2>/dev/null | sort | tail -1
}

resolve_artifacts() { # docker route: locate AppImage + deb, set candidate
  appimage="$(find_bundle "$BUNDLES" '*.AppImage')"
  deb="$(find_bundle "$BUNDLES" '*.deb')"
  if [[ -z "$appimage" ]]; then
    echo "error: no .AppImage under $BUNDLES — build first (scripts/release/build-linux.sh)" >&2
    exit 2
  fi
  if [[ -z "$deb" ]]; then
    echo "error: no .deb under $BUNDLES" >&2
    exit 2
  fi
  mkdir -p "$WORK_DIR"
  chmod +x "$appimage"
  if ! (cd "$WORK_DIR" && "$appimage" --appimage-extract) >"$LOGS/appimage-extract.log" 2>&1; then
    echo "error: --appimage-extract failed; see $LOGS/appimage-extract.log" >&2
    exit 2
  fi
  candidate_bin="$WORK_DIR/squashfs-root/mailchat-vault"
  if [[ ! -x "$candidate_bin" ]]; then
    echo "error: extracted candidate binary missing: $candidate_bin" >&2
    exit 2
  fi
}

# ------------------------------------------------------------------ plan

print_plan() {
  echo "== verify-export-restore plan (nothing is built or executed) =="
  echo "   repo:    $repo @ $short ($branch)"
  echo "   commit:  $commit"
  echo "   mode:    $([[ -n "$BINARY" ]] && echo "host route (--binary)" || echo "docker route (matrix-style pinned containers)")"
  echo
  echo "-- artifacts this run would bind (SHA-256):"
  if [[ -n "$BINARY" ]]; then
    if [[ -f "$BINARY" ]]; then
      printf '   %s  %s\n' "$(digest_of "$BINARY")" "$BINARY"
    else
      echo "   $BINARY (NOT FOUND — digest unavailable until the file exists)"
    fi
  else
    local root="$BUNDLES" ai de would_build="(none found)"
    [[ -z "$root" ]] && root="$repo/target/release/bundle"
    if [[ -z "$BUNDLES" && "$SKIP_BUILD" -eq 0 ]]; then
      would_build="(would build via scripts/release/build-linux.sh)"
    fi
    ai="$(find_bundle "$root" '*.AppImage')"
    de="$(find_bundle "$root" '*.deb')"
    if [[ -n "$ai" ]]; then printf '   %s  %s\n' "$(digest_of "$ai")" "$ai"; else echo "   no AppImage under $root $would_build"; fi
    if [[ -n "$de" ]]; then printf '   %s  %s\n' "$(digest_of "$de")" "$de"; else echo "   no deb under $root $would_build"; fi
  fi
  echo
  echo "-- legs:"
  echo "   export   e2e spec 'export' (parity: export count == search total, ids in"
  echo "            search order, all formats) and spec 'search-v2-fixes' (cross-page"
  echo "            selection: every checked id from beyond page one exported), against"
  echo "            the candidate binary via MAILCHAT_E2E_BINARY under xvfb"
  [[ "$SKIP_EXPORT" -eq 1 ]] && echo "            SKIPPED (--skip-export — not a certification run)"
  echo "   restore  e2e spec 'vault-backup' (known messages+files backed up on the"
  echo "            candidate, restored and reopened; return-to-previous vault; honest"
  echo "            prior-recovery lines; emits container digest + manifest counts as"
  echo "            [evidence] lines) and spec 'vault-restart' (interrupted-switch"
  echo "            startup recovery) against the same candidate binary"
  [[ "$SKIP_RESTORE" -eq 1 ]] && echo "            SKIPPED (--skip-restore — not a certification run)"
  echo "   suites   cargo test -p vault-core --test backup (M4.4 suite incl. the"
  echo "            interrupted-switch boundary matrix, itemized in the record) and"
  echo "            cargo test -p vault-core --test export_v2 (v2 export contract),"
  echo "            at the candidate commit"
  [[ "$SKIP_SUITES" -eq 1 ]] && echo "            SKIPPED (--skip-suites — not a certification run)"
  echo
  echo "-- record:"
  echo "   per-run evidence record -> $LOGS/evidence-record.md"
  echo "   committed copy          -> docs/evidence/export-restore-candidate-$(date -u +%Y-%m-%d).md"
  echo "   (the support matrix flips to verified-on-candidate only on a green run)"
}

if [[ "$PLAN" -eq 1 ]]; then
  print_plan
  exit 0
fi

# --------------------------------------------------------- route selection

if [[ -n "$BINARY" ]]; then
  HOST_ROUTE=1
  if [[ ! -f "$BINARY" ]]; then
    echo "error: --binary $BINARY not found" >&2
    exit 2
  fi
  candidate_bin="$BINARY"
else
  HOST_ROUTE=0
  command -v docker >/dev/null || { echo "error: docker not found (use --binary PATH on a host without docker)" >&2; exit 2; }
  docker info >/dev/null 2>&1 || { echo "error: docker daemon unreachable" >&2; exit 2; }
fi

mkdir -p "$LOGS"

SUITE_IMAGE="mailchat-vault-suite:24.04"
E2E_IMAGE="mailchat-vault-e2e:24.04"

# Run as the invoking user's uid/gid so files the containers write into
# the mounted repo are owned by the host user — same contract as
# matrix-linux.sh.
run_uid="$(id -u)"
run_gid="$(id -g)"

echo "== verify-export-restore: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "   repo:   $repo @ $short ($branch)"
echo "   host:   $(uname -sr) ($(uname -m)), $(nproc) cpus"
echo "   route:  $([[ "$HOST_ROUTE" -eq 1 ]] && echo "host (--binary $BINARY)" || echo "docker (pinned containers)")"
echo "   logs:   $LOGS"
{
  echo "verify-export-restore $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "repo $commit ($branch)"
  echo "host $(uname -sr) $(uname -m) $(nproc)cpus"
  if [[ "$HOST_ROUTE" -eq 0 ]]; then
    docker --version
  else
    echo "host route; node $(node --version 2>/dev/null || echo '?') cargo $(cargo --version 2>/dev/null || echo '?')"
  fi
} > "$LOGS/environment.txt"

# ------------------------------------------------------- artifacts & digests

if [[ "$HOST_ROUTE" -eq 0 ]]; then
  if [[ -z "$BUNDLES" && "$SKIP_BUILD" -eq 0 ]]; then
    echo
    echo "==== candidate build (build-linux.sh — pinned 22.04 toolchain)"
    if "$here/build-linux.sh" >"$LOGS/build.log" 2>&1; then
      record "build" "PASS"
    else
      echo "error: candidate build failed; see $LOGS/build.log" >&2
      record "build" "FAIL (see $LOGS/build.log)"
      printf '%s\n' "${summary[@]}"
      exit 1
    fi
    BUNDLES="$repo/target/release/bundle"
  elif [[ -z "$BUNDLES" ]]; then
    BUNDLES="$repo/target/release/bundle"
    record "build" "SKIPPED (--skip-build; using bundles under $BUNDLES)"
  else
    record "build" "SKIPPED (--bundles $BUNDLES)"
  fi
  resolve_artifacts
  artifacts=("$appimage" "$deb")
else
  artifacts=("$BINARY")
fi

echo
echo "==== candidate artifacts (SHA-256 binding)"
: > "$LOGS/artifacts.txt"
for f in "${artifacts[@]}"; do
  printf '%s  %s\n' "$(digest_of "$f")" "$f" | tee -a "$LOGS/artifacts.txt"
done
# SHA256SUMS-format manifest over exactly what this run proves, written
# the same way prepare-release.sh writes it (names relative to the
# artifact directory).
artifact_names=()
for f in "${artifacts[@]}"; do artifact_names+=("$(basename "$f")"); done
( cd "$(dirname "${artifacts[0]}")" && hash256 "${artifact_names[@]}" ) > "$LOGS/SHA256SUMS"
echo "   digests recorded in $LOGS/artifacts.txt (+ SHA256SUMS-format $LOGS/SHA256SUMS)"

# ------------------------------------------------------------- e2e helpers

container_e2e() { # container_e2e <log> <e2e-only-spec> — one focused spec
  local log="$1" spec="$2"
  local bin_in_container="/src/${candidate_bin#$repo/}"
  docker run --rm \
    -u "${run_uid}:${run_gid}" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e MAILCHAT_E2E_BINARY="$bin_in_container" \
    -e MAILCHAT_E2E_ONLY="$spec" \
    -v "$repo":/src \
    -w /src \
    -v mailchat-build-cargo:/cargo \
    "$E2E_IMAGE" \
    bash -eu -c '
      set -o pipefail
      [[ -d node_modules ]] || npm ci --no-audit --no-fund
      [[ -x "$MAILCHAT_E2E_BINARY" ]] || { echo "candidate binary missing: $MAILCHAT_E2E_BINARY" >&2; exit 2; }
      xvfb-run -a --server-args="-screen 0 1280x960x24" npm run test:e2e
    ' 2>&1 | tee "$log"
  return "${PIPESTATUS[0]}"
}

host_e2e() { # host_e2e <log> <e2e-only-spec>
  local log="$1" spec="$2"
  local -a runner=(env MAILCHAT_E2E_BINARY="$candidate_bin" MAILCHAT_E2E_ONLY="$spec")
  if command -v xvfb-run >/dev/null 2>&1; then
    runner+=(xvfb-run -a --server-args="-screen 0 1280x960x24")
  fi
  "${runner[@]}" npm run test:e2e 2>&1 | tee "$log"
  return "${PIPESTATUS[0]}"
}

run_e2e_spec() { # run_e2e_spec <item> <spec>
  local item="$1" spec="$2" log="$LOGS/e2e-$spec.log"
  echo "---- e2e spec '$spec' against the candidate binary (log: $log)"
  local rc=0
  if [[ "$HOST_ROUTE" -eq 1 ]]; then
    host_e2e "$log" "$spec" || rc=1
  else
    container_e2e "$log" "$spec" || rc=1
  fi
  item_status["$item"]="$([[ $rc -eq 0 ]] && echo PASS || echo FAIL)"
  [[ $rc -eq 0 ]] || overall=1
}

run_suite() { # run_suite <item> <test-name>
  local item="$1" test="$2" log="$LOGS/suite-$test.log"
  echo "---- cargo test -p vault-core --test $test (log: $log)"
  local rc=0
  if [[ "$HOST_ROUTE" -eq 1 ]]; then
    ( cd "$repo" && cargo test -p vault-core --test "$test" ) >"$log" 2>&1 || rc=1
  else
    docker run --rm \
      -u "${run_uid}:${run_gid}" \
      -e HOME=/tmp \
      -e CARGO_HOME=/cargo \
      -v "$repo":/src \
      -w /src \
      -v mailchat-build-cargo:/cargo \
      "$SUITE_IMAGE" \
      cargo test -p vault-core --test "$test" >"$log" 2>&1 || rc=1
  fi
  item_status["$item"]="$([[ $rc -eq 0 ]] && echo PASS || echo FAIL)"
  [[ $rc -eq 0 ]] || overall=1
}

leg_record() { # leg_record <leg> <item1> <item2> — PASS only if both items passed
  local leg="$1" a="$2" b="$3"
  if [[ "${item_status[$a]:-}" == "PASS" && "${item_status[$b]:-}" == "PASS" ]]; then
    record "$leg" "PASS"
  else
    record "$leg" "FAIL"
    overall=1
  fi
}

# ------------------------------------------------------------- leg 1: export

if [[ "$SKIP_EXPORT" -eq 1 ]]; then
  record "export" "SKIPPED (--skip-export)"
else
  echo
  echo "==== leg 1/3: export parity + cross-page selection (against the candidate)"
  run_e2e_spec "export: parity + all formats (spec 'export')" "export"
  run_e2e_spec "export: cross-page selection (spec 'search-v2-fixes')" "search-v2-fixes"
  leg_record "export" \
    "export: parity + all formats (spec 'export')" \
    "export: cross-page selection (spec 'search-v2-fixes')"
fi

# ------------------------------------------------------------ leg 2: restore

if [[ "$SKIP_RESTORE" -eq 1 ]]; then
  record "restore" "SKIPPED (--skip-restore)"
else
  echo
  echo "==== leg 2/3: backup/restore journey + prior recovery (against the candidate)"
  run_e2e_spec "restore: journey + prior recovery (spec 'vault-backup')" "vault-backup"
  run_e2e_spec "restore: restart recovery (spec 'vault-restart')" "vault-restart"
  leg_record "restore" \
    "restore: journey + prior recovery (spec 'vault-backup')" \
    "restore: restart recovery (spec 'vault-restart')"
fi

# ------------------------------------------------------------- leg 3: suites

if [[ "$SKIP_SUITES" -eq 1 ]]; then
  record "suites" "SKIPPED (--skip-suites)"
else
  echo
  echo "==== leg 3/3: M4.4 backup suite + export v2 suite at the candidate commit"
  run_suite "suites: backup (interrupt matrix included)" "backup"
  run_suite "suites: export_v2" "export_v2"
  leg_record "suites" \
    "suites: backup (interrupt matrix included)" \
    "suites: export_v2"
fi

# --------------------------------------------------------- evidence record

record_file="$LOGS/evidence-record.md"
date_iso="$(date -u +%Y-%m-%d)"
if [[ "$overall" -eq 0 ]]; then
  verdict="PASS — every requested leg green"
else
  verdict="FAIL — at least one leg failed (recorded honestly, not silently fixed)"
fi

evidence_lines="$(grep -h "\[evidence\]" "$LOGS"/e2e-*.log 2>/dev/null | sed 's/^ *//' || true)"
backup_ok="$(grep -c '^test .* \.\.\. ok' "$LOGS/suite-backup.log" 2>/dev/null || echo 0)"
exportv2_ok="$(grep -c '^test .* \.\.\. ok' "$LOGS/suite-export_v2.log" 2>/dev/null || echo 0)"
interrupt_lines="$(grep '^test interrupted' "$LOGS/suite-backup.log" 2>/dev/null || true)"

{
  echo "# Export & restore candidate proof — $date_iso (GP04)"
  echo
  echo "Status: **$verdict**. Produced by the one-command procedure \`scripts/release/verify-export-restore.sh\` (runbook: [validation and release](../../validation-and-release.md#export-and-restore-candidate-proof)). Commit \`$short\` ($branch), $(date -u +%Y-%m-%dT%H:%M:%SZ), route: $([[ "$HOST_ROUTE" -eq 1 ]] && echo "host (--binary)" || echo "docker (pinned containers)"). Per-leg logs and log digests live beside this record in the run's log directory."
  echo
  echo "## Artifact binding (SHA-256)"
  echo
  echo "| Artifact | SHA-256 |"
  echo "| --- | --- |"
  while IFS= read -r line; do
    echo "| \`${line##*/}\` | \`${line%%  *}\` |"
  done < "$LOGS/artifacts.txt"
  echo
  if [[ "$HOST_ROUTE" -eq 1 ]]; then
    echo "Candidate binary driven by the e2e legs: \`$candidate_bin\` (the digested artifact above)."
  else
    echo "Candidate binary driven by the e2e legs: the \`mailchat-vault\` payload extracted from the digested AppImage (\`--appimage-extract\`; extraction log \`appimage-extract.log\`)."
  fi
  echo
  echo "## Checks"
  echo
  echo "| Leg | Check | Result | Log |"
  echo "| --- | --- | --- | --- |"
  IFS='|' read -r leg item log <<< "export|export: parity + all formats (spec 'export')|e2e-export.log"
  echo "| $leg | $item | **${item_status[$item]:-SKIPPED}** | \`${log}\` |"
  IFS='|' read -r leg item log <<< "export|export: cross-page selection (spec 'search-v2-fixes')|e2e-search-v2-fixes.log"
  echo "| $leg | $item | **${item_status[$item]:-SKIPPED}** | \`${log}\` |"
  IFS='|' read -r leg item log <<< "restore|restore: journey + prior recovery (spec 'vault-backup')|e2e-vault-backup.log"
  echo "| $leg | $item | **${item_status[$item]:-SKIPPED}** | \`${log}\` |"
  IFS='|' read -r leg item log <<< "restore|restore: restart recovery (spec 'vault-restart')|e2e-vault-restart.log"
  echo "| $leg | $item | **${item_status[$item]:-SKIPPED}** | \`${log}\` |"
  IFS='|' read -r leg item log <<< "suites|suites: backup (interrupt matrix included)|suite-backup.log"
  echo "| $leg | $item | **${item_status[$item]:-SKIPPED}** | \`${log}\` |"
  IFS='|' read -r leg item log <<< "suites|suites: export_v2|suite-export_v2.log"
  echo "| $leg | $item | **${item_status[$item]:-SKIPPED}** | \`${log}\` |"
  echo
  echo "## Counts and digests emitted by the run"
  echo
  echo '```'
  if [[ -n "$evidence_lines" ]]; then
    printf '%s\n' "$evidence_lines"
  else
    echo "(no [evidence] lines captured in the e2e logs)"
  fi
  echo '```'
  echo
  echo "Suite detail — backup suite at the candidate commit: ${backup_ok} tests ok; the interrupted-switch matrix items:"
  echo
  echo '```'
  if [[ -n "$interrupt_lines" ]]; then
    printf '%s\n' "$interrupt_lines"
  else
    echo "(no interrupted* test lines found in suite-backup.log)"
  fi
  echo '```'
  echo
  echo "Suite detail — export v2 suite at the candidate commit: ${exportv2_ok} tests ok."
  echo
  echo "## Log digests (per run)"
  echo
  echo "| Log | SHA-256 |"
  echo "| --- | --- |"
  for f in "$LOGS"/*.log "$LOGS/artifacts.txt"; do
    [[ -f "$f" ]] || continue
    echo "| \`${f##*/}\` | \`$(digest_of "$f")\` |"
  done
  echo
  echo "## Honest-status rule"
  echo
  echo "The [support matrix](../../support-matrix.md) backup/restore and export rows flip to verified-on-candidate **only when this record's verdict is PASS**; a failing leg is a defect record for a separate fix decision, never a silent fix inside a proof run."
} > "$record_file"
echo
echo "==== evidence record: $record_file"

if [[ "$EVIDENCE_COPY" -eq 1 ]]; then
  target="$repo/docs/evidence/export-restore-candidate-$date_iso.md"
  n=1
  while [[ -e "$target" ]]; do
    n=$((n+1))
    target="$repo/docs/evidence/export-restore-candidate-$date_iso-$n.md"
  done
  cp "$record_file" "$target"
  echo "   committed copy: $target (commit it with the run)"
fi

# ----------------------------------------------------------------- summary

echo
echo "==================== export/restore proof summary ===================="
printf '%s\n' "${summary[@]}"
for key in "${!item_status[@]}"; do
  printf '   %-55s %s\n' "$key" "${item_status[$key]}"
done
echo "logs:    $LOGS"
echo "record:  $record_file"
if [[ "$overall" -eq 0 ]]; then
  echo "RESULT: PASS — candidate-bound export/restore evidence green"
else
  echo "RESULT: FAIL — at least one leg failed; recorded honestly above"
fi
echo "======================================================================="
exit "$overall"
