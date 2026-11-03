// One custom renderer; native Win32 controls retain interaction authority.
use ::windows::core::{w, Interface, Result as WinResult};
use ::windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use ::windows::Win32::Graphics::Direct2D::{Common::*, *};
use ::windows::Win32::Graphics::DirectWrite::*;
use ::windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use ::windows::Win32::Graphics::Gdi::HDC;
include!("presentation_layout.rs");

#[cfg(test)]
mod presentation_tests {
    include!("presentation_tests.rs");
}

// Infer the API's transitive Vector2 type without a new direct dependency.
macro_rules! dpoint {
    ($x:expr, $y:expr) => {{
        let mut point = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES::default().startPoint;
        point.X = $x;
        point.Y = $y;
        point
    }};
}
fn dr(r: BoxDip) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}
fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}
const RED: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 1.0,
    g: 52.0 / 255.0,
    b: 62.0 / 255.0,
    a: 1.0,
};
const GREEN: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 57.0 / 255.0,
    g: 220.0 / 255.0,
    b: 137.0 / 255.0,
    a: 1.0,
};
const TEXT: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.98,
    g: 0.98,
    b: 0.99,
    a: 1.0,
};
const SECONDARY: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.78,
    g: 0.80,
    b: 0.83,
    a: 1.0,
};
const META: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.43,
    g: 0.46,
    b: 0.49,
    a: 1.0,
};
const SURFACE_BASE: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.017,
    g: 0.020,
    b: 0.026,
    a: 1.0,
};
const SURFACE_TOP: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.050,
    g: 0.056,
    b: 0.065,
    a: 1.0,
};
const SURFACE_BOTTOM: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.015,
    g: 0.017,
    b: 0.021,
    a: 1.0,
};
const PRIMARY_TOP: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.43,
    g: 0.035,
    b: 0.065,
    a: 1.0,
};
const PRIMARY_BOTTOM: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.11,
    g: 0.012,
    b: 0.022,
    a: 1.0,
};
const STROKE_MUTED: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.60,
    g: 0.62,
    b: 0.65,
    a: 0.7,
};
const SEPARATOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.28,
    g: 0.29,
    b: 0.32,
    a: 0.45,
};
const FRAME_CORE: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.24,
    g: 0.255,
    b: 0.275,
    a: 0.75,
};
fn opacity(color: D2D1_COLOR_F, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: alpha, ..color }
}
impl VisualRole {
    fn color(self) -> D2D1_COLOR_F {
        match self {
            Self::PrimaryAction | Self::PrimaryDestructive | Self::PrimaryValue => TEXT,
            Self::Danger => rgba(1.0, 0.68, 0.70, 1.0),
            Self::StatusHealthy => GREEN,
            Self::SecondaryAction | Self::StatusIdle => SECONDARY,
            Self::Meta => META,
        }
    }
}
#[link(name = "user32")]
extern "system" {
    fn SetThreadDpiAwarenessContext(context: I) -> I;
    fn GetDpiForWindow(h: W) -> u32;
    fn GetDpiForSystem() -> u32;
    fn GetFocus() -> W;
    fn SetCaretPos(x: i32, y: i32) -> i32;
    fn IsWindowEnabled(h: W) -> i32;
    fn IsWindowVisible(h: W) -> i32;
}
struct UiDpiContext(I);
impl UiDpiContext {
    unsafe fn enter() -> Self {
        Self(SetThreadDpiAwarenessContext(-4))
    }
}
impl Drop for UiDpiContext {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}
fn logical_point(point: Pt, dpi: u32) -> Pt {
    let scale = 96.0 / dpi as f32;
    Pt {
        x: (point.x as f32 * scale).round() as i32,
        y: (point.y as f32 * scale).round() as i32,
    }
}
#[link(name = "gdi32")]
extern "system" {
    fn AddFontMemResourceEx(
        data: *const core::ffi::c_void,
        size: u32,
        reserved: *const core::ffi::c_void,
        count: *mut u32,
    ) -> I;
    fn RemoveFontMemResourceEx(font: I) -> i32;
}
// Only native EDIT caret/selection machinery uses this GDI registration.
// Visible custom text, including edit content, is DirectWrite-owned.
struct PrivateFont(I);
impl PrivateFont {
    unsafe fn load() -> Self {
        let bytes = include_bytes!("../assets/presentation/jura/Jura-Variable.ttf");
        let mut count = 0;
        Self(AddFontMemResourceEx(
            bytes.as_ptr().cast(),
            bytes.len() as u32,
            core::ptr::null(),
            &mut count,
        ))
    }
}
impl Drop for PrivateFont {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                RemoveFontMemResourceEx(self.0);
            }
        }
    }
}
struct FontOwner {
    factory: IDWriteFactory5,
    loader: IDWriteInMemoryFontFileLoader,
    collection: IDWriteFontCollection1,
}
impl FontOwner {
    unsafe fn new() -> WinResult<Self> {
        let factory: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_ISOLATED)?;
        let loader = factory.CreateInMemoryFontFileLoader()?;
        factory.RegisterFontFileLoader(&loader)?;
        let result = (|| {
            let bytes = include_bytes!("../assets/presentation/jura/Jura-Variable.ttf");
            let file = loader.CreateInMemoryFontFileReference(
                &factory,
                bytes.as_ptr().cast(),
                bytes.len() as u32,
                None,
            )?;
            let builder = factory.CreateFontSetBuilder()?;
            builder.AddFontFile(&file)?;
            factory.CreateFontCollectionFromFontSet(&builder.CreateFontSet()?)
        })();
        match result {
            Ok(collection) => Ok(Self {
                factory,
                loader,
                collection,
            }),
            Err(error) => {
                let _ = factory.UnregisterFontFileLoader(&loader);
                Err(error)
            }
        }
    }
}
impl Drop for FontOwner {
    fn drop(&mut self) {
        unsafe {
            let _ = self.factory.UnregisterFontFileLoader(&self.loader);
        }
    }
}
#[derive(Clone)]
struct MeasuredText {
    layout: IDWriteTextLayout,
    width: f32,
    height: f32,
    baseline: f32,
    cap_height: f32,
}
struct TextEntry {
    text: String,
    role: TypeRole,
    measured: MeasuredText,
}
#[derive(Clone, Copy)]
enum Shape {
    Frame,
    Field,
    Pair,
    KeyFill,
    KeyBorder,
    Cancel,
    Destructive,
    Capture,
    ToggleShell,
    ToggleOn,
    ToggleOff,
    ToggleDimOn,
    ToggleDimOff,
}
impl Shape {
    fn index(self) -> usize {
        self as usize
    }
}
struct Surface {
    target: ID2D1RenderTarget,
    dc: Option<ID2D1DCRenderTarget>,
    solid: ID2D1SolidColorBrush,
    raised: ID2D1LinearGradientBrush,
    primary: ID2D1LinearGradientBrush,
    scene_layer: ID2D1Layer,
    framed_fill: ID2D1LinearGradientBrush,
    segment_fill: ID2D1LinearGradientBrush,
    segment_off: ID2D1LinearGradientBrush,
    inner_top: ID2D1LinearGradientBrush,
    bitmaps: Vec<ID2D1Bitmap>,
}
impl Surface {
    unsafe fn new(target: ID2D1RenderTarget, dc: Option<ID2D1DCRenderTarget>) -> WinResult<Self> {
        target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
        let solid = target.CreateSolidColorBrush(&TEXT, None)?;
        let gradient =
            |top: D2D1_COLOR_F, bottom: D2D1_COLOR_F| -> WinResult<ID2D1LinearGradientBrush> {
                let stops = target.CreateGradientStopCollection(
                    &[
                        D2D1_GRADIENT_STOP {
                            position: 0.0,
                            color: top,
                        },
                        D2D1_GRADIENT_STOP {
                            position: 1.0,
                            color: bottom,
                        },
                    ],
                    D2D1_GAMMA_2_2,
                    D2D1_EXTEND_MODE_CLAMP,
                )?;
                target.CreateLinearGradientBrush(
                    &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                        startPoint: dpoint!(0.0, 0.0),
                        endPoint: dpoint!(0.0, 64.0),
                    },
                    None,
                    &stops,
                )
            };
        let raised = gradient(SURFACE_TOP, SURFACE_BOTTOM)?;
        let primary = gradient(PRIMARY_TOP, PRIMARY_BOTTOM)?;
        let scene_layer = target.CreateLayer(None)?;
        let framed_fill = gradient(
            rgba(0.016, 0.019, 0.024, 1.0),
            rgba(0.006, 0.008, 0.011, 1.0),
        )?;
        let segment_fill = gradient(rgba(0.70, 0.025, 0.045, 1.0), rgba(0.28, 0.008, 0.02, 1.0))?;
        let segment_off = gradient(rgba(0.20, 0.215, 0.235, 1.0), rgba(0.08, 0.09, 0.105, 1.0))?;
        let inner_top = gradient(opacity(RED, 0.035), opacity(RED, 0.0))?;
        let mut bitmaps = Vec::with_capacity(ASSET_BYTES.len());
        for (index, bytes) in ASSET_BYTES.iter().enumerate() {
            let width = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
            let height = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            let limit = if index == 1 { 2048 } else { 640 };
            if width == 0
                || height == 0
                || width > limit
                || height > limit
                || bytes.len() != 8 + width as usize * height as usize * 4
            {
                return Err(::windows::core::Error::from_hresult(
                    ::windows::core::HRESULT(0x8007000Du32 as i32),
                ));
            }
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            bitmaps.push(target.CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(bytes[8..].as_ptr().cast()),
                width * 4,
                &props,
            )?);
        }
        Ok(Self {
            target,
            dc,
            solid,
            raised,
            primary,
            scene_layer,
            framed_fill,
            segment_fill,
            segment_off,
            inner_top,
            bitmaps,
        })
    }
    unsafe fn color(&self, color: D2D1_COLOR_F) {
        self.solid.SetColor(&color);
    }
    unsafe fn bitmap(&self, index: usize, rect: BoxDip, opacity: f32) {
        self.target.DrawBitmap(
            &self.bitmaps[index],
            Some(&dr(rect)),
            opacity,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
    }
    unsafe fn line(
        &self,
        from: (f32, f32),
        to: (f32, f32),
        color: D2D1_COLOR_F,
        width: f32,
        stroke: &ID2D1StrokeStyle,
    ) {
        self.color(color);
        self.target.DrawLine(
            dpoint!(from.0, from.1),
            dpoint!(to.0, to.1),
            &self.solid,
            width,
            stroke,
        );
    }
    unsafe fn circle(&self, x: f32, y: f32, diameter: f32, color: D2D1_COLOR_F) {
        self.color(color);
        self.target.FillEllipse(
            &D2D1_ELLIPSE {
                point: dpoint!(x, y),
                radiusX: diameter / 2.0,
                radiusY: diameter / 2.0,
            },
            &self.solid,
        );
    }
}
const ASSET_BYTES: [&[u8]; 19] = [
    include_bytes!("../assets/presentation/pair-background.bgra"),
    include_bytes!("../assets/presentation/main-background.bgra"),
    include_bytes!("../assets/presentation/mark.bgra"),
    include_bytes!("../assets/presentation/wordmark.bgra"),
    include_bytes!("../assets/presentation/key-neutral.bgra"),
    include_bytes!("../assets/presentation/key-red.bgra"),
    include_bytes!("../assets/presentation/volume-neutral.bgra"),
    include_bytes!("../assets/presentation/unlink-neutral.bgra"),
    include_bytes!("../assets/presentation/chevron-right-neutral.bgra"),
    include_bytes!("../assets/presentation/access-point-neutral.bgra"),
    include_bytes!("../assets/presentation/antenna-red.bgra"),
    include_bytes!("../assets/presentation/server-neutral.bgra"),
    include_bytes!("../assets/presentation/server-red.bgra"),
    include_bytes!("../assets/presentation/chevrons-right-red.bgra"),
    include_bytes!("../assets/presentation/chevrons-right-green.bgra"),
    include_bytes!("../assets/presentation/unlink-red.bgra"),
    include_bytes!("../assets/presentation/access-point-green.bgra"),
    include_bytes!("../assets/presentation/server-green.bgra"),
    include_bytes!("../assets/presentation/chevron-right-red.bgra"),
];
#[cfg(test)]
#[derive(Clone)]
struct AlignmentRecord {
    name: &'static str,
    rect: BoxDip,
    origin_x: f32,
    origin_y: f32,
    width: f32,
    height: f32,
    baseline: f32,
}
struct Renderer {
    factory: ID2D1Factory,
    formats: Vec<IDWriteTextFormat>,
    text_cache: Vec<TextEntry>,
    paths: Vec<ID2D1PathGeometry>,
    core_paths: Vec<ID2D1PathGeometry>,
    corners: Vec<ID2D1PathGeometry>,
    stroke: ID2D1StrokeStyle,
    fonts: FontOwner,
    hwnd_surface: Option<Surface>,
    print_surface: Option<Surface>,
    layout: Layout,
    dpi: u32,
    cap_ratio: f32,
    scene_clip: D2D1_LAYER_PARAMETERS,
    consecutive_losses: u8,
    target_generation: u64,
    last_error: Option<::windows::core::HRESULT>,
    #[cfg(test)]
    grid: bool,
    #[cfg(test)]
    background_only: bool,
    #[cfg(test)]
    skip_main_dim: bool,
    #[cfg(test)]
    records: Vec<AlignmentRecord>,
    #[cfg(test)]
    hide_text: bool,
    #[cfg(test)]
    typography_records: Vec<String>,
}
impl Renderer {
    unsafe fn new(dpi: u32) -> WinResult<Self> {
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let fonts = FontOwner::new()?;
        let mut formats = Vec::with_capacity(TypeRole::ALL.len());
        for role in TypeRole::ALL {
            let (size, weight, _) = role.token();
            let f = fonts.factory.CreateTextFormat(
                w!("Jura"),
                &fonts.collection,
                DWRITE_FONT_WEIGHT(weight),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            formats.push(f);
        }
        let stroke = factory.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 10.0,
                dashStyle: D2D1_DASH_STYLE_SOLID,
                dashOffset: 0.0,
            },
            None,
        )?;
        let font = fonts.collection.GetFontFamily(0)?.GetFirstMatchingFont(
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
        )?;
        let mut font_metrics = DWRITE_FONT_METRICS::default();
        font.GetMetrics(&mut font_metrics);
        let cap_ratio = font_metrics.capHeight as f32 / font_metrics.designUnitsPerEm as f32;
        let mut layout = Layout::new();
        // Center visible uppercase body, excluding Jura's ascender/line leading.
        // Freeze the established optical geometry independently of new type sizes.
        let brand_cap = 10.5 * MAIN_BRAND_SCALE * cap_ratio;
        let brand_gap = GRID * MAIN_BRAND_SCALE;
        layout.header_wordmark.y =
            layout.header_logo.cy() - (layout.header_wordmark.h + brand_gap + brand_cap) / 2.0;
        layout.header_descriptor.y = layout.header_wordmark.bottom() + brand_gap + brand_cap / 2.0
            - layout.header_descriptor.h / 2.0;
        let heading_cap = 13.0 * cap_ratio;
        layout.pair_heading.y =
            layout.pair_field.y - MODULE_GAP - heading_cap / 2.0 - layout.pair_heading.h / 2.0;
        let mut renderer = Self {
            factory,
            fonts,
            formats,
            stroke,
            layout,
            text_cache: Vec::with_capacity(64),
            paths: Vec::new(),
            core_paths: Vec::with_capacity(13),
            corners: Vec::new(),
            hwnd_surface: None,
            print_surface: None,
            dpi,
            cap_ratio,
            scene_clip: D2D1_LAYER_PARAMETERS::default(),
            consecutive_losses: 0,
            target_generation: 0,
            last_error: None,
            #[cfg(test)]
            grid: false,
            #[cfg(test)]
            background_only: false,
            #[cfg(test)]
            skip_main_dim: false,
            #[cfg(test)]
            records: Vec::new(),
            #[cfg(test)]
            hide_text: false,
            #[cfg(test)]
            typography_records: Vec::new(),
        };
        let caption = renderer
            .measure(l::KEY_CHANGE_HINT, TypeRole::Metadata)?
            .width
            + g(2.0);
        for (rect, gap) in renderer.shape_specs(caption) {
            renderer.paths.push(renderer.path(rect, gap, false)?);
        }
        let frame = layout.viewport.inset(HALF_GRID, HALF_GRID);
        renderer.scene_clip.contentBounds = dr(frame);
        renderer.scene_clip.geometricMask = core::mem::ManuallyDrop::new(Some(
            renderer.paths[Shape::Frame.index()].cast::<ID2D1Geometry>()?,
        ));
        renderer.scene_clip.maskAntialiasMode = D2D1_ANTIALIAS_MODE_PER_PRIMITIVE;
        renderer.scene_clip.maskTransform.M11 = 1.0;
        renderer.scene_clip.maskTransform.M22 = 1.0;
        renderer.scene_clip.opacity = 1.0;
        for (x, y, dx, dy) in [
            (frame.x, frame.y, 1.0, 1.0),
            (frame.right(), frame.y, -1.0, 1.0),
            (frame.right(), frame.bottom(), -1.0, -1.0),
            (frame.x, frame.bottom(), 1.0, -1.0),
        ] {
            let path = renderer.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.BeginFigure(dpoint!(x + dx * g(3.0), y), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLines(&[
                dpoint!(x + dx * GRID, y),
                dpoint!(x, y + dy * GRID),
                dpoint!(x, y + dy * g(3.0)),
            ]);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            renderer.corners.push(path);
        }
        Ok(renderer)
    }
    fn shape_specs(&self, caption: f32) -> [(BoxDip, Option<f32>); 13] {
        let l = self.layout;
        [
            (l.viewport.inset(HALF_GRID, HALF_GRID), None),
            (l.pair_field, None),
            (l.pair_action, None),
            (l.key, None),
            (l.key, Some(caption)),
            (l.confirm_cancel, None),
            (l.confirm_unpair, None),
            (l.key, None),
            (l.toggle, None),
            (l.sound_active(true), None),
            (l.sound_active(false), None),
            (l.sound_half(true), None),
            (l.sound_half(false), None),
        ]
    }
    unsafe fn ensure_core_paths(&mut self) -> WinResult<()> {
        if self.core_paths.len() == 13 {
            return Ok(());
        }
        self.core_paths.clear();
        let caption = self.measure(l::KEY_CHANGE_HINT, TypeRole::Metadata)?.width + g(2.0);
        for (r, gap) in self.shape_specs(caption) {
            self.core_paths.push(self.path(r, gap, true)?);
        }
        Ok(())
    }
    unsafe fn crisp_shape(&self, s: &Surface, shape: Shape, color: D2D1_COLOR_F) {
        s.color(color);
        s.target.DrawGeometry(
            &self.core_paths[shape.index()],
            &s.solid,
            96.0 / self.dpi as f32,
            &self.stroke,
        );
    }
    unsafe fn path(&self, r: BoxDip, gap: Option<f32>, core: bool) -> WinResult<ID2D1PathGeometry> {
        let geometry = self.factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        let point = |x: f32, y: f32| {
            dpoint!(
                if core { snap_hairline(x, self.dpi) } else { x },
                if core { snap_hairline(y, self.dpi) } else { y }
            )
        };
        let cut = GRID;
        let points = [
            point(r.x + cut, r.y),
            point(r.right() - cut, r.y),
            point(r.right(), r.y + cut),
            point(r.right(), r.bottom() - cut),
            point(r.right() - cut, r.bottom()),
            point(r.x + cut, r.bottom()),
            point(r.x, r.bottom() - cut),
            point(r.x, r.y + cut),
        ];
        if let Some(width) = gap {
            sink.BeginFigure(
                point(r.cx() - width / 2.0, r.bottom()),
                D2D1_FIGURE_BEGIN_HOLLOW,
            );
            sink.AddLines(&[
                points[5],
                points[6],
                points[7],
                points[0],
                points[1],
                points[2],
                points[3],
                points[4],
                point(r.cx() + width / 2.0, r.bottom()),
            ]);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        } else {
            sink.BeginFigure(points[0], D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLines(&points[1..]);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        }
        sink.Close()?;
        Ok(geometry)
    }
    unsafe fn measure(&mut self, text: &str, role: TypeRole) -> WinResult<MeasuredText> {
        if let Some(entry) = self
            .text_cache
            .iter()
            .find(|entry| entry.role == role && entry.text == text)
        {
            return Ok(entry.measured.clone());
        }
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let layout = self.fonts.factory.CreateTextLayout(
            &utf16,
            &self.formats[role.index()],
            2048.0,
            128.0,
        )?;
        let extended: IDWriteTextLayout1 = layout.cast()?;
        extended.SetCharacterSpacing(
            if role == TypeRole::UiLabel {
                role.token().2 / 2.0
            } else {
                0.0
            },
            if role == TypeRole::UiLabel {
                role.token().2 / 2.0
            } else {
                role.token().2
            },
            0.0,
            DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: utf16.len() as u32,
            },
        )?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;
        let mut lines = [DWRITE_LINE_METRICS::default()];
        let mut count = 0;
        layout.GetLineMetrics(Some(&mut lines), &mut count)?;
        let measured = MeasuredText {
            layout,
            width: metrics.widthIncludingTrailingWhitespace,
            height: metrics.height,
            baseline: lines[0].baseline,
            cap_height: role.token().0 * self.cap_ratio,
        };
        if self.text_cache.len() == 64 {
            self.text_cache.remove(0);
        }
        self.text_cache.push(TextEntry {
            text: text.into(),
            role,
            measured: measured.clone(),
        });
        Ok(measured)
    }
    #[allow(clippy::too_many_arguments)]
    unsafe fn text(
        &mut self,
        s: &Surface,
        name: &'static str,
        text: &str,
        role: TypeRole,
        rect: BoxDip,
        h: AlignH,
        v: AlignV,
        color: D2D1_COLOR_F,
    ) -> WinResult<MeasuredText> {
        let measured = self.measure(text, role)?;
        let x = match h {
            AlignH::Left => rect.x,
            AlignH::Center => rect.cx() - measured.width / 2.0,
            AlignH::Right => rect.right() - measured.width,
            AlignH::AnchorO => rect.cx() - Self::o_midpoint(&measured, text)?,
        };
        let y = match v {
            AlignV::Middle => rect.cy() - measured.height / 2.0,
            AlignV::CapMiddle => rect.cy() + measured.cap_height / 2.0 - measured.baseline,
            AlignV::Baseline(baseline) => baseline - measured.baseline,
        };
        #[cfg(test)]
        let visible = !self.hide_text;
        #[cfg(not(test))]
        let visible = true;
        if visible
            && matches!(
                name,
                "header-descriptor" | "descriptor-left" | "descriptor-right"
            )
        {
            s.target
                .SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            s.color(opacity(TEXT, 0.045));
            for (dx, dy) in [
                (-0.8, 0.0),
                (0.8, 0.0),
                (0.0, -0.8),
                (0.0, 0.8),
                (-0.6, -0.6),
                (0.6, -0.6),
                (-0.6, 0.6),
                (0.6, 0.6),
            ] {
                s.target.DrawTextLayout(
                    dpoint!(x + dx, y + dy),
                    &measured.layout,
                    &s.solid,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            s.target
                .SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
        }
        if visible {
            s.color(color);
            s.target.DrawTextLayout(
                dpoint!(x, y),
                &measured.layout,
                &s.solid,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        #[cfg(test)]
        {
            let mut family = [0u16; 32];
            measured.layout.GetFontFamilyName(0, &mut family, None)?;
            let family =
                String::from_utf16_lossy(&family[..family.iter().position(|c| *c == 0).unwrap()]);
            assert_eq!(family, "Jura");
            let mut size = 0.0;
            let mut weight = DWRITE_FONT_WEIGHT(0);
            measured.layout.GetFontSize(0, &mut size, None)?;
            measured.layout.GetFontWeight(0, &mut weight, None)?;
            let (expected_size, expected_weight, tracking) = role.token();
            assert_eq!((size, weight.0), (expected_size, expected_weight));
            let extended: IDWriteTextLayout1 = measured.layout.cast()?;
            let (mut leading, mut trailing, mut minimum) = (0.0, 0.0, 0.0);
            extended.GetCharacterSpacing(0, &mut leading, &mut trailing, &mut minimum, None)?;
            assert!((leading + trailing - tracking).abs() < 0.001);
            if role == TypeRole::UiLabel {
                assert_eq!(leading, trailing);
            }
            let o = if text.contains('O') {
                x + Self::o_midpoint(&measured, text)?
            } else {
                -1.0
            };
            self.typography_records.push(format!("{name}\t{text}\t{family}\t{size}\t{}\t{leading}\t{trailing}\t{x}\t{y}\t{}\t{}\t{}\t{}\t{}\t{}\t{o}\n",weight.0,measured.width,measured.height,color.r,color.g,color.b,color.a));
        }
        #[cfg(test)]
        self.records.push(AlignmentRecord {
            name,
            rect,
            origin_x: x,
            origin_y: y,
            width: measured.width,
            height: measured.height,
            baseline: y + measured.baseline,
        });
        #[cfg(not(test))]
        let _ = name;
        Ok(measured)
    }
    unsafe fn o_midpoint(measured: &MeasuredText, text: &str) -> WinResult<f32> {
        let Some(index) = text.encode_utf16().position(|c| c == b'O' as u16) else {
            return Ok(measured.width / 2.0);
        };
        let mut x = 0.0;
        let mut y = 0.0;
        let mut glyph = DWRITE_HIT_TEST_METRICS::default();
        measured
            .layout
            .HitTestTextPosition(index as u32, false, &mut x, &mut y, &mut glyph)?;
        Ok(x + glyph.width / 2.0)
    }
    unsafe fn glow(&self, s: &Surface, shape: Shape, glow: Glow, core: D2D1_COLOR_F) {
        for (width, opacity) in [(12.0, 0.035), (7.0, 0.065), (3.0, 0.14)] {
            s.color(rgba(RED.r, RED.g, RED.b, opacity * glow.intensity()));
            s.target
                .DrawGeometry(&self.paths[shape.index()], &s.solid, width, &self.stroke);
        }
        self.crisp_shape(s, shape, core);
    }
    unsafe fn interaction_glow(
        &self,
        s: &Surface,
        shape: Shape,
        signal: D2D1_COLOR_F,
        energy: f32,
        core: D2D1_COLOR_F,
    ) {
        for (width, alpha) in [(12.0, 0.035), (7.0, 0.065), (3.0, 0.14)] {
            s.color(opacity(signal, alpha * energy));
            s.target
                .DrawGeometry(&self.paths[shape.index()], &s.solid, width, &self.stroke);
        }
        self.crisp_shape(s, shape, core);
    }
    unsafe fn interaction_fill(
        &self,
        s: &Surface,
        shape: Shape,
        hot: bool,
        pressed: bool,
        primary: bool,
    ) {
        if pressed || hot {
            s.color(if pressed {
                rgba(0.0, 0.0, 0.0, 0.25)
            } else {
                opacity(
                    if primary { RED } else { TEXT },
                    if primary { 0.11 } else { 0.06 },
                )
            });
            s.target
                .FillGeometry(&self.paths[shape.index()], &s.solid, None);
        }
    }
    unsafe fn lit_line(
        &self,
        s: &Surface,
        from: (f32, f32),
        to: (f32, f32),
        glow: Glow,
        core: D2D1_COLOR_F,
    ) {
        for (width, alpha) in [(7.0, 0.035), (3.0, 0.08)] {
            s.line(
                from,
                to,
                rgba(RED.r, RED.g, RED.b, alpha * glow.intensity()),
                width,
                &self.stroke,
            );
        }
        self.crisp_line(s, from, to, core);
    }
    // Keep cached geometry and the halo unchanged; snap only the core pass.
    unsafe fn crisp_geometry(
        &self,
        s: &Surface,
        path: &ID2D1PathGeometry,
        rect: BoxDip,
        color: D2D1_COLOR_F,
    ) {
        let mut original = core::mem::zeroed();
        s.target.GetTransform(&mut original);
        let mut snapped = original;
        snapped.M31 += snap_hairline(rect.x, self.dpi) - rect.x;
        snapped.M32 += snap_hairline(rect.y, self.dpi) - rect.y;
        s.target.SetTransform(&snapped);
        s.color(color);
        s.target
            .DrawGeometry(path, &s.solid, 96.0 / self.dpi as f32, &self.stroke);
        s.target.SetTransform(&original);
    }
    unsafe fn crisp_line(
        &self,
        s: &Surface,
        from: (f32, f32),
        to: (f32, f32),
        color: D2D1_COLOR_F,
    ) {
        let scale = self.dpi as f32 / 96.0;
        let horizontal = from.1 == to.1;
        let a = if horizontal {
            (
                (from.0 * scale).round() / scale,
                snap_hairline(from.1, self.dpi),
            )
        } else {
            (
                snap_hairline(from.0, self.dpi),
                (from.1 * scale).round() / scale,
            )
        };
        let b = if horizontal {
            ((to.0 * scale).round() / scale, a.1)
        } else {
            (a.0, (to.1 * scale).round() / scale)
        };
        s.target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
        s.line(a, b, color, 96.0 / self.dpi as f32, &self.stroke);
        s.target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
    unsafe fn material(&self, s: &Surface, shape: Shape, rect: BoxDip, primary: bool) {
        let brush = if primary { &s.primary } else { &s.raised };
        brush.SetStartPoint(dpoint!(rect.x, rect.y));
        brush.SetEndPoint(dpoint!(rect.x, rect.bottom()));
        s.target
            .FillGeometry(&self.paths[shape.index()], brush, None);
    }
    unsafe fn wordmark(&self, s: &Surface, rect: BoxDip) {
        for (spread, alpha) in [(1.2, 0.06), (0.5, 0.10)] {
            s.bitmap(3, rect.inset(-spread, -spread), alpha);
        }
        s.bitmap(3, rect, 1.0);
    }
    unsafe fn logo(&self, s: &Surface, rect: BoxDip) {
        for (spread, alpha) in [(GRID, 0.10), (HALF_GRID, 0.16), (2.0, 0.20)] {
            s.bitmap(
                2,
                rect.inset(-spread, -spread),
                alpha * Glow::Primary.intensity(),
            );
        }
        s.bitmap(2, rect, 1.0);
    }
    unsafe fn descriptor(&mut self, s: &Surface, rect: BoxDip, centered: bool) -> WinResult<()> {
        if !centered {
            self.text(
                s,
                "header-descriptor",
                l::MAIN_BRAND_LINE,
                TypeRole::Metadata,
                rect,
                AlignH::Left,
                AlignV::CapMiddle,
                SECONDARY,
            )?;
            return Ok(());
        }
        let left = self.measure(l::BRAND_NAME, TypeRole::Metadata)?;
        let right = self.measure(l::BRAND_DESCRIPTOR, TypeRole::Metadata)?;
        let dot = 4.0;
        let gap = GRID;
        let (lr, rr, dx) = if centered {
            let width = left.width.max(right.width);
            (
                BoxDip::new(rect.cx() - dot / 2.0 - gap - width, rect.y, width, rect.h),
                BoxDip::new(rect.cx() + dot / 2.0 + gap, rect.y, width, rect.h),
                rect.cx(),
            )
        } else {
            (
                BoxDip::new(rect.x, rect.y, left.width, rect.h),
                BoxDip::new(
                    rect.x + left.width + gap * 2.0 + dot,
                    rect.y,
                    right.width,
                    rect.h,
                ),
                rect.x + left.width + gap + dot / 2.0,
            )
        };
        self.text(
            s,
            "descriptor-left",
            l::BRAND_NAME,
            TypeRole::Metadata,
            lr,
            if centered {
                AlignH::Right
            } else {
                AlignH::Left
            },
            AlignV::Middle,
            SECONDARY,
        )?;
        self.text(
            s,
            "descriptor-right",
            l::BRAND_DESCRIPTOR,
            TypeRole::Metadata,
            rr,
            AlignH::Left,
            AlignV::Middle,
            SECONDARY,
        )?;
        s.circle(dx, rect.cy(), dot, RED);
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    unsafe fn icon_group(
        &mut self,
        s: &Surface,
        name: &'static str,
        text: &str,
        role: TypeRole,
        rect: BoxDip,
        h: AlignH,
        icon: Option<usize>,
        healthy: bool,
        color: D2D1_COLOR_F,
    ) -> WinResult<()> {
        let metrics = self.measure(text, role)?;
        let size = if icon.is_some() { g(2.0) } else { 11.5 * 0.9 };
        let gap = GRID;
        let width = size + gap + metrics.width;
        let left = match h {
            AlignH::Left => rect.x,
            AlignH::Center => rect.cx() - width / 2.0,
            AlignH::Right => rect.right() - width,
            AlignH::AnchorO => rect.cx() - size - gap - Self::o_midpoint(&metrics, text)?,
        };
        if let Some(index) = icon {
            s.bitmap(
                index,
                BoxDip::new(left, rect.cy() - size / 2.0, size, size),
                1.0,
            );
        } else {
            s.circle(
                left + size / 2.0,
                rect.cy(),
                size,
                if healthy {
                    VisualRole::StatusHealthy.color()
                } else {
                    RED
                },
            );
        }
        self.text(
            s,
            name,
            text,
            role,
            BoxDip::new(left + size + gap, rect.y, metrics.width, rect.h),
            AlignH::Left,
            AlignV::CapMiddle,
            color,
        )?;
        #[cfg(test)]
        if matches!(h, AlignH::AnchorO) {
            self.records.push(AlignmentRecord {
                name: "ready-o-anchor",
                rect,
                origin_x: left + size + gap + Self::o_midpoint(&metrics, text)?,
                origin_y: rect.cy(),
                width: 0.0,
                height: 0.0,
                baseline: rect.cy() + metrics.cap_height / 2.0,
            });
        }
        #[cfg(test)]
        self.records.push(AlignmentRecord {
            name: "measured-group",
            rect,
            origin_x: left,
            origin_y: rect.cy() - metrics.cap_height / 2.0,
            width,
            height: metrics.cap_height,
            baseline: rect.cy() + metrics.cap_height / 2.0,
        });
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    unsafe fn action(
        &mut self,
        s: &Surface,
        x: &S,
        hwnd: W,
        shape: Shape,
        rect: BoxDip,
        label: &str,
        role: VisualRole,
    ) -> WinResult<()> {
        let hot = x.hover_button
            == match shape {
                Shape::Pair => PAIR,
                Shape::Cancel => APPLY,
                _ => UNPAIR,
            }
            || GetFocus() == hwnd;
        let disabled = IsWindowEnabled(hwnd) == 0;
        let primary = matches!(
            role,
            VisualRole::PrimaryAction | VisualRole::PrimaryDestructive
        );
        let pressed = SendMessageW(hwnd, 0x00F2, 0, 0) & 4 != 0;
        self.material(s, shape, rect, primary);
        self.interaction_fill(s, shape, hot && !disabled, pressed && !disabled, primary);
        let energy = if disabled {
            0.15
        } else if pressed {
            if primary {
                0.48
            } else {
                0.35
            }
        } else if hot {
            if primary {
                1.55
            } else {
                0.80
            }
        } else if primary {
            Glow::Primary.intensity()
        } else {
            Glow::Ambient.intensity()
        };
        let signal = if primary || !hot { RED } else { TEXT };
        let core = if primary {
            opacity(RED, if hot { 1.0 } else { 0.9 })
        } else if hot {
            opacity(TEXT, if pressed { 0.65 } else { 0.85 })
        } else {
            opacity(STROKE_MUTED, 0.75)
        };
        self.interaction_glow(s, shape, signal, energy, core);
        self.text(
            s,
            "action-label",
            label,
            TypeRole::PrimaryAction,
            rect,
            AlignH::Center,
            AlignV::Middle,
            if disabled { META } else { role.color() },
        )?;
        Ok(())
    }
    unsafe fn pair(&mut self, s: &Surface, x: &S) -> WinResult<()> {
        let l = self.layout;
        self.wordmark(s, l.pair_wordmark);
        self.logo(s, l.pair_logo);
        self.descriptor(s, l.pair_descriptor, true)?;
        let heading = self.measure(l::PAIR_IDENTITY_PROMPT, TypeRole::UiLabel)?;
        let flank = g(6.0);
        let gap = g(2.0);
        // Retain the existing decorative flank positions while text changes.
        let width = 125.188 + 2.0 * (flank + gap);
        let start = l.pair_heading.cx() - width / 2.0;
        let tr = BoxDip::new(
            l.pair_heading.cx() - heading.width / 2.0,
            l.pair_heading.y,
            heading.width,
            l.pair_heading.h,
        );
        self.lit_line(
            s,
            (start, l.pair_heading.cy()),
            (start + flank, l.pair_heading.cy()),
            Glow::Ambient,
            STROKE_MUTED,
        );
        self.lit_line(
            s,
            (start + width - flank, l.pair_heading.cy()),
            (start + width, l.pair_heading.cy()),
            Glow::Ambient,
            STROKE_MUTED,
        );
        self.text(
            s,
            "pair-heading",
            l::PAIR_IDENTITY_PROMPT,
            TypeRole::UiLabel,
            tr,
            AlignH::Center,
            AlignV::CapMiddle,
            TEXT,
        )?;
        #[cfg(test)]
        self.records.push(AlignmentRecord {
            name: "heading-group",
            rect: l.pair_heading,
            origin_x: start,
            origin_y: l.pair_heading.y,
            width,
            height: l.pair_heading.h,
            baseline: l.pair_heading.cy(),
        });
        self.material(s, Shape::Field, l.pair_field, false);
        let focused = GetFocus() == x.code;
        self.glow(
            s,
            Shape::Field,
            if focused {
                Glow::Active
            } else {
                Glow::Interactive
            },
            if focused {
                rgba(RED.r, RED.g, RED.b, 0.85)
            } else {
                STROKE_MUTED
            },
        );
        let size = g(3.0);
        s.bitmap(
            if focused { 5 } else { 4 },
            BoxDip::new(
                l.pair_icon.cx() - size / 2.0,
                l.pair_icon.cy() - size / 2.0,
                size,
                size,
            ),
            1.0,
        );
        self.lit_line(
            s,
            (l.pair_accent.cx(), l.pair_accent.y),
            (l.pair_accent.cx(), l.pair_accent.bottom()),
            Glow::Interactive,
            rgba(RED.r, RED.g, RED.b, 0.65),
        );
        let code = get(x.code);
        s.target
            .PushAxisAlignedClip(&dr(l.pair_content), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let field_text = if code.is_empty() {
            l::PAIR_PROMPT
        } else {
            &code
        };
        // PairFieldText: the same rect, role, tracking, alignment and cap baseline
        // for cue and typed values. Feedback is a separate lower cluster.
        let result = (|| {
            let measured = self.measure(field_text, TypeRole::UiLabel)?;
            let tx = l.pair_content.cx() - measured.width / 2.0;
            let ty = l.pair_content.cy() + measured.cap_height / 2.0 - measured.baseline;
            if !code.is_empty() && focused {
                let mut start = 0u32;
                let mut end = 0u32;
                SendMessageW(
                    x.code,
                    0x00B0,
                    (&mut start as *mut u32) as usize,
                    (&mut end as *mut u32) as isize,
                );
                let mut hit = [DWRITE_HIT_TEST_METRICS::default()];
                let mut count = 0;
                if end > start
                    && measured
                        .layout
                        .HitTestTextRange(start, end - start, tx, ty, Some(&mut hit), &mut count)
                        .is_ok()
                {
                    s.color(rgba(0.3, 0.36, 0.45, 0.6));
                    let m = hit[0];
                    s.target.FillRectangle(
                        &dr(BoxDip::new(m.left, m.top, m.width, m.height)),
                        &s.solid,
                    );
                }
                let mut cx = 0.0;
                let mut cy = 0.0;
                let mut metrics = DWRITE_HIT_TEST_METRICS::default();
                if measured
                    .layout
                    .HitTestTextPosition(end, false, &mut cx, &mut cy, &mut metrics)
                    .is_ok()
                {
                    let scale = x.dpi as f32 / 96.0;
                    let edit = l.edit();
                    SetCaretPos(
                        ((tx + cx - edit.x) * scale).round() as i32,
                        ((ty + cy - edit.y) * scale).round() as i32,
                    );
                }
            }
            self.text(
                s,
                "pair-field",
                field_text,
                TypeRole::UiLabel,
                l.pair_content,
                AlignH::Center,
                AlignV::CapMiddle,
                if !code.is_empty() { TEXT } else { SECONDARY },
            )
        })();
        s.target.PopAxisAlignedClip();
        result?;
        self.action(
            s,
            x,
            x.pair_btn,
            Shape::Pair,
            l.pair_action,
            l::PAIR_BUTTON,
            VisualRole::PrimaryAction,
        )?;
        let feedback = x
            .notice
            .filter(|(message, _)| *message != l::PAIR_EMPTY_CODE || code.is_empty());
        if let Some((message, error)) = feedback {
            self.text(
                s,
                "pair-feedback",
                if message == l::PAIR_EMPTY_CODE {
                    l::PAIR_ERROR_CUE
                } else {
                    message
                },
                TypeRole::Metadata,
                l.pair_ready,
                AlignH::Center,
                AlignV::CapMiddle,
                if error { RED } else { SECONDARY },
            )?;
        } else {
            self.icon_group(
                s,
                "pair-ready",
                if x.r.is_pairing() {
                    l::STATUS_PAIRING
                } else {
                    l::STATUS_READY_FOR_PAIRING
                },
                TypeRole::UiLabel,
                l.pair_ready,
                AlignH::AnchorO,
                None,
                false,
                VisualRole::StatusIdle.color(),
            )?;
        }
        Ok(())
    }
    unsafe fn main(&mut self, s: &Surface, x: &S) -> WinResult<()> {
        let l = self.layout;
        self.wordmark(s, l.header_wordmark);
        self.logo(s, l.header_logo);
        self.descriptor(s, l.header_descriptor, false)?;
        let words = ["COMMAND", "CONNECT", "COORDINATE"];
        let widths = [
            self.measure(words[0], TypeRole::Metadata)?.width,
            self.measure(words[1], TypeRole::Metadata)?.width,
            self.measure(words[2], TypeRole::Metadata)?.width,
        ];
        let gap = g(3.0);
        let dot = 4.0;
        let total = widths.iter().sum::<f32>() + 4.0 * gap + 2.0 * dot;
        let mut at = l.tagline.cx() - total / 2.0;
        for (i, word) in words.iter().enumerate() {
            self.text(
                s,
                "tagline",
                word,
                TypeRole::Metadata,
                BoxDip::new(at, l.tagline.y, widths[i], l.tagline.h),
                AlignH::Left,
                AlignV::Middle,
                SECONDARY,
            )?;
            at += widths[i];
            if i < 2 {
                s.circle(at + gap + dot / 2.0, l.tagline.cy(), dot, RED);
                at += gap * 2.0 + dot;
            }
        }
        if matches!(x.mode, View::Main | View::Config) {
            let marker = g(2.0);
            self.lit_line(
                s,
                (l.section.x, l.section.cy()),
                (l.section.x + marker, l.section.cy()),
                Glow::Interactive,
                RED,
            );
            self.text(
                s,
                "radio-section",
                l::RADIO_KEY,
                TypeRole::UiLabel,
                BoxDip::new(
                    l.section.x + marker + g(2.0),
                    l.section.y,
                    l.section.w - marker - g(2.0),
                    l.section.h,
                ),
                AlignH::Left,
                AlignV::Middle,
                SECONDARY,
            )?;
            self.material(s, Shape::KeyFill, l.key, false);
            let hot = x.hover_button == KEY_DISPLAY || GetFocus() == x.key;
            let pressed = SendMessageW(x.key, 0x00F2, 0, 0) & 4 != 0;
            self.interaction_fill(s, Shape::KeyFill, hot, pressed, false);
            self.interaction_glow(
                s,
                Shape::KeyBorder,
                RED,
                if pressed {
                    0.72
                } else if hot {
                    1.45
                } else {
                    Glow::Interactive.intensity()
                },
                if pressed {
                    opacity(TEXT, 0.90)
                } else {
                    opacity(RED, if hot { 1.0 } else { 0.78 })
                },
            );
            self.text(
                s,
                "radio-value",
                &get(x.key),
                TypeRole::HeroValue,
                l.key.inset(g(5.0), 0.0),
                AlignH::Center,
                AlignV::Middle,
                VisualRole::PrimaryValue.color(),
            )?;
            self.text(
                s,
                "radio-caption",
                l::KEY_CHANGE_HINT,
                TypeRole::Metadata,
                l.key_caption,
                AlignH::Center,
                AlignV::Middle,
                SECONDARY,
            )?;
            s.bitmap(
                if hot || pressed { 18 } else { 8 },
                BoxDip::new(l.key.right() - g(4.0), l.key.cy() - GRID, g(2.0), g(2.0)),
                if pressed {
                    0.55
                } else if hot {
                    1.0
                } else {
                    0.7
                },
            );
            let label = BoxDip::new(
                l.sound.x + GRID,
                l.sound.y,
                l.toggle.x - g(2.0) - l.sound.x - GRID,
                l.sound.h,
            );
            self.icon_group(
                s,
                "ptt-sound",
                l::PTT_SOUND,
                TypeRole::UiLabel,
                label,
                AlignH::Left,
                Some(6),
                false,
                TEXT,
            )?;
            self.toggle(s, x)?;
            let unpair_hot = x.hover_button == UNPAIR || GetFocus() == x.unpair_btn;
            if IsWindowVisible(x.unpair_btn) != 0 {
                self.icon_group(
                    s,
                    "unpair",
                    l::UNPAIR,
                    TypeRole::UiLabel,
                    l.unpair.inset(GRID, 0.0),
                    AlignH::Left,
                    Some(15),
                    false,
                    opacity(
                        VisualRole::Danger.color(),
                        if unpair_hot { 1.0 } else { 0.85 },
                    ),
                )?;
            }
            for y in [
                l.sound.y - UTILITY_PADDING,
                l.sound.bottom() + UTILITY_PADDING,
            ] {
                self.lit_line(
                    s,
                    (l.content.x, y),
                    (l.content.right(), y),
                    Glow::Ambient,
                    SEPARATOR,
                );
            }
        } else if x.mode == View::Capture {
            self.material(s, Shape::Capture, l.key, false);
            self.glow(
                s,
                Shape::Capture,
                Glow::Interactive,
                rgba(RED.r, RED.g, RED.b, 0.7),
            );
            self.text(
                s,
                "capture-title",
                l::CAPTURE_KEY,
                TypeRole::UiLabel,
                BoxDip::new(l.key.x, l.key.y + GRID, l.key.w, g(6.0)),
                AlignH::Center,
                AlignV::Middle,
                TEXT,
            )?;
            self.text(
                s,
                "capture-cancel",
                l::FOOTER_CAPTURE,
                TypeRole::Metadata,
                BoxDip::new(l.key.x, l.key.y + g(7.0), l.key.w, g(3.0)),
                AlignH::Center,
                AlignV::Middle,
                SECONDARY,
            )?;
        } else if x.mode == View::ConfirmUnpair {
            self.text(
                s,
                "confirm-title",
                l::UNPAIR_CONFIRM_TITLE,
                TypeRole::HeroValue,
                BoxDip::new(l.key.x, l.key.y, l.key.w, g(5.0)),
                AlignH::Center,
                AlignV::Middle,
                TEXT,
            )?;
            self.text(
                s,
                "confirm-body",
                l::UNPAIR_CONFIRM_BODY,
                TypeRole::UiLabel,
                BoxDip::new(l.key.x, l.key.y + g(8.0), l.key.w, g(4.0)),
                AlignH::Center,
                AlignV::Middle,
                SECONDARY,
            )?;
            self.action(
                s,
                x,
                x.apply_btn,
                Shape::Cancel,
                l.confirm_cancel,
                l::CANCEL,
                VisualRole::SecondaryAction,
            )?;
            self.action(
                s,
                x,
                x.unpair_btn,
                Shape::Destructive,
                l.confirm_unpair,
                l::UNPAIR,
                VisualRole::PrimaryDestructive,
            )?;
        }
        self.icon_group(
            s,
            "status-paired",
            l::PAIR_STATUS_PAIRED,
            TypeRole::Metadata,
            l.status_cells[0],
            AlignH::Center,
            None,
            true,
            SECONDARY,
        )?;
        self.icon_group(
            s,
            "status-standby",
            if x.tx {
                l::STATUS_TRANSMITTING
            } else {
                l::STATUS_STANDBY
            },
            TypeRole::Metadata,
            l.status_cells[1],
            AlignH::Center,
            Some(if x.tx { 16 } else { 9 }),
            false,
            if x.tx { GREEN } else { SECONDARY },
        )?;
        self.icon_group(
            s,
            "status-relay",
            if x.tx { l::LOCAL_OPEN } else { l::LOCAL_CLOSED },
            TypeRole::Metadata,
            l.status_cells[2],
            AlignH::Center,
            Some(if x.tx { 17 } else { 11 }),
            false,
            if x.tx { GREEN } else { SECONDARY },
        )?;
        for cell in &l.status_cells[1..] {
            self.crisp_line(
                s,
                (cell.x, l.status.y + GRID),
                (cell.x, l.status.bottom() - GRID),
                opacity(STROKE_MUTED, 0.45),
            );
        }
        for y in l.status_separators {
            self.lit_line(
                s,
                (l.content.x, y),
                (l.content.right(), y),
                Glow::Ambient,
                SEPARATOR,
            );
        }
        self.footer(s, x)?;
        Ok(())
    }
    unsafe fn footer(&mut self, s: &Surface, x: &S) -> WinResult<()> {
        let l = self.layout;
        let tx = x.tx;
        let (text, color) = x
            .notice
            .filter(|_| matches!(x.mode, View::Main | View::Config))
            .map(|(text, error)| (text, if error { RED } else { TEXT }))
            .unwrap_or((
                default_footer(x),
                if tx {
                    SECONDARY
                } else {
                    VisualRole::Meta.color()
                },
            ));
        let metrics = self.measure(l::VERSION_FOOTER, TypeRole::Metadata)?;
        let baseline = l.footer.cy() + metrics.baseline - metrics.height / 2.0;
        s.bitmap(
            if tx { 14 } else { 13 },
            BoxDip::new(l.footer.x, l.footer.cy() - GRID, g(2.0), g(2.0)),
            1.0,
        );
        self.text(
            s,
            "footer-message",
            text,
            TypeRole::Metadata,
            BoxDip::new(
                l.footer_message.x + g(3.0),
                l.footer_message.y,
                l.footer_message.w - g(3.0),
                l.footer_message.h,
            ),
            AlignH::Left,
            AlignV::Baseline(baseline),
            color,
        )?;
        self.text(
            s,
            "footer-version",
            l::VERSION_FOOTER,
            TypeRole::Metadata,
            l.footer_version,
            AlignH::Right,
            AlignV::Baseline(baseline),
            META,
        )?;
        Ok(())
    }
    unsafe fn toggle(&mut self, s: &Surface, x: &S) -> WinResult<()> {
        let l = self.layout;
        let on_hot = x.hover_button == SOUND_ON || GetFocus() == x.sound_on_btn;
        let off_hot = x.hover_button == SOUND_OFF || GetFocus() == x.sound_off_btn;
        let on_pressed = SendMessageW(x.sound_on_btn, 0x00F2, 0, 0) & 4 != 0;
        let off_pressed = SendMessageW(x.sound_off_btn, 0x00F2, 0, 0) & 4 != 0;
        let hot = if x.ptt_sound { on_hot } else { off_hot };
        let pressed = if x.ptt_sound { on_pressed } else { off_pressed };
        s.framed_fill.SetStartPoint(dpoint!(l.toggle.x, l.toggle.y));
        s.framed_fill
            .SetEndPoint(dpoint!(l.toggle.x, l.toggle.bottom()));
        s.target.FillGeometry(
            &self.paths[Shape::ToggleShell.index()],
            &s.framed_fill,
            None,
        );
        self.crisp_shape(s, Shape::ToggleShell, opacity(STROKE_MUTED, 0.60));
        self.crisp_line(
            s,
            (l.toggle.cx(), l.toggle.y + HALF_GRID),
            (l.toggle.cx(), l.toggle.bottom() - HALF_GRID),
            opacity(STROKE_MUTED, 0.25),
        );
        let inactive_on = !x.ptt_sound;
        let inactive = if inactive_on {
            Shape::ToggleDimOn
        } else {
            Shape::ToggleDimOff
        };
        let inactive_hot = if inactive_on { on_hot } else { off_hot };
        let inactive_pressed = if inactive_on { on_pressed } else { off_pressed };
        if inactive_on || inactive_hot || inactive_pressed {
            let rect = l.sound_half(inactive_on);
            let brush = if inactive_on {
                &s.segment_fill
            } else {
                &s.segment_off
            };
            brush.SetOpacity(if inactive_pressed {
                0.12
            } else if inactive_hot {
                0.30
            } else {
                0.18
            });
            brush.SetStartPoint(dpoint!(rect.x, rect.y));
            brush.SetEndPoint(dpoint!(rect.x, rect.bottom()));
            s.target
                .FillGeometry(&self.paths[inactive.index()], brush, None);
            self.interaction_glow(
                s,
                inactive,
                if inactive_on { RED } else { TEXT },
                if inactive_hot { 0.35 } else { 0.0 },
                opacity(
                    if inactive_on { RED } else { TEXT },
                    if inactive_pressed {
                        0.65
                    } else if inactive_hot {
                        0.48
                    } else {
                        0.18
                    },
                ),
            );
        }
        let active = if x.ptt_sound {
            Shape::ToggleOn
        } else {
            Shape::ToggleOff
        };
        let rect = l.sound_active(x.ptt_sound);
        let brush = if x.ptt_sound {
            &s.segment_fill
        } else {
            &s.segment_off
        };
        brush.SetOpacity(if pressed { 0.72 } else { 1.0 });
        brush.SetStartPoint(dpoint!(rect.x, rect.y));
        brush.SetEndPoint(dpoint!(rect.x, rect.bottom()));
        let signal = if x.ptt_sound { RED } else { TEXT };
        let energy = if pressed {
            0.60
        } else if hot {
            1.45
        } else {
            1.0
        };
        let strength = if x.ptt_sound { 1.0 } else { 0.60 };
        for (width, alpha) in [(12.0, 0.025), (6.0, 0.065), (2.5, 0.18)] {
            s.color(opacity(signal, alpha * energy * strength));
            s.target
                .DrawGeometry(&self.paths[active.index()], &s.solid, width, &self.stroke);
        }
        // Paint above the dim shell; only the selected half protrudes.
        s.target
            .FillGeometry(&self.paths[active.index()], brush, None);
        self.interaction_fill(s, active, hot, pressed, x.ptt_sound);
        s.inner_top.SetOpacity(if x.ptt_sound { 0.75 } else { 0.0 });
        s.inner_top.SetStartPoint(dpoint!(rect.x, rect.y + 0.5));
        s.inner_top.SetEndPoint(dpoint!(rect.x, rect.y + 4.0));
        s.target
            .FillGeometry(&self.paths[active.index()], &s.inner_top, None);
        self.crisp_shape(
            s,
            active,
            opacity(signal, if x.ptt_sound { 0.95 } else { 0.55 }),
        );
        for (on, text) in [(true, l::PTT_SOUND_ON), (false, l::PTT_SOUND_OFF)] {
            self.text(
                s,
                "toggle-label",
                text,
                TypeRole::UiLabel,
                l.sound_half(on),
                AlignH::Center,
                AlignV::Middle,
                if on == x.ptt_sound || (if on { on_hot } else { off_hot }) {
                    TEXT
                } else {
                    SECONDARY
                },
            )?;
        }
        Ok(())
    }
    unsafe fn background(&self, s: &Surface, pair: bool) {
        let l = self.layout;
        // Target-owned layer, factory-owned cut-corner mask. Never clip text or
        // controls into this background-only layer; discard it with its target.
        s.target.PushLayer(&self.scene_clip, &s.scene_layer);
        let rect = if pair {
            BoxDip::new(-3.0, l.viewport.y, l.viewport.w, l.viewport.h)
        } else {
            let source = s.bitmaps[1].GetPixelSize();
            let scale =
                (l.viewport.w / source.width as f32).max(l.viewport.h / source.height as f32);
            let w = source.width as f32 * scale;
            let h = source.height as f32 * scale;
            BoxDip::new(
                l.viewport.cx() - w / 2.0,
                l.viewport.cy() - h / 2.0 + 10.0,
                w,
                h,
            )
        };
        s.bitmap(if pair { 0 } else { 1 }, rect, 1.0);
        if !pair {
            let dim = 0.50;
            #[cfg(test)]
            let dim = if self.skip_main_dim { 0.0 } else { dim };
            s.color(rgba(0.0, 0.0, 0.0, dim));
            s.target.FillRectangle(&dr(l.viewport), &s.solid);
        }
        s.target.PopLayer();
    }
    unsafe fn draw(&mut self, s: &Surface, x: &S) -> WinResult<()> {
        #[cfg(test)]
        self.records.clear();
        #[cfg(test)]
        self.typography_records.clear();
        self.ensure_core_paths()?;
        s.target.BeginDraw();
        s.target.Clear(Some(&SURFACE_BASE));
        let l = self.layout;
        self.background(s, x.mode == View::Pair);
        #[cfg(test)]
        if self.background_only {
            return s.target.EndDraw(None, None);
        }
        let drawing = if x.mode == View::Pair {
            self.pair(s, x)
        } else {
            self.main(s, x)
        };
        self.crisp_geometry(
            s,
            &self.paths[Shape::Frame.index()],
            l.viewport.inset(HALF_GRID, HALF_GRID),
            FRAME_CORE,
        );
        for corner in &self.corners {
            for (width, alpha) in [(12.0, 0.06), (7.0, 0.15), (3.0, 0.38)] {
                s.color(rgba(RED.r, RED.g, RED.b, alpha));
                s.target.DrawGeometry(corner, &s.solid, width, &self.stroke);
            }
            self.crisp_geometry(
                s,
                corner,
                l.viewport.inset(HALF_GRID, HALF_GRID),
                opacity(RED, 0.98),
            );
        }
        let pressed = SendMessageW(x.close_btn, 0x00F2, 0, 0) & 4 != 0; // native BST_PUSHED
        let close = l.close.inset(
            GRID + 1.0 + if pressed { 1.0 } else { 0.0 },
            GRID + 1.0 + if pressed { 1.0 } else { 0.0 },
        );
        let color = if pressed {
            SECONDARY
        } else if x.hover_button == CLOSE_BUTTON {
            TEXT
        } else {
            META
        };
        s.line(
            (close.x, close.y),
            (close.right(), close.bottom()),
            color,
            1.2,
            &self.stroke,
        );
        s.line(
            (close.x, close.bottom()),
            (close.right(), close.y),
            color,
            1.2,
            &self.stroke,
        );
        #[cfg(test)]
        if self.grid {
            self.draw_grid(s, x.mode);
        }
        let end = s.target.EndDraw(None, None);
        drawing.and(end)
    }
    fn properties(&self) -> D2D1_RENDER_TARGET_PROPERTIES {
        D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: self.dpi as f32,
            dpiY: self.dpi as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        }
    }
    unsafe fn paint(&mut self, h: W, x: &S) -> WinResult<()> {
        let mut rect: Rect = core::mem::zeroed();
        GetClientRect(h, &mut rect);
        if self.hwnd_surface.is_none() {
            let target = self.factory.CreateHwndRenderTarget(
                &self.properties(),
                &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd: HWND(h as *mut _),
                    pixelSize: D2D_SIZE_U {
                        width: rect.right as u32,
                        height: rect.bottom as u32,
                    },
                    presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                },
            )?;
            self.hwnd_surface = Some(Surface::new(target.cast()?, None)?);
            self.target_generation += 1;
        }
        let surface = self.hwnd_surface.take().unwrap();
        let result = self.draw(&surface, x);
        if self.finish_draw(h, &result) {
            self.hwnd_surface = Some(surface);
        }
        result
    }
    // Only dependent resources are discarded. One immediate retry is allowed;
    // repeated loss waits for a genuine future visual event, never a busy loop.
    unsafe fn finish_draw(&mut self, h: W, result: &WinResult<()>) -> bool {
        if result.is_ok() {
            self.consecutive_losses = 0;
            self.last_error = None;
            true
        } else if let Err(error) = result {
            self.hwnd_surface = None;
            self.last_error = Some(error.code());
            if error.code() == D2DERR_RECREATE_TARGET {
                self.consecutive_losses = self.consecutive_losses.saturating_add(1);
                if self.consecutive_losses == 1 {
                    InvalidateRect(h, core::ptr::null(), 0);
                }
            }
            false
        } else {
            false
        }
    }
    unsafe fn print(&mut self, dc: I, rect: Rect, x: &S) -> WinResult<()> {
        if self.print_surface.is_none() {
            let target = self.factory.CreateDCRenderTarget(&self.properties())?;
            self.print_surface = Some(Surface::new(target.cast()?, Some(target))?);
        }
        let surface = self.print_surface.take().unwrap();
        surface.dc.as_ref().unwrap().BindDC(
            HDC(dc as *mut _),
            &RECT {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            },
        )?;
        let result = self.draw(&surface, x);
        if result.is_ok() {
            self.print_surface = Some(surface);
        } else if let Err(error) = &result {
            self.last_error = Some(error.code());
        }
        result
    }
    fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.core_paths.clear();
        self.hwnd_surface = None;
        self.print_surface = None;
        self.consecutive_losses = 0;
    }
    #[cfg(test)]
    unsafe fn draw_grid(&self, s: &Surface, mode: View) {
        let l = self.layout;
        let grid = rgba(0.18, 0.65, 0.8, 0.15);
        for x in (0..=WINDOW_WIDTH).step_by(GRID as usize) {
            s.line(
                (x as f32, 0.0),
                (x as f32, l.viewport.h),
                grid,
                0.5,
                &self.stroke,
            );
        }
        for y in (0..=WINDOW_HEIGHT).step_by(GRID as usize) {
            s.line(
                (0.0, y as f32),
                (l.viewport.w, y as f32),
                grid,
                0.5,
                &self.stroke,
            );
        }
        s.line(
            (l.viewport.cx(), 0.0),
            (l.viewport.cx(), l.viewport.h),
            rgba(0.0, 1.0, 0.9, 0.8),
            1.0,
            &self.stroke,
        );
        if mode == View::Pair {
            s.line(
                (l.viewport.cx() - 3.0, 0.0),
                (l.viewport.cx() - 3.0, l.viewport.h),
                rgba(1.0, 0.7, 0.1, 0.8),
                0.7,
                &self.stroke,
            );
            // The vertical yellow spans are the two equal module gaps.
            for (top, bottom) in [
                (l.pair_field.y - MODULE_GAP, l.pair_field.y),
                (l.pair_field.bottom(), l.pair_action.y),
            ] {
                s.line(
                    (l.content.x - GRID, top),
                    (l.content.x - GRID, bottom),
                    rgba(1.0, 0.9, 0.2, 0.9),
                    2.0,
                    &self.stroke,
                );
            }
        } else {
            s.line(
                (l.header_logo.x, l.header_logo.cy()),
                (l.content.right(), l.header_logo.cy()),
                rgba(1.0, 0.9, 0.2, 0.8),
                0.7,
                &self.stroke,
            );
        }
        for rect in if mode == View::Pair {
            vec![
                l.content,
                l.pair_logo,
                l.pair_wordmark,
                l.pair_descriptor,
                l.pair_heading,
                l.pair_field,
                l.pair_icon,
                l.pair_content,
                l.pair_accent,
                l.pair_action,
                l.pair_ready,
                l.footer,
            ]
        } else {
            vec![
                l.content,
                l.header_logo,
                l.header_wordmark,
                l.header_descriptor,
                l.close,
                l.key,
                l.sound,
                l.toggle,
                l.unpair,
                l.status,
                l.status_cells[0],
                l.status_cells[1],
                l.status_cells[2],
                l.footer,
                l.footer_version,
            ]
        } {
            s.color(rgba(0.0, 0.9, 0.7, 0.65));
            s.target
                .DrawRectangle(&dr(rect), &s.solid, 0.8, &self.stroke);
        }
        for record in &self.records {
            if record.name == "measured-group" {
                let ink = BoxDip::new(
                    record.origin_x,
                    record.origin_y,
                    record.width,
                    record.height,
                );
                s.color(rgba(1.0, 0.9, 0.2, 0.8));
                s.target
                    .DrawRectangle(&dr(ink), &s.solid, 0.7, &self.stroke);
                s.line(
                    (record.rect.cx(), record.rect.y),
                    (record.rect.cx(), record.rect.bottom()),
                    rgba(1.0, 0.9, 0.2, 0.8),
                    0.7,
                    &self.stroke,
                );
            }
            if record.name.starts_with("footer-") {
                s.line(
                    (l.content.x, record.baseline),
                    (l.content.right(), record.baseline),
                    rgba(1.0, 0.9, 0.2, 0.8),
                    0.7,
                    &self.stroke,
                );
            }
        }
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        self.hwnd_surface = None;
        self.print_surface = None;
        unsafe { core::mem::ManuallyDrop::drop(&mut self.scene_clip.geometricMask) };
        self.text_cache.clear();
        self.formats.clear();
    }
}
unsafe fn layout_controls(x: &S) {
    let l = Layout::new();
    let confirm = x.mode == View::ConfirmUnpair;
    for (control, rect) in [
        (x.code, l.edit()),
        (x.pair_btn, l.pair_action),
        (x.key, l.key_control()),
        (x.sound_on_btn, l.sound_hit(true)),
        (x.sound_off_btn, l.sound_hit(false)),
        (x.apply_btn, l.confirm_cancel),
        (
            x.unpair_btn,
            if confirm { l.confirm_unpair } else { l.unpair },
        ),
        (x.close_btn, l.close),
    ] {
        let r = rect.native(x.dpi);
        MoveWindow(
            control,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            0,
        );
    }
}
unsafe fn set_presentation_dpi(h: W, x: &mut S, dpi: u32, left: i32, top: i32) {
    x.dpi = dpi.max(96);
    x.renderer.borrow_mut().set_dpi(x.dpi);
    let r = Layout::new().viewport.native(x.dpi);
    SetWindowPos(
        h,
        0,
        left,
        top,
        r.right,
        r.bottom,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
    apply_window_region(h, r.right, r.bottom);
    let font = native_edit_font(x.dpi);
    if font != 0 {
        SendMessageW(x.code, SET_FONT, font as usize, 0);
        DeleteObject(x.font_edit);
        x.font_edit = font;
    }
    layout_controls(x);
    InvalidateRect(h, core::ptr::null(), 0);
}
unsafe fn native_edit_font(dpi: u32) -> I {
    CreateFontW(
        -(TypeRole::UiLabel.token().0 * dpi as f32 / 96.0).round() as i32,
        0,
        0,
        0,
        500,
        0,
        0,
        0,
        1,
        0,
        0,
        CLEARTYPE_QUALITY,
        0,
        ws("Jura").as_ptr(),
    )
}
