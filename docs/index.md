---
layout: home
title: Spotifast
description: A fast, lightweight Spotify app for Linux, macOS, and Windows.
permalink: /
hero:
  name: Spotifast
  text: Spotify, native and fast
  tagline: Formerly Fastpotify. A lightweight Spotify app for Linux, macOS, and Windows. Listen on your computer, browse your music, and control your other devices.
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
    title: Winamp mini player
    details: Bring back the classic Winamp look, with skins, a playlist, sound controls, and animations that move to your music.
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

## It turns into Winamp

Load a classic `.wsz` skin from the
[Winamp Skin Museum](https://skins.webamp.org). The mini player includes
animated sound displays, an equalizer to adjust your sound, and a playlist.
Roll it up into a thin bar or enlarge it while keeping the classic pixels
sharp. [See the mini player in detail](/winamp/).

<div class="winamp-showcase">
  <img src="/assets/images/winamp.png" alt="The mini player wearing the built-in skin" width="550" height="812">
</div>

## MilkDrop with more than 10,000 presets

On first use, Spotifast automatically downloads the original MilkDrop 2
presets and projectM's Cream of the Crop collection. These visual designs
react to music playing on your computer, in a resizable window or full screen.
[See the controls and preset details](/milkdrop/).

<video class="milkdrop-showcase" autoplay loop muted playsinline preload="metadata" poster="/assets/images/milkdrop-poster.jpg" aria-label="MilkDrop presets reacting to music in Spotifast">
  <source src="/assets/images/milkdrop.mp4" type="video/mp4">
</video>

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
  .winamp-showcase {
    text-align: center;
  }
  .winamp-showcase img,
  .milkdrop-showcase {
    max-width: 100%;
    height: auto;
    border-radius: 12px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.35);
  }
  .milkdrop-showcase {
    display: block;
    width: 100%;
  }
  @media (max-width: 959px) {
    .VPHero .image {
      margin: 0 0 24px !important;
    }
  }
</style>
