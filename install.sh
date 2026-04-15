#!/usr/bin/env sh
# claudehud installer
# usage: curl -fsSL https://raw.githubusercontent.com/fyko/claudehud/main/install.sh | sh
set -eu

REPO="fyko/claudehud"
INSTALL_DIR="${CLAUDEHUD_INSTALL_DIR:-$HOME/.local/bin}"

# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
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

configure_claude() {
    settings="$HOME/.claude/settings.json"

    # only touch the file if claude settings dir exists or user ran claude already
    [ -d "$HOME/.claude" ] || return 0

    if [ ! -f "$settings" ]; then
        printf '{\n  "statusLine": {\n    "command": "%s/claudehud"\n  }\n}\n' \
            "$INSTALL_DIR" > "$settings"
        say "created $settings with statusLine config"
        return
    fi

    # already has statusLine key — don't stomp it
    if grep -q '"statusLine"' "$settings" 2>/dev/null; then
        say "~/.claude/settings.json already has statusLine — skipping (edit manually if needed)"
        return
    fi

    # inject before the last closing brace
    tmp="$(mktemp)"
    sed 's/}[[:space:]]*$/,\n  "statusLine": {\n    "command": "'"$INSTALL_DIR"'\/claudehud"\n  }\n}/' \
        "$settings" > "$tmp" && mv "$tmp" "$settings"
    say "added statusLine to $settings"
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
        systemctl --user enable --now claudehud-daemon
        say "daemon enabled via systemd (claudehud-daemon.service)"
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

    say "fetching latest release tag..."
    tag="$(latest_tag)"
    say "installing claudehud $tag"

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
