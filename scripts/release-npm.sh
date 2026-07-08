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
done

# Publish main wrapper package
rm -rf "$RELEASE_DIR/npm"
mkdir -p "$RELEASE_DIR/npm"

cp README.md "$RELEASE_DIR/npm/README.md" 2>/dev/null || true

# installArchSpecificPackage.js — embedded to keep it versioned with the script
cat <<'JSEOF' >"$RELEASE_DIR/npm/installArchSpecificPackage.js"
var spawn = require('child_process').spawn;
var path = require('path');
var fs = require('fs');

function installArchSpecificPackage(version) {
    process.env.npm_config_global = 'false';

    var platform = process.platform;
    var arch = process.arch;

    var pkg = ['@getappz', 'agentflare', platform, arch].join('-');
    console.log('Installing platform-specific package:', pkg + '@' + version);

    var cp = spawn(platform === 'win32' ? 'npm.cmd' : 'npm', ['install', '--no-save', pkg + '@' + version], {
        stdio: 'inherit',
        shell: true
    });

    cp.on('close', function(code) {
        if (code !== 0) {
            return process.exit(code);
        }

        var pkgJson;
        try {
            pkgJson = require.resolve(pkg + '/package.json');
        } catch (e) {
            console.error('Failed to resolve platform package:', pkg);
            return process.exit(1);
        }

        var subpkg = JSON.parse(fs.readFileSync(pkgJson, 'utf8'));
        var executable = subpkg.bin.agentflare;
        var bin = path.resolve(path.dirname(pkgJson), executable);

        try {
            fs.mkdirSync(path.resolve(process.cwd(), 'bin'));
        } catch (e) {
            if (e.code !== 'EEXIST') {
                throw e;
            }
        }

        linkSync(bin, path.resolve(process.cwd(), executable));

        if (platform === 'win32') {
            var mainPkg = JSON.parse(fs.readFileSync(path.resolve(process.cwd(), 'package.json'), 'utf8'));
            fs.writeFileSync(path.resolve(process.cwd(), 'bin/agentflare'), 'placeholder');
            mainPkg.bin.agentflare = 'bin/agentflare.exe';
            fs.writeFileSync(path.resolve(process.cwd(), 'package.json'), JSON.stringify(mainPkg, null, 2));
        }

        return process.exit(0);
    });
}

function linkSync(src, dest) {
    try {
        fs.unlinkSync(dest);
    } catch (e) {
        if (e.code !== 'ENOENT') {
            throw e;
        }
    }
    return fs.linkSync(src, dest);
}

var pjson = require('./package.json');
installArchSpecificPackage(pjson.version);
JSEOF

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
    "installArchSpecificPackage.js",
    "README.md"
  ],
  "scripts": {
    "prepack": "rm -rf bin",
    "preinstall": "node installArchSpecificPackage.js"
  },
  "bin": {
    "agentflare": "./bin/agentflare"
  },
  "license": "MIT",
  "engines": {
    "node": ">=5.0.0"
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
