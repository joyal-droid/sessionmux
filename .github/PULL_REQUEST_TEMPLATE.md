## What this changes

<!-- The behaviour change, and the issue it addresses (for example "Fixes #12"). -->

## How it was checked

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo build --release` without warnings
- [ ] `cargo test`
- [ ] `Sessionmux.exe --smoke` passes (attach the relevant `smoke\*.png` for UI changes)

## Checklist

- [ ] New and changed source files carry the CPAL-1.0 header comment.
- [ ] The attribution in the panel header, the About card and the bubble is unchanged
      (docs/attribution.md).
- [ ] UI changes follow docs/design.md (tokens from `web/shared.css`, flat fills, contrast >= 4.5:1).
- [ ] No new dependencies, or the reason is explained above.
- [ ] README.md, PRIVACY.md or docs/ updated if behaviour, stored data or architecture changed.
- [ ] A line was added under "Unreleased" in CHANGELOG.md.
- [ ] No personal data (absolute paths with user names, account ids, e-mail addresses, real
      session titles) in code, tests or screenshots.
