# LinkNest

**Local-first bookmarks for Orbit: normalization, dedup, Markdown/HTML export.**

> Orbit companion module — phase 2/4 of the [Orbit ecosystem](https://github.com/Can-5/orbit).
> Standalone crate: usable without Orbit, without other modules, and without network.

## Implemented now

- normalize_url(): trim/lowercase scheme+host, strip fragment, default-port removal, path trailing-slash rule
- find_duplicates() across a bookmark set
- export_markdown() / export_html() with full HTML escaping

## Not implemented yet (explicitly out of scope for this crate)

- No browser extension, no import from browsers — host feature, future phase
- No network fetching or link health checks

## Build & test

```sh
cargo test  # 10 tests
```

Dependencies: serde, serde_json, thiserror. No cross-repo path dependencies, no Git dependencies, no downloads at build time beyond crates.io.

## Privacy

Bookmarks are yours. Exports are files you choose; there is no sync and no network code in this crate.

## License

MIT © The Orbit contributors. See [LICENSE](./LICENSE).
