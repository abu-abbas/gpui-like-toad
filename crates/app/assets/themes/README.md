# Bundled color themes

These JSON files are copied unchanged from the `themes/` folder of
[longbridge/gpui-kit](https://github.com/longbridge/gpui-kit/tree/main/themes)
(Apache-2.0, see `LICENSE-APACHE`; each file names its original author).
They are embedded into the binary with `include_str!` and registered in
gpui-kit's `ThemeRegistry` at startup (`crates/app/src/theme.rs`).

To refresh them, copy the folder again from gpui-kit and rebuild. A new file
also needs a line in `BUNDLED_THEMES` in `theme.rs`.
