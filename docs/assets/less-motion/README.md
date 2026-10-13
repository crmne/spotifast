# Less motion comparison

Native Windows 11 demo captures (`--demo --demo-language en`) at 1200x800 and
1000x700, in both themes, from the window's own frame buffer at 100% scaling.
Before: upstream `main` 0e87b95. After: `pr/reduced-motion`.
Frames after switching the theme in Settings, with less motion asked for. Before, the new theme is revealed from the middle of the window regardless; after, it replaces the old one at once.
The captures force the preference with a capture-only environment variable standing in for the desktop setting (Windows "Animation effects" off).
These are deterministic demo captures, not signed-in Spotify evidence.
