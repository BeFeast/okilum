# Third-party notices

Tessera is built with open-source software. Each dependency retains its own
license and copyright notices; Tessera's MIT license does not replace them.

- [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) powers the native interface.
- [gpui-kit](https://github.com/longbridge/gpui-kit) supplies UI components
  ([Apache 2.0 license](https://github.com/longbridge/gpui-kit/blob/main/LICENSE-APACHE)).
- [Tantivy](https://github.com/quickwit-oss/tantivy) provides full-text search.
- [Syntect](https://github.com/trishume/syntect) provides syntax highlighting.
- [Sparkle](https://sparkle-project.org/) provides macOS updates. Its distribution
  includes the framework's license and third-party acknowledgements.

Bundled font notices:

- [Noto Sans](../crates/tessera-shell/assets/brand/fonts/notosans-OFL.txt)
- [Cascadia Code](../crates/tessera-shell/assets/brand/fonts/cascadiacode-OFL.txt)
- [Excalifont](../crates/tessera-shell/assets/drawings/fonts/Excalifont-LICENSE.txt)
- [Nunito](../crates/tessera-shell/assets/drawings/fonts/Nunito-LICENSE.txt)
- [Liberation Sans](../crates/tessera-shell/assets/drawings/fonts/LiberationSans-LICENSE.txt)
- [Virgil](../crates/tessera-shell/assets/drawings/fonts/Virgil-LICENSE.txt)

This is an overview, not an exhaustive dependency inventory. Exact Rust dependency
versions are recorded in [Cargo.lock](../Cargo.lock); their source distributions
contain the applicable licenses and notices.
