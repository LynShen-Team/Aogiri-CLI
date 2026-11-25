# LynShen software library

`https://software.lynshen.org` serves LynShen releases as static files. The
CLI updates from it; nothing else is needed on the server.

```
/cli/latest.json            signed manifest of the newest release (never cached)
/cli/<version>/<file>       release files (immutable)
/cli/<version>/SHA256SUMS   sha256 of every other file of the release
/cli/install.sh             the newest release's installers (never cached)
/cli/install.ps1
/skills/latest.json         the skills mirror's signed manifest (see below)
/skills/<version>/<file>    catalog.json and one archive per skill
```

## Manifest

```json
{
  "release": "{\"product\":\"cli\",\"version\":\"0.4.9\",\"files\":[{\"name\":\"lynshen-aarch64-apple-darwin\",\"sha256\":\"…\",\"size\":123}]}",
  "signature": "<base64 Ed25519 signature over the exact bytes of release>",
  "key": "<base64 public key, informational>"
}
```

`release` is a string so the signed bytes are exactly the served bytes. File
names are `lynshen-<target>` (`.exe` on Windows), as `update::binary_name`
expects.

## How the CLI uses it

- At startup the CLI fetches `/cli/latest.json` in the background and verifies
  the signature with the public key built into it
  (`MANIFEST_PUBLIC_KEY` in `crates/agent-core/src/update.rs`). A manifest
  signed by any other key, or with a file name that could leave its version
  directory, is refused.
- When the release is newer, a release binary (not npm, not the copy Desktop
  manages, not a debug build) downloads its file, checks the sha256 from the
  manifest, runs it with `--version`, and replaces itself. The new version
  runs from the next start. `auto_update: false` in `config.json` turns this
  off; the CLI then only prints a notice.
- `lynshen update` does the same in the foreground.

## Installers

`curl -fsSL https://software.lynshen.org/cli/install.sh | bash` (macOS, Linux)
and `irm https://software.lynshen.org/cli/install.ps1 | iex` (Windows) install
the CLI in `~/.local/bin` and put that folder on PATH. The scripts live in
`packaging/installer/` (their options are at the top of each file;
`packaging/installer/test.sh` and `test.ps1` test them end to end, on CI in
`.github/workflows/installers.yml`).

- They read the version from `latest.json` (or `LYNSHEN_VERSION`), the
  expected sha256 from `/cli/<version>/SHA256SUMS`, download
  `/cli/<version>/lynshen-<target>`, check it, run it with `--version`, and
  only then put it in place. They do not check the manifest's signature: they
  come from the same server over HTTPS, so it would add nothing.
- Not `~/.lynshen/bin`: LynShen Desktop owns that folder, and the CLI treats
  a binary there as Desktop's (no self-update). A binary in `~/.local/bin`
  updates itself like any release binary.
- On macOS and Linux they add one block, between `# >>> lynshen installer >>>`
  and `# <<< lynshen installer <<<`, to the login shell's startup file: zsh
  `.zshrc`, bash `.bashrc` plus the login file bash reads (`.bash_profile`,
  `.bash_login` or `.profile`), fish `config.fish`. `--uninstall` removes the
  blocks. On Windows they add the folder to the user PATH (REG_EXPAND_SZ kept)
  and remove it on `-Uninstall` only when nothing else is left in it.
- Versions published before the installers (0.8.10 and older) have no
  `SHA256SUMS` and cannot be installed with them.

`publish.sh cli` adds `install.sh`, `install.ps1` (from the checkout it runs
in, which is the release tag) and `SHA256SUMS` to the files it signs, so every
release carries its own installers. `receive-software.py` then copies the new
release's installers to `/cli/install.sh` and `/cli/install.ps1`. The Caddy
site serves those two with `Cache-Control: no-cache` and
`Content-Type: text/plain; charset=utf-8` (Windows PowerShell reads text
without a charset as Latin-1).

## Keys

- `software-release keygen <file>` writes a new private key (mode 0600) and
  prints its public key. The private key never goes into a repository or CI
  log. The current key lives on the release machine in
  `~/.lynshen-release/software-signing.key`.
- Rotating the key: put the new public key in `update.rs` and
  `receive-software.py`, ship one release signed with the old key that
  contains the new public key, then sign with the new key.

## Publishing

```sh
SOFTWARE_SIGNING_KEY=~/.lynshen-release/software-signing.key \
SOFTWARE_SSH=software@40.160.141.21 \
deploy/software-library/publish.sh cli 0.4.9 ./release-files
```

The release workflows build the binaries (GitHub Release of the tag). Once
both have finished, put the four `lynshen-<target>` files in one directory
and run `publish.sh` on the machine that holds the key. CI never sees the
private key, and each version is published once with every platform.

`publish.sh` signs the files with `software-release sign` and streams
`latest.json` plus `<version>/<files>` to the server. The server's
`receive-software.py` checks the signature again, every sha256, that the
archive holds exactly the listed files, and that the version is not older
than the published one. Published versions cannot change. Then it switches
`latest.json` atomically.

## Server setup (40.160.141.21)

1. DNS: `software.lynshen.org` A record → `40.160.141.21`.
2. `apt install python3 python3-cryptography` (Python 3.11 or newer).
3. Create `/srv/software`, owned by a `software` user, readable by Caddy.
4. Install `deploy/software-library/receive-software.py` root-owned at
   `/usr/local/lib/lynshen/receive-software.py`.
5. In `~software/.ssh/authorized_keys`, add the publisher's key with:
   `restrict,command="/usr/bin/python3 /usr/local/lib/lynshen/receive-software.py /srv/software" ssh-ed25519 …`
6. Add `deploy/software-library/Caddyfile` to Caddy and reload it. Caddy gets
   the certificate itself once DNS points at the server.

## Skills mirror

The `skills` product mirrors the desktop marketplace's skills
(`skills_catalog` in docs/daemon-protocol.md), so computers that cannot reach
GitHub list and install them from here. The engine reads
`/skills/latest.json` first (5 s to connect, 10 s in all), checks its
signature with the same built-in key as CLI updates and the sha256 of every
file it uses, and falls back to GitHub when the mirror fails or lacks a skill.

`deploy/skills-mirror/build.py build <dir>` writes:

- `catalog.json`: `{"format": 1, "sources": [...]}`, one source per
  marketplace repository (`name`, `repository`, `revision` (the commit it was
  read at), `dir`, `skills`), each skill with `id`, `name`, `description`,
  `tags`, `license`, `redistributable` and, when the mirror carries it,
  `archive`.
- `<owner>_<repo>__<id>.tar.gz` (`anthropic__<id>.tar.gz` for Anthropic):
  `<id>/` with the skill's files, plus the repository's top-level LICENSE and
  NOTICE files when the skill folder has none of its own. The same commit
  packs to the same bytes.

Anthropic's skills are read at the commit the bundled index pins, and only
the ones it marks redistributable are packed. A community skill is packed
only with a license (its own or its repository's); without one it is listed
and the engine installs it from GitHub. One whose frontmatter `license` says
proprietary or source-available is listed with it and never packed, as the
engine lists it. The community list is
`deploy/skills-mirror/sources.json`, which a test keeps equal to
`COMMUNITY_SKILL_SOURCES`.

`deploy/skills-mirror/publish.sh` builds the mirror, compares every file's
sha256 with the published manifest, and publishes only when something
changed, as version `<UTC date>.<UTC hour and minute>.0` through
`deploy/software-library/publish.sh skills …`. Run it where the signing key
lives (the release machine), never on the server:

```sh
SOFTWARE_SIGNING_KEY=~/.lynshen-release/software-signing.key \
SOFTWARE_SSH=software@40.160.141.21 \
deploy/skills-mirror/publish.sh
```

The server's `receive-software.py` accepts the `skills` product and up to
1024 entries per release. Old versions stay until removed by hand; keep the
newest few.
