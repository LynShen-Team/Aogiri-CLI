#!/bin/bash
# End-to-end test of install.sh (CI: .github/workflows/installers.yml).
#
#   packaging/installer/test.sh <lynshen binary> <its version> <shell>...
#
# Serves a copy of the software library on 127.0.0.1 (the binary under its
# release name, this checkout's installers, a SHA256SUMS as publish.sh writes
# it) and installs with `curl | bash` for each shell, every time in a
# throwaway HOME: the real startup files are never touched.
set -u
binary=$1 version=$2
shift 2
here=$(cd "$(dirname "$0")" && pwd)
case $(uname -s)/$(uname -m) in
Darwin/arm64) file=lynshen-aarch64-apple-darwin ;;
Darwin/x86_64) file=lynshen-x86_64-apple-darwin ;;
Linux/x86_64) file=lynshen-x86_64-unknown-linux-gnu ;;
*) echo "no release file for $(uname -sm)" && exit 1 ;;
esac
unset ALL_PROXY HTTP_PROXY HTTPS_PROXY all_proxy http_proxy https_proxy
lib=$(mktemp -d)
mkdir -p "$lib/cli/$version" "$lib/bad/$version"
cp "$binary" "$lib/cli/$version/$file"
cp "$here/install.sh" "$here/install.ps1" "$lib/cli/$version/"
cp "$here/install.sh" "$lib/cli/"
(cd "$lib/cli/$version" && shasum -a 256 -- * >SHA256SUMS)
release="{\\\"product\\\":\\\"cli\\\",\\\"version\\\":\\\"$version\\\",\\\"files\\\":[]}"
printf '{\n  "key": "",\n  "release": "%s",\n  "signature": ""\n}\n' "$release" >"$lib/cli/latest.json"
# A release whose SHA256SUMS does not match its binary.
cp "$binary" "$lib/bad/$version/$file"
printf '%064d  %s\n' 0 "$file" >"$lib/bad/$version/SHA256SUMS"
port=18471
python3 -m http.server $port --bind 127.0.0.1 --directory "$lib" >/dev/null 2>&1 &
server=$!
trap '{ kill $server && wait $server; } 2>/dev/null; rm -rf "$lib"' EXIT
for _ in 1 2 3 4 5 6 7 8 9 10; do
	curl -fsS -o /dev/null "http://127.0.0.1:$port/cli/latest.json" 2>/dev/null && break
	sleep 1
done
base=http://127.0.0.1:$port/cli
clean_path=/usr/bin:/bin:/usr/sbin:/sbin
passed=0 failed=0
check() {
	if eval "$2"; then
		echo "ok   $1"
		passed=$((passed + 1))
	else
		echo "FAIL $1"
		failed=$((failed + 1))
	fi
}
# Installer blocks in all the startup files the installer may touch.
blocks() {
	cat "$1/.zshrc" "$1/.bashrc" "$1/.bash_profile" "$1/.bash_login" "$1/.profile" \
		"$1/.config/fish/config.fish" 2>/dev/null | grep -c '>>> lynshen installer >>>'
}

for shell in "$@"; do
	shell_path=$(command -v "$shell") || {
		check "$shell is installed" false
		continue
	}
	home=$(mktemp -d)
	# How a new terminal starts this shell: fish reads config.fish even with -c.
	case $shell in
	zsh) rc=$home/.zshrc new_shell="$shell_path -ic" ;;
	bash) rc=$home/.bashrc new_shell="$shell_path -ic" ;;
	fish) rc=$home/.config/fish/config.fish new_shell="$shell_path -c" ;;
	esac
	mkdir -p "$(dirname "$rc")"
	printf '# the user'"'"'s own line\n' >"$rc"
	cp "$rc" "$home/rc.before"
	echo "== $shell"
	in_home() {
		# XDG_CONFIG_HOME and ZDOTDIR would point the shells (and the installer)
		# outside the throwaway HOME.
		env -u XDG_CONFIG_HOME -u ZDOTDIR HOME="$home" SHELL="$shell_path" PATH="$clean_path" \
			LYNSHEN_DOWNLOAD_BASE="$base" "$@"
	}
	out=$(in_home bash -c "curl -fsSL $base/install.sh | bash" 2>&1)
	check "curl | bash installs" '[ $? = 0 ] && [ -x "$home/.local/bin/lynshen" ]'
	echo "$out" | sed 's/^/     /'
	check "a new $shell finds lynshen" \
		'[ "$(in_home $new_shell "command -v lynshen" 2>/dev/null)" = "$home/.local/bin/lynshen" ]'
	check "lynshen runs in it" \
		'in_home $new_shell "lynshen --version" 2>/dev/null | grep -qx "lynshen $version"'
	if [ "$shell" = bash ]; then
		check "a bash login shell finds lynshen" \
			'[ "$(in_home "$shell_path" -lc "command -v lynshen" 2>/dev/null)" = "$home/.local/bin/lynshen" ]'
	fi
	in_home bash -c "curl -fsSL $base/install.sh | bash -s -- --upgrade" >/dev/null 2>&1
	expected=1
	[ "$shell" = bash ] && expected=2 # .bashrc and the login file
	check "running it again adds no lines" \
		'[ "$(blocks "$home")" = $expected ]'
	out=$(in_home bash -c "curl -fsSL $base/install.sh | bash -s -- --uninstall" 2>&1)
	echo "$out" | sed 's/^/     /'
	check "--uninstall removes lynshen" '[ ! -e "$home/.local/bin/lynshen" ]'
	check "--uninstall restores $(basename "$rc")" 'cmp -s "$rc" "$home/rc.before"'
	check "no installer lines are left" \
		'[ "$(blocks "$home")" = 0 ]'
	rm -rf "$home"
done

echo "== failures"
unset XDG_CONFIG_HOME ZDOTDIR
home=$(mktemp -d)
out=$(env HOME="$home" SHELL=/bin/sh LYNSHEN_DOWNLOAD_BASE="${base%/cli}/bad" bash "$here/install.sh" --version "$version" 2>&1)
check "a checksum mismatch fails" '[ $? = 1 ] && echo "$out" | grep -q "checksum mismatch"'
check "and leaves nothing behind" '[ -z "$(ls -A "$home/.local/bin")" ]'
out=$(env HOME="$home" LYNSHEN_DOWNLOAD_BASE="$base" bash "$here/install.sh" --version 0.0.1 2>&1)
check "a missing version fails" '[ $? = 1 ] && echo "$out" | grep -q "no such version"'
out=$(env HOME="$home" PATH="$clean_path" SHELL=/bin/zsh LYNSHEN_DOWNLOAD_BASE="$base" bash "$here/install.sh" --no-modify-path 2>&1)
check "--no-modify-path writes no startup file" '[ $? = 0 ] && [ -z "$(ls -A "$home" | grep -v "^\.local$" | grep -v "^\.lynshen$")" ]'
rm -rf "$home"

echo "passed $passed, failed $failed"
[ "$failed" = 0 ]
