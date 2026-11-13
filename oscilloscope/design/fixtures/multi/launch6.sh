#!/bin/sh
# launch6.sh: six orchestrations at once, four on cof and two on electricity, snapshotted 2.8 s after launch.
# Produces run6-<key>/ (full runs), run6-<key>.log, snap6/<key>/ (the moment) and snap6/manifest.json
# (what osi knew at launch: document, config, run directory; this stands in for the proposed run registry).
cd "$(dirname "$0")" || exit 1
rm -rf run6-* snap6; mkdir -p snap6
start() { key=$1; doc=$2; cfg=$3; engine=$4
  "$HOME/.local/bin/osi" "$doc" "$cfg" --engine "$engine" --out-dir "run6-$key" --no-history --log > "run6-$key.log" 2>&1 &
  printf '  {"key": "%s", "orchestration": "%s", "config": "%s", "engine": "%s", "run_dir": "run6-%s", "pid": %s}' "$key" "$doc" "$cfg" "$engine" "$key" "$!" >> snap6/manifest.part
  printf ',\n' >> snap6/manifest.part
}
: > snap6/manifest.part
start triage    triage.yml    config-triage.json    cof
start triage-b  triage.yml    config-triage-b.json  cof
start digest    digest.yml    config-digest.json    cof
start review    review.yml    config-review.json    cof
start summarize summarize.yml config-summarize.json electricity
start classify  classify.yml  config-classify.json  electricity
sleep 2.8
for k in triage triage-b digest review summarize classify; do cp -R "run6-$k" "snap6/$k"; done
perl -MTime::HiRes=time -e 'printf "%.3f\n", time' > snap6/taken-at
{ printf '[\n'; sed '$ s/,$//' snap6/manifest.part; printf ']\n'; } > snap6/manifest.json; rm snap6/manifest.part
wait
echo "done: $(ls -d run6-*/ | wc -l | tr -d ' ') runs, snapshot at $(cat snap6/taken-at)"
