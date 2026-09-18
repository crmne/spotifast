# Top-result artist navigation review

Native Linux demo captures compare `main` at `69c2a72` with the
`top-result-artist-navigation` candidate branch. The captures use the same
search data at 900x700 and 620x700 in light and dark themes. The `main` capture
has only the candidate's `song-top-result` demo-state hook applied so both sides
show the same song result.

The resting layout is intentionally unchanged. The candidate makes the artist
name in the Top result a link: it underlines on hover and opens the artist page
when selected. The demo interaction test covers the click state because the
deterministic screenshot runner does not place a pointer over controls.

Open `review.html` and use the theme, size, and Before/After controls to inspect
the matching captures.
