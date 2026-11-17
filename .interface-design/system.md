# Ferrogram interface system

The source of truth for values is the `Theme` global in `crates/ui/ui/app.slint`. This file records
the decisions behind it. Code uses `Theme.*` tokens only: no hex colours, font sizes or corner radii
written inline.

## Direction

Ferrogram looks like Telegram Desktop's night theme on purpose. People switch from the official client
and should feel at home, so we do not invent our own visual identity. The craft goal is consistency
and restraint, not novelty.

- **Feel:** a calm chat app at night. Dark blue-grey surfaces and one blue accent. Content (messages,
  media) leads; chrome recedes.
- **OLED mode:** surfaces turn black. Accents and text stay the same.
- **Focal element:** the open conversation. The chat list and header are quieter.

## Colour

All colours are tokens in `Theme`, in these groups:

| Group | Tokens | Rule |
| --- | --- | --- |
| Surfaces | `window` < `panel` < `field`, `incoming`, `outgoing`, `menu` | One blue-grey hue; elevation is a small step in lightness. Popovers (`menu`) sit one step above `panel`. |
| Text | `text`, `muted`, `time-in`, `time-out` | `text` for content and names, `muted` for previews and metadata. |
| Accent | `accent`, `accent-hover`, `badge`, `badge-hover`, `badge-muted`, `check`, `selected`, `highlight`, `accent-wash` | One accent, used for actions, links, unread badges and own read state. Unmotivated colour is not allowed. |
| On strong surfaces | `on-strong`, `on-strong-2`, `on-strong-3`, `track` | White at three levels (100 %, 82 %, 69 %) over accent, selection, media and scrims. |
| Overlays | `scrim`, `scrim-hover`, `shadow`, `menu-hover`, `menu-edge`, `date` | Translucent black for pills over media and dialog backdrops. One shadow strength. |
| Status | `danger`, `warning-surface`/`warning-text`, `error-surface`/`error-text` | Banners use a dark tint of the status hue with a light text of the same hue. |
| Fixed | `qr-paper`, `media-black`, `avatar-fallback` | Must not follow the theme (a QR code needs white paper). |

Per-person name colours are computed in Rust (`peer_color`), as in Telegram, and are not tokens.

## Depth

The theme uses surface shifts and few borders. Floating layers (the message menu, the chat-list
menu, the scroll-down button) get the single `shadow` token. The message menu also gets a 1 px
`menu-edge` line, because shadows read weakly on dark surfaces. There are no decorative borders.
Input fields have a `field-edge` outline that turns `accent` on focus.

## Type

The font is the system UI font. Message text is rendered by the Rust text engine (parley and swash)
at 14 px; service pills and single emoji use the same engine at other sizes (`render_sized`).

| Token | Size | Use |
| --- | --- | --- |
| `type-caption` | 11 px | Download percentage, the failed-send mark |
| `type-meta` | 12 px | Times, counters, durations |
| `type-small` | 13 px | Sender names, quotes, banners, date pills |
| `type-body` | 14 px | UI text, chat titles in the list, buttons, menus |
| `type-title` | 15 px | Open chat title, inputs |
| `type-heading` | 16 px | Panel heading ("Chats") |
| `type-dialog` | 19 px | Dialog titles |
| `type-display` | 22 px | Sign-in heading |

Hierarchy comes from weight and colour as much as size: 600 for names and titles, regular for
content, and `muted` for secondary text.

## Shape

| Token | Value | Use |
| --- | --- | --- |
| `radius-small` | 4 px | Bubble tail corner, quote block, checkbox |
| `radius-control` | 6 px | Buttons, text inputs |
| `radius-panel` | 10 px | Menus, dialogs |
| `radius-bubble` | 12 px | Message bubbles, media corners, date pills, QR card |

Circles and pills (avatars, badges, round buttons, time and duration pills, dots, the seek knob)
use `self.height / 2` and never a literal. Waveform bars use `self.width / 2` for round caps.

## Spacing and layout

- The base unit is 2 px with a preference for multiples of 4. The feed is laid out in Rust
  (`crates/ui/src/models.rs`: `PAD` 10, `PAD_TOP` 6, `GAP` 8, `GAP_RUN` 2, `DATE` 40). Keep Slint
  offsets consistent with those constants.
- Bubbles are at most 504 px wide, with 14 px from the feed edge.
- The chat list is 300 px wide and has the same surface as the header (`panel`).

## Component patterns

- **Message menu:** 210 px wide. Rows are 38 px high, with a 20 px outline icon at x 16 and the
  label at x 52. Uses `menu`, `menu-edge` and `radius-panel`; row hover is `menu-hover`.
- **Service pill:** centred. Text rendered at 13 px, with padding 10 × 4 px, background `date` and
  radius `min(radius-bubble, height / 2)`.
- **Time over media:** an 18 px pill on `scrim` with `on-strong` text.
- **Round media button:** 48 px, `scrim`, or `scrim-hover` on hover.
- **Action button:** 40 px high, `badge`, or `badge-hover` on hover, with `radius-control` and
  14 px weight 600 text.
