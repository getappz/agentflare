#!/usr/bin/env bash
set -euxo pipefail

error() {
	echo "$@" >&2
	exit 1
}

: "${RELEASE_DIR:?RELEASE_DIR must be set}"
: "${AGENTFLARE_VERSION:?AGENTFLARE_VERSION must be set}"
NPM_PREFIX="${NPM_PREFIX:-agentflare}"
NPM_SCOPE="${NPM_SCOPE:-@getappz}"
NPM_MAIN="${NPM_MAIN:-${NPM_SCOPE}/${NPM_PREFIX}}"

mkdir -p "$RELEASE_DIR/npm"

dist_tag_from_version() {
	IFS="-" read -r -a version_split <<<"$1"
	IFS="." read -r -a version_split <<<"${version_split[1]:-latest}"
	echo "${version_split[0]}"
}
dist_tag="$(dist_tag_from_version "$AGENTFLARE_VERSION")"

# Strip leading 'v' for npm (npm versions don't allow 'v' prefix)
AGENTFLARE_NPM_VERSION="${AGENTFLARE_VERSION#v}"

# Map npm platform id → Rust target triple + archive type
# Format: npm_os-npm_arch:rust_target:archive_ext
platforms=(
	"linux-x64:x86_64-unknown-linux-gnu:tar.gz"
	"linux-arm64:aarch64-unknown-linux-gnu:tar.gz"
	"darwin-x64:x86_64-apple-darwin:tar.gz"
	"darwin-arm64:aarch64-apple-darwin:tar.gz"
	"win32-x64:x86_64-pc-windows-msvc:zip"
)

download_asset() {
	local asset="$1"
	local dest="$2"
	# Always fetch fresh: asset names carry no version, so a leftover file in
	# a reused RELEASE_DIR would silently republish the previous release's
	# binary under the new version tag.
	rm -f "$dest"
	gh release download "$AGENTFLARE_VERSION" \
		--repo getappz/agentflare \
		--pattern "$asset" \
		--dir "$RELEASE_DIR" || {
		echo "Warning: $asset not found, skipping"
		return 1
	}
}

extract_asset() {
	local archive="$1"
	local dest_dir="$2"
	case "$archive" in
	*.tar.gz)
		tar -xzf "$archive" -C "$dest_dir"
		;;
	*.zip)
		unzip -qo "$archive" -d "$dest_dir"
		;;
	*)
		error "Unknown archive format: $archive"
		;;
	esac
	# Move binary into dest_dir/bin/ if extracted at top level
	if [ -f "$dest_dir/agentflare" ] || [ -f "$dest_dir/agentflare.exe" ]; then
		mkdir -p "$dest_dir/bin"
		mv "$dest_dir"/agentflare* "$dest_dir/bin/" 2>/dev/null || true
	fi
}

skipped_platforms=()
published_platform_deps=() # "@scope/agentflare-os-arch" entries actually published, for the main package's optionalDependencies

for entry in "${platforms[@]}"; do
	IFS=":" read -r npm_plat rust_target ext <<<"$entry"
	IFS="-" read -r os arch <<<"$npm_plat"

	asset="agentflare-${rust_target}.${ext}"
	archive_path="$RELEASE_DIR/$asset"

	download_asset "$asset" "$archive_path" || {
		skipped_platforms+=("$npm_plat")
		continue
	}

	rm -rf "$RELEASE_DIR/npm"
	mkdir -p "$RELEASE_DIR/npm"

	extract_asset "$archive_path" "$RELEASE_DIR/npm"

	pkg_name="${NPM_SCOPE}/${NPM_PREFIX}-${os}-${arch}"

	# Determine binary name for the "bin" field
	bin_name="agentflare"
	if [ "$os" = "win32" ]; then
		bin_name="agentflare.exe"
	fi

	cat <<EOF >"$RELEASE_DIR/npm/package.json"
{
  "name": "$pkg_name",
  "version": "$AGENTFLARE_NPM_VERSION",
  "description": "Optimize AI CLI agents for cost and performance",
  "bin": {
    "agentflare": "bin/${bin_name}"
  },
  "repository": {
    "type": "git",
    "url": "https://github.com/getappz/agentflare"
  },
  "files": [
    "bin",
    "README.md"
  ],
  "license": "MIT",
  "os": ["$os"],
  "cpu": ["$arch"]
}
EOF

	cp README.md "$RELEASE_DIR/npm/README.md" 2>/dev/null || true

	pushd "$RELEASE_DIR/npm"
	if [ "${DRY_RUN:-0}" == 1 ]; then
		echo "DRY RUN: would publish $pkg_name@$AGENTFLARE_NPM_VERSION"
	else
		npm publish --access public --tag "$dist_tag" --provenance || {
			if npm view "$pkg_name@$AGENTFLARE_NPM_VERSION" version &>/dev/null; then
				echo "Version $AGENTFLARE_NPM_VERSION already published for $pkg_name, skipping"
			else
				echo "Failed to publish $pkg_name"
				exit 1
			fi
		}
	fi
	popd

	published_platform_deps+=("$pkg_name")
done

# Publish main wrapper package. It carries no compiled binary and no
# install-time script -- the platform packages above are declared as
# optionalDependencies (npm/yarn/pnpm resolve+fetch+verify only the one
# matching this machine's os/cpu natively, the same way they already do for
# every other dependency), and bin/agentflare.js is a small shim that, at
# *invocation* time, resolves whichever platform package actually got
# installed and execs it. No script runs at `npm install` time.
rm -rf "$RELEASE_DIR/npm"
mkdir -p "$RELEASE_DIR/npm/bin"

cp README.md "$RELEASE_DIR/npm/README.md" 2>/dev/null || true

# bin/agentflare.js -- embedded to keep it versioned with the script
cat <<'JSEOF' >"$RELEASE_DIR/npm/bin/agentflare.js"
#!/usr/bin/env node
"use strict";

var spawnSync = require("child_process").spawnSync;
var path = require("path");

// Keep in sync with the `platforms` array in scripts/release-npm.sh.
var PLATFORM_PACKAGES = {
    "linux-x64": "@getappz/agentflare-linux-x64",
    "linux-arm64": "@getappz/agentflare-linux-arm64",
    "darwin-x64": "@getappz/agentflare-darwin-x64",
    "darwin-arm64": "@getappz/agentflare-darwin-arm64",
    "win32-x64": "@getappz/agentflare-win32-x64"
};

function resolveBinary() {
    var key = process.platform + "-" + process.arch;
    var pkg = PLATFORM_PACKAGES[key];
    if (!pkg) {
        console.error("agentflare: unsupported platform " + key);
        process.exit(1);
    }

    var pkgJsonPath;
    try {
        pkgJsonPath = require.resolve(pkg + "/package.json");
    } catch (e) {
        console.error(
            "agentflare: platform package " + pkg + " is not installed.\n" +
            "This usually means your package manager skipped optionalDependencies\n" +
            "(e.g. --no-optional / --omit=optional), or " + key + " has no published build."
        );
        process.exit(1);
    }

    var pkgJson = require(pkgJsonPath);
    var binRel = typeof pkgJson.bin === "string" ? pkgJson.bin : pkgJson.bin.agentflare;
    return path.join(path.dirname(pkgJsonPath), binRel);
}

var bin = resolveBinary();
var result = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
    console.error("agentflare: failed to launch " + bin + ": " + result.error.message);
    process.exit(1);
}
if (result.signal) {
    process.kill(process.pid, result.signal);
}
process.exit(result.status === null ? 1 : result.status);

JSEOF
chmod +x "$RELEASE_DIR/npm/bin/agentflare.js"

# Build the optionalDependencies object from whichever platform packages
# actually published above (a platform whose release asset was missing gets
# neither a package nor an entry here -- see the skipped_platforms check at
# the end of this script).
optional_deps_json="{}"
if [ "${#published_platform_deps[@]}" -gt 0 ]; then
	optional_deps_entries=""
	for dep in "${published_platform_deps[@]}"; do
		optional_deps_entries+="    \"$dep\": \"$AGENTFLARE_NPM_VERSION\",\n"
	done
	optional_deps_json="{\n${optional_deps_entries%',\n'}\n  }"
fi
optional_deps_json="$(printf '%b' "$optional_deps_json")"

cat <<EOF >"$RELEASE_DIR/npm/package.json"
{
  "name": "${NPM_MAIN}",
  "description": "Optimize AI CLI agents for cost and performance",
  "version": "$AGENTFLARE_NPM_VERSION",
  "repository": {
    "type": "git",
    "url": "https://github.com/getappz/agentflare"
  },
  "files": [
    "bin",
    "README.md"
  ],
  "bin": {
    "agentflare": "bin/agentflare.js"
  },
  "optionalDependencies": $optional_deps_json,
  "license": "MIT",
  "engines": {
    "node": ">=16"
  }
}
EOF

pushd "$RELEASE_DIR/npm"
if [ "${DRY_RUN:-0}" == 1 ]; then
	echo "DRY RUN: would publish ${NPM_MAIN}@$AGENTFLARE_NPM_VERSION"
else
	npm publish --access public --tag "$dist_tag" --provenance || {
		if npm view "${NPM_MAIN}@$AGENTFLARE_NPM_VERSION" version &>/dev/null; then
			echo "Version $AGENTFLARE_NPM_VERSION already published, skipping"
		else
			echo "Failed to publish main package"
			exit 1
		fi
	}
fi
popd

# A release that quietly ships fewer platform packages than declared leaves
# those users with broken npm installs and no CI signal — fail loudly.
if [ "${#skipped_platforms[@]}" -gt 0 ]; then
	error "npm publish incomplete for v$AGENTFLARE_NPM_VERSION — missing platform assets: ${skipped_platforms[*]}"
fi

echo "npm publish complete for v$AGENTFLARE_NPM_VERSION"
