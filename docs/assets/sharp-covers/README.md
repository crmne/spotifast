# Sharp covers scaled to their pixels comparison

Native Windows 11 demo captures (`--demo --demo-language en`) at 1200x800 and
1000x700, in both themes, from the window's own frame buffer at 100% scaling.
Before: upstream `main` 0e87b95. After: `pr/covers`.
Covers are decoded and scaled on the CPU to the pixels they cover, from artwork asked for at a quarter more than that, and drawn through mipmaps. Library rows stop using Spotify's 64 pixel thumbnail. The demo's artwork is photographs, so the difference is subtle at full size; the zoomed crops show it.
Signed-in captures of Spotify-made covers with text (the Spotify logo, "Your Top Songs") show the difference most; they are kept out of the repository because they come from a personal library.
These are deterministic demo captures, not signed-in Spotify evidence.
