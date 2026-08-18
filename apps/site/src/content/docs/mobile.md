---
title: "Phones and tablets"
description: "The phone and tablet client: PWA install, the on-screen key row, swipe gestures, why the soft keyboard never reflows the terminal, and iOS notifications."
order: 6
section: "Concepts"
---

## The same app, not a companion

There is no separate mobile app to install and nothing cut down about the mobile
surface. The same web app runs on iPhone, Android, iPad, and Android tablets, and
it renders the same real PTY as the desktop. A tablet with a keyboard keeps the
desktop layout, panes, and keyboard shortcuts unchanged. A phone gets a layout
designed for one thumb, described below.

## Install it to the home screen

Roost ships a web app manifest with `display: standalone`, so **Add to Home
Screen** gives you a standalone app with its own icon and no browser chrome. The
manifest declares an SVG icon plus 192 px, 512 px, and maskable 512 px PNGs, and a
`#0d0d0d` theme and background colour so the launch and status-bar chrome match
the app.

Installing is also a prerequisite for notifications on iOS — see below.

## The on-screen key row

A terminal needs keys a soft keyboard does not have. Tap the keyboard button to
open Roost's key pad; the open/closed state is remembered per device.

The pad carries `esc`, `tab`, a **latching ctrl**, backspace, `home`, `end`, page
up, page down, all four arrows, and `enter`, plus a toggle for mouse forwarding.

`ctrl` latches rather than requiring a chord: tap it to arm, then tap a letter — so
`Ctrl+C` is two taps and never a two-finger gymnastics exercise. Closing the pad
clears an armed `ctrl`, so it can never leak into the next thing you type.

The mouse key toggles this device's mouse-forwarding preference. Forwarding still
only happens when the running application actually requested mouse tracking; see
[the terminal](/docs/terminal/).

## The soft keyboard translates the layout, it does not reflow the terminal

This is the detail that makes typing on a phone usable. Roost does **not** resize
the terminal when the keyboard opens. Resizing would resize the PTY and re-wrap
every pane, which is disruptive at the exact moment you are trying to type — and
worse with several terminals on screen.

Instead the keyboard overlays the viewport, Roost measures how much of the layout
it covers, publishes that as a CSS offset, and the app shell translates the
content up by that amount. The input rides just above the keyboard, the top of
the layout scrolls off, and the terminal keeps its exact size and grid. Coverage
below 80 px is ignored as noise, and the measurement re-settles at 80 ms, 240 ms,
and 500 ms after the event, because browsers report the transition at different
moments. iOS Safari is handled through the visual viewport API and Chrome through
`interactive-widget=resizes-visual`.

## The card deck

On a phone, the sessions in a workspace are a swipeable deck of terminal cards
rather than a row of tiny tabs. The gesture thresholds are borrowed from the
platform conventions people already have in their fingers:

- **Switch card** on release when the drag has travelled at least 40% of the card
  width, or on a directional flick faster than 0.6 px/ms that moved at least 12%
  of the width. A release in the wrong direction never commits.
- **Peek** while the gesture is armed: the current card scales to 90%, slides up
  to 5% of the width, and its corners round to 28 px, so you can see the neighbour
  you are about to land on.
- **Dismiss** a card on 144 px of travel, or a flick faster than 0.5 px/ms that
  moved at least 24 px. Both directions dismiss.

## Selecting, copying, and files

Terminal text is selectable by touch, and each terminal is a real terminal — full
ANSI, colours, and scrollback, on a phone. Copy-on-select is a per-device toggle
in **Settings → Terminal**, off by default because it silently overwrites the
system clipboard. You can attach a file into a session and download files from a
worker to the device you are holding.

## Dictation

Typing a long prompt on a phone is miserable, so tap the mic and dictate: the
recognized text is typed into the session as real input and sent, with no review
step in between.

The zero-setup path uses the browser's own speech recognition, which is a rough
fallback. The recommended path is a Deepgram API key added once in **Settings →
Voice**: the key is stored on the coordinator and shared to every paired device,
so you configure it once rather than per phone. Language can be a single language,
multilingual, or automatic. Roost also extracts terminal jargon from the screen
you are dictating into and passes it as keyterm bias, so project vocabulary and
identifiers transcribe as themselves rather than as phonetic mush.

## Notifications, and the iOS prerequisite

Agent state — working, needs input, done — updates the sidebar row, tab, mobile
card, and folder rollup with no setup at all, and a background agent that stops
for input or finishes raises an in-app toast plus an unseen count in the tab
title.

OS notifications, the kind that reach you when Roost is not the tab you are
looking at, need one explicit grant per device, because a browser only asks on a
real click:

1. Open Roost on that device and go to **Settings → Notifications**.
2. Turn on **Desktop notifications** and accept the browser permission prompt.
   **On iPhone and iPad, add Roost to the Home Screen first and open it from
   there** — Safari only allows notifications for installed web apps.
3. Optionally turn on a sound for "needs input" and for "finished".

Each device subscribes separately, and a device that is actively viewing the
session a notification is about does not get an OS notification for it. Tapping a
notification opens that session.

## Next

- [Agents](/docs/agents/) — what the badges mean and how they are derived
- [The terminal](/docs/terminal/) — mouse modes, links, predictive echo
- [Networking](/docs/networking/) — reaching your fleet from a phone with no VPN client
- [Quickstart](/docs/quickstart/) — pair a phone by QR
