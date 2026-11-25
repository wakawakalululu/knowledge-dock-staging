#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
HELPER="$ROOT/.claude/skills/rust-ci-dev-loop/scripts/loop_json.js"
REPO="${LOOP_REPO:-wakawakalululu/knowledge-dock-staging}"
WORKFLOW="${LOOP_WORKFLOW:-build-loop.yml}"
BRANCH="${LOOP_BRANCH:-staging}"
ARTIFACT="${LOOP_ARTIFACT:-knowledge-dock-windows-loop}"
API="https://api.github.com/repos/$REPO"
if [[ -z "${NODE_BIN:-}" ]]; then
  if [[ -x "/mnt/c/Program Files/nodejs/node.exe" ]]; then
    NODE_BIN="/mnt/c/Program Files/nodejs/node.exe"
  elif [[ -x "/c/Program Files/nodejs/node.exe" ]]; then
    NODE_BIN="/c/Program Files/nodejs/node.exe"
  else
    NODE_BIN="$(command -v node || true)"
  fi
fi
if [[ -z "${GIT_BIN:-}" ]]; then
  if [[ -x "/mnt/c/Program Files/Git/cmd/git.exe" ]]; then
    GIT_BIN="/mnt/c/Program Files/Git/cmd/git.exe"
  elif [[ -x "/c/Program Files/Git/cmd/git.exe" ]]; then
    GIT_BIN="/c/Program Files/Git/cmd/git.exe"
  else
    GIT_BIN="$(command -v git || true)"
  fi
fi
if [[ -z "${POWERSHELL_BIN:-}" ]]; then
  if [[ -x "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe" ]]; then
    POWERSHELL_BIN="/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"
  elif [[ -x "/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe" ]]; then
    POWERSHELL_BIN="/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"
  else
    POWERSHELL_BIN="$(command -v powershell.exe || true)"
  fi
fi
if [[ -z "${MSYS_BASH:-}" ]]; then
  if [[ -x "/mnt/c/msys64/usr/bin/bash.exe" ]]; then
    MSYS_BASH="/mnt/c/msys64/usr/bin/bash.exe"
  elif [[ -x "/c/msys64/usr/bin/bash.exe" ]]; then
    MSYS_BASH="/c/msys64/usr/bin/bash.exe"
  else
    MSYS_BASH="$(command -v bash || true)"
  fi
fi
TMP_DIR=""

cleanup() {
  if [[ -n "$TMP_DIR" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

fail() {
  echo "LOOP_ERROR $*" >&2
  exit 1
}

win_path() {
  if command -v wslpath >/dev/null 2>&1; then
    wslpath -w "$1"
  elif command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}

# Windows Node cannot reliably write to a WSL pipe (EISDIR). Capture through a
# regular file, then let Bash read and forward the result.
run_node() {
  local output status contents
  output="$(mktemp)"
  if "$NODE_BIN" "$@" >"$output" 2>&1; then
    status=0
  else
    status=$?
  fi
  IFS= read -r -d '' contents < "$output" || true
  rm -f "$output"
  if [[ -n "$contents" ]]; then
    printf '%s\n' "$contents"
  fi
  return "$status"
}

require_tools() {
  [[ -x "$NODE_BIN" ]] || fail "Windows Node not found: $NODE_BIN"
  [[ -x "$GIT_BIN" ]] || fail "Windows Git not found: $GIT_BIN"
  [[ -f "$HELPER" ]] || fail "loop_json.js missing"
  command -v curl >/dev/null 2>&1 || fail "curl not found"
}

get_token() {
  if [[ -n "${GH_TOKEN:-}" ]]; then
    TOKEN="$GH_TOKEN"
  elif [[ -n "${GITHUB_TOKEN:-}" ]]; then
    TOKEN="$GITHUB_TOKEN"
  elif [[ -x "$POWERSHELL_BIN" ]]; then
    TOKEN="$("$POWERSHELL_BIN" -NoProfile -Command '[Environment]::GetEnvironmentVariable("GH_TOKEN","User")' 2>/dev/null | tr -d '\r\n')"
  else
    TOKEN=""
  fi
  [[ -n "$TOKEN" ]] || fail "GH_TOKEN is not set in the process or Windows user environment"
}

api_get() {
  local url="$1"
  local output="$2"
  get_token
  curl -fsSL \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' \
    "$url" -o "$output"
}

precheck() {
  require_tools
  cd "$ROOT"

  [[ -f node_modules/@tauri-apps/cli/tauri.js ]] || fail "run npm ci before precheck"
  [[ -f src-tauri/icons/icon.ico ]] || fail "generate src-tauri/icons/icon.ico before precheck"

  run_node "$(win_path "$ROOT/tools/check-versions.js")"
  run_node --check "$(win_path "$ROOT/ui/app.js")"
  run_node "$(win_path "$ROOT/tools/selection-ui-test.js")"
  run_node "$(win_path "$ROOT/tools/selection-ui-test.js")" once
  run_node "$(win_path "$ROOT/tools/selection-ui-test.js")" always

  if [[ -x "$MSYS_BASH" ]]; then
    local msys_root
    if [[ "$ROOT" == /mnt/c/* ]]; then
      msys_root="/c${ROOT#/mnt/c}"
    else
      msys_root="$ROOT"
    fi
    if [[ "$msys_root" == /c/* ]]; then
      local quoted_manifest_dir
      printf -v quoted_manifest_dir '%q' "$msys_root/src-tauri"
      "$MSYS_BASH" -lc \
        "export PATH=/c/msys64/ucrt64/bin:/c/Users/Admin/.cargo/bin:\$PATH; cd $quoted_manifest_dir; cargo check --locked"
    else
      (cd "$ROOT/src-tauri" && cargo check --locked)
    fi
  else
    (cd "$ROOT/src-tauri" && cargo check --locked)
  fi

  local sha
  sha="$(git_sha)"
  echo "PRECHECK_OK $sha"
}

git_sha() {
  "$GIT_BIN" -C "$(win_path "$ROOT")" rev-parse HEAD
}

watch() {
  require_tools
  TMP_DIR="$(mktemp -d)"
  local sha
  sha="$(git_sha)"
  local timeout_seconds="${LOOP_WATCH_TIMEOUT:-1800}"
  local deadline=$((SECONDS + timeout_seconds))
  local runs_file="$TMP_DIR/runs.json"

  echo "WATCH repo=$REPO workflow=$WORKFLOW branch=$BRANCH sha=$sha"
  while (( SECONDS < deadline )); do
    if ! api_get "$API/actions/workflows/$WORKFLOW/runs?branch=$BRANCH&per_page=30" "$runs_file"; then
      echo "WAIT GitHub Actions API unavailable; retrying"
      sleep 15
      continue
    fi

    local run_line=""
    if run_line="$(run_node "$(win_path "$HELPER")" find-run "$(win_path "$runs_file")" "$sha")"; then
      local id status conclusion url head
      IFS=$'\t' read -r id status conclusion url head <<< "$run_line"
      echo "RUN id=$id status=$status conclusion=${conclusion:-pending} url=$url"
      if [[ "$status" == "completed" ]]; then
        if [[ "$conclusion" == "success" ]]; then
          echo "CI_SUCCESS id=$id sha=$head url=$url"
          return 0
        fi

        echo "CI_FAILED id=$id conclusion=$conclusion url=$url"
        api_get "$API/actions/runs/$id/jobs" "$TMP_DIR/jobs.json" || true
        run_node "$(win_path "$HELPER")" failed-jobs "$(win_path "$TMP_DIR/jobs.json")" || true

        local logs_zip="$TMP_DIR/logs.zip"
        local logs_dir="$TMP_DIR/logs"
        mkdir -p "$logs_dir"
        if api_get "$API/actions/runs/$id/logs" "$logs_zip"; then
          "$POWERSHELL_BIN" -NoProfile -Command \
            "Expand-Archive -LiteralPath '$(win_path "$logs_zip")' -DestinationPath '$(win_path "$logs_dir")' -Force" >/dev/null
          run_node "$(win_path "$HELPER")" filter-log "$(win_path "$logs_dir")" 120 || true
        else
          echo "LOG_FETCH_FAILED inspect $url in GitHub UI"
        fi
        return 1
      fi
    else
      echo "WAIT no run yet for sha=$sha"
    fi
    sleep 15
  done

  fail "watch timed out after ${timeout_seconds}s"
}

fetch_run() {
  [[ $# -ge 3 ]] || fail "usage: loop_driver.sh fetch_run <artifact> <binary> <timeout-seconds> [args...]"
  require_tools
  TMP_DIR="$(mktemp -d)"

  local artifact_name="$1"
  local binary_name="$2"
  local timeout_seconds="$3"
  shift 3
  local sha
  sha="$(git_sha)"
  local runs_file="$TMP_DIR/runs.json"
  api_get "$API/actions/workflows/$WORKFLOW/runs?branch=$BRANCH&per_page=30" "$runs_file"

  local run_line
  run_line="$(run_node "$(win_path "$HELPER")" find-run "$(win_path "$runs_file")" "$sha" success)" \
    || fail "no successful run for sha=$sha"
  local run_id status conclusion url head
  IFS=$'\t' read -r run_id status conclusion url head <<< "$run_line"

  local artifacts_file="$TMP_DIR/artifacts.json"
  api_get "$API/actions/runs/$run_id/artifacts" "$artifacts_file"
  local artifact_line
  artifact_line="$(run_node "$(win_path "$HELPER")" artifact "$(win_path "$artifacts_file")" "$artifact_name")" \
    || fail "artifact $artifact_name not found in run $run_id"
  local artifact_id artifact_actual artifact_size
  IFS=$'\t' read -r artifact_id artifact_actual artifact_size <<< "$artifact_line"

  local artifact_root="$ROOT/.local-artifacts/$sha"
  local extract_dir="$artifact_root/extracted"
  local zip_file="$artifact_root/$artifact_name.zip"
  mkdir -p "$artifact_root" "$extract_dir" "$ROOT/.local-logs"
  api_get "$API/actions/artifacts/$artifact_id/zip" "$zip_file"
  "$POWERSHELL_BIN" -NoProfile -Command \
    "Expand-Archive -LiteralPath '$(win_path "$zip_file")' -DestinationPath '$(win_path "$extract_dir")' -Force" >/dev/null

  local binary
  binary="$(find "$extract_dir" -type f -iname "$binary_name" -print -quit)"
  [[ -n "$binary" ]] || fail "binary $binary_name not found in artifact"

  local timestamp
  timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
  local log_file="$ROOT/.local-logs/run_${timestamp}_${sha:0:7}.log"
  local started=$SECONDS
  local code=0
  set +e
  RUST_LOG=debug RUST_BACKTRACE=1 timeout --signal=TERM --kill-after=5 "$timeout_seconds" "$binary" "$@" >"$log_file" 2>&1
  code=$?
  set -e
  local elapsed=$((SECONDS - started))

  echo "RUN_RESULT exit_code=$code elapsed=${elapsed}s timeout=${timeout_seconds}s log=$(win_path "$log_file")"
  echo "RUN_ERROR_SUMMARY"
  run_node "$(win_path "$HELPER")" filter-file "$(win_path "$log_file")" 40 || true
  echo "RUN_LOG_TAIL"
  run_node "$(win_path "$HELPER")" tail-log "$(win_path "$log_file")" 40 || true
  return "$code"
}

case "${1:-}" in
  precheck)
    shift
    precheck "$@"
    ;;
  watch)
    shift
    watch "$@"
    ;;
  fetch_run)
    shift
    fetch_run "$@"
    ;;
  *)
    echo "usage: loop_driver.sh {precheck|watch|fetch_run ...}" >&2
    exit 2
    ;;
esac
