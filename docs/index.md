---
layout: home
title: Spotifast
description: A fast, lightweight Spotify app for Linux, macOS, and Windows.
permalink: /
hero:
  name: Spotifast
  text: Spotify, native and fast
  tagline: A lightweight Spotify app for Linux, macOS, and Windows. Listen on your computer, browse your music, and control your other devices.
  actions:
    - theme: brand
      text: Download
      link: /download/
    - theme: alt
      text: What is Spotifast?
      link: /what-is-spotifast/
    - theme: alt
      text: GitHub
      link: https://github.com/crmne/spotifast
  image:
    src: /screenshot.png
    alt: "Spotifast Home with the playlist library, recommendations, queue, and player visible"
    width: 2018
    height: 1198

features:
  - icon: ⚡
    title: Lightweight
    details: Opens in well under a second and typically uses just 100–250 MB of memory.
  - icon: 🦀
    title: Native, in Rust
    details: "Built with [egui](https://www.egui.rs) and [fastframe](https://fastframe.dev), the framework behind a family of native desktop apps. No browser engine inside."
  - icon: 🔊
    title: Spotify Connect
    details: Listen on your computer with smooth transitions between songs, or control music on a speaker, phone, or TV. Playback needs Spotify Premium.
  - icon: 📚
    title: Library and search
    details: Browse playlists, Liked Songs, albums, artists, and podcasts. Search the catalogue and edit playlists you own.
  - icon: 🎨
    title: Themes
    details: Choose light, dark, or your own colours. On Omarchy, Spotifast can follow your desktop theme as it changes, while the music keeps playing.
  - icon: 📻
    title: Skins
    details: Wear Spotifast's own Tall and Wide skins, or classic Winamp and Windows Media Player skins, with their playlists, equalizers, and MilkDrop inside.
    link: /winamp/
    link_text: See it in action
  - icon: 🌀
    title: MilkDrop
    details: Watch colourful animations react to your music in their own window or full screen. Choose from more than 10,000 designs.
    link: /milkdrop/
    link_text: Open the guide
  - icon: ⌨️
    title: Desktop controls
    details: Use keyboard shortcuts and your keyboard's media keys. Keep the music playing after you close the window.
  - icon: 🔓
    title: Open source
    details: Free to use, study, and improve. Explore the code, report a problem, or help build the next version.
    link: https://github.com/crmne/spotifast
    link_text: Read the source
---

<video class="hero-film image-src" controls muted loop playsinline preload="metadata" poster="/assets/images/launch-film-poster.jpg" aria-label="Spotifast in a minute: a film of the app, made with its built-in demo content" hidden>
  <source src="/assets/videos/launch-film.mp4" type="video/mp4">
</video>

<script>
  // The theme's hero takes a picture; the film takes its place where
  // scripts run, and the screenshot stays where they do not.
  (function () {
    var film = document.querySelector(".hero-film");
    var slot = document.querySelector(".VPHero .image-container");
    if (!film || !slot) return;
    slot.replaceChildren(film);
    film.hidden = false;
    if (!window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      film.play().catch(function () {});
    }
  })();
</script>

<style>
  .hero-film {
    display: block;
    background: #0b0e0c;
  }
</style>

## Skins of its own, and the classics

Spotifast comes with two skins of its own. **Spotifast Tall** is a small white
ceramic ribbon round a smoked-glass core, made to live at the side of your
screen, and **Spotifast Wide** is the same object laid on its side. A thread of
green light traces each one as it opens, and a click fills the glass with
MilkDrop. The glass also turns to an equalizer or your queue.

<video class="skins-showcase" autoplay loop muted playsinline preload="metadata" poster="/assets/images/skins-poster.jpg" aria-label="Spotifast Tall and Spotifast Wide opening with a thread of green light, then filling their glass with MilkDrop">
  <source src="/assets/images/skins.mp4" type="video/mp4">
</video>

The mini player wears classic skins too. Load a Winamp `.wsz` skin from the
[Winamp Skin Museum](https://skins.webamp.org), with its equalizer, playlist,
and a MilkDrop panel in the skin's own frame, or a Windows Media Player `.wmz`
skin from the Internet Archive's
[collection](https://archive.org/details/windowsmediaplayerskinscollection),
whose drawers and scripts run as they did in Windows Media Player, in a
sandbox. MilkDrop plays in their visualization areas.
[See the mini player in detail](/winamp/).

<div class="skins-gallery">
  <img src="/assets/images/skins-gallery.jpg" alt="Spotifast's Winamp skin with its equalizer, playlist and MilkDrop panel, Spotifast Tall and Spotifast Wide, beside Headspace, Cerulean and Activate, three classic Windows Media Player skins, every one running MilkDrop" width="1600" height="790" loading="lazy">
</div>

Watch them run, with MilkDrop inside, in the 0.17 film:

<video class="skins-showcase skins-film" controls playsinline preload="none" poster="/assets/images/skins-0-17-poster.jpg" aria-label="Spotifast 0.17 in 31 seconds: Spotifast Tall and Wide, the Winamp skin and three classic Windows Media Player skins, all running MilkDrop, then MilkDrop behind lyrics and alone">
  <source src="/assets/videos/skins-0-17.mp4" type="video/mp4">
</video>

## MilkDrop with more than 10,000 presets

On first use, Spotifast automatically downloads the original MilkDrop 2
presets and projectM's Cream of the Crop collection. These visual designs
react to music playing on your computer, in a resizable window or full screen.
[See the controls and preset details](/milkdrop/).

<video class="milkdrop-showcase" autoplay loop muted playsinline preload="metadata" poster="/assets/images/milkdrop-poster.jpg" aria-label="MilkDrop presets reacting to music in Spotifast">
  <source src="/assets/images/milkdrop.mp4" type="video/mp4">
</video>

MilkDrop can also play behind your lyrics in full screen, dimmed until the
words are clear, with the line being sung in the accent colour. Click the album
art and everything else steps aside for MilkDrop, edge to edge.

<video class="milkdrop-showcase lyrics-showcase" autoplay loop muted playsinline preload="metadata" poster="/assets/images/lyrics-milkdrop-poster.jpg" aria-label="Full screen lyrics with MilkDrop behind the words, then MilkDrop alone">
  <source src="/assets/images/lyrics-milkdrop.mp4" type="video/mp4">
</video>

## WhatsApp, just as fast

**Want WhatsApp just as fast and native?** [ZapFast](https://zapfast.rocks) is
Spotifast's sibling: the same native interface, for WhatsApp. Both are built on [fastframe](https://fastframe.dev), the shared
foundation for native Rust apps built with [egui](https://www.egui.rs).

<style>
  /* The hero image slot is sized for a square logo; the screenshot needs the
     room. Page-scoped overrides, so the theme stays untouched. */
  .VPHero .image-container {
    width: 100% !important;
    height: auto !important;
    transform: none !important;
  }
  .VPHero .image-src {
    position: relative !important;
    top: auto !important;
    left: auto !important;
    transform: none !important;
    width: 100% !important;
    height: auto !important;
    max-width: 100% !important;
    max-height: none !important;
    padding: 0 !important;
    border-radius: 12px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.45);
  }
  .skins-gallery {
    margin-top: 16px;
  }
  .skins-film {
    margin-top: 12px;
  }
  .skins-gallery img,
  .skins-showcase,
  .milkdrop-showcase {
    display: block;
    width: 100%;
    max-width: 100%;
    height: auto;
    border-radius: 12px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.35);
  }
  .lyrics-showcase {
    margin-top: 16px;
  }
  @media (max-width: 959px) {
    .VPHero .image {
      margin: 0 0 24px !important;
    }
  }
</style>
