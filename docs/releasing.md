# Releasing net-pan

Releases are created only from tags matching `v<major>.<minor>.<patch>`. The tag version must equal
the version in `Cargo.toml`.

## Prepare

1. Ensure the worktree is clean.
2. Update `Cargo.toml` and `CHANGELOG.md`.
3. Run `just release-check <version>`.
4. Review `git log --format='%an <%ae>'` and accept that historical author addresses are public.
5. Create and push the tag manually after review.

The release helper deliberately does not commit, tag, push, or publish.

## Published assets

GitHub Actions builds Linux x86_64 plus macOS x86_64 and arm64 archives. Every archive contains:

- `net-pan`
- `install.sh`
- `README.md`
- `README_zh.md`
- `LICENSE`
- `THIRD_PARTY_NOTICES`

The release also contains `checksums.txt`, a checksum-verifying bootstrap installer, a generated
Homebrew formula, and release notes generated from Conventional Commits through `git-cliff`.

Every archive is checked against an exact allowlist before upload. On a matching runner architecture,
the packaged binary is also executed with `--version` as a smoke test.

## Homebrew

The release workflow generates `net-pan.rb` from the final three archives, so its SHA-256 values are
derived from the exact uploaded bytes. To generate the same formula locally after placing all three
archives in `dist/`, run:

```bash
just homebrew-formula v0.1.1
```

Use `just homebrew-tap v0.1.1 ../homebrew-tools` to write it into a tap repository. Neither command
commits or pushes the tap.

## Privacy gate

`./scripts/privacy-check.sh --history` blocks release when repository files contain credential
material, private infrastructure identifiers, personal home paths, or consumer email addresses.
Historical Git author/committer addresses are commit metadata rather than credentials, so personal
domains produce a warning without blocking release. Use a GitHub noreply address for new commits
when possible, and never bypass failures for actual sensitive content.
