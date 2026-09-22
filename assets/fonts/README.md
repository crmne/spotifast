# Inter tabular figures

`InterVariable.ttf` is Inter 4.001 (<https://github.com/rsms/inter>, SIL OFL 1.1,
see `Inter-LICENSE.txt`) with its `tnum` feature frozen into the character map.
egui cannot select OpenType features, so every digit, and the comma, period and
colon that sit with them, point at the fixed-width `.tf` glyph instead of the
proportional default. Without this, a value changing width reflows the time and
date labels around it, such as the elapsed time in the player bar.

Regenerate from the upstream release with the `opentype-feature-freezer`
package:

```sh
uvx --from opentype-feature-freezer pyftfeatfreeze -f tnum \
  InterVariable-upstream.ttf InterVariable.ttf
```

The freeze changes only which glyph a codepoint maps to. The font names,
version, the `opsz` and `wght` axes, and all metrics stay as upstream.
