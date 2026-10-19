//! Exercise the actual D3D11 software renderer, not a mock of dirty rectangles.
use super::{DirectXDevices, DirectXRenderer};
use gpui::{
    AtlasKey, AtlasTile, MonochromeSprite, PlatformAtlas, Radians, RenderSvgParams,
    TransformationMatrix,
};
use gpui::{
    Bounds, ContentMask, Corners, DevicePixels, Path, Quad, ScaledPixels, Scene, Shadow,
    WindowBackgroundAppearance, point, px, rgb, rgba, size,
};
use std::borrow::Cow;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::{Direct3D::*, Direct3D11::*, Dxgi::*};
use windows::Win32::{Foundation::HMODULE, UI::WindowsAndMessaging::*};
use windows::core::Interface;
use windows::core::w;

struct HiddenWindow(HWND);
impl HiddenWindow {
    fn new() -> Self {
        Self(unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("render-test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                320,
                200,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        })
    }
}
impl Drop for HiddenWindow {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0).unwrap() };
    }
}

fn warp() -> DirectXDevices {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_WARP,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .unwrap();
    }
    let device: ID3D11Device = device.unwrap();
    let dxgi: IDXGIDevice = device.cast().unwrap();
    DirectXDevices {
        adapter: unsafe { dxgi.GetAdapter().unwrap().cast().unwrap() },
        dxgi_factory: unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).unwrap() },
        device,
        device_context: context.unwrap(),
    }
}

fn area(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
    Bounds::new(
        point(ScaledPixels(x), ScaledPixels(y)),
        size(ScaledPixels(width), ScaledPixels(height)),
    )
}

fn glyph(renderer: &DirectXRenderer) -> AtlasTile {
    renderer
        .atlas
        .get_or_insert_with(
            &AtlasKey::Svg(RenderSvgParams {
                path: "test-glyph".into(),
                size: size(DevicePixels(8), DevicePixels(12)),
            }),
            &mut || {
                Ok(Some((
                    size(DevicePixels(8), DevicePixels(12)),
                    Cow::Owned(
                        (0..96)
                            .map(|index| {
                                if index % 8 < 2 || index / 8 == 6 {
                                    255
                                } else {
                                    0
                                }
                            })
                            .collect(),
                    ),
                )))
            },
        )
        .unwrap()
        .unwrap()
}

fn scene(step: usize, width: f32, height: f32, tile: AtlasTile) -> Scene {
    let mut scene = Scene::default();
    let mask = ContentMask {
        bounds: area(0., 0., width, height),
    };
    let dark = step < 7;
    scene.insert_primitive(Quad {
        bounds: mask.bounds,
        content_mask: mask,
        background: rgb(if dark { 0x202020 } else { 0xeeeeee }).into(),
        ..Default::default()
    });
    // Many stable glyph-shaped primitives across both terminal panes.
    for row in 0..8 {
        for col in 0..30 {
            if step == 3 && row == 2 && col > 12 {
                continue;
            }
            scene.insert_primitive(Quad {
                bounds: area(5. + col as f32 * 9., 7. + row as f32 * 17., 5., 10.),
                content_mask: mask,
                background: rgb(if dark { 0xbbbbbb } else { 0x333333 }).into(),
                ..Default::default()
            });
        }
    }
    // Cursor appears, moves and disappears without changing other rows.
    if step % 2 == 0 {
        scene.insert_primitive(Quad {
            bounds: area(18. + step as f32 * 9., 41., 8., 16.),
            content_mask: mask,
            background: rgb(0x55cc77).into(),
            ..Default::default()
        });
    }
    // Translucent popup, including its removal and an antialiased path.
    if step == 4 || step == 5 {
        scene.insert_primitive(Shadow {
            order: 0,
            blur_radius: ScaledPixels(5.),
            bounds: area(60.5, 20.5, 120., 90.),
            corner_radii: Corners::all(ScaledPixels(8.)),
            content_mask: mask,
            color: rgba(0x00000088).into(),
            element_bounds: area(60.5, 20.5, 120., 90.),
            element_corner_radii: Corners::all(ScaledPixels(8.)),
            inset: 0,
            pad: 0,
        });
        scene.insert_primitive(Quad {
            bounds: area(60.5, 20.5, 120., 90.),
            content_mask: mask,
            background: rgba(0x4466bb99).into(),
            corner_radii: Corners::all(ScaledPixels(8.)),
            ..Default::default()
        });
        let mut path = Path::new(point(px(70.), px(30.)));
        path.line_to(point(px(100.), px(80.)));
        path.line_to(point(px(120.), px(30.)));
        path.line_to(point(px(70.), px(30.)));
        let mut path = path.scale(1.);
        path.color = rgb(0xffee55).into();
        path.content_mask = mask;
        scene.insert_primitive(path);
    }
    scene.insert_primitive(MonochromeSprite {
        order: 0,
        pad: 0,
        bounds: area(10., 160., 8., 12.),
        content_mask: mask,
        color: rgb(if step % 2 == 0 { 0xff5555 } else { 0x55aaff }).into(),
        tile,
        transformation: TransformationMatrix::unit()
            .translate(point(ScaledPixels(35.), ScaledPixels(2.)))
            .rotate(Radians(if step % 2 == 0 { 0.05 } else { -0.05 })),
    });
    scene.finish();
    scene
}

#[test]
fn partial_warp_frames_match_full_redraw_after_cursor_delete_overlay_resize_and_recovery() {
    exercise_frames(true);
    exercise_frames(false);
}

fn exercise_frames(disable_composition: bool) {
    let window = HiddenWindow::new();
    let reference_window = HiddenWindow::new();
    let devices = warp();
    let mut partial = DirectXRenderer::new(window.0, &devices, disable_composition).unwrap();
    let mut full = DirectXRenderer::new(reference_window.0, &devices, disable_composition).unwrap();
    partial.partial_rendering = true;
    full.partial_rendering = false;
    let mut repaint = 0u64;
    let mut full_area = 0u64;
    for step in 0..12 {
        let (width, height) = if step < 9 { (320, 200) } else { (400, 240) };
        for renderer in [&mut partial, &mut full] {
            renderer
                .resize(size(DevicePixels(width), DevicePixels(height)))
                .unwrap();
            if step == 11 {
                renderer.handle_device_lost_impl(&devices).unwrap();
                renderer.mark_drawable();
            }
        }
        let scene = scene(step, width as f32, height as f32, glyph(&partial));
        let reference_scene = self::scene(step, width as f32, height as f32, glyph(&full));
        let dirty = partial
            .render_frame(&scene, WindowBackgroundAppearance::Opaque)
            .unwrap();
        assert!(!dirty.is_empty());
        repaint += dirty
            .iter()
            .map(|rect| ((rect.right - rect.left) * (rect.bottom - rect.top)) as u64)
            .sum::<u64>();
        full_area += (width * height) as u64;
        let actual = partial
            .render_to_image(&scene, WindowBackgroundAppearance::Opaque)
            .unwrap();
        let expected = full
            .render_to_image(&reference_scene, WindowBackgroundAppearance::Opaque)
            .unwrap();
        assert!(
            actual == expected,
            "frame {step} differs from full redraw ({} differing pixels)",
            actual
                .pixels()
                .zip(expected.pixels())
                .filter(|(a, b)| a != b)
                .count()
        );
        // Exercise rotating swap-chain buffers too, using the coherent retained target.
        partial.present(Some(&dirty)).unwrap();
        assert!(
            partial
                .render_frame(&scene, WindowBackgroundAppearance::Opaque)
                .unwrap()
                .is_empty()
        );
    }
    eprintln!("WARP dirty pixel area: {repaint}/{full_area}");
    assert!(repaint < full_area);
}

#[test]
fn partial_warp_distant_changes_keep_separate_regions_and_match_full_pixels() {
    let window = HiddenWindow::new();
    let reference_window = HiddenWindow::new();
    let devices = warp();
    let mut partial = DirectXRenderer::new(window.0, &devices, true).unwrap();
    let mut full = DirectXRenderer::new(reference_window.0, &devices, true).unwrap();
    partial.partial_rendering = true;
    full.partial_rendering = false;
    for renderer in [&mut partial, &mut full] {
        renderer
            .resize(size(DevicePixels(3840), DevicePixels(2160)))
            .unwrap();
    }
    for step in 0..5 {
        let mut scene = Scene::default();
        let mask = ContentMask {
            bounds: area(0., 0., 3840., 2160.),
        };
        scene.insert_primitive(Quad {
            bounds: mask.bounds,
            content_mask: mask,
            background: rgb(0x202020).into(),
            ..Default::default()
        });
        for x in [20., 1800., 3750.] {
            for row in 0..15 {
                scene.insert_primitive(Quad {
                    bounds: area(x, 15. + row as f32 * 20., 8., 12.),
                    content_mask: mask,
                    background: rgb(0xdddddd).into(),
                    ..Default::default()
                });
            }
        }
        // Insert/remove a primitive at the left, while moving a translucent
        // cursor at the far right. The middle must remain undamaged.
        if step % 2 == 0 {
            scene.insert_primitive(Quad {
                bounds: area(23., 127., 8., 18.),
                content_mask: mask,
                background: rgba(0x55ff5588).into(),
                ..Default::default()
            });
        }
        scene.insert_primitive(Quad {
            bounds: area(3752., 127. + step as f32 * 12., 8., 18.),
            content_mask: mask,
            background: rgba(0xff555588).into(),
            ..Default::default()
        });
        scene.finish();
        let dirty = partial
            .render_frame(&scene, WindowBackgroundAppearance::Opaque)
            .unwrap();
        if step > 0 {
            let pixels: i64 = dirty
                .iter()
                .map(|r| i64::from(r.right - r.left) * i64::from(r.bottom - r.top))
                .sum();
            assert!(dirty.len() >= 2 && dirty.len() <= 16, "{dirty:?}");
            assert!(pixels < 3840 * 2160 / 100, "{pixels} dirty pixels");
            eprintln!(
                "4K sparse frame {step}: {} regions, {pixels} dirty pixels",
                dirty.len()
            );
        }
        let actual = partial
            .render_to_image(&scene, WindowBackgroundAppearance::Opaque)
            .unwrap();
        let expected = full
            .render_to_image(&scene, WindowBackgroundAppearance::Opaque)
            .unwrap();
        assert!(actual == expected, "4K sparse frame {step}");
        partial.present(Some(&dirty)).unwrap();
    }
}
