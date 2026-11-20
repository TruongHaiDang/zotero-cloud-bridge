#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

usage() {
  printf 'Usage: sudo %s DOMAIN [WEBDAV_PORT]\n' "$0"
  printf 'Example: sudo %s dav.example.com 18765\n' "$0"
}

case ${1:-} in
  -h|--help) usage; exit 0 ;;
esac
if (( $# < 1 || $# > 2 )); then
  usage >&2
  exit 2
fi

[[ $(uname -s) == Linux && -d /run/systemd/system ]] || fail 'A Linux host running systemd is required.'
(( EUID == 0 )) || fail 'Run this script with sudo.'
command -v caddy >/dev/null || fail 'Install Caddy with its systemd service first.'

domain=$1
port=${2:-18765}
export LC_ALL=C
label='[[:alnum:]]([[:alnum:]-]{0,61}[[:alnum:]])?'
[[ ${#domain} -le 253 && $domain =~ ^($label\.)+$label$ && $domain =~ [[:alpha:]] ]] || fail 'Enter a hostname such as dav.example.com.'
[[ $port =~ ^[0-9]{1,5}$ ]] && (( 10#$port >= 1 && 10#$port <= 65535 )) || fail 'Enter a WebDAV port from 1 to 65535.'
port=$((10#$port))

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
template="$script_dir/../deploy/Caddyfile"
caddyfile=/etc/caddy/Caddyfile
[[ -r $template ]] || fail "Deployment template missing: $template"
[[ -f $caddyfile && ! -L $caddyfile ]] || fail "Expected a regular file at $caddyfile."
[[ $(systemctl show caddy.service -p ExecStart --value) == *"$caddyfile"* ]] || fail "The Caddy systemd service must use $caddyfile."

exec 9>/run/lock/zotero-webdav-setup.lock
flock -n 9 || fail 'Another WebDAV proxy setup is running.'

caddy_active=false
if systemctl is-active --quiet caddy.service; then
  caddy_active=true
fi

# Keep the candidate beside the Caddyfile so relative imports keep working.
candidate=$(mktemp /etc/caddy/.zotero-webdav.XXXXXXXX)
backup="$candidate.backup"
published=false
cleanup() {
  local status=$?
  if (( status != 0 )) && "$published"; then
    if mv -f -- "$backup" "$caddyfile"; then
      if "$caddy_active"; then
        systemctl reload caddy.service || printf 'Could not reload the restored Caddyfile.\n' >&2
      else
        systemctl stop caddy.service || printf 'Could not stop Caddy.\n' >&2
      fi
    else
      printf 'Could not restore %s. Backup is at %s.\n' "$caddyfile" "$backup" >&2
      exit "$status"
    fi
  fi
  rm -f -- "$candidate" "$backup"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cp -p -- "$caddyfile" "$backup"
cp -p -- "$caddyfile" "$candidate"
{
  awk '
    $0 == "# BEGIN zotero-webdav setup" { if (managed++) exit 1; next }
    $0 == "# END zotero-webdav setup" { if (!managed) exit 1; managed = 0; next }
    !managed { print }
    END { if (managed) exit 1 }
  ' "$caddyfile"
  printf '%s\n' '# BEGIN zotero-webdav setup'
  sed -e 's/{\$BRIDGE_WEBDAV_HOST}/'"$domain"'/g' -e 's/{\$BRIDGE_WEBDAV_PORT:18765}/'"$port"'/g' "$template"
  printf '%s\n' '# END zotero-webdav setup'
} > "$candidate"
caddy validate --config "$candidate" --adapter caddyfile

published=true
mv -f -- "$candidate" "$caddyfile"
if "$caddy_active"; then
  systemctl reload caddy.service
else
  systemctl start caddy.service
fi
systemctl enable caddy.service
printf 'Zotero WebDAV URL: https://%s/\n' "$domain"
printf 'Upstream: http://127.0.0.1:%s/zotero/\n' "$port"
printf 'Use the existing WebDAV credentials on each device and select Verify Server.\n'
