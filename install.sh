#!/usr/bin/env sh
# claudehud installer
# usage: curl -fsSL https://raw.githubusercontent.com/fyko/claudehud/main/install.sh | sh
#
# Re-running this script upgrades an existing install to the latest release.
# No-op if the installed version already matches the target tag.
#
# Options:
#   CLAUDEHUD_SKIP_CONFIG=1   Skip configuration of Claude Code statusline
#   CLAUDEHUD_FORCE_CONFIG=1  Override existing statusLine configuration
#                             (otherwise prompts interactively)
#   CLAUDEHUD_FORCE_INSTALL=1 Reinstall even if the target version is already present
#   CLAUDEHUD_VERSION=vX.Y.Z  Pin a specific release tag (default: latest)
#   CLAUDEHUD_INSTALL_DIR     Custom installation directory (default: ~/.local/bin)
set -eu

REPO="fyko/claudehud"
INSTALL_DIR="${CLAUDEHUD_INSTALL_DIR:-$HOME/.local/bin}"

# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[33mwarn:\033[0m %s\n' "$*" >&2; }
err() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || err "required tool not found: $1"
}

# ---------------------------------------------------------------------------
# detect platform
# ---------------------------------------------------------------------------

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Darwin)
            case "$arch" in
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                x86_64)        echo "x86_64-apple-darwin" ;;
                *)             err "unsupported macOS architecture: $arch" ;;
            esac
            ;;
        Linux)
            case "$arch" in
                x86_64)        echo "x86_64-unknown-linux-musl" ;;
                aarch64|arm64) echo "aarch64-unknown-linux-musl" ;;
                *)             err "unsupported Linux architecture: $arch" ;;
            esac
            ;;
        *)
            err "unsupported OS: $os"
            ;;
    esac
}

# ---------------------------------------------------------------------------
# fetch latest release tag from github
# ---------------------------------------------------------------------------

latest_tag() {
    url="https://api.github.com/repos/${REPO}/releases/latest"
    # try curl, fall back to wget
    if command -v curl >/dev/null 2>&1; then
        response="$(curl -fsSL "$url")"
    elif command -v wget >/dev/null 2>&1; then
        response="$(wget -qO- "$url")"
    else
        err "neither curl nor wget found"
    fi

    tag="$(printf '%s' "$response" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')"
    [ -n "$tag" ] || err "could not determine latest release tag (is the repo public with at least one release?)"
    echo "$tag"
}

# ---------------------------------------------------------------------------
# download a file, curl or wget
# ---------------------------------------------------------------------------

download() {
    url="$1"
    dest="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$url" -o "$dest"
    else
        wget -qO "$dest" "$url"
    fi
}

# ---------------------------------------------------------------------------
# configure claude code statusline
# ---------------------------------------------------------------------------

# set $1's top-level .statusLine to claudehud's command. assignment works
# whether the key exists or not. returns 0 on success, 1 if no JSON tool is
# available. sed would corrupt files with nested objects (every `}` at EOL
# matches), so we require a real JSON parser.
set_statusline() {
    _settings="$1"
    _tmp="$(mktemp)"
    _rc=1
    if command -v jq >/dev/null 2>&1; then
        jq --arg cmd "$INSTALL_DIR/claudehud" '.statusLine = {command: $cmd}' \
            "$_settings" > "$_tmp" && mv "$_tmp" "$_settings" && _rc=0
    elif command -v python3 >/dev/null 2>&1; then
        if CLAUDEHUD_CMD="$INSTALL_DIR/claudehud" python3 - "$_settings" "$_tmp" <<'PY'
import json, os, sys
src, dst = sys.argv[1], sys.argv[2]
with open(src) as f:
    data = json.load(f)
data["statusLine"] = {"command": os.environ["CLAUDEHUD_CMD"]}
with open(dst, "w") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
PY
        then
            mv "$_tmp" "$_settings" && _rc=0
        fi
    else
        warn "neither jq nor python3 found — cannot safely edit $_settings"
        printf '       manually set statusLine.command to: %s/claudehud\n' "$INSTALL_DIR" >&2
    fi
    rm -f "$_tmp"
    return "$_rc"
}

configure_claude() {
    [ -n "${CLAUDEHUD_SKIP_CONFIG:-}" ] && {
        say "skipping Claude Code configuration (CLAUDEHUD_SKIP_CONFIG is set)"
        return 0
    }

    settings="$HOME/.claude/settings.json"

    [ -d "$HOME/.claude" ] || return 0

    if [ ! -f "$settings" ]; then
        printf '{\n  "statusLine": {\n    "command": "%s/claudehud"\n  }\n}\n' \
            "$INSTALL_DIR" > "$settings"
        say "created $settings with statusLine config"
        return
    fi

    if ! grep -q '"statusLine"' "$settings" 2>/dev/null; then
        set_statusline "$settings" && say "added statusLine to $settings"
        return
    fi

    if [ -n "${CLAUDEHUD_FORCE_CONFIG:-}" ]; then
        set_statusline "$settings" && say "updated statusLine in $settings"
        return
    fi

    printf '\033[33m%s\033[0m already has a statusLine configuration.\n' "$settings"
    printf 'Do you want to override it with claudehud? [y/N] '
    read -r response || true  # EOF (non-interactive pipe) shouldn't abort install
    case "$response" in
        [yY][eE][sS]|[yY])
            set_statusline "$settings" && say "updated statusLine in $settings"
            ;;
        *)
            say "skipping statusLine configuration"
            ;;
    esac
}

# ---------------------------------------------------------------------------
# start daemon
# ---------------------------------------------------------------------------

start_daemon_macos() {
    plist_dir="$HOME/Library/LaunchAgents"
    plist="$plist_dir/com.claudehud.daemon.plist"
    mkdir -p "$plist_dir"
    cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.claudehud.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>${INSTALL_DIR}/claudehud-daemon</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/tmp/claudehud-daemon.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/claudehud-daemon.err</string>
</dict>
</plist>
EOF
    # unload first in case it was already registered (ignore errors)
    launchctl unload "$plist" 2>/dev/null || true
    launchctl load "$plist"
    say "daemon registered via launchd (com.claudehud.daemon)"
}

start_daemon_linux() {
    svc_dir="$HOME/.config/systemd/user"
    svc="$svc_dir/claudehud-daemon.service"
    mkdir -p "$svc_dir"
    cat > "$svc" <<EOF
[Unit]
Description=claudehud git cache daemon

[Service]
ExecStart=${INSTALL_DIR}/claudehud-daemon
Restart=always

[Install]
WantedBy=default.target
EOF
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user daemon-reload
        systemctl --user enable claudehud-daemon
        # `restart` starts the unit if inactive, or restarts it (picking up a
        # freshly-installed binary) if already running. covers both fresh
        # install and in-place upgrade.
        systemctl --user restart claudehud-daemon
        say "daemon enabled + restarted via systemd (claudehud-daemon.service)"
    else
        say "wrote $svc — run: systemctl --user enable --now claudehud-daemon"
    fi
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

main() {
    need uname

    target="$(detect_target)"
    say "detected target: $target"

    if [ -n "${CLAUDEHUD_VERSION:-}" ]; then
        tag="$CLAUDEHUD_VERSION"
        say "using pinned version $tag (CLAUDEHUD_VERSION)"
    else
        say "fetching latest release tag..."
        tag="$(latest_tag)"
    fi

    # strip leading 'v' so "v0.1.0" compares equal to "0.1.0" from --version
    tag_ver="${tag#v}"

    # up-to-date short-circuit: if the installed binary already matches the
    # target tag, skip the download. still re-run configure_claude +
    # start_daemon so users who ran with CLAUDEHUD_SKIP_CONFIG the first time
    # can pick up statusline config on a later run. set CLAUDEHUD_FORCE_INSTALL
    # to bypass.
    if [ -z "${CLAUDEHUD_FORCE_INSTALL:-}" ] && [ -x "$INSTALL_DIR/claudehud" ]; then
        installed_ver="$("$INSTALL_DIR/claudehud" --version 2>/dev/null | awk '{print $2}')"
        if [ -n "$installed_ver" ] && [ "$installed_ver" = "$tag_ver" ]; then
            say "claudehud $installed_ver is already up to date"
            say "(set CLAUDEHUD_FORCE_INSTALL=1 to reinstall)"
            return 0
        fi
        if [ -n "$installed_ver" ]; then
            say "upgrading claudehud $installed_ver → $tag_ver"
        else
            say "installing claudehud $tag"
        fi
    else
        say "installing claudehud $tag"
    fi

    [ -n "${CLAUDEHUD_SKIP_CONFIG:-}" ] && say "CLAUDEHUD_SKIP_CONFIG set — will not configure Claude Code statusline"
    [ -n "${CLAUDEHUD_FORCE_CONFIG:-}" ] && say "CLAUDEHUD_FORCE_CONFIG set — will override existing statusLine config if present"

    # ensure install dir exists and is in PATH
    mkdir -p "$INSTALL_DIR"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    base_url="https://github.com/${REPO}/releases/download/${tag}"

    for bin in claudehud claudehud-daemon; do
        url="${base_url}/${bin}-${target}"
        dest="${tmpdir}/${bin}"
        say "downloading $bin..."
        download "$url" "$dest"
        chmod +x "$dest"
        mv "$dest" "${INSTALL_DIR}/${bin}"
    done

    say "installed to $INSTALL_DIR"

    configure_claude

    case "$(uname -s)" in
        Darwin) start_daemon_macos ;;
        Linux)  start_daemon_linux ;;
    esac

    # PATH hint
    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            printf '\n\033[33mhint:\033[0m %s is not in your PATH.\n' "$INSTALL_DIR"
            printf 'add this to your shell rc:\n\n  export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
            ;;
    esac

    say "done. claudehud is ready."
}

main "$@"
