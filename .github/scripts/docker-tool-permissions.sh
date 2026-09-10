#!/bin/sh
# Run inside each image as its default user, with networking disabled.
set -eu

variant=${1:?expected runtime, dev, or heavy}
case "$variant" in
    runtime|dev|heavy) ;;
    *) exit 2 ;;
esac

test "$(id -un)" = nenjo
test "$(id -u)" = 10001
test -z "$(find "$HOME" ! -user nenjo -print -quit)"

# These remain shell variables when testing the worker's filtered environment.
PIP_CACHE_DIR=${PIP_CACHE_DIR:-$HOME/.cache/pip}
NPM_CONFIG_CACHE=${NPM_CONFIG_CACHE:-$HOME/.npm}
NPM_CONFIG_PREFIX=${NPM_CONFIG_PREFIX:-$HOME/.local}
CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}

smoke_dir=$(mktemp -d)
trap 'rm -rf "$smoke_dir"' EXIT
cd "$smoke_dir"

# Build a tiny wheel with the standard library, avoiding registry dependencies.
python3 - <<'PY'
import base64
import csv
import hashlib
import io
import zipfile

files = {
    "nenjo_pip_smoke.py": "VALUE = 'pip installation works'\n",
    "nenjo_pip_smoke-1.0.dist-info/METADATA":
        "Metadata-Version: 2.1\nName: nenjo-pip-smoke\nVersion: 1.0\n",
    "nenjo_pip_smoke-1.0.dist-info/WHEEL":
        "Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
}
record = io.StringIO()
writer = csv.writer(record)
for name, content in files.items():
    data = content.encode()
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    writer.writerow([name, f"sha256={digest}", len(data)])
writer.writerow(["nenjo_pip_smoke-1.0.dist-info/RECORD", "", ""])
files["nenjo_pip_smoke-1.0.dist-info/RECORD"] = record.getvalue()
with zipfile.ZipFile("nenjo_pip_smoke-1.0-py3-none-any.whl", "w") as wheel:
    for name, content in files.items():
        wheel.writestr(name, content)
PY

test "$(pip cache dir)" = "$PIP_CACHE_DIR"
touch "$PIP_CACHE_DIR/permission-smoke"
pip install --no-index ./nenjo_pip_smoke-1.0-py3-none-any.whl
python3 -c 'import nenjo_pip_smoke; assert nenjo_pip_smoke.VALUE == "pip installation works"'
pip uninstall -y nenjo-pip-smoke

if [ "$variant" != runtime ]; then
    test "$(npm config get cache)" = "$NPM_CONFIG_CACHE"
    test "$(npm config get prefix)" = "$NPM_CONFIG_PREFIX"
    mkdir npm-smoke
    cat > npm-smoke/package.json <<'JSON'
{"name":"nenjo-npm-smoke","version":"1.0.0","bin":{"nenjo-npm-smoke":"cli.js"}}
JSON
    cat > npm-smoke/cli.js <<'JS'
#!/usr/bin/env node
console.log('npm installation works');
JS
    chmod +x npm-smoke/cli.js
    npm pack ./npm-smoke --offline
    test "$(npx --yes --offline --package "$smoke_dir/nenjo-npm-smoke-1.0.0.tgz" nenjo-npm-smoke)" = 'npm installation works'
    npm install --global --offline ./nenjo-npm-smoke-1.0.0.tgz
    test "$(nenjo-npm-smoke)" = 'npm installation works'
    npm uninstall --global --offline nenjo-npm-smoke
    npm cache verify

    cargo init --bin --vcs none --name nenjo-cargo-smoke cargo-smoke
    cargo install --offline --path cargo-smoke
    test "$(nenjo-cargo-smoke)" = 'Hello, world!'
    cargo uninstall nenjo-cargo-smoke
    touch "$CARGO_HOME/registry/permission-smoke" "$CARGO_HOME/git/permission-smoke"
    rustup toolchain link nenjo-permission-smoke "$(rustc --print sysroot)"
    rustup toolchain uninstall nenjo-permission-smoke
fi

if [ "$variant" = heavy ]; then
    test "$(command -v agent-browser)" = "$NPM_CONFIG_PREFIX/bin/agent-browser"
    agent-browser --version
fi

test -z "$(find "$HOME" ! -user nenjo -print -quit)"
printf '%s tool permissions passed\n' "$variant"
