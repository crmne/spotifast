# MacBook notch widget visual review

Native macOS captures on Apple Silicon M2 with hardware camera notch (macOS Sequoia) comparing `main` baseline with the `feature/mac-notch-widget` candidate branch.

Open `review.html` in any browser to inspect the matching captures with the Theme (Dark / Light), Window size (Normal / Narrow), and Revision (Before / After) controls.

## Visible Scope & Spotizgeg Design System Alignment

- **Seek Bar:** 4pt seek bar matching Spotizgeg's own `thin_slider` (shape, height, solid white progress fill, and 6pt radius solid white thumb handle drawn unconditionally) rather than an uncharacteristic wavy design.
- **Card Controls & Styling:**
  - Spotizgeg's native Lucide outlined SVG icons from `assets/icons/` (`heart.svg`, `skip-back.svg`, `skip-forward.svg`, `play-filled.svg`, `pause-filled.svg`, `shuffle.svg`, `repeat.svg`, `repeat-1.svg`, `speaker.svg`).
  - Inter font (`fastframe_fonts::INTER`) matching the application shell and player bar.
  - Active buttons (shuffle, repeat, liked song, remote Connect target) highlighted in Spotizgeg green (`#1ed760`); inactive controls rendered in crisp translucent white.
  - 36pt circular disc button (`theme::circle_button`) for Play/Pause.
  - Time indicators show elapsed time and total duration (`m:ss`) matching the player bar.
  - 18pt corner radius and 1px outline border matching Spotizgeg floating modals.
- **Resource Discipline:**
  - Window and tracking run entirely through native macOS AppKit hooks with zero idle wakeups.
  - Fast repaint is only scheduled while the notch card is actively expanding or animating.

## Interaction States

| State | Preview | Description |
|-------|---------|-------------|
| **Active Playback** | ![Expanded Playing](desktop-expanded-playing.png) | Active playback with Spotizgeg `thin_slider` seek bar and Lucide transport controls. |
| **Paused** | ![Expanded Paused](desktop-expanded-paused.png) | Paused playback with frozen playhead and play disc glyph. |
| **Collapsed** | ![Main Focused](desktop-collapsed-settings.png) | Concealed under the notch when the main window is focused. |
| **Detail Crop** | ![Notch Expanded Detail](notch-expanded.png) | Expanded card detail showing album artwork, Inter typography, and controls. |
