---
title: MilkDrop
description: Run the MilkDrop visualiser, install presets, and use its controls.
nav_order: 5
---

MilkDrop shows colourful animations that react to your music. Open it from
the top-bar visualiser button, Ctrl+Shift+K, Settings, or the mini player's
**V** menu. It runs in its own window.

<video autoplay loop muted playsinline preload="metadata" poster="/assets/images/milkdrop-poster.jpg" aria-label="MilkDrop running in Spotifast" style="width: 100%; height: auto;">
  <source src="/assets/images/milkdrop.mp4" type="video/mp4">
</video>

MilkDrop is included on Linux and macOS, and in the Windows download for
Intel or AMD PCs. It is not included in the Windows on ARM download.

## The window

Drag the image to move the window. Double-click or press **F** for fullscreen.
Press **Esc** to leave fullscreen or close the window. Drag the lower-right
corner to resize it.

Like the other visualisers, MilkDrop follows changes you make with the
equalizer. Turning down the volume does not change the picture, even at zero.
It reacts only to music playing on this computer.

## Presets

Each visual design is called a **preset**. They change every ten seconds by
default; choose a different interval in Settings.

On first use, Spotifast automatically downloads the 550 MilkDrop 2 presets
and the 9,800-preset Cream of the Crop pack. You can download either pack
again from Settings. A built-in animation appears while the download starts.

Presets are saved in the `milkdrop` folder alongside your settings; see
[file locations](/settings-and-files/). MilkDrop is powered by
[projectM](https://github.com/projectM-visualizer/projectm) and supports its
`.milk` preset files.

## Controls

- **N** or right arrow: next preset.
- **P** or left arrow: previous preset.
- **H**: switch on the next beat.
- **L**: keep the current preset.
- **R**: switch between random and folder order.
- **T**: show or hide the preset name.
- **D**: show or hide the frame rate.
- **I**: cycle the song display between a change notification, always visible,
  and hidden.
- **?** or **F1**: show all shortcuts.

The normal playback shortcuts also work.
