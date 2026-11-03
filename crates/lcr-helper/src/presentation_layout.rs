// Product-specific DIP layout; also owns the bounds used by native controls.
const GRID: f32 = 8.0;
const HALF_GRID: f32 = GRID / 2.0;
const MAIN_BRAND_SCALE: f32 = 1.10;
const MAIN_CONTENT_Y_OFFSET: f32 = -16.0;
const MAIN_UTILITY_Y_OFFSET: f32 = 6.0;
const WORDMARK_ASPECT: f32 = 43.0 / 192.0;
const WINDOW_WIDTH: i32 = 480;
const WINDOW_HEIGHT: i32 = 640;
const CONTENT_LEFT: i32 = 4 * GRID as i32;
const CONTENT_RIGHT: i32 = WINDOW_WIDTH - CONTENT_LEFT;
const FOOTER_TOP: i32 = 592;
const MODULE_GAP: f32 = 3.0 * GRID;
const UTILITY_PADDING: f32 = GRID;
const fn g(units: f32) -> f32 {
    units * GRID
}

// One physical pixel, centered on a device pixel at every current DPI.
fn snap_hairline(value: f32, dpi: u32) -> f32 {
    let scale = dpi as f32 / 96.0;
    (value.mul_add(scale, 0.0).floor() + 0.5) / scale
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BoxDip {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}
impl BoxDip {
    const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    fn right(self) -> f32 {
        self.x + self.w
    }
    fn bottom(self) -> f32 {
        self.y + self.h
    }
    fn cx(self) -> f32 {
        self.x + self.w / 2.0
    }
    fn cy(self) -> f32 {
        self.y + self.h / 2.0
    }
    fn inset(self, horizontal: f32, vertical: f32) -> Self {
        Self::new(
            self.x + horizontal,
            self.y + vertical,
            self.w - 2.0 * horizontal,
            self.h - 2.0 * vertical,
        )
    }
    fn native(self, dpi: u32) -> Rect {
        let scale = dpi as f32 / 96.0;
        Rect {
            left: (self.x * scale).round() as i32,
            top: (self.y * scale).round() as i32,
            right: (self.right() * scale).round() as i32,
            bottom: (self.bottom() * scale).round() as i32,
        }
    }
}

#[derive(Clone, Copy)]
struct Layout {
    viewport: BoxDip,
    content: BoxDip,
    close: BoxDip,
    pair_logo: BoxDip,
    pair_wordmark: BoxDip,
    pair_descriptor: BoxDip,
    pair_heading: BoxDip,
    pair_field: BoxDip,
    pair_icon: BoxDip,
    pair_content: BoxDip,
    pair_accent: BoxDip,
    pair_action: BoxDip,
    pair_ready: BoxDip,
    header_logo: BoxDip,
    header_wordmark: BoxDip,
    header_descriptor: BoxDip,
    tagline: BoxDip,
    section: BoxDip,
    key: BoxDip,
    key_caption: BoxDip,
    sound: BoxDip,
    toggle: BoxDip,
    unpair: BoxDip,
    confirm_cancel: BoxDip,
    confirm_unpair: BoxDip,
    status: BoxDip,
    status_separators: [f32; 2],
    status_cells: [BoxDip; 3],
    footer: BoxDip,
    footer_message: BoxDip,
    footer_version: BoxDip,
}
impl Layout {
    fn new() -> Self {
        let viewport = BoxDip::new(0.0, 0.0, g(60.0), g(80.0));
        let content = BoxDip::new(
            CONTENT_LEFT as f32,
            0.0,
            (CONTENT_RIGHT - CONTENT_LEFT) as f32,
            viewport.h,
        );
        let row = |y: f32, h: f32| BoxDip::new(content.x, y, content.w, h);
        let main_row = |y: f32, h: f32| row(y + MAIN_CONTENT_Y_OFFSET, h);
        let pair_logo = BoxDip::new(viewport.cx() - g(12.0), g(6.0), g(24.0), g(24.0));
        // Deliberate one-grid logo/wordmark overlap; letters render below mark.
        let pair_wordmark = BoxDip::new(
            viewport.cx() - g(9.0),
            pair_logo.bottom() - g(2.0),
            g(18.0),
            g(18.0) * WORDMARK_ASPECT,
        );
        // Half-grid optical correction offsets Jura's measured line leading;
        // visible descriptor ink sits about one grid below the raster letters.
        let pair_descriptor = row(pair_wordmark.bottom() + HALF_GRID, g(2.0));
        let pair_field = row(g(50.0), g(7.0)).inset(g(2.0), 0.0);
        let pair_icon = BoxDip::new(pair_field.x, pair_field.y, g(5.0), pair_field.h);
        let pair_accent = BoxDip::new(
            pair_icon.right() + 3.0,
            pair_field.cy() - g(1.5),
            0.0,
            g(3.0),
        );
        let pair_content = BoxDip::new(
            pair_accent.x + GRID,
            pair_field.y,
            2.0 * (pair_field.cx() - (pair_accent.x + GRID)),
            pair_field.h,
        );
        let close = BoxDip::new(content.right() - g(3.0) + 10.0, g(2.0), g(4.0), g(4.0));
        let header_logo = BoxDip::new(
            g(3.0) - 10.0,
            g(2.0),
            g(13.0) + HALF_GRID,
            g(13.0) + HALF_GRID,
        );
        let header_wordmark = BoxDip::new(
            g(18.5) - 10.0 - 5.0,
            header_logo.cy() - (g(13.0) * WORDMARK_ASPECT + HALF_GRID + g(2.0)) / 2.0,
            g(13.0) * MAIN_BRAND_SCALE,
            g(13.0) * WORDMARK_ASPECT * MAIN_BRAND_SCALE,
        );
        let header_descriptor = BoxDip::new(
            header_wordmark.x,
            header_wordmark.bottom() + HALF_GRID,
            content.right() - header_wordmark.x,
            g(2.0) * MAIN_BRAND_SCALE,
        );
        let key = main_row(g(36.0) + 10.0, g(12.0)).inset(0.0, 5.0);
        let key_caption = BoxDip::new(key.x, key.bottom() - GRID, key.w, g(2.0));
        let sound = main_row(g(53.0) + MAIN_UTILITY_Y_OFFSET, g(6.0));
        let toggle = BoxDip::new(
            sound.right() - g(20.0),
            sound.cy() - g(2.5),
            g(20.0),
            g(5.0),
        );
        let unpair = row(sound.bottom() + 2.0 * UTILITY_PADDING, sound.h);
        let status_separators = [
            unpair.bottom() + UTILITY_PADDING,
            FOOTER_TOP as f32 - GRID + MAIN_CONTENT_Y_OFFSET + MAIN_UTILITY_Y_OFFSET,
        ];
        let status = row(
            (status_separators[0] + status_separators[1]) / 2.0 - g(2.0),
            g(4.0),
        );
        let status_cells = [
            BoxDip::new(status.x, status.y, g(16.0), status.h),
            BoxDip::new(status.x + g(16.0), status.y, g(18.0), status.h),
            BoxDip::new(status.x + g(34.0), status.y, g(18.0), status.h),
        ];
        let footer = row(FOOTER_TOP as f32, g(3.0));
        let footer_version = BoxDip::new(footer.right() - g(13.0), footer.y, g(13.0), footer.h);
        Self {
            viewport,
            content,
            close,
            pair_logo,
            pair_wordmark,
            pair_descriptor,
            pair_heading: row(pair_field.y - MODULE_GAP - g(4.0), g(4.0)),
            pair_field,
            pair_icon,
            pair_content,
            pair_accent,
            pair_action: row(pair_field.bottom() + MODULE_GAP, g(8.0)).inset(g(2.0), 0.0),
            pair_ready: row(g(72.0), g(4.0)),
            header_logo,
            header_wordmark,
            header_descriptor,
            tagline: main_row(g(28.0), g(3.0)),
            section: main_row(g(31.0) + GRID - 5.0 + 10.0 + 5.0, g(3.0)),
            key,
            key_caption,
            sound,
            toggle,
            unpair,
            confirm_cancel: BoxDip::new(content.x, g(54.0), g(25.0), g(6.0)),
            confirm_unpair: BoxDip::new(content.right() - g(25.0), g(54.0), g(25.0), g(6.0)),
            status,
            status_separators,
            status_cells,
            footer,
            footer_message: BoxDip::new(
                footer.x,
                footer.y,
                footer_version.x - GRID - footer.x,
                footer.h,
            ),
            footer_version,
        }
    }
    fn edit(self) -> BoxDip {
        BoxDip::new(
            self.pair_content.x,
            self.pair_content.cy() - g(1.5),
            self.pair_content.w,
            g(3.0),
        )
    }
    fn sound_half(self, on: bool) -> BoxDip {
        BoxDip::new(
            self.toggle.x + if on { 0.0 } else { self.toggle.w / 2.0 },
            self.toggle.y,
            self.toggle.w / 2.0,
            self.toggle.h,
        )
    }
    fn sound_active(self, on: bool) -> BoxDip {
        self.sound_half(on).inset(-3.0, -3.0)
    }
    fn sound_hit(self, on: bool) -> BoxDip {
        // Both halves toggle identically; partition the expanded surface once.
        BoxDip::new(
            if on {
                self.toggle.x - 3.0
            } else {
                self.toggle.cx()
            },
            self.toggle.y - 3.0,
            self.toggle.w / 2.0 + 3.0,
            self.toggle.h + 6.0,
        )
    }
    fn key_control(self) -> BoxDip {
        // The border caption remains part of the clickable Radio Key surface.
        BoxDip::new(
            self.key.x,
            self.key_caption.y.min(self.key.y),
            self.key.w,
            self.key.bottom().max(self.key_caption.bottom()) - self.key_caption.y.min(self.key.y),
        )
    }
}

#[derive(Clone, Copy)]
enum AlignH {
    Left,
    Center,
    Right,
    AnchorO,
}
#[derive(Clone, Copy)]
enum AlignV {
    Middle,
    CapMiddle,
    Baseline(f32),
}

// Four rendered text recipes; UI-label tracking is exactly14% of its em.
const UI_LABEL_SIZE: f32 = 12.0;
const UI_LABEL_TRACKING: f32 = UI_LABEL_SIZE * 0.14;
#[derive(Clone, Copy, Debug, PartialEq)]
enum TypeRole {
    HeroValue,
    PrimaryAction,
    UiLabel,
    Metadata,
}
impl TypeRole {
    const ALL: [Self; 4] = [
        Self::HeroValue,
        Self::PrimaryAction,
        Self::UiLabel,
        Self::Metadata,
    ];
    fn token(self) -> (f32, i32, f32) {
        match self {
            Self::HeroValue => (32.0, 500, 0.0),
            Self::PrimaryAction => (20.0, 500, 2.8),
            Self::UiLabel => (UI_LABEL_SIZE, 500, UI_LABEL_TRACKING),
            Self::Metadata => (10.5, 400, 1.0),
        }
    }
    fn index(self) -> usize {
        Self::ALL.iter().position(|r| *r == self).unwrap()
    }
}

#[derive(Clone, Copy)]
enum Glow {
    Ambient,
    Interactive,
    Primary,
    Active,
}
impl Glow {
    fn intensity(self) -> f32 {
        match self {
            Self::Ambient => 0.22,
            Self::Interactive => 0.52,
            Self::Primary => 0.82,
            Self::Active => 1.0,
        }
    }
}
#[derive(Clone, Copy)]
enum VisualRole {
    PrimaryAction,
    PrimaryDestructive,
    Danger,
    SecondaryAction,
    PrimaryValue,
    StatusHealthy,
    StatusIdle,
    Meta,
}

#[cfg(test)]
mod grid_contract_tests {
    use super::*;
    #[test]
    fn shared_bounds_remain_inside_fixed_viewport_at_supported_scales() {
        let l = Layout::new();
        for dpi in [96, 120, 144, 192] {
            let viewport = l.viewport.native(dpi);
            for cell in [
                l.close,
                l.edit(),
                l.pair_action,
                l.key,
                l.toggle,
                l.confirm_cancel,
                l.confirm_unpair,
                l.status,
                l.footer,
            ] {
                let r = cell.native(dpi);
                assert!(
                    r.left >= 0
                        && r.top >= 0
                        && r.right <= viewport.right
                        && r.bottom <= viewport.bottom
                );
            }
        }
        assert_eq!(l.status_cells[0].right(), l.status_cells[1].x);
        assert_eq!(l.status_cells[1].right(), l.status_cells[2].x);
        assert_eq!(l.status_cells[2].right(), l.content.right());
        assert_eq!(l.key, BoxDip::new(32.0, 287.0, 416.0, 86.0));
        assert_eq!(l.pair_field.y - l.pair_heading.bottom(), MODULE_GAP);
        assert_eq!(l.pair_action.y - l.pair_field.bottom(), MODULE_GAP);
        assert_eq!(l.unpair.y - l.sound.bottom(), 2.0 * UTILITY_PADDING);
        assert_eq!(l.pair_heading.cx(), l.viewport.cx());
        assert_eq!(l.pair_ready.cx(), l.viewport.cx());
        assert_eq!(l.footer_message.cy(), l.footer_version.cy());
        assert_eq!(l.pair_accent.x, l.pair_icon.right() + 3.0);
        assert_eq!(l.pair_field.cx(), 240.0);
        assert_eq!(l.pair_field.w, 384.0);
        assert_eq!(l.pair_action.cx(), 240.0);
        assert_eq!(l.pair_action.w, 384.0);
        assert_eq!(
            l.status.cy() - l.status_separators[0],
            l.status_separators[1] - l.status.cy()
        );
        assert!(
            l.pair_content.x > l.pair_accent.x && l.pair_content.right() < l.pair_field.right()
        );
        for dpi in [96, 120, 144, 192] {
            let scale = dpi as f32 / 96.0;
            assert!((snap_hairline(32.0, dpi) * scale).fract() == 0.5);
            assert!((96.0 / dpi as f32 * scale - 1.0).abs() < 0.001);
        }
    }
}
