# PLOMID release runbook (beta line)

## One-time setup

1. **Docker Hub**: repository `plomid/plomid`; add repo secrets
   `DOCKERHUB_USERNAME` + `DOCKERHUB_TOKEN` (write access).
2. **Homebrew tap**: create `github.com/plomid/homebrew-plomid`, copy
   `dist/homebrew/plomid.rb.template` to `Formula/plomid.rb` on each release
   with real `VERSION` + `sha256` values (printed by CI in the build logs).
3. **Signing (optional but recommended before stable)**:
   - macOS: Apple Developer ID secret `APPLE_DEVELOPER_ID` (+ `APPLE_APP_PASSWORD`,
     `APPLE_TEAM_ID` for notarization). Without it the .dmg ships unsigned
     (works via right-click > Open).
   - Windows: code-signing certificate secret `WINDOWS_CERT`; sign
     `dist/stage/plomid.exe` with `signtool` before the `makensis` step.
     Without it SmartScreen shows "unknown publisher".
4. **Website artifacts** (`plomid.in` serves these, not this repo):
   `install.sh`, download page, checksums page — all point at
   `github.com/plomid/plomid/releases`.

## Cutting a beta

```bash
# 1. Bump the version (single source of truth for tags + --version).
sed -i '' 's/^version = ".*"/version = "0.1.0-beta.1"/' Cargo.toml
git add Cargo.toml && git commit -m "release: v0.1.0-beta.1"

# 2. Gate everything, then tag. NOTHING pushes unless fmt, check, clippy,
#    deny, the full test suite, and a locked release build all succeed.
make release-beta VERSION=0.1.0-beta.1
```

`make release-beta` refuses when: the tree is dirty, `VERSION` is empty or
still the default, or Cargo.toml disagrees. After the tag is pushed, the
`Release` workflow builds all 7 platform packages + checksums + GitHub
Release + multi-arch Docker (`plomid/plomid:<tag>` and `:latest`).

## Platform matrix (all from one tag)

| Artifact | Builder | Notes |
|---|---|---|
| macOS arm64 `.dmg` + `.tar.gz` | `macos-14` runner | validated locally 2026-09-28 |
| macOS x64 `.dmg` + `.tar.gz` | `macos-13` runner | same scripts, CI-only validation |
| Windows `.zip` + installer `.exe` | `windows-latest` + NSIS | script-reviewed; CI-validated |
| Linux x64/arm64 `.tar.gz` | native runners | trivially reproducible |
| Linux `.deb` + `.rpm` | nfpm | validated locally (arm64) incl. contents |
| Docker `linux/amd64,arm64` | buildx + existing Dockerfile | reuses per-arch binaries |

## Pre-stable hardening still open

- Code signing + notarization wired but cert-less (see above).
- Windows build never executed (no Windows host available here).
- `install.sh` and the download page live outside this repo.
