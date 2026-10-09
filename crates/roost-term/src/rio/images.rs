//! Retained terminal image pixels and placements, independent of cell frames.
//!
//! Graphics events update this store; the frame emitter reads stable content
//! keys while image bytes are encoded only when a browser asks for them.

use std::collections::HashMap;
use std::sync::Arc;

use image::ImageEncoder;
use rio_graphics::{ColorType, GraphicData};
use rio_vt::ansi::graphics::UpdateQueues;
use rio_vt::ansi::kitty_virtual::{
    IncompletePlacement, PLACEHOLDER, compute_run_geometry, resolve_virtual_placement,
};
use rio_vt::crosswords::Crosswords;
use rio_vt::crosswords::grid::Dimensions;
use rio_vt::crosswords::pos::{Column, Line};
use rio_vt::event::EventListener;
use sha2::{Digest, Sha256};

use super::listener::RioListener;
use crate::core::CoreImagePlacement;

pub(crate) const IMAGE_STORE_MAX_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_TERMINAL_IMAGE_PNG_BYTES: usize = 2 * 1024 * 1024;

struct StoredImage {
    content_key: u64,
    rgba: Arc<[u8]>,
    width: u32,
    height: u32,
    png: Option<Arc<[u8]>>,
    age: u64,
}

#[derive(Default)]
pub(crate) struct ImageStore {
    images: HashMap<u64, StoredImage>,
    age: u64,
    bytes: usize,
    changed: bool,
    pub(super) last_placements: Option<Vec<CoreImagePlacement>>,
}

impl ImageStore {
    pub(crate) fn ingest(&mut self, queues: UpdateQueues, term: &Crosswords<RioListener>) {
        let mut protected: std::collections::HashSet<u64> = term
            .graphics
            .kitty_placements
            .values()
            .map(|placement| rio_graphics::kitty_image_key(placement.image_id))
            .collect();
        protected.extend(
            term.graphics
                .atlas_placements
                .iter()
                .map(|placement| placement.image_key),
        );
        protected.extend(
            term.graphics
                .kitty_virtual_placements
                .values()
                .map(|placement| rio_graphics::kitty_image_key(placement.image_id)),
        );
        self.changed = true;
        for data in queues.pending {
            self.insert(rio_graphics::atlas_image_key(data.id.get()), data);
        }
        for (id, data) in queues.pending_images {
            self.insert(rio_graphics::kitty_image_key(id), data);
        }
        for key in queues.remove_queue {
            if let Some(image) = self.images.remove(&key) {
                self.bytes = self.bytes.saturating_sub(image.rgba.len());
            }
        }
        while self.bytes > IMAGE_STORE_MAX_BYTES {
            let oldest = self
                .images
                .iter()
                .filter(|(key, _)| !protected.contains(key))
                .min_by_key(|(_, image)| image.age)
                .map(|(key, _)| *key);
            let Some(key) = oldest else { break };
            if let Some(image) = self.images.remove(&key) {
                self.bytes = self.bytes.saturating_sub(image.rgba.len());
            }
        }
    }

    fn insert(&mut self, rio_key: u64, data: GraphicData) {
        let pixels = match data.color_type {
            ColorType::Rgba => data.pixels,
            ColorType::Rgb => data
                .pixels
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|[red, green, blue]| [*red, *green, *blue, 255])
                .collect(),
        };
        let mut digest = Sha256::new();
        digest.update((data.width as u32).to_be_bytes());
        digest.update((data.height as u32).to_be_bytes());
        digest.update(&pixels);
        let hash = digest.finalize();
        let content_key = u64::from_be_bytes([
            hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
        ]);
        if let Some(old) = self.images.remove(&rio_key) {
            self.bytes = self.bytes.saturating_sub(old.rgba.len());
        }
        self.age = self.age.wrapping_add(1);
        self.bytes = self.bytes.saturating_add(pixels.len());
        self.images.insert(
            rio_key,
            StoredImage {
                content_key,
                rgba: Arc::from(pixels),
                width: data.width as u32,
                height: data.height as u32,
                png: None,
                age: self.age,
            },
        );
    }

    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub(crate) fn placements<U: EventListener>(
        &self,
        term: &Crosswords<U>,
    ) -> Vec<CoreImagePlacement> {
        let origin = (term.lines_evicted() + term.grid.history_size() as u64) as i64;
        let mut result = Vec::new();
        for placement in term.graphics.kitty_placements.values() {
            self.push_direct(
                &mut result,
                rio_graphics::kitty_image_key(placement.image_id),
                placement.dest_row,
                placement.dest_col,
                placement.columns as usize,
                placement.rows as usize,
                placement.source_x,
                placement.source_y,
                placement.source_width,
                placement.source_height,
                placement.cell_x_offset,
                placement.cell_y_offset,
                placement.z_index,
                origin,
            );
        }
        for placement in &term.graphics.atlas_placements {
            self.push_direct(
                &mut result,
                placement.image_key,
                placement.abs_row,
                placement.col,
                placement.columns,
                placement.rows,
                placement.src_x,
                placement.src_y,
                placement.src_width,
                placement.src_height,
                0,
                0,
                0,
                origin,
            );
        }
        self.virtual_placements(term, origin, &mut result);
        result.sort_by_key(|placement| std::cmp::Reverse(placement.viewport_row));
        result.truncate(roost_protocol::cell::MAX_IMAGE_PLACEMENTS);
        result
    }

    fn virtual_placements<U: EventListener>(
        &self,
        term: &Crosswords<U>,
        origin: i64,
        output: &mut Vec<CoreImagePlacement>,
    ) {
        let history = term.grid.history_size();
        for row_index in -(history as i32)..term.grid.screen_lines() as i32 {
            let line = Line(row_index);
            let grid_row = &term.grid[line];
            if !grid_row.kitty_virtual_placeholder {
                continue;
            }
            let absolute_row = if row_index < 0 {
                term.lines_evicted() as i64 + history as i64 + i64::from(row_index)
            } else {
                origin + i64::from(row_index)
            };
            let mut active: Option<(IncompletePlacement, usize)> = None;
            for col in 0..grid_row.len() {
                let square = grid_row[Column(col)];
                if square.c() != PLACEHOLDER {
                    if let Some((run, start)) = active.take() {
                        self.push_virtual_run(term, run, absolute_row, start, origin, output);
                    }
                    continue;
                }
                let style = term.grid.style_of(&square);
                let combining: Vec<char> = square
                    .extras_id_checked()
                    .and_then(|id| term.grid.extras_table.get(id))
                    .map(|extras| extras.zerowidth.clone())
                    .unwrap_or_default();
                let next =
                    IncompletePlacement::from_cell(style.fg, style.underline_color, &combining);
                match active.as_mut() {
                    Some((run, _)) if run.can_append(&next) => run.append(),
                    Some(_) => {
                        let (run, start) = active.take().unwrap_or((next, col));
                        self.push_virtual_run(term, run, absolute_row, start, origin, output);
                        active = Some((next, col));
                    }
                    None => active = Some((next, col)),
                }
            }
            if let Some((run, start)) = active {
                self.push_virtual_run(term, run, absolute_row, start, origin, output);
            }
        }
    }

    fn push_virtual_run<U: EventListener>(
        &self,
        term: &Crosswords<U>,
        partial: IncompletePlacement,
        absolute_row: i64,
        screen_col: usize,
        origin: i64,
        output: &mut Vec<CoreImagePlacement>,
    ) {
        let run = partial.complete();
        let Some(placement) = resolve_virtual_placement(
            &term.graphics.kitty_virtual_placements,
            run.image_id,
            run.placement_id,
        ) else {
            return;
        };
        let rio_key = rio_graphics::kitty_image_key(run.image_id);
        let Some(image) = self.images.get(&rio_key) else {
            return;
        };
        let Some(geometry) = compute_run_geometry(
            &run,
            placement.columns,
            placement.rows,
            image.width,
            image.height,
            (placement.x, placement.y, placement.width, placement.height),
            8.0,
            16.0,
            0.0,
            0.0,
            0,
            0,
        ) else {
            return;
        };
        let [u0, v0, u1, v1] = geometry.source_rect;
        let sx = (u0 * image.width as f32).floor().max(0.0) as u32;
        let sy = (v0 * image.height as f32).floor().max(0.0) as u32;
        let ex = (u1 * image.width as f32).ceil().min(image.width as f32) as u32;
        let ey = (v1 * image.height as f32).ceil().min(image.height as f32) as u32;
        let width = ex.saturating_sub(sx);
        let height = ey.saturating_sub(sy);
        if width == 0 || height == 0 {
            return;
        }
        output.push(CoreImagePlacement {
            image_key: image.content_key,
            viewport_row: absolute_row.saturating_sub(origin),
            col: screen_col.min(u16::MAX as usize) as u16,
            columns: (geometry.width / 8.0).ceil().max(1.0).min(u16::MAX as f32) as u16,
            rows: (geometry.height / 16.0)
                .ceil()
                .max(1.0)
                .min(u16::MAX as f32) as u16,
            source_x: sx,
            source_y: sy,
            source_width: width,
            source_height: height,
            image_width: image.width,
            image_height: image.height,
            offset_x_px: geometry.x.round().max(0.0).min(u16::MAX as f32) as u16,
            offset_y_px: geometry.y.round().max(0.0).min(u16::MAX as f32) as u16,
            z_index: 0,
        });
    }
    #[allow(clippy::too_many_arguments)]
    fn push_direct(
        &self,
        result: &mut Vec<CoreImagePlacement>,
        rio_key: u64,
        abs_row: i64,
        col: usize,
        columns: usize,
        rows: usize,
        sx: u32,
        sy: u32,
        sw: u32,
        sh: u32,
        ox: u32,
        oy: u32,
        z: i32,
        origin: i64,
    ) {
        let Some(image) = self.images.get(&rio_key) else {
            return;
        };
        let sx = sx.min(image.width);
        let sy = sy.min(image.height);
        let width = if sw == 0 {
            image.width.saturating_sub(sx)
        } else {
            sw.min(image.width.saturating_sub(sx))
        };
        let height = if sh == 0 {
            image.height.saturating_sub(sy)
        } else {
            sh.min(image.height.saturating_sub(sy))
        };
        let columns = columns.min(u16::MAX as usize);
        let rows = rows.min(u16::MAX as usize);
        if columns == 0 || rows == 0 || width == 0 || height == 0 {
            return;
        }
        result.push(CoreImagePlacement {
            image_key: image.content_key,
            viewport_row: abs_row.saturating_sub(origin),
            col: col.min(u16::MAX as usize) as u16,
            columns: columns as u16,
            rows: rows as u16,
            source_x: sx,
            source_y: sy,
            source_width: width,
            source_height: height,
            image_width: image.width,
            image_height: image.height,
            offset_x_px: ox.min(u16::MAX as u32) as u16,
            offset_y_px: oy.min(u16::MAX as u32) as u16,
            z_index: z,
        });
    }

    pub(crate) fn png(&mut self, content_key: u64) -> Option<Arc<[u8]>> {
        let image = self
            .images
            .values_mut()
            .find(|image| image.content_key == content_key)?;
        if let Some(png) = &image.png {
            return Some(Arc::clone(png));
        }
        let mut bitmap =
            image::RgbaImage::from_raw(image.width, image.height, image.rgba.to_vec())?;
        for _ in 0..=4 {
            let mut encoded = Vec::new();
            image::codecs::png::PngEncoder::new(&mut encoded)
                .write_image(
                    bitmap.as_raw(),
                    bitmap.width(),
                    bitmap.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .ok()?;
            if encoded.len() <= MAX_TERMINAL_IMAGE_PNG_BYTES {
                let png: Arc<[u8]> = Arc::from(encoded);
                image.png = Some(Arc::clone(&png));
                return Some(png);
            }
            if bitmap.width() == 1 || bitmap.height() == 1 {
                break;
            }
            bitmap = image::imageops::resize(
                &bitmap,
                (bitmap.width() / 2).max(1),
                (bitmap.height() / 2).max(1),
                image::imageops::FilterType::Triangle,
            );
        }
        None
    }
}
