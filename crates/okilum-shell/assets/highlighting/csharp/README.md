# C# highlight query

`highlights.scm` is copied unchanged from `tree-sitter-c-sharp` 0.23.5,
`queries/highlights.scm`, under the adjacent MIT license.
Source: https://docs.rs/crate/tree-sitter-c-sharp/0.23.5/source/queries/highlights.scm
SHA-256: `ab8a9930aeeee70fa2dbfde82e4763170b7e826bc642338ad0683772c20c060f`.

The pinned gpui-component includes the grammar but leaves its C# highlight
query empty. Okilum registers this query through the public LanguageRegistry
for both Reader and Live Preview. No additional grammar or dependency is used.
