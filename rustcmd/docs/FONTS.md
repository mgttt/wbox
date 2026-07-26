# Terminal fonts and licensing

## Default and fallback behavior

RustCmd defaults to `Consolas` at 12 pt and asks Windows GDI to resolve the
requested face. The Settings panel and `get-settings` show both:

- requested family and size;
- the family Windows actually resolved.

If a requested font is missing, Windows may substitute another face. RustCmd
does not claim the requested font is active when the resolved face differs.

Settings are stored in:

```text
%LOCALAPPDATA%\RustCmd\settings.json
```

## Recommended Chinese/Japanese/Korean font

The recommended optional font is **Sarasa Fixed SC / 更纱黑体 Fixed SC**:

- Sarasa combines programming-oriented Latin glyphs with Source Han Sans CJK;
- the `Fixed` style disables ligatures and uses half-width non-CJK characters;
- CJK glyphs occupy the terminal's two-cell wide positions;
- the project is licensed under the **SIL Open Font License 1.1**, allowing
  commercial use and redistribution subject to its notice/name conditions.

Official sources:

- <https://github.com/be5invis/sarasa-gothic>
- <https://github.com/be5invis/sarasa-gothic/releases>

No professional font should be described as “without copyright.” The relevant
property is an open license that permits use and distribution. RustCmd does not
bundle Sarasa because CJK packages are large and users may prefer another
locale/style. Install it separately, then set:

```powershell
rustcmd set-setting terminal.font-family "Sarasa Fixed SC"
rustcmd set-setting terminal.font-size 12
```

## Grid guarantees

The VT parser determines cell width. RustCmd positions each glyph at that cell:

- ASCII and ordinary narrow characters advance one cell;
- CJK wide characters occupy two cells;
- wide continuation cells are not painted as an extra space.

Changing font or size changes the number of visible rows and columns. RMUX
0.9.1 on Windows may not consume a live ConPTY resize reliably, so an attached
RMUX tab may need to be recreated after typography changes.
