# Native Helper presentation assets

`source/mark.png`, `source/wordmark.png` and `source/background.png` are the
Owner-supplied inputs. `SOURCE_HASHES.txt` identifies them. The mark's alpha is
canonical geometry; no tracing, dilation, negative-space fill or generative
replacement is permitted.
For the V6.2.1/V6.3 candidate, `source/mark.png` is the exact Owner-supplied
`New Logo.png`, superseding the older mark. Tiny supplied alpha speckles are
preserved. The wordmark raster uses only the original LCR letters; the separated
descriptor is native Jura text, with independent size and vertical spacing.

`tools/generate-presentation-assets.cjs` (relative to the Helper crate) prepares
the static assets offline with Node and sharp. Those tools are not Helper build
or runtime dependencies. Rerun it only when intentionally regenerating this
asset set. It needs no network: the exact Tabler SVG subset and MIT license are
vendored under `tabler/` with upstream revision recorded in `UPSTREAM.txt`.
Jura is bundled under `jura/` with its OFL and pinned upstream provenance.
DirectWrite loads it through an isolated in-memory font collection, released
with the renderer. GDI registration is retained only for the native EDIT caret /
selection integration. There is no global font install or custom-font fallback.

Each `.bgra` contains little-endian u32 width/height followed by top-down,
premultiplied BGRA8 pixels. Direct2D uploads them directly into cached bitmaps
for each render target; no runtime decoder/WIC/SVG library is needed. Backgrounds contain atmosphere only, never
text, logos, controls, frames or state. The latest Owner-supplied
`C:/Users/Kjndst/Pictures/Menu-Background.png` replaces `source/menu-background.png`
byte-for-byte (1087 x 1800). Its decoded pixels are cached at full resolution,
with only the required lossless premultiplied-BGRA representation; no offline
crop, resize, brightness, blur or gradient. Only this Main asset may reach the
bounded 2048-pixel dimension limit; all other cached assets retain the 640 limit.
Its BGRA payload is 7,826,408 bytes including the eight-byte header. Direct2D
applies uniform cover scaling, centered geometric clipping to the 480 x 640 DIP
cut-corner frame, then a uniform black 50% overlay before foreground rendering.
Main/Confirm/Capture use this same scene, translated down 10 DIPs after cover placement. The previous offline Main processing
and diagonal shade brush are removed.
Pair retains its exact original `source/background.png`; its cached globe/radar
plate now uses brightness 0.60 instead of 0.52 (+15.4%) and draws at X=-3 DIPs. Its geometric center
is therefore x=237 while foreground groups remain x=240. Original castle/planet
details and any source marks remain untouched. Edge clipping is deterministic;
the scene is clipped to the existing cut-corner frame, so no source pixels
can reach outside that frame. The cached mask is factory-owned; its layer
is recreated with each dependent render target; dimming reuses the
existing solid brush.

The app PNGs and `../LCR_Helper.ico` now derive only from the separate exact
Owner attachment `source/app-icon.png` (Biểu tượng năng lượng đỏ tương lai.png).
Resize its supplied RGBA artwork directly, preserving its canvas/material/alpha;
no logo replacement, recoloring, tracing or generative edit. ICO sizes are256,
128,64,48,32,16. UI `mark.png`/`mark.bgra` and wordmark remain byte-exact.
`--app-icon-only` regenerates only these app PNG/ICO
payloads and provenance, leaving every UI/tray asset untouched.
Owner's interaction-polish gate separately approves new tray artwork from this
app source. `--tray-icons-only` emits64/48/32/24/16px ICOs with the original
disconnected gray, standby green and transmitting red state mapping. It preserves
source alpha/material intensity and does not regenerate any app or UI payload.

The UI uses a fixed 480 × 640 DIP viewport and an 8 DIP / 4 DIP optical grid.
Win32 bounds derive from the same layout under per-monitor V2 DPI awareness.
Transient capture and Unpair confirmation
reuse the existing shell states; Pair/Main are the primary surfaces. No artwork
requires a cosmetic redraw timer.
The sole custom renderer is Direct2D/DirectWrite: measured Jura roles, cached
factory paths, per-target materials/bitmaps and shared antialiased multi-pass
glow. Separators/edges are faint, corners/fields stronger, Pair/destructive action
strongest. Canonical logo pixels/alpha remain unchanged; a stronger three-pass scaled bitmap
aura (8/4/2 DIP spread, Primary glow intensity) renders underneath. Dots use Direct2D circles; functional icons are Tabler.
The grid overlay and metric recorder exist only in test builds and are disabled
by default. HWND output and the Direct2D DC proof target share one drawing path.
V6.4 retains logo, wordmark, app/ICO, font and existing Tabler payloads exactly
to V6.3. The follow-up changes derived Pair/Main plates and adds the access-point
subset at the same pinned Tabler revision, with neutral/green cached variants.
The latest Menu source supersedes the earlier attachment; Pair source,
logo/wordmark/font and all existing icon payloads stay exact.
The core stroke is one physical pixel, snapped by current DPI; antialiased halos
and rounded/vector geometry remain separate. Pair field states share one style
and uppercase-body baseline. Main/Confirm/Capture share one footer component. Long outer edges use a crisp
muted dark gray core; only the four outer corners have strong red glow. Ready
anchors its measured O glyph to the window axis, rather than centering its
complete width. Main text branding alone scales by 10% and shifts left 15 DIPs in total.
Pair/Main wordmark and descriptor add small cached-bitmap/text halo passes;
value/control text stays clean. No source/cache pixels are modified.

Owner's subsequent refinement restores Pair action/field and Radio Key/capture
materials to the pre-control-material V6.4.2 candidate, with prior dimensions
and positions. CLICK TO CHANGE returns to the genuine bottom-border gap and
original text alignment. Native Radio hit bounds reunite caption+box at
(32,303,416,94). DPI-snapped core paths remain separate from halos; only four
96-DPI caption endpoint pixels differ from the earlier fractional endpoint phase.

The chamfered160x40 DIP toggle shell is muted gray and dark inside. The active
80x40 DIP half expands3 DIP on each side into86x46 DIP, drawn above the shell:
active ON has brighter red fill/core/halo; active OFF has brighter dim-gray fill
and white core/halo. Inactive OFF stays dark; inactive ON uses dim red within
the shell. Text remains near-white active / muted inactive. Static target-owned
red/gray materials and bounded cached geometry add no cosmetic clock/dependency.
Native hit rectangles partition the enlarged surface at the original midpoint
without overlap, covering protruding top/bottom/outer edges. Creation and DPI
layout share these same bounds. Both ON/OFF regions still invert the persisted
preference on every click, including selected regions; failed save retains prior
state and feedback. Main background down10 and branding aura/X133 remain;
the latest Main legibility refinement increases render dim from35% to50%.
Pair background stays unchanged.


Contextual guidance refinement: Main/Confirm/Capture native and rendered footer
share default_footer copy; genuine PTT transitions refresh the native copy while
keeping bounded region invalidation. Transmitting footer says RELEASE RADIO KEY
TO FINISH, leaving status in the status row. Relevant notices override guidance.
Pair's lower area replaces ready with one concise error/notice cluster; EDIT keeps
its ordinary cue/value. Empty-code feedback yields visually after actual input.
No bitmap/geometry/material changes; idle captures remain exact to the prior gate.

Interaction polish keeps all layout/hit geometry and bottom caption placement.
Native hover/focus and BM_GETSTATE pressed feedback adjust cached material,
halo and crisp core independently for actions, hollow Radio Key and each toggle
half. Selected toggle remains expanded/strongest; either half still inverts the
persisted preference. The HWND region now ends at the rendered frame core instead
of including its4 DIP dark clear gutter; controls have no outer dark stroke.
Class48/16px icons and explicit WM_SETICON big/small use the approved app ICO;
canonical Start Menu shortcut still selects the installed executable's icon0.

`../connected.wav` is a separate170ms PCM16 mono22050Hz ascending620->1040Hz
cue (0.13 amplitude, subtle0.12 harmonic,9ms attack/52ms release). Only successful
Pair polling that transitions Pair->Main schedules it; redraw, failure, startup
restore and repeated Main completion do not. PTT cue assets/preference stay exact.
Native EDIT Enter sends the same PAIR WM_COMMAND as the button; held key repeats
and its WM_CHAR carriage return are consumed without a duplicate submit path.

Owner accepts the three new tray icons. Main's middle group now has a shared
-16 DIP Y offset (16px at96DPI): tagline, Radio label/key/caption, Sound/toggle,
UNPAIR, status row and separators retain their internal distances/sizes. Native
controls derive from the same layout. Header, scene and footer remain fixed;
the bottom status separator moves from584 to568 while footer stays at592.
The next optical correction adds+6 DIP only to the utility group (Sound/toggle,
UNPAIR and status/separators): its net offset is now-10. Tagline/Radio Key stay
at-16, status lower separator574, footer592. Internal utility geometry is exact.

Golden typography uses four actual Jura formats: HeroValue32/500/0 tracking,
PrimaryAction20/500/2.8, UiLabel12/500/1.68 and Metadata10.5/400/1.0 (DIPs).
UI tracking is14% of em, split0.84 leading/0.84 trailing. Main RADIO KEY,
PTT SOUND and UNPAIR plus Pair READY FOR PAIRING share UiLabel. Modal action
buttons use PrimaryAction. No600 weight is needed. O anchoring remains measured
through DirectWrite; layout's established branding/heading optical anchors are
independent of the new type sizes. Decorative heading flanks retain their span.
Functional colors become near-white TEXT, lighter SECONDARY, readable pink
destructive copy and higher-contrast gray control cores; the decorative outer
frame/separators and interaction energy/geometry remain unchanged.

Test-only receipts query actual DirectWrite family/size/weight/character spacing,
and text-free captures retain the real scene/materials/icons underneath glyphs.
Contrast proof uses specified/composited foreground against those underlying
pixels at glyph ink locations, excluding antialias edge colors as foreground.
The test instrumentation is absent from shipping builds.
