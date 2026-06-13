# Vendored fonts

`snapshot --png` renders glyphs from vendored [JetBrains Mono] faces so the
output needs no system fonts. All four faces are the same family and are
metric-compatible (identical advance width and line metrics), so the cell grid
stays aligned regardless of per-cell weight/slant.

| File | Face | Used for |
|---|---|---|
| `JetBrainsMono-Regular.ttf` | Regular | Default text; drives cell metrics |
| `JetBrainsMono-Bold.ttf` | Bold | Cells with the bold attribute |
| `JetBrainsMono-Italic.ttf` | Italic | Cells with the italic attribute |
| `JetBrainsMono-BoldItalic.ttf` | Bold Italic | Cells with both attributes |

## License

JetBrains Mono is licensed under the SIL Open Font License, Version 1.1 — see
`OFL.txt` (which covers the whole family). The OFL is permissive and compatible
with this project's Apache-2.0 license.

[JetBrains Mono]: https://github.com/JetBrains/JetBrainsMono
