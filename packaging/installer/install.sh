#!/bin/sh
# LynShen CLI installer for macOS and Linux.
#
#   curl -fsSL https://software.lynshen.org/cli/install.sh | bash
#
# Downloads the `lynshen` binary for this computer from the LynShen software
# library, checks its sha256 against the release's SHA256SUMS, installs it in
# ~/.local/bin and adds that directory to PATH in your shell's startup file
# (once, between "lynshen installer" marker lines). Never uses sudo.
#
# Options (when piped, pass them after `bash -s --`):
#   --version X.Y.Z    install this version instead of the latest
#   --upgrade          install the latest version over the current one
#                      (the same as running the installer again)
#   --uninstall        remove lynshen and the PATH lines this installer added;
#                      settings and sessions in ~/.lynshen stay
#   --no-modify-path   leave shell startup files alone
#
# Environment:
#   LYNSHEN_VERSION        same as --version
#   LYNSHEN_INSTALL_DIR    install here instead of ~/.local/bin
#   LYNSHEN_DOWNLOAD_BASE  software library URL (default
#                          https://software.lynshen.org/cli)
#
# Not ~/.lynshen/bin: LynShen Desktop keeps its own copy there and updates it
# with the app.
#
# The whole script is one function called on the last line, so a download
# cut short runs nothing.

main() {
	set -eu
	base=${LYNSHEN_DOWNLOAD_BASE:-https://software.lynshen.org/cli}
	base=${base%/}
	version=${LYNSHEN_VERSION:-}
	action=install
	modify_path=1
	while [ $# -gt 0 ]; do
		case $1 in
		--version)
			[ $# -ge 2 ] || fail "--version needs a version, for example --version 0.8.11"
			version=$2
			shift
			;;
		--version=*) version=${1#*=} ;;
		--upgrade) action=install ;;
		--uninstall) action=uninstall ;;
		--no-modify-path) modify_path=0 ;;
		-h | --help)
			usage
			return 0
			;;
		*) fail "unknown option $1 (see --help)" ;;
		esac
		shift
	done
	[ -n "${HOME:-}" ] || fail "HOME is not set"
	install_dir=${LYNSHEN_INSTALL_DIR:-$HOME/.local/bin}
	install_dir=${install_dir%/}
	case $install_dir in
	/*) ;;
	*) fail "LYNSHEN_INSTALL_DIR must be an absolute path" ;;
	esac
	if [ "$action" = uninstall ]; then
		remove_lynshen
	else
		install_lynshen
	fi
}

usage() {
	cat <<'EOF'
LynShen CLI installer

  curl -fsSL https://software.lynshen.org/cli/install.sh | bash
  curl -fsSL https://software.lynshen.org/cli/install.sh | bash -s -- [options]

Options:
  --version X.Y.Z    install this version instead of the latest
  --upgrade          install the latest version over the current one
  --uninstall        remove lynshen and the PATH lines this installer added
  --no-modify-path   leave shell startup files alone

Environment: LYNSHEN_VERSION, LYNSHEN_INSTALL_DIR (default ~/.local/bin),
LYNSHEN_DOWNLOAD_BASE (default https://software.lynshen.org/cli).
EOF
}

say() {
	printf '%s\n' "$*"
}

fail() {
	printf 'lynshen installer: %s\n' "$*" >&2
	exit 1
}

# The release file for this computer, as `lynshen-<target>`.
detect_file() {
	os=$(uname -s)
	arch=$(uname -m)
	case $os in
	Darwin)
		# A shell running under Rosetta reports x86_64 on Apple silicon.
		if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
			arch=arm64
		fi
		case $arch in
		arm64 | aarch64) target=aarch64-apple-darwin ;;
		x86_64) target=x86_64-apple-darwin ;;
		*) fail "there is no lynshen for macOS on $arch" ;;
		esac
		;;
	Linux)
		case $arch in
		x86_64 | amd64) ;;
		*) fail "there is no lynshen for Linux on $arch yet; only x86_64" ;;
		esac
		case $(ldd --version 2>&1 || true) in
		*musl*) fail "lynshen for Linux needs glibc; this system uses musl (Alpine?)" ;;
		esac
		target=x86_64-unknown-linux-gnu
		;;
	MINGW* | MSYS* | CYGWIN*)
		fail "on Windows, run this in PowerShell: irm https://software.lynshen.org/cli/install.ps1 | iex"
		;;
	*) fail "there is no lynshen for $os" ;;
	esac
	file=lynshen-$target
}

# fetch <url> <output file>: a progress bar only for the binary, and only
# when someone is watching.
fetch() {
	if [ "${3:-}" = progress ] && [ -t 2 ]; then
		curl -fL --retry 2 --progress-bar -o "$2" "$1"
	else
		curl -fsSL --retry 2 -o "$2" "$1"
	fi
}

sha256_of() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d ' ' -f 1
	else
		shasum -a 256 "$1" | cut -d ' ' -f 1
	fi
}

install_lynshen() {
	command -v curl >/dev/null 2>&1 || fail "curl is required"
	detect_file
	scratch=$(mktemp -d 2>/dev/null || mktemp -d -t lynshen)
	partial=""
	trap 'rm -rf "$scratch"; [ -z "$partial" ] || rm -f "$partial"' EXIT
	trap 'exit 130' INT TERM

	version=${version#v}
	if [ -z "$version" ]; then
		fetch "$base/latest.json" "$scratch/latest.json" ||
			fail "could not read $base/latest.json"
		# The signed manifest carries the release as an escaped JSON string;
		# drop the escapes and read its version.
		version=$(tr -d '\\' <"$scratch/latest.json" | sed -n 's/.*"version": *"\([0-9][0-9.]*\)".*/\1/p' | head -n 1)
		tr -d '\\' <"$scratch/latest.json" | grep -q '"product": *"cli"' ||
			fail "$base/latest.json is not a LynShen CLI release"
	fi
	printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
		fail "not a version: '$version' (expected something like 0.8.11)"

	fetch "$base/$version/SHA256SUMS" "$scratch/SHA256SUMS" ||
		fail "could not read $base/$version/SHA256SUMS: no such version, or a version released before this installer (pick a newer one)"
	expected=$(awk -v name="$file" '$2 == name || $2 == "*" name { print $1 }' "$scratch/SHA256SUMS")
	printf '%s\n' "$expected" | grep -Eq '^[0-9a-f]{64}$' ||
		fail "release $version has no checksum for $file"

	mkdir -p "$install_dir" || fail "cannot create $install_dir"
	[ -w "$install_dir" ] || fail "cannot write to $install_dir (set LYNSHEN_INSTALL_DIR to a folder you own)"
	dest=$install_dir/lynshen
	# Download next to the destination, so the final step is a rename.
	partial=$install_dir/.lynshen.download.$$
	say "Downloading lynshen $version ($file)"
	fetch "$base/$version/$file" "$partial" progress || fail "download failed: $base/$version/$file"
	actual=$(sha256_of "$partial")
	[ "$actual" = "$expected" ] ||
		fail "checksum mismatch for $file (expected $expected, got $actual); nothing was installed"
	chmod 755 "$partial"
	reported=$("$partial" --version </dev/null 2>/dev/null) || {
		[ "$os" != Linux ] || fail "the downloaded lynshen does not run here; it needs glibc 2.34 or newer (Ubuntu 22.04, Debian 12, RHEL 9 or later)"
		fail "the downloaded lynshen does not run on this computer"
	}
	[ "${reported##* }" = "$version" ] || fail "the downloaded binary reports '$reported', not $version"
	# LynShen Desktop's "use lynshen in a terminal" puts a link to its own
	# copy here; the downloaded one replaces the link, never Desktop's copy.
	if [ -L "$dest" ]; then
		say "Replacing the link $dest -> $(readlink "$dest" || true)"
	fi
	mv -f "$partial" "$dest"
	partial=""
	say "Installed lynshen $version at $dest"

	if [ "$os" = Linux ] && ! command -v bwrap >/dev/null 2>&1; then
		if command -v apt-get >/dev/null 2>&1; then
			get="sudo apt install bubblewrap"
		elif command -v dnf >/dev/null 2>&1; then
			get="sudo dnf install bubblewrap"
		elif command -v pacman >/dev/null 2>&1; then
			get="sudo pacman -S bubblewrap"
		else
			get="your package manager"
		fi
		say "Note: lynshen runs shell commands in a bubblewrap sandbox; install it with $get"
	fi

	other=$(command -v lynshen 2>/dev/null || true)
	if on_path "$install_dir"; then
		if [ -n "$other" ] && [ "$other" != "$dest" ]; then
			say "Note: another lynshen comes first on your PATH: $other"
		fi
		say "Next: run lynshen"
		return 0
	fi
	if [ "$modify_path" = 0 ]; then
		say "Next: add $install_dir to your PATH, then run lynshen"
		return 0
	fi
	add_to_path
}

on_path() {
	case ":$PATH:" in
	*":$1:"*) return 0 ;;
	esac
	return 1
}

# The directory as written into startup files: $HOME-relative when it can be.
rc_dir() {
	case $install_dir in
	"$HOME"/*) dir_expr="\$HOME/${install_dir#"$HOME"/}" ;;
	*) dir_expr=$install_dir ;;
	esac
	case $install_dir in
	*[\"\\\`\$]* | *"
"*) return 1 ;;
	esac
}

bash_login_file() {
	for candidate in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
		if [ -e "$candidate" ]; then
			printf '%s\n' "$candidate"
			return 0
		fi
	done
	printf '%s\n' "$HOME/.bash_profile"
}

marker_start='# >>> lynshen installer >>>'
marker_end='# <<< lynshen installer <<<'

# append_block <file> sh|fish: adds the PATH block unless the file has it.
append_block() {
	if [ -f "$1" ] && grep -qF "$marker_start" "$1"; then
		return 0
	fi
	mkdir -p "$(dirname "$1")" 2>/dev/null || true
	if [ "$2" = fish ]; then
		line="contains -- \"$dir_expr\" \$PATH; or set -gx PATH \"$dir_expr\" \$PATH"
	else
		line="case \":\$PATH:\" in *\":$dir_expr:\"*) ;; *) export PATH=\"$dir_expr:\$PATH\" ;; esac"
	fi
	{
		printf '\n%s\n' "$marker_start"
		printf '%s\n' "$line"
		printf '%s\n' "$marker_end"
	} >>"$1" || return 1
	changed="$changed $(pretty "$1")"
}

pretty() {
	case $1 in
	"$HOME"/*) printf '~/%s' "${1#"$HOME"/}" ;;
	*) printf '%s' "$1" ;;
	esac
}

add_to_path() {
	shell_name=$(basename "${SHELL:-sh}")
	changed=""
	if ! rc_dir; then
		say "Next: add $install_dir to your PATH, then run lynshen"
		return 0
	fi
	case $shell_name in
	zsh)
		rc=${ZDOTDIR:-$HOME}/.zshrc
		append_block "$rc" sh || rc=""
		;;
	bash)
		rc=$HOME/.bashrc
		append_block "$rc" sh || rc=""
		# Login shells (macOS Terminal, ssh) read one of these instead.
		append_block "$(bash_login_file)" sh || true
		;;
	fish)
		rc=${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish
		append_block "$rc" fish || rc=""
		;;
	*) rc="" ;;
	esac
	if [ -z "$rc" ]; then
		say "Next: add $install_dir to your PATH (for example in your shell's startup file), then run lynshen"
		return 0
	fi
	[ -z "$changed" ] || say "Added $install_dir to PATH in:$changed"
	say "Next: open a new terminal (or run: source $(pretty "$rc")), then run lynshen"
}

# remove_block <file>: drops the marker block, keeping the file itself (and
# any symlink to it) in place.
remove_block() {
	[ -f "$1" ] && grep -qF "$marker_start" "$1" || return 0
	kept=$(mktemp)
	awk -v start="$marker_start" -v end="$marker_end" '
		$0 == start { skip = 1; blank = 0; next }
		skip { if ($0 == end) skip = 0; next }
		blank { print ""; blank = 0 }
		$0 == "" { blank = 1; next }
		{ print }
		END { if (blank) print "" }
	' "$1" >"$kept"
	cat "$kept" >"$1"
	rm -f "$kept"
	say "Removed the PATH lines from $(pretty "$1")"
}

remove_lynshen() {
	dest=$install_dir/lynshen
	if [ -e "$dest" ]; then
		rm -f "$dest"
		say "Removed $dest"
	else
		say "No lynshen at $dest"
	fi
	rm -f "$install_dir"/.lynshen.download.* 2>/dev/null || true
	for rc in "${ZDOTDIR:-$HOME}/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile" \
		"$HOME/.bash_login" "$HOME/.profile" "${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"; do
		remove_block "$rc"
	done
	say "Settings and sessions in ~/.lynshen were kept; delete that folder to remove them too."
	other=$(command -v lynshen 2>/dev/null || true)
	if [ -n "$other" ] && [ "$other" != "$dest" ]; then
		say "Another lynshen is still on your PATH: $other (npm, Homebrew or LynShen Desktop)"
	fi
}

main "$@"
