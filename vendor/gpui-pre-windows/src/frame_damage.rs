//! Exact scene comparison, without hashes or reading padding bytes. Old and new
//! bounds are both damaged so removed glyphs, cursors and overlays are erased.
//! Compare ordered primitives in spatial tiles, then submit a bounded set of
//! exact rectangles. Distant changes need not repaint everything between them.

use gpui::*;
use windows::Win32::Foundation::RECT;

#[derive(Default)]
pub(crate) struct FrameDamage {
    previous: Option<Vec<Primitive>>,
    tiles: Vec<Vec<usize>>,
    dimensions: (u32, u32),
    clear_color: [f32; 4],
}

impl FrameDamage {
    pub(crate) fn reset(&mut self) {
        self.previous = None;
        self.tiles.clear();
    }

    pub(crate) fn current(&self) -> &[Primitive] {
        self.previous.as_deref().unwrap_or_default()
    }

    pub(crate) fn update(
        &mut self,
        scene: &Scene,
        width: u32,
        height: u32,
        clear: [f32; 4],
    ) -> Vec<RECT> {
        let next = primitives(scene);
        let full = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let next_tiles = tile_indices(&next, full);
        let mut regions = Vec::new();
        if let Some(previous) = &self.previous {
            if self.clear_color != clear
                || self.dimensions != (width, height)
                || !scene.surfaces.is_empty()
            {
                regions.push(full);
            } else {
                let columns = width.div_ceil(TILE_SIZE) as usize;
                for (index, (old, new)) in self.tiles.iter().zip(&next_tiles).enumerate() {
                    let prefix = old
                        .iter()
                        .zip(new)
                        .take_while(|(a, b)| same(&previous[**a], &next[**b]))
                        .count();
                    let suffix = old[prefix..]
                        .iter()
                        .rev()
                        .zip(new[prefix..].iter().rev())
                        .take_while(|(a, b)| same(&previous[**a], &next[**b]))
                        .count();
                    let clip = RECT {
                        left: (index % columns) as i32 * TILE_SIZE as i32,
                        top: (index / columns) as i32 * TILE_SIZE as i32,
                        right: ((index % columns + 1) as i32 * TILE_SIZE as i32).min(full.right),
                        bottom: ((index / columns + 1) as i32 * TILE_SIZE as i32).min(full.bottom),
                    };
                    let mut damage = None;
                    for item in old[prefix..old.len() - suffix]
                        .iter()
                        .map(|i| &previous[*i])
                        .chain(new[prefix..new.len() - suffix].iter().map(|i| &next[*i]))
                    {
                        include(&mut damage, bounds(item), clip);
                    }
                    if let Some(rect) = damage {
                        regions.push(rect);
                    }
                }
                // Image atlas contents can change without their tile changing.
                for sprite in &scene.polychrome_sprites {
                    let mut damage = None;
                    include(
                        &mut damage,
                        bounds(&Primitive::PolychromeSprite(*sprite)),
                        full,
                    );
                    if let Some(rect) = damage {
                        regions.push(rect);
                    }
                }
            }
        } else {
            regions.push(full);
        }
        self.previous = Some(next);
        self.tiles = next_tiles;
        self.dimensions = (width, height);
        self.clear_color = clear;
        merge_regions(regions, full)
    }
}

const TILE_SIZE: u32 = 128;
const MAX_REGIONS: usize = 16;

fn tile_indices(primitives: &[Primitive], full: RECT) -> Vec<Vec<usize>> {
    let columns = (full.right as u32).div_ceil(TILE_SIZE) as usize;
    let rows = (full.bottom as u32).div_ceil(TILE_SIZE) as usize;
    let mut tiles = vec![Vec::new(); columns * rows];
    for (index, primitive) in primitives.iter().enumerate() {
        let mut area = None;
        include(&mut area, bounds(primitive), full);
        let Some(area) = area else { continue };
        for row in
            area.top as usize / TILE_SIZE as usize..=(area.bottom - 1) as usize / TILE_SIZE as usize
        {
            for col in area.left as usize / TILE_SIZE as usize
                ..=(area.right - 1) as usize / TILE_SIZE as usize
            {
                tiles[row * columns + col].push(index);
            }
        }
    }
    tiles
}

fn area(rect: RECT) -> i64 {
    i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top)
}

fn union(a: RECT, b: RECT) -> RECT {
    RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn merge_regions(regions: Vec<RECT>, full: RECT) -> Vec<RECT> {
    if regions.iter().map(|r| area(*r)).sum::<i64>() * 10 >= area(full) * 7 {
        return vec![full];
    }
    let mut merged: Vec<RECT> = Vec::new();
    for mut rect in regions {
        let mut index = 0;
        while index < merged.len() {
            let other = merged[index];
            let combined = union(rect, other);
            // A little extra painting costs less than another draw submission.
            if area(combined) * 10 <= (area(rect) + area(other)) * 12 {
                rect = combined;
                merged.swap_remove(index);
                index = 0;
            } else {
                index += 1;
            }
        }
        merged.push(rect);
    }
    if merged.len() > MAX_REGIONS {
        let combined = merged.into_iter().reduce(union).unwrap();
        return vec![if area(combined) * 10 >= area(full) * 7 {
            full
        } else {
            combined
        }];
    }
    merged
}

pub(crate) fn primitives(scene: &Scene) -> Vec<Primitive> {
    let mut result = Vec::new();
    for batch in scene.batches() {
        match batch {
            PrimitiveBatch::Shadows(range) => {
                result.extend(scene.shadows[range].iter().copied().map(Primitive::Shadow))
            }
            PrimitiveBatch::Quads(range) => {
                result.extend(scene.quads[range].iter().copied().map(Primitive::Quad))
            }
            PrimitiveBatch::Paths(range) => {
                result.extend(scene.paths[range].iter().cloned().map(Primitive::Path))
            }
            PrimitiveBatch::Underlines(range) => result.extend(
                scene.underlines[range]
                    .iter()
                    .copied()
                    .map(Primitive::Underline),
            ),
            PrimitiveBatch::MonochromeSprites { range, .. } => result.extend(
                scene.monochrome_sprites[range]
                    .iter()
                    .copied()
                    .map(Primitive::MonochromeSprite),
            ),
            PrimitiveBatch::SubpixelSprites { range, .. } => result.extend(
                scene.subpixel_sprites[range]
                    .iter()
                    .copied()
                    .map(Primitive::SubpixelSprite),
            ),
            PrimitiveBatch::PolychromeSprites { range, .. } => result.extend(
                scene.polychrome_sprites[range]
                    .iter()
                    .copied()
                    .map(Primitive::PolychromeSprite),
            ),
            PrimitiveBatch::Surfaces(range) => result.extend(
                scene.surfaces[range]
                    .iter()
                    .cloned()
                    .map(Primitive::Surface),
            ),
        }
    }
    result
}

/// Keep every overlapping primitive, including unchanged backgrounds and
/// translucent overlays. Repainting only changed primitives would leave holes.
pub(crate) fn intersecting(primitives: &[Primitive], rect: RECT) -> Scene {
    let mut result = Scene::default();
    for primitive in primitives {
        let area = bounds(primitive);
        if area.right().0 + 2. <= rect.left as f32
            || area.left().0 - 2. >= rect.right as f32
            || area.bottom().0 + 2. <= rect.top as f32
            || area.top().0 - 2. >= rect.bottom as f32
        {
            continue;
        }
        match primitive.clone() {
            Primitive::Shadow(value) => result.shadows.push(value),
            Primitive::Quad(value) => result.quads.push(value),
            Primitive::Path(value) => result.paths.push(value),
            Primitive::Underline(value) => result.underlines.push(value),
            Primitive::MonochromeSprite(value) => result.monochrome_sprites.push(value),
            Primitive::SubpixelSprite(value) => result.subpixel_sprites.push(value),
            Primitive::PolychromeSprite(value) => result.polychrome_sprites.push(value),
            Primitive::Surface(value) => result.surfaces.push(value),
        }
    }
    result
}

fn include(result: &mut Option<RECT>, area: Bounds<ScaledPixels>, full: RECT) {
    // Round outward and cover antialiasing at fractional coordinates.
    let rect = RECT {
        left: (area.left().0.floor() as i32)
            .saturating_sub(2)
            .max(full.left),
        top: (area.top().0.floor() as i32)
            .saturating_sub(2)
            .max(full.top),
        right: (area.right().0.ceil() as i32)
            .saturating_add(2)
            .min(full.right),
        bottom: (area.bottom().0.ceil() as i32)
            .saturating_add(2)
            .min(full.bottom),
    };
    if rect.left >= rect.right || rect.top >= rect.bottom {
        return;
    }
    *result = Some(match *result {
        Some(previous) => RECT {
            left: rect.left.min(previous.left),
            top: rect.top.min(previous.top),
            right: rect.right.max(previous.right),
            bottom: rect.bottom.max(previous.bottom),
        },
        None => rect,
    });
}

fn transformed(area: Bounds<ScaledPixels>, matrix: TransformationMatrix) -> Bounds<ScaledPixels> {
    let mut left = f32::INFINITY;
    let mut top = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for x in [area.left().0, area.right().0] {
        for y in [area.top().0, area.bottom().0] {
            let point = matrix.apply(point(px(x), px(y)));
            left = left.min(point.x.as_f32());
            top = top.min(point.y.as_f32());
            right = right.max(point.x.as_f32());
            bottom = bottom.max(point.y.as_f32());
        }
    }
    Bounds::new(
        point(ScaledPixels(left), ScaledPixels(top)),
        size(ScaledPixels(right - left), ScaledPixels(bottom - top)),
    )
}

fn bounds(item: &Primitive) -> Bounds<ScaledPixels> {
    let (area, mask) = match item {
        Primitive::Shadow(p) => {
            let area = if p.inset != 0 {
                p.element_bounds
            } else {
                let margin = p.blur_radius.0 * 3.;
                Bounds::new(
                    point(
                        ScaledPixels(p.bounds.left().0 - margin),
                        ScaledPixels(p.bounds.top().0 - margin),
                    ),
                    size(
                        ScaledPixels(p.bounds.size.width.0 + 2. * margin),
                        ScaledPixels(p.bounds.size.height.0 + 2. * margin),
                    ),
                )
            };
            (area, p.content_mask)
        }
        Primitive::Quad(p) => (p.bounds, p.content_mask),
        Primitive::Path(p) => (p.bounds, p.content_mask),
        Primitive::Underline(p) => (p.bounds, p.content_mask),
        Primitive::MonochromeSprite(p) => (transformed(p.bounds, p.transformation), p.content_mask),
        Primitive::SubpixelSprite(p) => (transformed(p.bounds, p.transformation), p.content_mask),
        Primitive::PolychromeSprite(p) => (p.bounds, p.content_mask),
        Primitive::Surface(p) => (p.bounds, p.content_mask),
    };
    area.intersect(&mask.bounds)
}

macro_rules! fields_equal {
    ($a:ident, $b:ident, $($field:ident),+ $(,)?) => { true $(&& $a.$field == $b.$field)+ };
}

fn same(a: &Primitive, b: &Primitive) -> bool {
    match (a, b) {
        (Primitive::Shadow(a), Primitive::Shadow(b)) => fields_equal!(
            a,
            b,
            order,
            blur_radius,
            bounds,
            corner_radii,
            content_mask,
            color,
            element_bounds,
            element_corner_radii,
            inset
        ),
        (Primitive::Quad(a), Primitive::Quad(b)) => fields_equal!(
            a,
            b,
            order,
            border_style,
            bounds,
            content_mask,
            background,
            border_color,
            corner_radii,
            border_widths
        ),
        (Primitive::Underline(a), Primitive::Underline(b)) => {
            fields_equal!(a, b, order, bounds, content_mask, color, thickness, wavy)
        }
        (Primitive::MonochromeSprite(a), Primitive::MonochromeSprite(b)) => fields_equal!(
            a,
            b,
            order,
            bounds,
            content_mask,
            color,
            tile,
            transformation
        ),
        (Primitive::SubpixelSprite(a), Primitive::SubpixelSprite(b)) => fields_equal!(
            a,
            b,
            order,
            bounds,
            content_mask,
            color,
            tile,
            transformation
        ),
        (Primitive::PolychromeSprite(a), Primitive::PolychromeSprite(b)) => fields_equal!(
            a,
            b,
            order,
            grayscale,
            opacity,
            bounds,
            content_mask,
            corner_radii,
            tile
        ),
        (Primitive::Path(a), Primitive::Path(b)) => {
            fields_equal!(a, b, order, bounds, content_mask, color)
                && a.vertices.len() == b.vertices.len()
                && a.vertices
                    .iter()
                    .zip(&b.vertices)
                    .all(|(a, b)| fields_equal!(a, b, xy_position, st_position, content_mask))
        }
        _ => false,
    }
}
