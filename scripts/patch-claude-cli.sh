#!/usr/bin/env bash
# patch-claude-cli.sh
#
# Patches the Claude CLI's system prompt identity strings so that
# third-party API providers (e.g. Zhipu AI) that only accept the standard
# "You are Claude Code, Anthropic's official CLI for Claude." prefix
# will not reject requests with error 1302.
#
# Root cause: When Claude CLI runs in isNonInteractive mode (triggered by
# --input-format stream-json which TOKENICODE always uses), gv8() returns
# feq or Zeq instead of Fh1. Zhipu rejects feq and Zeq.
#
# Fix: Set feq and Zeq to the same value as Fh1.
#
# Idempotent: safe to run multiple times.

set -euo pipefail

# ── Locate cli.js ──────────────────────────────────────────────────────────────

CLAUDE_BIN=$(which claude 2>/dev/null || true)
if [[ -z "$CLAUDE_BIN" ]]; then
    echo "ERROR: 'claude' not found in PATH. Install @anthropic-ai/claude-code first." >&2
    exit 1
fi

# Resolve symlinks to find the real binary
CLAUDE_REAL=$(realpath "$CLAUDE_BIN" 2>/dev/null || readlink -f "$CLAUDE_BIN")
CLAUDE_DIR=$(dirname "$CLAUDE_REAL")

# cli.js lives alongside the binary, or up one level in the package
if [[ -f "$CLAUDE_DIR/cli.js" ]]; then
    CLI_JS="$CLAUDE_DIR/cli.js"
elif [[ -f "$CLAUDE_DIR/../cli.js" ]]; then
    CLI_JS=$(realpath "$CLAUDE_DIR/../cli.js")
else
    # Fallback: search node_modules
    CLI_JS=$(find "$(npm root -g 2>/dev/null || echo /usr/local/lib/node_modules)" \
        -name "cli.js" -path "*/@anthropic-ai/claude-code/*" 2>/dev/null | head -1 || true)
    if [[ -z "$CLI_JS" ]]; then
        echo "ERROR: Could not locate cli.js for @anthropic-ai/claude-code." >&2
        exit 1
    fi
fi

echo "Target: $CLI_JS"

# ── Check / Apply patch ────────────────────────────────────────────────────────

OLD_SNIPPET='feq="You are Claude Code, Anthropic'"'"'s official CLI for Claude, running within the Claude Agent SDK.",Zeq="You are a Claude agent, built on Anthropic'"'"'s Claude Agent SDK."'
NEW_SNIPPET='feq="You are Claude Code, Anthropic'"'"'s official CLI for Claude.",Zeq="You are Claude Code, Anthropic'"'"'s official CLI for Claude."'
ALREADY_PATCHED='feq="You are Claude Code, Anthropic'"'"'s official CLI for Claude.",Zeq="You are Claude Code, Anthropic'"'"'s official CLI for Claude."'

python3 - "$CLI_JS" "$OLD_SNIPPET" "$NEW_SNIPPET" "$ALREADY_PATCHED" <<'PYEOF'
import sys

cli_path   = sys.argv[1]
old_str    = sys.argv[2]
new_str    = sys.argv[3]
patched_str = sys.argv[4]

with open(cli_path, 'r', encoding='utf-8') as f:
    content = f.read()

# Already patched?
if old_str not in content:
    if patched_str in content:
        print("Already patched — no changes needed.")
        sys.exit(0)
    else:
        print("ERROR: Neither original nor patched string found. The CLI version may have changed.", file=sys.stderr)
        sys.exit(1)

# Verify uniqueness before patching
count = content.count(old_str)
if count != 1:
    print(f"ERROR: Expected exactly 1 occurrence of the target string, found {count}.", file=sys.stderr)
    sys.exit(1)

new_content = content.replace(old_str, new_str, 1)

with open(cli_path, 'w', encoding='utf-8') as f:
    f.write(new_content)

# Verify write
with open(cli_path, 'r', encoding='utf-8') as f:
    verify = f.read()

if new_str in verify and old_str not in verify:
    print("Patch applied successfully.")
else:
    print("ERROR: Patch verification failed after write.", file=sys.stderr)
    sys.exit(1)
PYEOF
