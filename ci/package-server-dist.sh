#!/usr/bin/env bash
set -euo pipefail

usage() {
	printf 'usage: %s INSTALL_PREFIX TARGET OUTPUT_DIR\n' "$0" >&2
}

if [[ $# -ne 3 ]]; then
	usage
	exit 2
fi

INSTALL_PREFIX="$1"
TARGET="$2"
OUTPUT_DIR="$3"
PACKAGE_NAME="postgres-fastfork-${TARGET}"

if [[ ! -d "$INSTALL_PREFIX" ]]; then
	printf 'install prefix does not exist: %s\n' "$INSTALL_PREFIX" >&2
	exit 1
fi

case "$TARGET" in
	linux-x86_64|linux-aarch64|macos-aarch64)
		;;
	*)
		printf 'unsupported release target: %s\n' "$TARGET" >&2
		exit 1
		;;
esac

mkdir -p "$OUTPUT_DIR"
INSTALL_PREFIX="$(cd "$INSTALL_PREFIX" && pwd)"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
PACKAGE_DIR="$OUTPUT_DIR/$PACKAGE_NAME"
ARCHIVE="$OUTPUT_DIR/$PACKAGE_NAME.tar.gz"
CHECKSUM="$ARCHIVE.sha256"

rm -rf "$PACKAGE_DIR" "$ARCHIVE" "$CHECKSUM"
mkdir -p "$PACKAGE_DIR/bin" "$PACKAGE_DIR/lib" "$PACKAGE_DIR/share"

copy_required_binary() {
	local binary="$1"
	local source="$INSTALL_PREFIX/bin/$binary"

	if [[ ! -e "$source" ]]; then
		printf 'missing required binary: %s\n' "$source" >&2
		exit 1
	fi

	cp -pP "$source" "$PACKAGE_DIR/bin/"
}

copy_optional_binary() {
	local binary="$1"
	local source="$INSTALL_PREFIX/bin/$binary"

	if [[ -e "$source" ]]; then
		cp -pP "$source" "$PACKAGE_DIR/bin/"
	fi
}

copy_required_tree() {
	local relative_path="$1"
	local source="$INSTALL_PREFIX/$relative_path"
	local destination="$PACKAGE_DIR/$relative_path"

	if [[ ! -d "$source" ]]; then
		printf 'missing required runtime tree: %s\n' "$source" >&2
		exit 1
	fi

	rm -rf "$destination"
	mkdir -p "$(dirname "$destination")"
	cp -Rp "$source" "$destination"
}

# Keep the executable surface intentionally small: enough to initialize, run,
# and stop a fast-fork server without shipping client suites, benchmarks,
# headers, documentation, or developer tooling.
copy_required_binary initdb
copy_required_binary pg_ctl
copy_required_binary postgres
copy_optional_binary postmaster
copy_optional_binary pg_isready

copy_required_tree share
rm -rf "$PACKAGE_DIR/share/doc" "$PACKAGE_DIR/share/man"

if [[ -d "$INSTALL_PREFIX/lib" ]]; then
	while IFS= read -r -d '' library; do
		cp -pP "$library" "$PACKAGE_DIR/lib/"
	done < <(
		find "$INSTALL_PREFIX/lib" -maxdepth 1 \( -type f -o -type l \) \
			\( -name '*.so' -o -name '*.so.*' -o -name '*.dylib' \) -print0
	)

	if [[ -d "$INSTALL_PREFIX/lib/postgresql" ]]; then
		cp -Rp "$INSTALL_PREFIX/lib/postgresql" "$PACKAGE_DIR/lib/postgresql"
		rm -rf "$PACKAGE_DIR/lib/postgresql/pgxs"
	fi
fi

cat > "$PACKAGE_DIR/README.fastfork.txt" <<EOF
This archive contains a minimal PostgreSQL fast-fork server runtime for $TARGET.

Included:
- bin/initdb
- bin/pg_ctl
- bin/postgres
- bin/postmaster, when installed
- bin/pg_isready, when installed
- server runtime libraries
- share runtime data, including timezone and initdb files

Intentionally omitted:
- source code
- benchmark outputs and build directories
- headers and PGXS files
- documentation and manpages
- client and backup utilities such as psql, pg_dump, and pg_basebackup
EOF

rewrite_darwin_install_names() {
	local path
	local dep
	local base
	local replacement

	if [[ "$TARGET" != macos-* ]]; then
		return
	fi
	if ! command -v install_name_tool >/dev/null 2>&1 || ! command -v otool >/dev/null 2>&1; then
		printf 'install_name_tool and otool are required for macOS packaging\n' >&2
		exit 1
	fi

	while IFS= read -r -d '' path; do
		install_name_tool -id "@loader_path/$(basename "$path")" "$path" 2>/dev/null || true
	done < <(find "$PACKAGE_DIR/lib" -maxdepth 1 -type f -name '*.dylib' -print0)

	while IFS= read -r -d '' path; do
		while IFS= read -r dep; do
			base="$(basename "$dep")"
			if [[ ! -f "$PACKAGE_DIR/lib/$base" ]]; then
				continue
			fi

			case "$path" in
				"$PACKAGE_DIR/bin/"*)
					replacement="@loader_path/../lib/$base"
					;;
				"$PACKAGE_DIR/lib/postgresql/"*)
					replacement="@loader_path/../$base"
					;;
				"$PACKAGE_DIR/lib/"*)
					replacement="@loader_path/$base"
					;;
				*)
					continue
					;;
			esac

			install_name_tool -change "$dep" "$replacement" "$path" 2>/dev/null || true
		done < <(otool -L "$path" 2>/dev/null | awk 'NR > 1 {print $1}')
	done < <(
		find "$PACKAGE_DIR/bin" "$PACKAGE_DIR/lib" \
			\( -type f -o -type l \) \
			\( -perm -111 -o -name '*.dylib' -o -name '*.so' -o -name '*.so.*' \) \
			-print0
	)
}

rewrite_linux_rpaths() {
	local path
	local rpath

	if [[ "$TARGET" != linux-* ]]; then
		return
	fi
	if ! command -v patchelf >/dev/null 2>&1; then
		printf 'patchelf is required for Linux packaging\n' >&2
		exit 1
	fi

	while IFS= read -r -d '' path; do
		if ! file "$path" | grep -q 'ELF'; then
			continue
		fi

		case "$path" in
			"$PACKAGE_DIR/bin/"*)
				rpath='$ORIGIN/../lib'
				;;
			"$PACKAGE_DIR/lib/postgresql/"*)
				rpath='$ORIGIN/..'
				;;
			"$PACKAGE_DIR/lib/"*)
				rpath='$ORIGIN'
				;;
			*)
				continue
				;;
		esac

		patchelf --set-rpath "$rpath" "$path" 2>/dev/null || true
	done < <(
		find "$PACKAGE_DIR/bin" "$PACKAGE_DIR/lib" \
			-type f \
			\( -perm -111 -o -name '*.so' -o -name '*.so.*' \) \
			-print0
	)
}

rewrite_darwin_install_names
rewrite_linux_rpaths

tar -C "$OUTPUT_DIR" -czf "$ARCHIVE" "$PACKAGE_NAME"
(
	cd "$OUTPUT_DIR"
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$(basename "$ARCHIVE")" > "$(basename "$CHECKSUM")"
	else
		shasum -a 256 "$(basename "$ARCHIVE")" > "$(basename "$CHECKSUM")"
	fi
)

printf '%s\n' "$ARCHIVE"
