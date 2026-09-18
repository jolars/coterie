# Changelog

## [0.2.1](https://github.com/jolars/coterie/compare/v0.2.0...v0.2.1) (2026-09-18)

### Bug Fixes
- use nextest ([`31d57fc`](https://github.com/jolars/coterie/commit/31d57fcc199451cde8deb03f82abbbc4817b5c09))
- bound foreground notifications until receipt ([`658f681`](https://github.com/jolars/coterie/commit/658f681c07d4cafbdbd12d6a52f3c442dd91241d))

## [0.2.0](https://github.com/jolars/coterie/compare/v0.1.0...v0.2.0) (2026-09-16)

### Added

- Nonblocking project leases, a persistent active-run coordination index,
  versioned local RPC, and race-safe singleton supervisor startup with stale-run
  recovery.
- Installation and Codex prerequisites, Sidekick integration, the MVP trust and
  recovery model, and an exhaustive command and exit-code reference.

### Features
- simplify agent protocol bookkeeping ([`d0c98ce`](https://github.com/jolars/coterie/commit/d0c98ce31ae9395e2052ff936d2fec8561041da6))
- wake foreground agents with `codex queue` ([`94043e6`](https://github.com/jolars/coterie/commit/94043e6e773e78838db5e384ddc13af70cc3506c))
- make recovery handoffs self-contained ([`504e0e2`](https://github.com/jolars/coterie/commit/504e0e299e5187621e74f33ad73e80b066eb8a5d))
- bound repeated context inspection ([`d2446ed`](https://github.com/jolars/coterie/commit/d2446ed233b0e38f204198015faef210a4f8891a))
- default contribution integration to rebase ([`00a1ba3`](https://github.com/jolars/coterie/commit/00a1ba3bd6321dff85013843ba622829fbe2f94c))
- recover interrupted worktree assignments ([`0b95ac1`](https://github.com/jolars/coterie/commit/0b95ac1cff09384f2ee8b784ed4bc7dfd02dcf7c))
- record operator closure overrides ([`8d7d4e2`](https://github.com/jolars/coterie/commit/8d7d4e2f58cdb3def4bea60f3dc373dfcd74adce))
- recover unintegrated submissions ([`a1391b5`](https://github.com/jolars/coterie/commit/a1391b55f022717be817d6465be0c221f0617164))
- add scoped progress inspection ([`d5f4a0d`](https://github.com/jolars/coterie/commit/d5f4a0d7a129d7ec31ce90fa54567badd3a03b72))
- attach projects with exclusive run leases ([`eaed53d`](https://github.com/jolars/coterie/commit/eaed53d064fc2895cc76773d371301be2eac0629))
- use resolved configuration for runtime policy ([`eddabf5`](https://github.com/jolars/coterie/commit/eddabf5e41c6ee79db281e74a610824455e1ed54))
- add configuration inspection and portable locks ([`60906e7`](https://github.com/jolars/coterie/commit/60906e71631f6fc27a504fa7be41c8814c12f3b3))
- enforce monotone configuration policy intersection ([`da272b7`](https://github.com/jolars/coterie/commit/da272b7dab21391ba23a2f355d2c0d2731be05e4))
- track effective configuration provenance ([`0bc1264`](https://github.com/jolars/coterie/commit/0bc126440f539d4d6c1211542a5b83cdd5f00b9d))
- add layered configuration loading and resolution ([`11de8d7`](https://github.com/jolars/coterie/commit/11de8d72bef3f34926654bfa52fd650114346b81))
- add runtime diagnostics and safe recovery ([`c0d689c`](https://github.com/jolars/coterie/commit/c0d689c681894abba3ec9c43738b3e85bbd596a2))
- bound restarts and phase shutdown ([`ab51b0a`](https://github.com/jolars/coterie/commit/ab51b0a0570d114a06115b9c2bc57851d9f3f68e))
- fence resources by run and generation ([`3626b58`](https://github.com/jolars/coterie/commit/3626b58ba0c46d1a04dcc1306156c6e586bd8868))
- reconcile durable operations ([`258fd53`](https://github.com/jolars/coterie/commit/258fd536bb365f709b92850f59aede99cd93b23d))
- add nix flake ([`476c773`](https://github.com/jolars/coterie/commit/476c77382c586eec0b062c2eac6f6f411c86318d))
- complete operator loop ([`d36c07b`](https://github.com/jolars/coterie/commit/d36c07b406d32eca7577d946e1a1c7264bfed495))
- add guarded workspace integration ([`5ccec33`](https://github.com/jolars/coterie/commit/5ccec33f27f88e3faa41a2daffa647232edfe731))
- add Git workspaces ([`ecd5474`](https://github.com/jolars/coterie/commit/ecd5474f05ecd86d27c8e0231aa4d802f0bf9d9a))
- enforce Codex permission profiles ([`691d574`](https://github.com/jolars/coterie/commit/691d5749e5973e621f9ead3bfdd6d09e8c020dfb))
- launch Codex workers ([`ef2108d`](https://github.com/jolars/coterie/commit/ef2108d2ead9252fa62b05782f3c13de6db6a9e1))
- launch foreground Codex TUI ([`486b62f`](https://github.com/jolars/coterie/commit/486b62faed1995b16143157046dc20d77a829b6d))
- probe Codex compatibility ([`cd60734`](https://github.com/jolars/coterie/commit/cd60734c7f989a2d58b737232a96a40db9c3f0a6))
- add durable reconciliation ([`e3832f9`](https://github.com/jolars/coterie/commit/e3832f9c1b0d21f541e534a0c2fc1394248a6d20))
- add durable messaging events ([`f5dddd5`](https://github.com/jolars/coterie/commit/f5dddd55ae5905e944535218635ab3a5f7a6de5b))
- add delegation commands ([`decba35`](https://github.com/jolars/coterie/commit/decba35f6c1736727b98988594b15072bebc8534))
- add deterministic fake provider ([`ab7db3f`](https://github.com/jolars/coterie/commit/ab7db3f02433ee7ad039a1b22bf1fe39f50fa9d1))
- authenticate agent RPC sessions ([`2dee506`](https://github.com/jolars/coterie/commit/2dee5069331f41b6446ee6c48482bc3ad8124206))
- add singleton project supervisors ([`78e8f78`](https://github.com/jolars/coterie/commit/78e8f783b58f656857da88244c7d7b143e6fdd4f))
- discover project identity ([`6cf466f`](https://github.com/jolars/coterie/commit/6cf466fa1ff25acee6945fe7b3253cb60eea1f85))
- implement task lifecycle ([`9c210cb`](https://github.com/jolars/coterie/commit/9c210cbc76355d3f493bc0703b32eddc87ec55c6))
- enforce durable mutation invariants ([`02ec51f`](https://github.com/jolars/coterie/commit/02ec51fe77bd52cd1c947be8134ba15ba8e2c7cb))
- add durable state repositories ([`8639ffc`](https://github.com/jolars/coterie/commit/8639ffc2c1dc173a4b72df3d096dadc29d0c4423))
- add versioned CLI contracts ([`cd4164a`](https://github.com/jolars/coterie/commit/cd4164a8953dd744b0ea398cbb7dbd00ba158caf))
- add stable typed IDs ([`699d2f2`](https://github.com/jolars/coterie/commit/699d2f2cc8fdbad1b82dba5a6dc061d87e08a3c6))
- scaffold project ([`ce9349e`](https://github.com/jolars/coterie/commit/ce9349e10f751fe75b7176ea3ce74e418f9d1d31))

### Bug Fixes
- verify Git publication after write failures ([`dda6001`](https://github.com/jolars/coterie/commit/dda600115b756c5604029f3e39a88344f6e07bc3))
- recover offline runs for `coterie stop` ([`afb0e81`](https://github.com/jolars/coterie/commit/afb0e81e16df1a928c0bcdb95b0e974079583bdf))
- **flake:** add devenv files for pinned devenv ([`e4a0383`](https://github.com/jolars/coterie/commit/e4a038370497595ab09381a928388d6b413a5ef1))
- diagnose validation environment access ([`ad4de42`](https://github.com/jolars/coterie/commit/ad4de424c7b3b71ce0af4789d57a57d10ee4d9c0))
- add linked-worktree commit handoffs ([`1b8315c`](https://github.com/jolars/coterie/commit/1b8315c0a04b134217263e28fd74a8e978d2e71a))
- remove deprecated alias ([`7fb3aff`](https://github.com/jolars/coterie/commit/7fb3aff96d2f14f4ffbb42adc2003a82b528e946))
- migrate snapshots before recovery validation ([`19c6093`](https://github.com/jolars/coterie/commit/19c6093a320c0aff41388cac3970c58e528f9182))
- detect stranded foreground terminals in `doctor` ([`15fe08a`](https://github.com/jolars/coterie/commit/15fe08a88f9af729fb21f31f10907c19c34d6c2b))
- honor configured foreground provider names ([`5cb156e`](https://github.com/jolars/coterie/commit/5cb156e61495af8a2688d9eb01a36c969756228c))
- distinguish operator and agent health in `doctor` ([`3aa739a`](https://github.com/jolars/coterie/commit/3aa739a3199d41edbe81789d4c19331bc07bd840))
- route agent RPCs through stdio MCP ([`7d8bdbd`](https://github.com/jolars/coterie/commit/7d8bdbdad2ced35c7b5c6d1ad36fdcaf00ded970))
- allow repeated shared workspace assignments ([`99b9164`](https://github.com/jolars/coterie/commit/99b91647d39bca23a4e94078a9a758cb5c15f1e2))
- reap foreground providers after terminal closure ([`240ec18`](https://github.com/jolars/coterie/commit/240ec18c0f9ffed7f108deca18c8e387d3ac3fa8))
- stop idle supervisors automatically ([`ad21b48`](https://github.com/jolars/coterie/commit/ad21b482663d37f9d7c44b09afad20e7a71214ae))
- keep coordinating delegated work ([`1e89c04`](https://github.com/jolars/coterie/commit/1e89c04a201403f1048000a5cfcc3ad4621ef2c3))
- preserve worker shell tool discovery ([`1edfcca`](https://github.com/jolars/coterie/commit/1edfcca7bf671d7165e96f421e0b17c1ac1cfa01))
- explain supervisor protocol mismatch recovery ([`363abad`](https://github.com/jolars/coterie/commit/363abaddd1fd535ca9551cb91c9700aba8d1b0e8))
- unlock explicitly on drop ([`8709cf4`](https://github.com/jolars/coterie/commit/8709cf4a5f4495493afe0ba4779bcd9171d391d6))
- handle temporary dir correctly ([`8d469ca`](https://github.com/jolars/coterie/commit/8d469cae48eb8c0b998334582244847aec556e60))
- preserve sqlite locks ([`fa081da`](https://github.com/jolars/coterie/commit/fa081daf724ea4372b911a1a2d9b2562101c4023))
- bound submission diagnostics and unreadable checks ([`930520b`](https://github.com/jolars/coterie/commit/930520ba4e343a2b8cc8ec715a10bfde179f653d))
- reject incomplete worktree submissions ([`3372f7c`](https://github.com/jolars/coterie/commit/3372f7c97493e791c7566990ff1a2f3a2d1c7e84))
- fix failings tests ([`43567fb`](https://github.com/jolars/coterie/commit/43567fb8a597e737779adc6bc15be7127f984da9))
- guard destructive operations with ownership proofs ([`4c5573a`](https://github.com/jolars/coterie/commit/4c5573a7834760b136c2928fcf8526ea155ab3f0))

All notable changes to Coterie will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-09-05

### Added

- The behavior-free Coterie binary crate and its development foundation.
- Reproducible formatting, linting, testing, documentation, audit, coverage,
  pre-commit, and CI gates.

[Unreleased]: https://github.com/jolars/coterie/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jolars/coterie/releases/tag/v0.1.0
