#!/usr/bin/env bash
# ci-run.sh LABEL COMMAND [ARG...]
# Runs the command with its output logged to ci-logs/LABEL.log. When it fails, the end of the log is
# emitted as GitHub `::error::` annotations (readable without a login at
# https://api.github.com/repos/<owner>/<repo>/check-runs/<job id>/annotations), because the raw job
# logs of a public repository need an account to download.
label="$1"; shift
dir="${CI_LOG_DIR:-ci-logs}"
mkdir -p "$dir"
log="$dir/$label.log"
set -o pipefail
"$@" 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
if [ "$rc" -ne 0 ]; then
  esc() { sed -e 's/%/%25/g' -e 's/\r//g' | cut -c1-240 | awk 'BEGIN{ORS="%0A"} {print}'; }
  errs="$(grep -n -E 'error|Error|FAILED|undefined reference|fatal|cannot find|No such file|not found' "$log" | grep -v -E 'Werror|-Wno-error|error_code|\.error\(|error\.cpp|error\.h|error_categor' | tail -n 14 | esc)"
  tail="$(tail -n 30 "$log" | esc)"
  [ -n "$errs" ] && echo "::error title=$label failed ($rc): matching lines::$errs"
  echo "::error title=$label failed ($rc): end of log::$tail"
fi
if [ "$rc" -eq 0 ] && [ -n "${CI_NOTICE:-}" ]; then   # also show the end of a good log (job logs need a login)
  n="$(tail -n "${CI_NOTICE_LINES:-45}" "$log" | sed -e 's/%/%25/g' -e 's/\r//g' | cut -c1-200 | awk 'BEGIN{ORS="%0A"} {print}')"
  echo "::notice title=$label (ok)::$n"
fi
exit "$rc"
