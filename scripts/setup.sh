#!/usr/bin/env bash
# Sets up the tui-kanban database on macOS (and Linux). Safe to run repeatedly.
# The Windows counterpart is scripts/setup.ps1; both do the same steps.
#
# After one run, all of the following hold (docs/architecture.md section 3.3):
#   1. .env exists and holds the passwords of the three roles.
#   2. Container kb-postgres is running and healthy (127.0.0.1:55432).
#   3. ~/.config/kb/config.toml exists and holds only the writer and reader URLs.
#   4. The roles exist and the schema is current in databases kanban and kanban_test.
#
# The admin URL is never written to the config file: agents read that file
# (docs/board-v2.md section 15.4). The script sets KB_ADMIN_URL only for the
# `kb db migrate` processes it starts, and prints how to set it in a shell afterwards.
#
# Re-running never changes existing passwords or data. Passwords are taken, in order, from
# .env, then from an existing ~/.config/kb/config.toml (the admin password also from an old
# admin_url there, or from $KB_ADMIN_URL), and only generated when none has them.
#
# Options:
#   --skip-build  Do not run `cargo build --release`; use the existing target/release/kb.
#   --yes         Pass --yes to `kb db migrate --database kanban`: apply pending migrations
#                 even though the database already has boards (docs/board-v2.md section
#                 15.5). Without it, the script stops after printing the plan.
#                 kanban_test is always migrated with --yes.
#
# Portability: runs on the bash 3.2 that macOS ships and on Linux; uses only POSIX tools.

set -euo pipefail

SKIP_BUILD=0
YES=0
while [ $# -gt 0 ]; do
    case "$1" in
        --skip-build) SKIP_BUILD=1 ;;
        --yes) YES=1 ;;
        -h|--help)
            sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            printf 'error: unknown option: %s (use --skip-build, --yes or --help)\n' "$1" >&2
            exit 2
            ;;
    esac
    shift
done

ROOT=$(cd "$(dirname "$0")/.." && pwd)
ENV_FILE="$ROOT/.env"
COMPOSE_FILE="$ROOT/docker-compose.yml"
CONFIG_DIR="$HOME/.config/kb"
CONFIG_FILE="$CONFIG_DIR/config.toml"
CONTAINER='kb-postgres'
PORT=55432
# Keys in .env, in order, and the role of each.
KEYS='KB_ADMIN_PASSWORD KB_WRITER_PASSWORD KB_READER_PASSWORD'
NL=$'\n'
TMP_FILE=''

if [ -t 1 ]; then
    C_STEP=$'\033[36m'; C_DONE=$'\033[32m'; C_OFF=$'\033[0m'
else
    C_STEP=''; C_DONE=''; C_OFF=''
fi

cleanup() {
    if [ -n "$TMP_FILE" ]; then rm -f "$TMP_FILE"; fi
}
trap cleanup EXIT

step() { printf '%s==> %s%s\n' "$C_STEP" "$1" "$C_OFF"; }
info() { printf '    %s\n' "$1"; }
die() { printf 'error: %s\n' "$1" >&2; exit 1; }

role_of() {
    case "$1" in
        KB_ADMIN_PASSWORD) printf 'kb_admin' ;;
        KB_WRITER_PASSWORD) printf 'kb_writer' ;;
        KB_READER_PASSWORD) printf 'kb_reader' ;;
    esac
}

require_docker_cli() {
    if ! command -v docker >/dev/null 2>&1; then
        die 'docker is not on PATH. Install Docker Desktop (https://www.docker.com/products/docker-desktop/), start it, and run this script again.'
    fi
}

new_password() {
    # Alphanumeric only, so the password needs no escaping inside a connection URL.
    # tr drops every byte outside the 62 characters, so each kept character is uniform.
    local pw='' chunk tries=0
    while [ "${#pw}" -lt 32 ]; do
        tries=$((tries + 1))
        [ "$tries" -le 10 ] || return 1
        chunk=$(dd if=/dev/urandom bs=256 count=1 2>/dev/null | LC_ALL=C tr -dc 'A-Za-z0-9') || return 1
        pw="$pw$chunk"
    done
    printf '%s' "${pw:0:32}"
}

url_unescape() {
    # Decodes %XX sequences, like [uri]::UnescapeDataString; other text is kept as is.
    local s=$1 out='' hex ch p='%'
    while [[ $s == *"$p"* ]]; do
        out="$out${s%%"$p"*}"
        s=${s#*"$p"}
        hex=${s:0:2}
        if [[ $hex =~ ^[0-9A-Fa-f][0-9A-Fa-f]$ ]]; then
            printf -v ch '%b' "\\x$hex"
            out="$out$ch"
            s=${s:2}
        else
            out="$out$p"
        fi
    done
    printf '%s' "$out$s"
}

read_env_file() {
    # Sets ENV_<KEY> for each of the three keys found in .env; a later line wins.
    local line key value re='^[[:space:]]*([A-Z_]+)[[:space:]]*=[[:space:]]*(.*)$' first=1
    [ -f "$ENV_FILE" ] || return 0
    while IFS= read -r line || [ -n "$line" ]; do
        if [ "$first" -eq 1 ]; then line=${line#$'\xef\xbb\xbf'}; first=0; fi
        line=${line%$'\r'}
        if [[ $line =~ $re ]]; then
            key=${BASH_REMATCH[1]}
            value=${BASH_REMATCH[2]}
            value=${value%"${value##*[![:space:]]}"}
            case " $KEYS " in
                *" $key "*) printf -v "ENV_$key" '%s' "$value" ;;
            esac
        fi
    done < "$ENV_FILE"
}

read_config_passwords() {
    # Sets CFG_<KEY>. An old config file may still hold admin_url; it is only read here,
    # never written back.
    local text key role re
    if [ -f "$CONFIG_FILE" ]; then
        text=$(cat "$CONFIG_FILE")
        for key in $KEYS; do
            role=$(role_of "$key")
            re="postgres://${role}:([^@\"]+)@"
            if [[ $text =~ $re ]]; then
                printf -v "CFG_$key" '%s' "$(url_unescape "${BASH_REMATCH[1]}")"
            fi
        done
    fi
    re='^postgres(ql)?://kb_admin:([^@]+)@'
    if [ -n "${KB_ADMIN_URL:-}" ] && [ -z "${CFG_KB_ADMIN_PASSWORD+x}" ] &&
        [[ $KB_ADMIN_URL =~ $re ]]; then
        CFG_KB_ADMIN_PASSWORD=$(url_unescape "${BASH_REMATCH[2]}")
    fi
}

data_volume_exists() {
    require_docker_cli
    docker volume inspect kb-pgdata >/dev/null 2>&1
}

write_if_changed() {
    # Writes $2 to $1 when the bytes differ. Returns 0 when written, 1 when unchanged.
    # A new file is created with mode 600 (mktemp), then moved into place.
    local path=$1 content=$2 tmp
    tmp=$(mktemp "$(dirname "$path")/.kb-setup.XXXXXX") || die "cannot create a temporary file next to $path"
    TMP_FILE=$tmp
    chmod 600 "$tmp"
    printf '%s' "$content" > "$tmp"
    if [ -f "$path" ] && cmp -s "$tmp" "$path"; then
        rm -f "$tmp"; TMP_FILE=''
        return 1
    fi
    if [ -L "$path" ]; then
        # Keep a symbolic link (for example from a dotfiles manager): write through it.
        cat "$tmp" > "$path"
        rm -f "$tmp"
    else
        mv -f "$tmp" "$path"
    fi
    TMP_FILE=''
    return 0
}

db_url() { printf 'postgres://%s:%s@127.0.0.1:%s/kanban' "$1" "$2" "$PORT"; }

# --- 1. Passwords and .env ---------------------------------------------------------------
step 'Passwords (.env)'
read_env_file
read_config_passwords
for key in $KEYS; do
    env_var="ENV_$key"
    cfg_var="CFG_$key"
    if [ -n "${!env_var:-}" ]; then
        printf -v "PW_$key" '%s' "${!env_var}"
    elif [ -n "${!cfg_var:-}" ]; then
        printf -v "PW_$key" '%s' "${!cfg_var}"
        info "$key recovered from $CONFIG_FILE or KB_ADMIN_URL"
    elif [ "$key" = 'KB_ADMIN_PASSWORD' ] && data_volume_exists; then
        # The existing database only accepts its original admin password.
        die "KB_ADMIN_PASSWORD is not in $ENV_FILE, and volume kb-pgdata already exists. Restore .env, or set KB_ADMIN_URL in this shell, and run again."
    else
        new_pw=$(new_password) || die 'cannot read random bytes from /dev/urandom'
        [ "${#new_pw}" -eq 32 ] || die 'cannot read random bytes from /dev/urandom'
        printf -v "PW_$key" '%s' "$new_pw"
        info "$key generated"
    fi
done
env_text="# Generated by scripts/setup.sh. Do not commit.$NL"
for key in $KEYS; do
    pw_var="PW_$key"
    env_text="$env_text$key=${!pw_var}$NL"
done
if write_if_changed "$ENV_FILE" "$env_text"; then
    info "wrote $ENV_FILE"
else
    info "$ENV_FILE unchanged"
fi

# --- 2. Container ---------------------------------------------------------------------------
step "Container $CONTAINER"
require_docker_cli
if ! docker info --format '{{.ServerVersion}}' >/dev/null 2>&1; then
    die 'Docker engine is not reachable. Start Docker Desktop and run this script again.'
fi
if ! docker compose version >/dev/null 2>&1; then
    die "'docker compose' is not available. Update Docker Desktop (Compose v2 is included) and run this script again."
fi

docker compose --file "$COMPOSE_FILE" --env-file "$ENV_FILE" --project-directory "$ROOT" \
    up --detach --wait --wait-timeout 90 || die "docker compose up failed (exit code $?)"
health=$(docker inspect --format '{{.State.Health.Status}}' "$CONTAINER") ||
    die "docker inspect failed (exit code $?)"
[ "$health" = 'healthy' ] || die "container $CONTAINER is $health, expected healthy"
info "$CONTAINER is healthy on 127.0.0.1:$PORT"

# --- 3. Config file -------------------------------------------------------------------------
step "Config ($CONFIG_FILE)"
# No admin_url: kb ignores it, and agents read this file (docs/board-v2.md section 15.4).
config_text="# Generated by tui-kanban scripts/setup.sh.$NL"
config_text="$config_text# The admin URL is not kept here: set KB_ADMIN_URL in the shell for kb db migrate.$NL"
config_text="$config_text[database]$NL"
config_text="${config_text}writer_url = \"$(db_url kb_writer "$PW_KB_WRITER_PASSWORD")\"$NL"
config_text="${config_text}reader_url = \"$(db_url kb_reader "$PW_KB_READER_PASSWORD")\"$NL"
mkdir -p "$CONFIG_DIR"
if write_if_changed "$CONFIG_FILE" "$config_text"; then
    info "wrote $CONFIG_FILE"
else
    info "$CONFIG_FILE unchanged"
fi

# --- 4. Build kb and migrate ----------------------------------------------------------------
KB="$ROOT/target/release/kb"
if [ "$SKIP_BUILD" -eq 0 ]; then
    step 'cargo build --release'
    command -v cargo >/dev/null 2>&1 || die 'cargo is not on PATH. Install Rust (https://rustup.rs) and run this script again.'
    cargo build --release --manifest-path "$ROOT/Cargo.toml" --package kb ||
        die "cargo build failed (exit code $?)"
fi
[ -x "$KB" ] || die "$KB not found; run without --skip-build"

# KB_ADMIN_URL is passed only to the kb processes below; it is never exported.
admin_url=$(db_url kb_admin "$PW_KB_ADMIN_PASSWORD")
for database in kanban kanban_test; do
    if [ "$database" = 'kanban_test' ] || [ "$YES" -eq 1 ]; then
        step "kb db migrate --database $database --yes"
        rc=0
        KB_ADMIN_URL=$admin_url "$KB" db migrate --database "$database" --yes || rc=$?
    else
        step "kb db migrate --database $database"
        rc=0
        KB_ADMIN_URL=$admin_url "$KB" db migrate --database "$database" || rc=$?
        if [ "$rc" -ne 0 ]; then
            die "kb db migrate --database kanban stopped (see the plan above). Review it, then run: bash scripts/setup.sh --skip-build --yes"
        fi
    fi
    [ "$rc" -eq 0 ] || die "kb db migrate --database $database failed (exit code $rc)"
done
unset admin_url new_pw
# Like setup.ps1, which removes KB_ADMIN_URL from its process before this step.
unset KB_ADMIN_URL

step 'kb db status'
"$KB" db status || die "kb db status failed (exit code $?)"

printf '\n%sDone. kb is at %s.%s\n' "$C_DONE" "$KB" "$C_OFF"
printf 'Put it in ~/.cargo/bin, which is on PATH: cargo install --path crates/kb --locked\n\n'
echo 'kb db migrate (and cargo test) need KB_ADMIN_URL; it is not in the config file.'
echo "Set it only in the shell where you need it. The password is KB_ADMIN_PASSWORD in $ENV_FILE."
echo "  bash/zsh:    export KB_ADMIN_URL='postgres://kb_admin:<KB_ADMIN_PASSWORD>@127.0.0.1:$PORT/kanban'"
echo "  PowerShell:  \$env:KB_ADMIN_URL = 'postgres://kb_admin:<KB_ADMIN_PASSWORD>@127.0.0.1:$PORT/kanban'"
