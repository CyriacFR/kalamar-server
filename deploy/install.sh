#!/usr/bin/env bash
# Kalamar server installer: asks what to install and how, installs it, and prints where everything is.
#
#   ./deploy/install.sh                 answer the questions
#   ./deploy/install.sh --help          every question as an option (for scripts: add --yes)
#   ./deploy/install.sh --dry-run ...   show everything it would do, write nothing (deploy/test-install.sh uses it)
#
# What:  a whole server (the HTTP service and its relay), or one more relay for a server you already run.
# How:   Docker (docker compose), systemd (built with cargo, needs sudo), or only the files (nothing is started).
# What it downloads: Docker mode pulls base images (rust, debian, caddy; by tag) and cargo crates; systemd mode, cargo
# crates; files mode, nothing. It never changes a firewall: it tells you which ports to open. It replaces no binary,
# unit or Caddyfile without a dated copy (`<file>.bak-<date>`), and restarts a running service only after you confirm.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROLE="" MODE="" PRESET="" NAME="" HOST="" OPERATOR="" CONTACT="" DIR="" HTTPS="" RELAY_KEY="" RELAY_PORT=""
YES=0 DRY_RUN=0 UI="" BIN="${KALAMAR_BIN:-}"
# The Caddy image of Docker mode. A tag moves: pin it by digest for a reproducible install
# (KALAMAR_CADDY_IMAGE=caddy:2@sha256:<digest>, from `docker buildx imagetools inspect caddy:2`).
CADDY_IMAGE="${KALAMAR_CADDY_IMAGE:-caddy:2}"
STAMP="$(date +%Y%m%d-%H%M%S)"
SYSTEM_USER="kalamar"
RELAY_USER="kalamar-relay"  # systemd mode: the relay faces the internet; it runs apart from the service
WRITTEN=()   # every place this run wrote, for the summary and the uninstall steps
BACKUPS=()   # files copied aside before being replaced
UNINSTALL=() # the commands that undo this install

usage() {
  cat <<'EOF'
Usage: deploy/install.sh [options]      (questions are asked for whatever is not given)

  --role server|relay        a whole server (service + relay), or one more relay for an existing server
  --mode docker|systemd|files
                             docker: docker compose, in a folder of yours      (needs Docker)
                             systemd: built with cargo, services of the system (needs cargo, sudo, systemd 240+)
                             files: only write the files, start nothing        (needs a built kalamar-service)
  --preset friends|competitive|tournament
                             the starting configuration of a server (friends: the relay inside the service;
                             competitive, tournament: the relay as a program of its own)
  --name "My community"      the server's name, shown in launchers (1 to 64 characters)
  --host play.example.org    the public host name players type (HTTPS); for a relay: unused
  --operator "Camille"       your name or your team's, shown to players
  --contact mailto:you@example.org
  --https caddy|none         caddy: Caddy gets the certificate by itself (the name must point to this machine)
                             none: you put your own HTTPS proxy in front of 127.0.0.1:8080
  --relay-key FILE           role relay: use this secret (else a new one is made, to copy to the server)
  --relay-port N             role relay: UDP port, 1 to 65535 (default 47313)
  --dir DIR                  docker and files: where the files go (default ~/kalamar)
  --lang en|fr               language of the questions
  --yes                      ask nothing: fail if something needed is missing; confirms the summary and restarts
  --dry-run                  print every command and file it would write, change nothing

The relay admits at most 8 members of one room from one address (an IPv4 address, an IPv6 /64): a whole 4 v 4
behind one box. A larger LAN party behind the same box: after the install, raise it with --max-members-per-source 12
on the kalamar-relay line of its unit or compose file, or max_members_per_source = 12 in [embedded_relay] (preset
friends), then restart.
EOF
}

die_usage() { printf '%s\n' "$1" >&2; usage >&2; exit 2; }
need_value() { [ $# -ge 2 ] && [ -n "$2" ] || die_usage "option $1 needs a value"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --role | --mode | --preset | --name | --host | --operator | --contact | --https | --relay-key | --relay-port | --dir | --lang)
      need_value "$@"
      case "$1" in
        --role) ROLE="$2" ;; --mode) MODE="$2" ;; --preset) PRESET="$2" ;; --name) NAME="$2" ;;
        --host) HOST="$2" ;; --operator) OPERATOR="$2" ;; --contact) CONTACT="$2" ;; --https) HTTPS="$2" ;;
        --relay-key) RELAY_KEY="$2" ;; --relay-port) RELAY_PORT="$2" ;; --dir) DIR="$2" ;; --lang) UI="$2" ;;
      esac
      shift 2 ;;
    --yes | -y) YES=1; shift ;;
    --dry-run | -n) DRY_RUN=1; shift ;;
    --help | -h) usage; exit 0 ;;
    *) die_usage "unknown option: $1" ;;
  esac
done

# ---- small tools ----------------------------------------------------------------------------------------------------

if [ -z "$UI" ]; then
  case "${LC_ALL:-${LANG:-}}" in fr*) UI=fr ;; *) UI=en ;; esac
fi
case "$UI" in en | fr) ;; *) die_usage "--lang: en or fr" ;; esac
# t "English text" "French text": the text in the language of --lang
t() { if [ "$UI" = fr ]; then printf '%s' "$2"; else printf '%s' "$1"; fi; }
say() { printf '%s\n' "$(t "$1" "$2")"; }
die() { printf '\n%s\n' "$(t "$1" "${2:-$1}")" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
INTERACTIVE=0
# Questions are asked on a terminal; KALAMAR_INSTALL_ASK=1 asks them on a pipe too (deploy/test-install.sh feeds the
# answers that way).
if [ "$YES" = 0 ] && { [ -t 0 ] || [ -n "${KALAMAR_INSTALL_ASK:-}" ]; }; then INTERACTIVE=1; fi

# ---- what an answer may be (checked before any use: nothing unchecked reaches a file, a unit or a command) -------------

is_port() { [[ "$1" =~ ^[0-9]{1,5}$ ]] && (( 10#$1 >= 1 && 10#$1 <= 65535 )); }
is_host() {   # a DNS name or an IPv4 address, an optional :port; no scheme, no path, no space
  local h="${1%:*}"
  [ "$h" != "$1" ] && { is_port "${1##*:}" || return 1; }
  [ ${#h} -ge 1 ] && [ ${#h} -le 253 ] &&
    [[ "$h" =~ ^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?)*$ ]]
}
is_text() {   # a name shown to players: 1 to 64 characters, no control character
  [ -n "$1" ] && (( $(printf '%s' "$1" | wc -m) <= 64 )) && ! [[ "$1" =~ [[:cntrl:]] ]]
}
# Characters no path or link of this installer may hold: they would end a YAML string, start a substitution in a
# compose file or a shell, or break a unit line.
UNSAFE='[[:cntrl:][:space:]"\\`$'"'"']'
UNSAFE_PATH='[[:cntrl:]"\\`$'"'"']'
is_contact() {
  [ ${#1} -le 200 ] && [[ "$1" =~ ^(mailto:|https://).+ ]] && ! [[ "$1" =~ $UNSAFE ]]
}
is_contact_answer() { is_contact "$1" || { [[ "$1" == *@* ]] && is_contact "mailto:$1"; }; }
is_path() {   # absolute, and nothing a YAML string, a unit or a shell word would read otherwise
  [ ${#1} -le 1024 ] && [[ "$1" =~ ^(/|[A-Za-z]:) ]] && ! [[ "$1" =~ $UNSAFE_PATH ]]
}
is_path_answer() { [ "$1" = "-" ] || { [ ${#1} -le 1024 ] && ! [[ "$1" =~ $UNSAFE_PATH ]]; }; }
check() {     # check VALUE PREDICATE "message en" "message fr"
  "$2" "$1" || die "$3" "$4"
}

# ask VAR PREDICATE "question en" "question fr" "default"
ask() {
  local var="$1" pred="$2" cur="${!1}" def="$5" answer
  if [ -n "$cur" ]; then return; fi
  if [ "$INTERACTIVE" = 0 ]; then
    [ -n "$def" ] || die "missing: --$(printf '%s' "$var" | tr 'A-Z_' 'a-z-')"
    printf -v "$var" '%s' "$def"
    return
  fi
  while :; do
    if [ -n "$def" ]; then printf '%s [%s]: ' "$(t "$3" "$4")" "$def"; else printf '%s: ' "$(t "$3" "$4")"; fi
    IFS= read -r answer || die "no answer" "pas de réponse"
    answer="${answer:-$def}"
    if [ -n "$answer" ] && "$pred" "$answer"; then printf -v "$var" '%s' "$answer"; return; fi
    say "  (not a valid answer)" "  (réponse non valable)"
  done
}

# choose VAR "question en" "question fr" "value|english label|french label" ...   (the first one is the default)
choose() {
  local var="$1" cur="${!1}" qe="$2" qf="$3" i answer v
  shift 3
  local values=()
  for i in "$@"; do values+=("${i%%|*}"); done
  if [ -n "$cur" ]; then
    for v in "${values[@]}"; do [ "$v" = "$cur" ] && return; done
    die "--$(printf '%s' "$var" | tr 'A-Z_' 'a-z-'): ${values[*]}"
  fi
  if [ "$INTERACTIVE" = 0 ]; then printf -v "$var" '%s' "${values[0]}"; return; fi
  printf '\n%s\n' "$(t "$qe" "$qf")"
  i=1
  for v in "$@"; do
    local rest="${v#*|}"
    printf '  %d) %s\n' "$i" "$(t "${rest%%|*}" "${rest#*|}")"
    i=$((i + 1))
  done
  while :; do
    printf '%s [1]: ' "$(t "Your choice" "Votre choix")"
    IFS= read -r answer || die "no answer" "pas de réponse"
    answer="${answer:-1}"
    case "$answer" in
      '' | *[!0-9]*) ;;
      *) if [ "$answer" -ge 1 ] && [ "$answer" -le "${#values[@]}" ]; then
           printf -v "$var" '%s' "${values[$((answer - 1))]}"; return
         fi ;;
    esac
  done
}

confirm() {   # confirm "question en" "question fr": yes with --yes, else asks (default yes)
  [ "$INTERACTIVE" = 1 ] || return 0
  local a
  printf '%s [%s]: ' "$(t "$1" "$2")" "$(t "Y/n" "O/n")"
  IFS= read -r a || a="n"
  case "$a" in n | N | no | non) return 1 ;; *) return 0 ;; esac
}

SUDO=""
need_root() {
  if [ "$(id -u)" != 0 ]; then
    if [ "$DRY_RUN" = 1 ]; then SUDO="sudo"; return; fi
    have sudo || die "This step needs root: run the script as root, or install sudo." "Cette étape demande root : lancez le script en root, ou installez sudo."
    SUDO="sudo"
  fi
}

# run CMD...: prints the command (quoted as a shell would need it, on stderr so that a command's own output can be
# silenced without hiding it), runs it (not in a dry run).
run() {
  { printf '  $'; printf ' %q' "$@"; printf '\n'; } >&2
  [ "$DRY_RUN" = 1 ] || "$@"
}
# written PATH: remembered for the summary.
written() { WRITTEN+=("$1"); }
# exists PATH (as root when needed; a dry run never calls sudo)
exists() {
  if [ "$DRY_RUN" = 1 ]; then test -e "$1"; else ${SUDO:+$SUDO} test -e "$1"; fi
}
# backup PATH: a dated copy of an existing file before it is replaced.
backup() {
  if exists "$1"; then
    run ${SUDO:+$SUDO} cp -p "$1" "$1.bak-$STAMP"
    say "  (kept a copy of the previous $1: $1.bak-$STAMP)" "  (copie de l'ancien $1 gardée : $1.bak-$STAMP)"
    BACKUPS+=("$1.bak-$STAMP")
  fi
}
# put_file DEST MODE [OWNER]: writes stdin to DEST (a copy aside of the previous one if it differs).
put_file() {
  local dest="$1" mode="$2" owner="${3:-}" content
  content="$(cat)"
  if [ "$DRY_RUN" = 1 ]; then
    local lines
    lines="$(printf '%s\n' "$content" | wc -l)"
    exists "$dest" && backup "$dest"
    printf '  (would write %s, mode %s%s, %d lines:)\n' "$dest" "$mode" "${owner:+, owner $owner}" "$lines"
    printf '%s\n' "$content" | sed -n '1,80s/^/  | /p'
    (( lines <= 80 )) || printf '  | ... (%d more lines)\n' "$((lines - 80))"
    written "$dest"
    return
  fi
  if exists "$dest" && printf '%s\n' "$content" | ${SUDO:+$SUDO} cmp -s - "$dest"; then
    written "$dest"
    return
  fi
  backup "$dest"
  local tmp="$TMP/put.$RANDOM"
  printf '%s\n' "$content" >"$tmp"
  run ${SUDO:+$SUDO} install -m "$mode" ${owner:+-o "$owner" -g "$owner"} "$tmp" "$dest"
  rm -f "$tmp"
  written "$dest"
}
# install_bin SRC DEST: a program, a copy aside of the previous one first.
install_bin() {
  if [ "$DRY_RUN" = 0 ] && exists "$2" && ${SUDO:+$SUDO} cmp -s "$1" "$2"; then written "$2"; return; fi
  backup "$2"
  run ${SUDO:+$SUDO} install -m 755 "$1" "$2"
  written "$2"
}
# new_secret DEST [OWNER]: 32 random bytes in hexadecimal, created exclusively with mode 0600 (no instant where
# another account could read it); an existing file is kept.
new_secret() {
  local dest="$1" owner="${2:-}"
  if [ "$DRY_RUN" = 0 ] && exists "$dest"; then
    say "  $dest exists: it is kept." "  $dest existe : il est gardé."
    written "$dest"
    return
  fi
  run ${SUDO:+$SUDO} sh -c 'umask 077 && set -C && od -An -tx1 -N32 /dev/urandom | tr -d " \n" > "$1" && echo >> "$1"' sh "$dest"
  [ -z "$owner" ] || run ${SUDO:+$SUDO} chown "$owner:$owner" "$dest"
  written "$dest"
}
# copy_secret SRC DEST [OWNER]: a given secret, mode 0600, never readable by others.
copy_secret() {
  backup "$2"
  run ${SUDO:+$SUDO} install -m 600 ${3:+-o "$3" -g "$3"} "$1" "$2"
  written "$2"
}

# ---- questions ------------------------------------------------------------------------------------------------------

say "Kalamar server installer" "Installation d'un serveur Kalamar"
if [ "$DRY_RUN" = 1 ]; then
  say "DRY RUN: every command and file is shown, nothing is written or started." "ESSAI À BLANC : chaque commande et chaque fichier sont montrés, rien n'est écrit ni lancé."
fi
say "(Ctrl-C stops it; nothing is installed before the summary is confirmed.)" "(Ctrl-C arrête tout ; rien n'est installé avant la confirmation du récapitulatif.)"

choose ROLE "What do you want to install?" "Que voulez-vous installer ?" \
  "server|A whole server: the HTTP service and its relay (start here)|Un serveur complet : le service HTTP et son relais (commencez par là)" \
  "relay|One more relay for a server you already run (more players, another region)|Un relais de plus pour un serveur existant (plus de joueurs, autre région)"

choose MODE "How should it run?" "Comment le faire tourner ?" \
  "docker|Docker (docker compose): everything in one folder, easy to remove|Docker (docker compose) : tout dans un dossier, facile à retirer" \
  "systemd|systemd services, built here with cargo (needs sudo)|Services systemd, compilés ici avec cargo (demande sudo)" \
  "files|Only write the files; I start things myself|Écrire seulement les fichiers ; je lance moi-même"

# A missing tool stops a real run; a dry run only says so.
require() {   # require TOOL "message en" "message fr"
  have "$1" && return 0
  if [ "$DRY_RUN" = 1 ]; then
    say "  (dry run: $1 is not installed here; a real run would stop)" "  (essai à blanc : $1 n'est pas installé ici ; une vraie installation s'arrêterait)"
    return 0
  fi
  die "$2" "$3"
}
case "$MODE" in
  docker)
    require docker "Docker is not installed: install Docker Engine and its compose plugin (see Docker's own documentation), or choose another way." \
      "Docker n'est pas installé : installez Docker Engine et son module compose (voir la documentation de Docker), ou choisissez un autre mode."
    if have docker && ! docker compose version >/dev/null 2>&1; then
      require docker-compose-plugin "The 'docker compose' plugin is missing." "Le module « docker compose » manque."
    fi
    ;;
  systemd)
    require systemctl "systemd is not available here: choose Docker or files." "systemd n'est pas disponible ici : choisissez Docker ou fichiers."
    require cargo "cargo (Rust 1.97 or newer) is needed to build: install it from https://rustup.rs and run this again." \
      "cargo (Rust 1.97 ou plus récent) est nécessaire pour compiler : installez-le depuis https://rustup.rs puis relancez."
    if have systemctl; then
      # User=, StateDirectory= and the hardening of deploy/systemd need systemd 240 or newer (no %d specifier here).
      SYSTEMD_VERSION="$(systemctl --version 2>/dev/null | awk 'NR == 1 { print $2 }' | tr -cd '0-9')"
      if [ -n "$SYSTEMD_VERSION" ] && [ "$SYSTEMD_VERSION" -lt 240 ]; then
        die "systemd $SYSTEMD_VERSION is too old (240 or newer): choose Docker or files." "systemd $SYSTEMD_VERSION est trop ancien (240 ou plus récent) : choisissez Docker ou fichiers."
      fi
    fi
    ;;
esac

if [ "$ROLE" = server ]; then
  choose PRESET "Which kind of community? (a starting point: every line can be changed later)" "Quel genre de communauté ? (un point de départ : tout se modifie ensuite)" \
    "friends|friends: private rooms by code, friends list, no ranking (the relay inside the service)|friends : salons privés par code, liste d'amis, pas de classement (le relais dans le service)" \
    "competitive|competitive: ranked 1v1 and 4v4 queues, ladders, stage veto, referees (a relay of its own)|competitive : files classées 1c1 et 4c4, classements, veto des stages, arbitres (un relais à part)" \
    "tournament|tournament: an 8-player cup, best-of-5 challenges, public stands (a relay of its own)|tournament : une coupe à 8, des défis au meilleur des 5, tribunes ouvertes (un relais à part)"
  ask NAME is_text "Name of the server (shown in launchers)" "Nom du serveur (affiché dans les lanceurs)" "My community"
  ask HOST is_host "Public host name players will type (it must point to this machine)" "Nom d'hôte public que les joueurs taperont (il doit pointer vers cette machine)" ""
  ask OPERATOR is_text "Your name or your team's (shown to players)" "Votre nom ou celui de l'équipe (affiché aux joueurs)" ""
  ask CONTACT is_contact_answer "How players reach you (mailto:... or https://...)" "Comment vous joindre (mailto:... ou https://...)" ""
  case "$CONTACT" in mailto:* | https://*) ;; *@*) CONTACT="mailto:$CONTACT" ;; esac
  check "$NAME" is_text "The name is 1 to 64 characters, without control characters." "Le nom fait 1 à 64 caractères, sans caractère de contrôle."
  check "$HOST" is_host "The host name is a bare name such as play.example.org (no https://, no path, no space)." "Le nom d'hôte est un nom nu comme play.example.org (sans https://, sans chemin, sans espace)."
  check "$OPERATOR" is_text "The operator is 1 to 64 characters, without control characters." "L'opérateur fait 1 à 64 caractères, sans caractère de contrôle."
  check "$CONTACT" is_contact "The contact is a mailto: or https: link, without spaces or quotes." "Le contact est un lien mailto: ou https:, sans espace ni guillemet."
  if [ "$MODE" = docker ]; then
    choose HTTPS "HTTPS (launchers only talk HTTPS to a server)" "HTTPS (les lanceurs ne parlent qu'en HTTPS à un serveur)" \
      "caddy|Caddy in a container gets and renews the certificate (ports 80 and 443 must be free)|Caddy dans un conteneur obtient et renouvelle le certificat (ports 80 et 443 libres)" \
      "none|I have my own HTTPS proxy: the service listens on 127.0.0.1:8080|J'ai mon propre proxy HTTPS : le service écoute sur 127.0.0.1:8080"
  else
    HTTPS="${HTTPS:-none}"
  fi
  case "$HTTPS" in caddy | none) ;; *) die "--https: caddy or none" ;; esac
  # The relay: inside the service for friends, a program of its own otherwise; UDP 47313 either way.
  RELAY_PORT="${RELAY_PORT:-47313}"
  [ "$RELAY_PORT" = 47313 ] || die "--relay-port is for one more relay (--role relay): a server's relay port is set in its configuration ([[relays]], [embedded_relay]), 47313 here." \
    "--relay-port sert à un relais de plus (--role relay) : le port du relais d'un serveur se règle dans sa configuration ([[relays]], [embedded_relay]), 47313 ici."
  if [ "$PRESET" = friends ]; then EMBEDDED=1; else EMBEDDED=0; fi
else
  if [ -z "$RELAY_KEY" ] && [ "$INTERACTIVE" = 1 ]; then
    ask RELAY_KEY is_path_answer "Path of an existing relay secret to use (Enter: make a new one, the safer choice)" "Chemin d'un secret de relais existant à utiliser (Entrée : en créer un nouveau, le plus sûr)" "-"
    [ "$RELAY_KEY" = "-" ] && RELAY_KEY=""
  fi
  if [ -n "$RELAY_KEY" ]; then
    case "$RELAY_KEY" in /* | [A-Za-z]:*) ;; *) RELAY_KEY="$PWD/$RELAY_KEY" ;; esac
    check "$RELAY_KEY" is_path "The secret's path has a character this installer does not accept." "Le chemin du secret contient un caractère que l'installateur n'accepte pas."
    [ -f "$RELAY_KEY" ] && [ -r "$RELAY_KEY" ] || die "Cannot read $RELAY_KEY" "Impossible de lire $RELAY_KEY"
    grep -Eq '^[0-9a-fA-F]{32,}$' "$RELAY_KEY" || die "$RELAY_KEY is not a relay secret (hexadecimal, at least 32 digits)." "$RELAY_KEY n'est pas un secret de relais (hexadécimal, au moins 32 chiffres)."
  fi
  ask RELAY_PORT is_port "UDP port" "Port UDP" "47313"
  EMBEDDED=0
fi
check "$RELAY_PORT" is_port "The UDP port is a number from 1 to 65535." "Le port UDP est un nombre de 1 à 65535."
check "$REPO" is_path "The repository's path has a character this installer does not accept (quote, backslash, \$)." "Le chemin du dépôt contient un caractère que l'installateur n'accepte pas (guillemet, barre oblique inverse, \$)."

if [ "$MODE" != systemd ]; then
  ask DIR is_path_answer "Folder for the files" "Dossier des fichiers" "$HOME/kalamar"
  case "$DIR" in /* | [A-Za-z]:*) ;; *) DIR="$PWD/$DIR" ;; esac
  check "$DIR" is_path "The folder's path has a character this installer does not accept (quote, backslash, \$)." "Le chemin du dossier contient un caractère que l'installateur n'accepte pas (guillemet, barre oblique inverse, \$)."
fi
USER_ID="$(id -u)" GROUP_ID="$(id -g)"

printf '\n%s\n' "$(t "Summary" "Récapitulatif")"
printf '  %-10s %s\n' "$(t what quoi)" "$ROLE" "$(t how comment)" "$MODE"
if [ "$ROLE" = server ]; then
  printf '  %-10s %s\n' preset "$PRESET" "$(t name nom)" "$NAME" "$(t address adresse)" "https://$HOST" https "$HTTPS"
  printf '  %-10s %s\n' relay "$(if [ "$EMBEDDED" = 1 ]; then t "inside the service, UDP $RELAY_PORT" "dans le service, UDP $RELAY_PORT"; else t "kalamar-relay, UDP $RELAY_PORT" "kalamar-relay, UDP $RELAY_PORT"; fi)"
else
  printf '  %-10s %s\n' "$(t key clé)" "${RELAY_KEY:-$(t "a new one" "un nouveau")}" port "UDP $RELAY_PORT"
fi
[ "$MODE" = systemd ] || printf '  %-10s %s\n' "$(t folder dossier)" "$DIR"
confirm "Install now?" "Installer maintenant ?" || { say "Nothing was installed." "Rien n'a été installé."; exit 0; }
echo

# A private folder for the files written as root (never a predictable /tmp name), removed whatever happens.
TMP=""
if [ "$DRY_RUN" = 0 ]; then
  TMP="$(mktemp -d "${TMPDIR:-/tmp}/kalamar-install.XXXXXXXX")"
  chmod 700 "$TMP"
  trap 'rm -rf "$TMP"' EXIT
fi

# ---- what gets written ----------------------------------------------------------------------------------------------

INIT_ARGS=()
if [ "$ROLE" = server ]; then
  INIT_ARGS=(init --preset "$PRESET" --name "$NAME" --host "$HOST" --operator "$OPERATOR" --contact "$CONTACT")
fi

privacy_page() {  # the notice Caddy serves at /privacy: the template, to adapt
  printf '<!doctype html><html lang="en"><meta charset="utf-8"><title>Privacy</title>\n<body style="max-width:50rem;margin:2rem auto;font-family:sans-serif"><pre style="white-space:pre-wrap">\n'
  sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' "$REPO/PRIVACY-TEMPLATE.md"
  printf '</pre></body></html>\n'
}

caddyfile() {  # $1: where the service is (kalamar:8080 or 127.0.0.1:8080), $2: site folder as Caddy sees it
  cat <<EOF
# Caddy in front of kalamar-service (written by deploy/install.sh): the certificate of $HOST is obtained and renewed
# by Caddy itself.
$HOST {
	reverse_proxy /.well-known/kalamar-server.json $1
	reverse_proxy /api/* $1 {
		header_up -X-Real-IP
	}
	# Your privacy notice: edit privacy.html (it starts as the template, to adapt).
	handle /privacy {
		root * $2
		rewrite * /privacy.html
		file_server
	}
	request_body {
		max_size 128KB
	}
	log {
		output discard
	}
}
EOF
}

compose_server() {
  cat <<EOF
# Kalamar server (written by deploy/install.sh). In this folder:
#   docker compose up -d        start          docker compose logs -f kalamar   logs
#   docker compose down         stop           docker compose build             after a git pull of the sources
# The images are built from the sources (base images rust and debian, by tag) or pulled by tag ($CADDY_IMAGE):
# pin them by digest (image@sha256:...) for an install that never changes by itself.
name: kalamar
services:
  kalamar:                                # the HTTP service$([ "$EMBEDDED" = 1 ] && printf ', with the relay inside (UDP %s)' "$RELAY_PORT")
    build:
      context: "$REPO"
      dockerfile: deploy/Dockerfile
    image: kalamar
    restart: unless-stopped
    volumes:
      - ./config:/etc/kalamar:ro          # kalamar.toml: edit it, then "admin reload" (INSTALL-INFO.txt)
      - kalamar-data:/data                # the database, secret.key, relay.key: back this volume up
EOF
  if [ "$EMBEDDED" = 1 ] || [ "$HTTPS" != caddy ]; then
    echo "    ports:"
    [ "$EMBEDDED" = 1 ] && echo "      - \"$RELAY_PORT:$RELAY_PORT/udp\"                 # the relay: players reach it directly"
    [ "$HTTPS" != caddy ] && echo "      - \"127.0.0.1:8080:8080\"             # the API, for your own HTTPS proxy"
  fi
  if [ "$EMBEDDED" = 0 ]; then
    cat <<EOF
  relay:                                  # kalamar-relay, bound to this server by /data/relay.key
    image: kalamar                        # the image built above, never one pulled under that name
    pull_policy: never
    restart: unless-stopped
    depends_on: [kalamar]
    command: ["kalamar-relay", "--bind", "0.0.0.0", "--port", "$RELAY_PORT", "--room-secret-file", "/data/relay.key"]
    volumes:
      - kalamar-data:/data:ro
    ports:
      - "$RELAY_PORT:$RELAY_PORT/udp"
EOF
  fi
  if [ "$HTTPS" = caddy ]; then
    cat <<EOF
  caddy:                                  # HTTPS in front of the service
    image: $CADDY_IMAGE
    restart: unless-stopped
    depends_on: [kalamar]
    ports: ["80:80", "443:443"]
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - ./site:/srv/site:ro
      - caddy-data:/data
volumes:
  kalamar-data:
  caddy-data:
EOF
  else
    printf 'volumes:\n  kalamar-data:\n'
  fi
}

compose_relay() {
  cat <<EOF
# One more Kalamar relay (written by deploy/install.sh), bound to its server by secrets/relay.key.
#   docker compose up -d   start     docker compose logs -f relay   logs     docker compose down   stop
name: kalamar-relay
services:
  relay:
    build:
      context: "$REPO"
      dockerfile: deploy/Dockerfile
    image: kalamar
    restart: unless-stopped
    user: "$USER_ID:$GROUP_ID"            # the owner of secrets/relay.key (mode 0600)
    command: ["kalamar-relay", "--bind", "0.0.0.0", "--port", "$RELAY_PORT", "--room-secret-file", "/etc/kalamar/relay.key"]
    volumes:
      - ./secrets/relay.key:/etc/kalamar/relay.key:ro
    ports:
      - "$RELAY_PORT:$RELAY_PORT/udp"
EOF
}

# A unit of deploy/systemd for this machine: an account of the system instead of a dynamic one (so that the relay and
# the service can read their own files, with no credential: systemd 240 is enough), and this ExecStart.
unit_from() {  # $1: unit of deploy/systemd, $2: ExecStart line, $3: account (the service's by default), $4: its group
  local line user="${3:-$SYSTEM_USER}" group="${4:-${3:-$SYSTEM_USER}}"
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      DynamicUser=yes) printf 'User=%s\nGroup=%s\n' "$user" "$group" ;;
      LoadCredential=*) ;;
      ExecStart=*) printf '# Written by deploy/install.sh from deploy/systemd/%s\n%s\n' "${1##*/}" "$2" ;;
      *) printf '%s\n' "$line" ;;
    esac
  done <"$1"
}

INFO=""
info() { INFO="$INFO$1"$'\n'; }

finish() {  # $1: where INSTALL-INFO.txt goes
  local w
  info ""
  info "$(t "WRITTEN BY THIS INSTALL" "ÉCRIT PAR CETTE INSTALLATION")"
  for w in "${WRITTEN[@]}"; do info "  $w"; done
  info "  $1"
  if [ ${#BACKUPS[@]} -gt 0 ]; then
    info "$(t "  Copies of the files replaced:" "  Copies des fichiers remplacés :")"
    for w in "${BACKUPS[@]}"; do info "    $w"; done
  fi
  info ""
  info "$(t "UNINSTALL (keeps nothing: back up the data first)" "DÉSINSTALLER (ne garde rien : sauvegardez d'abord les données)")"
  for w in "${UNINSTALL[@]}"; do info "  $w"; done
  printf '\n============================================================\n%s============================================================\n' "$INFO"
  if [ "$DRY_RUN" = 1 ]; then
    say "(dry run: $1 would hold this summary; nothing was written)" "(essai à blanc : $1 garderait ce récapitulatif ; rien n'a été écrit)"
    return
  fi
  if put_file "$1" 644 < <(printf '%s' "$INFO") >/dev/null; then
    say "(This summary is kept in $1)" "(Ce récapitulatif est gardé dans $1)"
  fi
}

server_info() {  # $1 config file, $2 reload, $3 admin prefix, $4 logs, $5 restart
  info "$(t "Kalamar server \"$NAME\" ($PRESET)" "Serveur Kalamar « $NAME » ($PRESET)")"
  info ""
  info "$(t "FOR PLAYERS" "POUR LES JOUEURS")"
  info "$(t "  In the launcher, \"Add a server\":  $HOST" "  Dans le lanceur, « Ajouter un serveur » :  $HOST")"
  info ""
  info "$(t "PORTS TO OPEN on this machine (firewall, router)" "PORTS À OUVRIR sur cette machine (pare-feu, box)")"
  info "  TCP 80, 443   $(t "HTTPS (the launchers; port 80 for the certificate)" "HTTPS (les lanceurs ; le port 80 pour le certificat)")"
  info "  UDP $RELAY_PORT     $(t "the relay (the matches), directly, never through the HTTPS proxy" "le relais (les matchs), en direct, jamais par le proxy HTTPS")"
  info "$(t "  The name $HOST must point to this machine's public address." "  Le nom $HOST doit pointer vers l'adresse publique de cette machine.")"
  info ""
  info "$(t "CHANGING THINGS" "MODIFIER")"
  info "$(t "  The configuration (queues, rooms, events, texts, limits...):" "  La configuration (files, salons, événements, textes, limites...) :")"
  info "    $1"
  info "$(t "  Every option is explained in:" "  Chaque option est expliquée dans :")  $REPO/deploy/kalamar.example.toml"
  info "$(t "  Apply it without a restart (players stay connected; prints what was applied, or the errors):" "  L'appliquer sans redémarrer (les joueurs restent connectés ; affiche ce qui est appliqué, ou les erreurs) :")"
  info "    $2"
  info "$(t "  Logins, rate limits, the listening address, the relay inside the service need a restart:" "  Connexions, limites de débit, adresse d'écoute, relais intégré demandent un redémarrage :")"
  info "    $5"
  info ""
  info "$(t "DAY TO DAY" "AU QUOTIDIEN")"
  info "  $(t logs journaux):   $4"
  info "  $(t "players" "joueurs"):  $3 admin users"
  info "  $(t "ban" "bannir"):   $3 admin ban <id> --for 7d --reason \"...\""
  info "  $(t "reports" "signalements"):  $3 admin reports"
  info ""
  info "$(t "MORE PLAYERS OR ANOTHER REGION: ONE MORE RELAY" "PLUS DE JOUEURS OU UNE AUTRE RÉGION : UN RELAIS DE PLUS")"
  info "$(t "  1. On the other machine: deploy/install.sh --role relay   (it makes that relay's own secret)" "  1. Sur l'autre machine : deploy/install.sh --role relay   (il crée le secret propre à ce relais)")"
  info "$(t "  2. Copy that secret here (privately), readable by the service only, then add to the configuration and reload:" "  2. Copiez ce secret ici (en privé), lisible par le seul service, puis ajoutez à la configuration et rechargez :")"
  info "       [[relays]]"
  info "       host = \"relay2.example.org\""
  info "       id = \"relay2\""
  info "       secret_file = \"<$(t "the copy of that relay's secret" "la copie du secret de ce relais")>\""
  info "$(t "  One secret per relay: whoever holds a relay's secret can open rooms on that relay." "  Un secret par relais : qui détient le secret d'un relais peut ouvrir des salons sur ce relais.")"
  info ""
  info "$(t "BACK UP: the database, secret.key and relay.key (without secret.key nobody finds their account again)." "À SAUVEGARDER : la base, secret.key et relay.key (sans secret.key, plus personne ne retrouve son compte).")"
  info "$(t "  Before an update too: once a newer version has started on the database, an older one refuses it." "  Avant une mise à jour aussi : une fois qu'une version plus récente a démarré sur la base, une plus ancienne la refuse.")"
  info "$(t "PRIVACY: adapt the notice players are shown (https://$HOST/privacy) from PRIVACY-TEMPLATE.md." "VIE PRIVÉE : adaptez la notice montrée aux joueurs (https://$HOST/privacy) à partir de PRIVACY-TEMPLATE.md.")"
}

relay_info() {  # $1 logs, $2 restart, $3 where the secret is
  info "$(t "Kalamar relay, bound to its server by its secret" "Relais Kalamar, lié à son serveur par son secret")"
  info ""
  info "$(t "PORT TO OPEN on this machine: UDP $RELAY_PORT (nothing else; no HTTPS here)." "PORT À OUVRIR sur cette machine : UDP $RELAY_PORT (rien d'autre ; pas de HTTPS ici).")"
  info ""
  info "$(t "ITS SECRET: $3 (keep it private; back it up)" "SON SECRET : $3 (gardez-le privé ; sauvegardez-le)")"
  info "$(t "ON THE SERVER: copy that file there (privately, readable by the service only), add to its configuration, then" "SUR LE SERVEUR : copiez ce fichier là-bas (en privé, lisible par le seul service), ajoutez à sa configuration, puis")"
  info "$(t "run kalamar-service -c <its configuration> admin reload:" "lancez kalamar-service -c <sa configuration> admin reload :")"
  info "    [[relays]]"
  info "    host = \"<$(t "this machine's public name or address" "le nom ou l'adresse publique de cette machine")>\""
  info "    port = $RELAY_PORT"
  info "    id = \"relay2\""
  info "    secret_file = \"<$(t "the copy of this secret on the server" "la copie de ce secret sur le serveur")>\""
  info ""
  info "  $(t logs journaux):  $1"
  info "  $(t restart redémarrer):  $2"
  info "$(t "  It opens only room codes sealed with its secret: strangers who know its address cannot open rooms on it." "  Il n'ouvre que les codes de salon scellés avec son secret : un inconnu qui connaît son adresse ne peut pas y ouvrir de salon.")"
  info "$(t "  A room code works for 24 to 48 hours for whoever holds it (the players of its match)." "  Un code de salon fonctionne 24 à 48 h pour qui le détient (les joueurs de son match).")"
}

# ---- the three ways -------------------------------------------------------------------------------------------------

relay_secret_here() {  # $1 destination, $2 owner: the given secret, or a new one
  if [ -n "$RELAY_KEY" ]; then copy_secret "$RELAY_KEY" "$1" "${2:-}"; else new_secret "$1" "${2:-}"; fi
}

install_files() {
  run mkdir -p "$DIR"
  written "$DIR"
  if [ "$ROLE" = relay ]; then
    run mkdir -p "$DIR/secrets"
    run chmod 700 "$DIR/secrets"
    relay_secret_here "$DIR/secrets/relay.key"
    put_file "$DIR/docker-compose.yml" 644 < <(compose_relay)
    relay_info "$(t "where you started it" "là où vous l'avez lancé")" "$(t "stop and start it again" "arrêtez-le et relancez-le")" "$DIR/secrets/relay.key"
    info ""
    info "$(t "Nothing was started. With Docker: cd $DIR && docker compose up -d --build" "Rien n'a été lancé. Avec Docker : cd $DIR && docker compose up -d --build")"
    info "$(t "Without: kalamar-relay --port $RELAY_PORT --room-secret-file $DIR/secrets/relay.key" "Sans : kalamar-relay --port $RELAY_PORT --room-secret-file $DIR/secrets/relay.key")"
    UNINSTALL+=("rm -r \"$DIR\"")
    finish "$DIR/INSTALL-INFO.txt"
    return
  fi
  if [ -z "$BIN" ]; then
    for c in "$REPO/target/release/kalamar-service" "$REPO/target/debug/kalamar-service" "$REPO/target/release/kalamar-service.exe" "$REPO/target/debug/kalamar-service.exe"; do
      [ -x "$c" ] && BIN="$c" && break
    done
  fi
  [ -n "$BIN" ] || BIN="$(command -v kalamar-service || true)"
  if [ -z "$BIN" ]; then
    [ "$DRY_RUN" = 1 ] || die "kalamar-service is not built: cargo build --release, or set KALAMAR_BIN." "kalamar-service n'est pas compilé : cargo build --release, ou définissez KALAMAR_BIN."
    BIN="kalamar-service"
  fi
  run mkdir -p "$DIR/config" "$DIR/data"
  run chmod 700 "$DIR/data"
  local c="$DIR/config/kalamar.toml"
  if [ "$DRY_RUN" = 0 ] && [ -e "$c" ]; then
    say "$c exists: it is kept as it is." "$c existe : il est gardé tel quel."
  else
    run "$BIN" --config "$c" "${INIT_ARGS[@]}" --data-dir "$DIR/data" >/dev/null ||
      die "$BIN could not write the configuration (if it is older than these sources: cargo build --release)." "$BIN n'a pas pu écrire la configuration (s'il est plus ancien que ces sources : cargo build --release)."
  fi
  written "$c"
  [ "$EMBEDDED" = 1 ] || new_secret "$DIR/data/relay.key"
  run mkdir -p "$DIR/site"
  if [ "$DRY_RUN" = 1 ] || [ ! -e "$DIR/site/privacy.html" ]; then put_file "$DIR/site/privacy.html" 644 < <(privacy_page); fi
  put_file "$DIR/Caddyfile" 644 < <(caddyfile "127.0.0.1:8080" "$DIR/site")
  server_info "$c" "kalamar-service -c $c admin reload" "kalamar-service -c $c" "$(t "where you started it" "là où vous l'avez lancé")" "$(t "stop and start it again" "arrêtez-le et relancez-le")"
  info ""
  info "$(t "Nothing was started. To run it:" "Rien n'a été lancé. Pour le lancer :")"
  if [ "$EMBEDDED" = 0 ]; then
    info "    kalamar-relay --port $RELAY_PORT --room-secret-file $DIR/data/relay.key    $(t "(the relay, UDP $RELAY_PORT)" "(le relais, UDP $RELAY_PORT)")"
    info "    kalamar-service -c $c serve          $(t "(the API on 127.0.0.1:8080)" "(l'API sur 127.0.0.1:8080)")"
  else
    info "    kalamar-service -c $c serve          $(t "(the API on 127.0.0.1:8080, its relay on UDP $RELAY_PORT)" "(l'API sur 127.0.0.1:8080, son relais sur UDP $RELAY_PORT)")"
  fi
  info "    caddy run --config $DIR/Caddyfile    $(t "(HTTPS in front; or your own proxy)" "(HTTPS devant ; ou votre propre proxy)")"
  UNINSTALL+=("$(t "stop what you started, then:" "arrêtez ce que vous avez lancé, puis :") rm -r \"$DIR\"")
  finish "$DIR/INSTALL-INFO.txt"
}

install_docker() {
  run mkdir -p "$DIR"
  written "$DIR"
  if [ "$ROLE" = relay ]; then
    run mkdir -p "$DIR/secrets"
    run chmod 700 "$DIR/secrets"
    relay_secret_here "$DIR/secrets/relay.key"
    put_file "$DIR/docker-compose.yml" 644 < <(compose_relay)
    run docker compose -f "$DIR/docker-compose.yml" up -d --build
    relay_info "cd $DIR && docker compose logs -f relay" "cd $DIR && docker compose restart relay" "$DIR/secrets/relay.key"
    UNINSTALL+=("cd \"$DIR\" && docker compose down --rmi local" "rm -r \"$DIR\"")
    finish "$DIR/INSTALL-INFO.txt"
    return
  fi
  run mkdir -p "$DIR/config"
  put_file "$DIR/docker-compose.yml" 644 < <(compose_server)
  if [ "$HTTPS" = caddy ]; then
    run mkdir -p "$DIR/site"
    if [ "$DRY_RUN" = 1 ] || [ ! -e "$DIR/site/privacy.html" ]; then put_file "$DIR/site/privacy.html" 644 < <(privacy_page); fi
    put_file "$DIR/Caddyfile" 644 < <(caddyfile "kalamar:8080" "/srv/site")
  fi
  local dc=(docker compose -f "$DIR/docker-compose.yml")
  run "${dc[@]}" build kalamar
  written "$(t "Docker: the image kalamar, the volume kalamar_kalamar-data" "Docker : l'image kalamar, le volume kalamar_kalamar-data")$([ "$HTTPS" = caddy ] && t ", the image $CADDY_IMAGE and the volume kalamar_caddy-data" ", l'image $CADDY_IMAGE et le volume kalamar_caddy-data")"
  if [ "$DRY_RUN" = 0 ] && [ -e "$DIR/config/kalamar.toml" ]; then
    say "$DIR/config/kalamar.toml exists: it is kept as it is." "$DIR/config/kalamar.toml existe : il est gardé tel quel."
  else
    # Written by the image's own kalamar-service, in a one-off container, as you (so that you can edit the file).
    run "${dc[@]}" run --rm --no-deps -T -u "$USER_ID:$GROUP_ID" -v "$DIR/config:/out" kalamar \
      kalamar-service --config /out/kalamar.toml "${INIT_ARGS[@]}" \
      --bind 0.0.0.0:8080 --data-dir /data --no-relay-secret \
      --trusted-proxies 127.0.0.1/32,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16 >/dev/null
  fi
  written "$DIR/config/kalamar.toml"
  if [ "$EMBEDDED" = 0 ]; then
    # The relay's secret, in the data volume, as the image's account (mode 0600): an existing one is kept.
    run "${dc[@]}" run --rm --no-deps -T kalamar sh -c 'test -e /data/relay.key || kalamar-relay --new-room-secret /data/relay.key'
    written "$(t "the volume kalamar_kalamar-data: relay.key" "le volume kalamar_kalamar-data : relay.key")"
  fi
  local action="up -d"
  if [ "$DRY_RUN" = 0 ] && [ -n "$("${dc[@]}" ps -q 2>/dev/null)" ]; then
    if confirm "The server is running: restart it with the new version now?" "Le serveur tourne : le redémarrer maintenant avec la nouvelle version ?"; then
      action="up -d --force-recreate"
    else
      say "Not restarted: docker compose up -d --force-recreate when you are ready." "Pas redémarré : docker compose up -d --force-recreate quand vous serez prêt."
      action=""
    fi
  fi
  # shellcheck disable=SC2086 # two words on purpose
  [ -z "$action" ] || run "${dc[@]}" $action
  local x="cd $DIR && docker compose exec kalamar kalamar-service -c /etc/kalamar/kalamar.toml"
  server_info "$DIR/config/kalamar.toml" "$x admin reload" "$x" "cd $DIR && docker compose logs -f kalamar" "cd $DIR && docker compose restart kalamar"
  if [ "$HTTPS" != caddy ]; then
    info ""
    info "$(t "HTTPS is yours to set up: proxy https://$HOST/api/ and /.well-known/kalamar-server.json to 127.0.0.1:8080 (deploy/Caddyfile, deploy/nginx.conf)." "Le HTTPS reste à mettre en place : renvoyez https://$HOST/api/ et /.well-known/kalamar-server.json vers 127.0.0.1:8080 (deploy/Caddyfile, deploy/nginx.conf).")"
  fi
  UNINSTALL+=("cd \"$DIR\" && docker compose down --rmi local    $(t "(add -v to delete the data volume too)" "(ajoutez -v pour supprimer aussi le volume de données)")" "rm -r \"$DIR\"")
  finish "$DIR/INSTALL-INFO.txt"
}

# start_or_restart UNIT: starts a new service, or restarts an updated one after confirmation.
start_or_restart() {
  if [ "$DRY_RUN" = 0 ] && systemctl is-active --quiet "$1"; then
    if confirm "$1 is running: restart it now with what was just installed?" "$1 tourne : le redémarrer maintenant avec ce qui vient d'être installé ?"; then
      run $SUDO systemctl restart "$1"
    else
      say "  Not restarted: sudo systemctl restart $1 when you are ready." "  Pas redémarré : sudo systemctl restart $1 quand vous serez prêt."
    fi
  else
    run $SUDO systemctl enable --now "$1"
  fi
}

install_systemd() {
  need_root
  say "Building (several minutes the first time)..." "Compilation (plusieurs minutes la première fois)..."
  local jobs
  jobs="$(nproc 2>/dev/null || echo 4)"
  run cargo build --manifest-path "$REPO/Cargo.toml" --release --locked -j "$jobs" -p kalamar-service -p kalamar-relay
  written "$REPO/target $(t "(the build; cargo also fills ~/.cargo with the crates it downloads)" "(la compilation ; cargo remplit aussi ~/.cargo des crates qu'il télécharge)")"
  # The account both programs run as (their own files in /var/lib/kalamar, nothing else).
  if [ "$DRY_RUN" = 1 ] || ! id -u "$SYSTEM_USER" >/dev/null 2>&1; then
    local nologin
    nologin="$(command -v nologin || echo /bin/false)"
    run $SUDO useradd --system --user-group --home-dir /var/lib/kalamar --no-create-home --shell "$nologin" "$SYSTEM_USER"
    written "$(t "the system account" "le compte système") $SYSTEM_USER"
    UNINSTALL+=("sudo userdel $SYSTEM_USER")
  fi
  # The relay's own account (security review L-S6): its group is the service's, so it reads the relay secret (mode
  # 0640) and nothing else of /var/lib/kalamar (the database and secret.key are 0600, the folder 0750).
  if [ "$ROLE" = relay ] || [ "$EMBEDDED" = 0 ]; then
    if [ "$DRY_RUN" = 1 ] || ! id -u "$RELAY_USER" >/dev/null 2>&1; then
      run $SUDO useradd --system --gid "$SYSTEM_USER" --home-dir /nonexistent --no-create-home --shell "$(command -v nologin || echo /bin/false)" "$RELAY_USER"
      written "$(t "the system account" "le compte système") $RELAY_USER"
      UNINSTALL+=("sudo userdel $RELAY_USER")
    fi
  fi
  install_bin "$REPO/target/release/kalamar-service" /usr/local/bin/kalamar-service
  install_bin "$REPO/target/release/kalamar-relay" /usr/local/bin/kalamar-relay
  run $SUDO mkdir -p /etc/kalamar
  written /etc/kalamar
  run $SUDO install -d -m 750 -o "$SYSTEM_USER" -g "$SYSTEM_USER" /var/lib/kalamar
  written /var/lib/kalamar
  local relay_exec="ExecStart=/usr/local/bin/kalamar-relay --bind 0.0.0.0 --port $RELAY_PORT --room-secret-file"
  if [ "$ROLE" = relay ]; then
    relay_secret_here /etc/kalamar/relay.key "$SYSTEM_USER"
    run $SUDO chmod 640 /etc/kalamar/relay.key
    put_file /etc/systemd/system/kalamar-relay.service 644 \
      < <(unit_from "$REPO/deploy/systemd/kalamar-relay.service" "$relay_exec /etc/kalamar/relay.key" "$RELAY_USER" "$SYSTEM_USER")
    run $SUDO systemctl daemon-reload
    start_or_restart kalamar-relay
    relay_info "sudo journalctl -u kalamar-relay -f" "sudo systemctl restart kalamar-relay" /etc/kalamar/relay.key
    UNINSTALL+=("sudo systemctl disable --now kalamar-relay" "sudo rm /etc/systemd/system/kalamar-relay.service /usr/local/bin/kalamar-relay /usr/local/bin/kalamar-service" "sudo rm -r /etc/kalamar /var/lib/kalamar" "sudo systemctl daemon-reload")
    finish /etc/kalamar/INSTALL-INFO.txt
    return
  fi
  if [ "$DRY_RUN" = 0 ] && exists /etc/kalamar/kalamar.toml; then
    say "/etc/kalamar/kalamar.toml exists: it is kept as it is." "/etc/kalamar/kalamar.toml existe : il est gardé tel quel."
  else
    # The relay's secret is made below, owned by the service's account (init would make it as root).
    run $SUDO /usr/local/bin/kalamar-service --config /etc/kalamar/kalamar.toml "${INIT_ARGS[@]}" --data-dir /var/lib/kalamar --no-relay-secret >/dev/null
  fi
  written /etc/kalamar/kalamar.toml
  put_file /etc/systemd/system/kalamar-service.service 644 < <(unit_from "$REPO/deploy/systemd/kalamar-service.service" \
    "ExecStart=/usr/local/bin/kalamar-service --config /etc/kalamar/kalamar.toml --log-format json serve")
  if [ "$EMBEDDED" = 0 ]; then
    new_secret /var/lib/kalamar/relay.key "$SYSTEM_USER"
    run $SUDO chmod 640 /var/lib/kalamar/relay.key
    put_file /etc/systemd/system/kalamar-relay.service 644 \
      < <(unit_from "$REPO/deploy/systemd/kalamar-relay.service" "$relay_exec /var/lib/kalamar/relay.key" "$RELAY_USER" "$SYSTEM_USER")
  fi
  run $SUDO mkdir -p /srv/kalamar-site
  written /srv/kalamar-site
  if [ "$DRY_RUN" = 1 ] || ! exists /srv/kalamar-site/privacy.html; then
    put_file /srv/kalamar-site/privacy.html 644 < <(privacy_page)
  fi
  put_file /etc/kalamar/Caddyfile 644 < <(caddyfile "127.0.0.1:8080" "/srv/kalamar-site")
  run $SUDO systemctl daemon-reload
  [ "$EMBEDDED" = 1 ] || start_or_restart kalamar-relay
  start_or_restart kalamar-service
  local x="sudo -u $SYSTEM_USER kalamar-service -c /etc/kalamar/kalamar.toml"
  server_info /etc/kalamar/kalamar.toml "$x admin reload" "$x" "sudo journalctl -u kalamar-service -f" "sudo systemctl restart kalamar-service"
  info "$(t "  (sudo systemctl reload kalamar-service also reloads, but reports nothing: errors only reach the journal.)" "  (sudo systemctl reload kalamar-service recharge aussi, mais ne rapporte rien : les erreurs ne vont qu'au journal.)")"
  info ""
  info "$(t "HTTPS is the last step: the service listens on 127.0.0.1:8080. With Caddy installed (see Caddy's own documentation):" "Le HTTPS est la dernière étape : le service écoute sur 127.0.0.1:8080. Avec Caddy installé (voir la documentation de Caddy) :")"
  info "    sudo cp /etc/kalamar/Caddyfile /etc/caddy/Caddyfile && sudo systemctl reload caddy"
  info "$(t "  (it replaces Caddy's configuration: merge by hand if Caddy already serves something)" "  (cela remplace la configuration de Caddy : fusionnez à la main s'il sert déjà autre chose)")"
  if [ "$EMBEDDED" = 1 ]; then
    UNINSTALL+=("sudo systemctl disable --now kalamar-service")
    UNINSTALL+=("sudo rm /etc/systemd/system/kalamar-service.service /usr/local/bin/kalamar-service /usr/local/bin/kalamar-relay")
  else
    UNINSTALL+=("sudo systemctl disable --now kalamar-service kalamar-relay")
    UNINSTALL+=("sudo rm /etc/systemd/system/kalamar-service.service /etc/systemd/system/kalamar-relay.service /usr/local/bin/kalamar-service /usr/local/bin/kalamar-relay")
  fi
  UNINSTALL+=("sudo rm -r /etc/kalamar /srv/kalamar-site    $(t "and the data, once backed up:" "et les données, une fois sauvegardées :") sudo rm -r /var/lib/kalamar")
  UNINSTALL+=("sudo systemctl daemon-reload")
  finish /etc/kalamar/INSTALL-INFO.txt
}

case "$MODE" in
  files) install_files ;;
  docker) install_docker ;;
  systemd) install_systemd ;;
esac
