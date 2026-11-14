#!/usr/bin/env bash
# Tests of deploy/install.sh that install nothing and change nothing on the machine that runs them: its syntax
# (bash -n), shellcheck when it is installed, and dry runs (--dry-run) of every role, mode and preset, of answers it
# must refuse, of answers fed to its questions, and of a file it would replace.
#
#   bash deploy/test-install.sh          (Linux, WSL, Git Bash, macOS with bash 4 or newer; no root, no Docker)

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/install.sh"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/home"
PASSED=0 FAILED=0

ok() { PASSED=$((PASSED + 1)); }
ko() { FAILED=$((FAILED + 1)); printf 'FAIL: %s\n' "$1" >&2; [ ! -f "$T/out" ] || sed 's/^/    /' "$T/out" | tail -n 25 >&2; }
expect() { if eval "$2"; then ok; else ko "$1"; fi; }

# dry EXIT ARGS...: a dry run, its output in $T/out, its exit status compared.
dry() {
  local want="$1" got=0
  shift
  HOME="$T/home" LANG=C bash "$SCRIPT" --dry-run "$@" >"$T/out" 2>&1 </dev/null || got=$?
  if [ "$want" = 0 ]; then [ "$got" = 0 ]; else [ "$got" != 0 ]; fi
}
has() { grep -qF -- "$1" "$T/out"; }
lacks() { ! grep -qF -- "$1" "$T/out"; }
nothing_written() { [ ! -e "$T/inst" ] && [ -z "$(ls -A "$T/home")" ]; }

SERVER=(--yes --role server --name 'The "Thursday" \ club' --host play.example.org --operator Camille --contact camille@example.org --dir "$T/inst")

# ---- syntax ----------------------------------------------------------------------------------------------------------
expect "bash -n" "bash -n '$SCRIPT'"
expect "bash -n of this file" "bash -n '$HERE/test-install.sh'"
if command -v shellcheck >/dev/null 2>&1; then
  expect "shellcheck" "shellcheck -S warning '$SCRIPT' '$HERE/test-install.sh'"
else
  printf 'shellcheck is not installed: skipped\n'
fi

# ---- every role, mode and preset: a dry run writes nothing ---------------------------------------------------------------
for mode in docker systemd files; do
  for preset in friends competitive tournament; do
    for https in caddy none; do
      [ "$mode" = docker ] || [ "$https" = none ] || continue
      name="server $mode $preset https=$https"
      if dry 0 "${SERVER[@]}" --mode "$mode" --preset "$preset" --https "$https"; then ok; else ko "$name: exit"; fi
      expect "$name: says it is a dry run" "has 'DRY RUN'"
      expect "$name: writes nothing" nothing_written
      expect "$name: lists what it would write" "has 'WRITTEN BY THIS INSTALL'"
      expect "$name: says how to uninstall" "has 'UNINSTALL'"
      expect "$name: no %d specifier (systemd 248)" "lacks '%d/'"
      expect "$name: no Docker documentation host" "lacks 'docs.docker.com'"
      expect "$name: never a relay secret in mode 644" "! grep -E 'relay\.key.*644|644.*relay\.key' '$T/out'"
      if [ "$preset" = friends ]; then
        expect "$name: the relay inside the service" "has 'inside the service, UDP 47313'"
        expect "$name: no relay of its own" "lacks 'kalamar-relay.service' && lacks '  relay:' && lacks 'kalamar-relay --'"
      else
        expect "$name: a relay of its own, bound by its secret" "has '--room-secret-file'"
      fi
      case "$mode" in
        systemd)
          expect "$name: init leaves the relay secret to the installer" "has '--no-relay-secret'"
          expect "$name: the account kalamar, not a dynamic one" "has 'User=kalamar' && lacks '| DynamicUser=yes'"
          expect "$name: /etc/systemd/system in the list" "has '  /etc/systemd/system/kalamar-service.service'"
          expect "$name: /srv/kalamar-site in the list" "has '  /srv/kalamar-site'"
          expect "$name: reload with admin reload" "has 'admin reload' && has 'reports nothing'"
          [ "$preset" = friends ] || expect "$name: the relay's secret 0600" "has 'umask\ 077'"
          if [ "$preset" != friends ]; then
            expect "$name: the relay runs as its own account, in the service's group" "has 'User=kalamar-relay' && has 'useradd --system --gid kalamar'"
            expect "$name: the relay's group reads its secret, nobody else" "has 'chmod 640 /var/lib/kalamar/relay.key'"
            expect "$name: no systemd credential in the written unit" "lacks '| LoadCredential'"
          fi
          ;;
        docker)
          expect "$name: compose by -f" "has 'docker compose -f'"
          expect "$name: init leaves the relay secret to the installer" "has '--no-relay-secret'"
          [ "$preset" = friends ] || expect "$name: the relay image is never pulled" "has 'pull_policy: never'"
          [ "$https" = none ] || expect "$name: says the Caddy image is a tag" "has 'caddy:2'"
          ;;
        files)
          [ "$preset" = friends ] || expect "$name: the relay's secret 0600" "has 'umask\ 077'"
          ;;
      esac
    done
  done
done

for mode in docker systemd files; do
  name="relay $mode with a new secret"
  if dry 0 --yes --role relay --mode "$mode" --relay-port 47400 --dir "$T/inst"; then ok; else ko "$name: exit"; fi
  expect "$name: writes nothing" nothing_written
  expect "$name: makes its own secret, 0600" "has 'umask\ 077'"
  expect "$name: its port" "has '47400'"
  [ "$mode" != docker ] || expect "$name: runs as the secret's owner" "has 'user: \"$(id -u):$(id -g)\"'"
  key="$T/given.key"
  (umask 077 && printf '%064d\n' 0 >"$key")
  name="relay $mode with a given secret"
  if dry 0 --yes --role relay --mode "$mode" --relay-key "$key" --dir "$T/inst"; then ok; else ko "$name: exit"; fi
  expect "$name: copied in mode 600" "has 'install -m 600'"
  expect "$name: never 644" "! grep -E 'relay\.key.*644|644.*relay\.key|chmod 644' '$T/out'"
done

# ---- answers it refuses (before anything is shown as done) ---------------------------------------------------------------
refuse() {  # refuse NAME ARGS...
  local name="$1"
  shift
  if dry 1 "$@"; then ok; else ko "$name: accepted"; fi
  expect "$name: writes nothing" nothing_written
  expect "$name: does nothing" "lacks '  \$ '"
}
for port in 0 65536 '47313; rm -rf /' 1e3 ' 47313' '$(id)' -1; do
  refuse "relay port [$port]" --yes --role relay --mode files --relay-port "$port" --dir "$T/inst"
done
refuse "a server's relay port" "${SERVER[@]}" --mode files --preset friends --relay-port 47400
for host in 'a b.example.org' 'https://play.example.org' 'play.example.org/api' '-play.example.org' 'play.example.org:0' "play.example.org'" 'play.example.org;id'; do
  refuse "host [$host]" --yes --role server --mode files --preset friends --name N --host "$host" --operator O --contact mailto:o@example.org --dir "$T/inst"
done
refuse "a name over two lines" --yes --role server --mode files --preset friends --name $'two\nlines' --host play.example.org --operator O --contact mailto:o@example.org --dir "$T/inst"
for contact in 'javascript:alert(1)' 'mailto:a b@example.org' 'https://x.example.org/"quote' 'mailto:$(id)@example.org'; do
  refuse "contact [$contact]" --yes --role server --mode files --preset friends --name N --host play.example.org --operator O --contact "$contact" --dir "$T/inst"
done
for dir in "$T/in\"st" "$T/\$(id)" "$T/a\`b"; do
  refuse "folder [$dir]" --yes --role relay --mode files --dir "$dir"
done
refuse "an unknown role" --yes --role everything --mode files --dir "$T/inst"
refuse "an unknown option" --yes --role relay --mode files --frobnicate
refuse "an option without its value" --yes --role relay --mode files --dir
refuse "a missing host" --yes --role server --mode files --preset friends --operator O --contact mailto:o@example.org --dir "$T/inst"
printf 'not a secret\n' >"$T/bad.key"
refuse "a file that is not a secret" --yes --role relay --mode files --relay-key "$T/bad.key" --dir "$T/inst"

# ---- the questions, answered on a pipe ---------------------------------------------------------------------------------------
# role 1 (server), mode 3 (files), preset 2 (competitive), the default name, a host refused then a good one, an operator,
# a bare e-mail address (made a mailto: link), the folder, and yes.
printf '1\n3\n2\n\nnot a host\nplay.example.org\nCamille\ncamille@example.org\n%s\ny\n' "$T/inst" |
  HOME="$T/home" LANG=C KALAMAR_INSTALL_ASK=1 bash "$SCRIPT" --dry-run >"$T/out" 2>&1 && ok || ko "questions: exit"
expect "questions: a bad answer is asked again" "has '(not a valid answer)'"
expect "questions: the address becomes a link" "has 'mailto:camille@example.org'"
expect "questions: the default name" "has 'My community'"
expect "questions: writes nothing" nothing_written
# Role 2 (a relay), mode 3 (files), a new secret, the default port, the folder, and "n" at the summary: nothing.
printf '2\n3\n\n\n%s\nn\n' "$T/inst" | HOME="$T/home" LANG=C KALAMAR_INSTALL_ASK=1 bash "$SCRIPT" --dry-run >"$T/out" 2>&1 && ok || ko "no at the summary: exit"
expect "no at the summary: nothing done" "has 'Nothing was installed.' && lacks '  \$ '"

# ---- a file it would replace: a dated copy first -----------------------------------------------------------------------------
mkdir -p "$T/inst"
printf 'old\n' >"$T/inst/Caddyfile"
if dry 0 "${SERVER[@]}" --mode files --preset friends; then ok; else ko "replace: exit"; fi
expect "replace: the previous Caddyfile is copied aside" "grep -qE 'cp -p .*/Caddyfile .*/Caddyfile\.bak-[0-9]{8}-[0-9]{6}' '$T/out'"
expect "replace: and listed" "has 'Copies of the files replaced'"
expect "replace: the file itself is untouched" "[ \"\$(cat '$T/inst/Caddyfile')\" = old ] && [ \"\$(ls -A '$T/inst')\" = Caddyfile ]"
rm -r "$T/inst"

printf '%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" = 0 ]
