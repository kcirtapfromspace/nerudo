# ts-rust, vendored

These files come from [ts-rust](https://github.com/pingdotgg/ts-rust) at commit
`887da59f7f02e91d362304b9e36d5f64ba4f7295`. ts-rust is a Rust port of the TypeScript 7
compiler (typescript-go). Its licence is MIT. The ported TypeScript parts keep their Apache-2.0
licence and notice. See `LICENSE`, `NOTICE.md` and `licenses/`.

| File | Source |
|---|---|
| `core.js` | `npm/wasm/core.js`, without change |
| `ts_rust.wasm` | built from `crates/ts_wasm` with `scripts/wasm/build.sh` |
| `LICENSE`, `NOTICE.md`, `licenses/` | the files of the same names, without change |

`ts_rust.wasm` (sha256 `798a7c6faf82a836296874270e80e63a6a9dca395d8d9a52839efcc828332c59`)
came from Rust 1.99.0 and binaryen 133 on macOS arm64. To build it again, run
`../scripts/build-ts-rust.sh` from `cf/nerudo-ci`. A different toolchain can give
different bytes.
