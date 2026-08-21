//! Provider-neutral image state, card geometry, and the path-free wire format.
//!
//! Agent adapters produce [`ImageMessage`] values. The app keeps the newest
//! message per pane and derives terminal-cell placement intent each frame.
//! Kitty image and placement identifiers belong to `dmux-host`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Rect};
use dmux_ui::Theme;
use sha2::{Digest, Sha256};

use crate::session::LogicalPane;

pub const MAX_ENCODED_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 40_000_000;
pub const MAX_IMAGES_PER_MESSAGE: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAsset {
    /// Provider-stable message id plus content index.
    pub event_id: String,
    pub media_type: String,
    pub png: Arc<[u8]>,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub digest: [u8; 32],
}

impl ImageAsset {
    pub fn from_png(
        event_id: String,
        png: Vec<u8>,
        pixel_width: u32,
        pixel_height: u32,
    ) -> Result<Self, &'static str> {
        if png.len() > MAX_ENCODED_BYTES {
            return Err("encoded image exceeds 20 MiB");
        }
        let dimensions = validate_png_dimensions(&png)?;
        if dimensions != (pixel_width, pixel_height) {
            return Err("PNG dimensions do not match the event metadata");
        }
        let digest: [u8; 32] = Sha256::digest(&png).into();
        Ok(Self {
            event_id,
            media_type: "image/png".to_string(),
            png: png.into(),
            pixel_width,
            pixel_height,
            digest,
        })
    }
}

/// Validate a complete PNG chunk envelope and return its IHDR dimensions.
/// CRC and pixel decoding remain the terminal decoder's responsibility.
pub fn validate_png_dimensions(bytes: &[u8]) -> Result<(u32, u32), &'static str> {
    if bytes.len() < 33 || bytes[..8] != [137, 80, 78, 71, 13, 10, 26, 10] {
        return Err("image is not a PNG");
    }
    if u32::from_be_bytes(bytes[8..12].try_into().unwrap()) != 13 || &bytes[12..16] != b"IHDR" {
        return Err("PNG has an invalid IHDR chunk");
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 || pixels > MAX_PIXELS {
        return Err("image dimensions exceed the supported limit");
    }
    let bit_depth = bytes[24];
    let color_type = bytes[25];
    let valid_depth = match color_type {
        0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
        2 | 4 | 6 => matches!(bit_depth, 8 | 16),
        3 => matches!(bit_depth, 1 | 2 | 4 | 8),
        _ => false,
    };
    if !valid_depth || bytes[26] != 0 || bytes[27] != 0 || bytes[28] > 1 {
        return Err("PNG uses an unsupported header encoding");
    }
    let mut cursor = 8usize;
    let mut saw_idat = false;
    let mut saw_iend = false;
    while cursor.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        let length = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        let kind = &bytes[cursor + 4..cursor + 8];
        let Some(end) = cursor
            .checked_add(12)
            .and_then(|base| base.checked_add(length))
        else {
            return Err("PNG chunk length overflow");
        };
        if end > bytes.len() {
            return Err("PNG has a truncated chunk");
        }
        saw_idat |= kind == b"IDAT";
        if kind == b"IEND" {
            if length != 0 || end != bytes.len() {
                return Err("PNG has an invalid IEND chunk");
            }
            saw_iend = true;
            break;
        }
        cursor = end;
    }
    if !saw_idat || !saw_iend {
        return Err("PNG is missing image data or its end marker");
    }
    Ok((width, height))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMessage {
    pub message_id: String,
    pub assets: Vec<ImageAsset>,
}

#[derive(Debug, Clone)]
pub struct DesiredImage {
    pub pane_slug: String,
    pub event_id: String,
    pub digest: [u8; 32],
    pub png: Arc<[u8]>,
    pub rect: Rect,
    pub z_index: i32,
}

#[derive(Debug)]
pub struct State {
    enabled: bool,
    loopback: bool,
    latest: HashMap<String, ImageMessage>,
    visible_placements: HashSet<u64>,
    visible_assets: HashSet<u64>,
}

impl State {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            loopback: std::env::var("DMUX_IMAGES_LOOPBACK").is_ok_and(|value| value == "1"),
            latest: HashMap::new(),
            visible_placements: HashSet::new(),
            visible_assets: HashSet::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Replaces a pane's card only when the source event changed.
    pub fn apply(&mut self, pane_slug: String, mut message: ImageMessage) -> bool {
        if !self.enabled || message.assets.is_empty() {
            return false;
        }
        message.assets.truncate(MAX_IMAGES_PER_MESSAGE);
        if self.latest.get(&pane_slug) == Some(&message) {
            return false;
        }
        self.latest.insert(pane_slug, message);
        true
    }

    pub fn retain_panes<'a>(&mut self, panes: impl Iterator<Item = &'a str>) {
        let live: std::collections::HashSet<&str> = panes.collect();
        self.latest.retain(|slug, _| live.contains(slug.as_str()));
    }

    /// Paints image-card chrome and returns semantic placements for the host.
    pub fn compose(
        &self,
        buf: &mut CellBuffer,
        panes: &[LogicalPane],
        theme: &Theme,
        suppress: bool,
    ) -> Vec<DesiredImage> {
        if !self.enabled || suppress {
            return Vec::new();
        }
        let mut desired = Vec::new();
        for pane in panes {
            let (Some(body), Some(message)) = (pane.rect, self.latest.get(&pane.slug)) else {
                continue;
            };
            let Some(card) = card_rect(body, message.assets.len()) else {
                continue;
            };
            draw_card(buf, card, message.assets.len(), theme);
            let slots = image_slots(card, &message.assets);
            for (asset, rect) in message.assets.iter().zip(slots) {
                let rect = if self.loopback {
                    loopback_placement(&asset.event_id, rect).unwrap_or(rect)
                } else {
                    rect
                };
                desired.push(DesiredImage {
                    pane_slug: pane.slug.clone(),
                    event_id: asset.event_id.clone(),
                    digest: asset.digest,
                    png: Arc::clone(&asset.png),
                    rect,
                    z_index: 1,
                });
            }
        }
        desired
    }

    /// Reconciles viewer-local Kitty objects into the current synchronized
    /// frame. Cell diffing follows this call and restores the final cursor.
    pub fn emit(
        &mut self,
        host: &mut dmux_host::HostTerminal,
        emitter: &mut dmux_compositor::Emitter,
        desired: &[DesiredImage],
    ) {
        let Some(kitty) = host.kitty_graphics_mut() else {
            return;
        };
        let wanted_placements: HashSet<u64> = desired.iter().map(placement_key).collect();
        let wanted_assets: HashSet<u64> = desired.iter().map(asset_key).collect();
        for key in self
            .visible_placements
            .difference(&wanted_placements)
            .copied()
            .collect::<Vec<_>>()
        {
            emitter.raw(&kitty.delete_placement(key));
        }
        for key in self
            .visible_assets
            .difference(&wanted_assets)
            .copied()
            .collect::<Vec<_>>()
        {
            emitter.raw(&kitty.release_image(key));
        }
        for image in desired {
            let key = asset_key(image);
            let (image_id, upload) = kitty.upload_png(key, &image.png);
            emitter.raw(&upload);
            let placement = dmux_host::KittyPlacement {
                x: image.rect.x,
                y: image.rect.y,
                cols: image.rect.w,
                rows: image.rect.h,
                z_index: image.z_index,
            };
            let place = kitty.place(placement_key(image), image_id, placement);
            if !place.is_empty() {
                emitter.move_to(placement.x, placement.y);
                emitter.raw(&place);
            }
        }
        self.visible_placements = wanted_placements;
        self.visible_assets = wanted_assets;
    }

    fn reset_viewer(&mut self) {
        self.visible_placements.clear();
        self.visible_assets.clear();
    }
}

impl crate::App {
    pub(super) fn compose_images(&mut self) -> Vec<DesiredImage> {
        let suppress = !self.views.is_empty() || self.welcome_active();
        self.images
            .compose(&mut self.back, &self.panes, &self.theme, suppress)
    }

    pub(super) fn emit_images(&mut self, desired: &[DesiredImage]) {
        self.images.emit(&mut self.host, &mut self.emitter, desired);
    }

    pub(super) fn invalidate_terminal_render_state(&mut self) {
        self.emitter.raw(&self.host.reset_graphics());
        self.images.reset_viewer();
        self.emitter.invalidate();
    }
}

fn asset_key(image: &DesiredImage) -> u64 {
    u64::from_be_bytes(image.digest[..8].try_into().unwrap())
}

fn placement_key(image: &DesiredImage) -> u64 {
    let mut hash = Sha256::new();
    hash.update(image.pane_slug.as_bytes());
    hash.update([0]);
    hash.update(image.event_id.as_bytes());
    let digest = hash.finalize();
    u64::from_be_bytes(digest[..8].try_into().unwrap())
}

fn loopback_placement(event_id: &str, rect: Rect) -> Result<Rect, &'static str> {
    let intent = PlacementIntent {
        event_id: event_id.to_string(),
        rect,
    };
    decode_placement(&encode_placement(&intent)?).map(|decoded| decoded.rect)
}

fn card_rect(body: Rect, count: usize) -> Option<Rect> {
    if count == 0 || body.w < 14 || body.h < 7 {
        return None;
    }
    let grid_rows = count.div_ceil(count.min(4)) as u16;
    let height = (grid_rows * 6 + 3).min(body.h.saturating_sub(2));
    (height >= 7).then(|| Rect::new(body.x + 1, body.y + 1, body.w - 2, height))
}

fn draw_card(buf: &mut CellBuffer, card: Rect, count: usize, theme: &Theme) {
    let surface = Cell {
        bg: theme.bg_raised,
        ..Cell::default()
    };
    buf.fill(card, &surface);
    let right = card.right() - 1;
    let bottom = card.bottom() - 1;
    for x in card.x..card.right() {
        let top_ch = if x == card.x {
            '┌'
        } else if x == right {
            '┐'
        } else {
            '─'
        };
        let bottom_ch = if x == card.x {
            '└'
        } else if x == right {
            '┘'
        } else {
            '─'
        };
        buf.set(
            x,
            card.y,
            Cell {
                ch: top_ch,
                fg: theme.border,
                bg: theme.bg_raised,
                ..Cell::default()
            },
        );
        buf.set(
            x,
            bottom,
            Cell {
                ch: bottom_ch,
                fg: theme.border,
                bg: theme.bg_raised,
                ..Cell::default()
            },
        );
    }
    for y in card.y + 1..bottom {
        for x in [card.x, right] {
            buf.set(
                x,
                y,
                Cell {
                    ch: '│',
                    fg: theme.border,
                    bg: theme.bg_raised,
                    ..Cell::default()
                },
            );
        }
    }
    let label = if count == 1 {
        " image attachment ".to_string()
    } else {
        format!(" {count} image attachments ")
    };
    buf.draw_text(
        card.x + 2,
        card.y,
        &label,
        theme.accent,
        theme.bg_raised,
        AttrFlags::BOLD,
        card,
    );
}

fn image_slots(card: Rect, assets: &[ImageAsset]) -> Vec<Rect> {
    let columns = assets.len().min(4) as u16;
    let rows = assets.len().div_ceil(usize::from(columns)) as u16;
    let content = Rect::new(
        card.x + 2,
        card.y + 2,
        card.w.saturating_sub(4),
        card.h.saturating_sub(3),
    );
    let slot_w = content.w / columns;
    let slot_h = content.h / rows;
    assets
        .iter()
        .enumerate()
        .map(|(index, asset)| {
            let col = index as u16 % columns;
            let row = index as u16 / columns;
            fit_image(
                Rect::new(
                    content.x + col * slot_w,
                    content.y + row * slot_h,
                    slot_w.saturating_sub(1).max(1),
                    slot_h.max(1),
                ),
                asset.pixel_width,
                asset.pixel_height,
            )
        })
        .collect()
}

/// Fit pixels to terminal cells using the common two-to-one cell aspect.
fn fit_image(slot: Rect, pixel_width: u32, pixel_height: u32) -> Rect {
    let width_limited_rows = (u64::from(slot.w) * u64::from(pixel_height))
        .div_ceil(u64::from(pixel_width) * 2)
        .max(1);
    let height_limited_cols = ((u64::from(slot.h) * u64::from(pixel_width) * 2)
        / u64::from(pixel_height))
    .clamp(1, u64::from(slot.w)) as u16;
    let (w, h) = if width_limited_rows <= u64::from(slot.h) {
        (slot.w, width_limited_rows as u16)
    } else {
        (height_limited_cols, slot.h)
    };
    Rect::new(slot.x + (slot.w - w) / 2, slot.y + (slot.h - h) / 2, w, h)
}

const ASSET_MAGIC: &[u8; 8] = b"DMUXIMG1";
const PLACEMENT_MAGIC: &[u8; 8] = b"DMUXPLC1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementIntent {
    pub event_id: String,
    pub rect: Rect,
}

pub fn encode_asset_message(message: &ImageMessage) -> Result<Vec<u8>, &'static str> {
    if message.assets.len() > MAX_IMAGES_PER_MESSAGE {
        return Err("too many images in message");
    }
    let mut out = ASSET_MAGIC.to_vec();
    put_string(&mut out, &message.message_id)?;
    out.push(message.assets.len() as u8);
    for asset in &message.assets {
        put_string(&mut out, &asset.event_id)?;
        put_string(&mut out, &asset.media_type)?;
        out.extend_from_slice(&asset.pixel_width.to_be_bytes());
        out.extend_from_slice(&asset.pixel_height.to_be_bytes());
        out.extend_from_slice(&asset.digest);
        put_bytes(&mut out, &asset.png)?;
    }
    Ok(out)
}

pub fn decode_asset_message(bytes: &[u8]) -> Result<ImageMessage, &'static str> {
    let mut input = bytes;
    take_magic(&mut input, ASSET_MAGIC)?;
    let message_id = take_string(&mut input)?;
    let count = usize::from(take(&mut input, 1)?[0]);
    if count > MAX_IMAGES_PER_MESSAGE {
        return Err("too many images in message");
    }
    let mut assets = Vec::with_capacity(count);
    for _ in 0..count {
        let event_id = take_string(&mut input)?;
        let media_type = take_string(&mut input)?;
        if media_type != "image/png" {
            return Err("unsupported media type");
        }
        let pixel_width = take_u32(&mut input)?;
        let pixel_height = take_u32(&mut input)?;
        let expected_digest: [u8; 32] = take(&mut input, 32)?.try_into().unwrap();
        let png = take_bytes(&mut input)?;
        let mut asset = ImageAsset::from_png(event_id, png, pixel_width, pixel_height)?;
        if asset.digest != expected_digest {
            return Err("image digest mismatch");
        }
        asset.media_type = media_type;
        assets.push(asset);
    }
    if !input.is_empty() {
        return Err("trailing asset bytes");
    }
    Ok(ImageMessage { message_id, assets })
}

pub fn encode_placement(intent: &PlacementIntent) -> Result<Vec<u8>, &'static str> {
    let mut out = PLACEMENT_MAGIC.to_vec();
    put_string(&mut out, &intent.event_id)?;
    for value in [intent.rect.x, intent.rect.y, intent.rect.w, intent.rect.h] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    Ok(out)
}

pub fn decode_placement(bytes: &[u8]) -> Result<PlacementIntent, &'static str> {
    let mut input = bytes;
    take_magic(&mut input, PLACEMENT_MAGIC)?;
    let event_id = take_string(&mut input)?;
    let mut value = || -> Result<u16, &'static str> {
        Ok(u16::from_be_bytes(take(&mut input, 2)?.try_into().unwrap()))
    };
    let rect = Rect::new(value()?, value()?, value()?, value()?);
    if rect.is_empty() || !input.is_empty() {
        return Err("invalid placement");
    }
    Ok(PlacementIntent { event_id, rect })
}

fn put_string(out: &mut Vec<u8>, value: &str) -> Result<(), &'static str> {
    put_bytes(out, value.as_bytes())
}

fn put_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), &'static str> {
    let len = u32::try_from(value.len()).map_err(|_| "wire field is too large")?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn take_magic(input: &mut &[u8], magic: &[u8]) -> Result<(), &'static str> {
    if take(input, magic.len())? != magic {
        return Err("invalid wire magic");
    }
    Ok(())
}

fn take_u32(input: &mut &[u8]) -> Result<u32, &'static str> {
    Ok(u32::from_be_bytes(take(input, 4)?.try_into().unwrap()))
}

fn take_string(input: &mut &[u8]) -> Result<String, &'static str> {
    String::from_utf8(take_bytes(input)?).map_err(|_| "wire string is not UTF-8")
}

fn take_bytes(input: &mut &[u8]) -> Result<Vec<u8>, &'static str> {
    let len = usize::try_from(take_u32(input)?).map_err(|_| "wire length overflow")?;
    if len > MAX_ENCODED_BYTES {
        return Err("wire field exceeds limit");
    }
    Ok(take(input, len)?.to_vec())
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], &'static str> {
    if input.len() < len {
        return Err("truncated wire value");
    }
    let (head, rest) = input.split_at(len);
    *input = rest;
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(id: &str, width: u32, height: u32) -> ImageAsset {
        const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        use base64::Engine;
        let mut png = base64::engine::general_purpose::STANDARD
            .decode(PNG_1X1)
            .unwrap();
        png[16..20].copy_from_slice(&width.to_be_bytes());
        png[20..24].copy_from_slice(&height.to_be_bytes());
        ImageAsset::from_png(id.to_string(), png, width, height).unwrap()
    }

    #[test]
    fn asset_codec_round_trips_without_local_identifiers() {
        let message = ImageMessage {
            message_id: "msg-7".to_string(),
            assets: vec![asset("msg-7:0", 640, 480)],
        };
        let bytes = encode_asset_message(&message).unwrap();
        assert_eq!(decode_asset_message(&bytes).unwrap(), message);
        let encoded = String::from_utf8_lossy(&bytes);
        assert!(!encoded.contains("/Users/"));
        assert!(!encoded.contains("placement_id"));
    }

    #[test]
    fn asset_codec_rejects_payload_tampering() {
        let message = ImageMessage {
            message_id: "m".to_string(),
            assets: vec![asset("m:0", 2, 2)],
        };
        let mut bytes = encode_asset_message(&message).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        assert_eq!(decode_asset_message(&bytes), Err("image digest mismatch"));
    }

    #[test]
    fn placement_codec_round_trips_cell_intent() {
        let intent = PlacementIntent {
            event_id: "m:0".to_string(),
            rect: Rect::new(4, 5, 20, 8),
        };
        let bytes = encode_placement(&intent).unwrap();
        assert_eq!(decode_placement(&bytes).unwrap(), intent);
    }

    #[test]
    fn geometry_stays_inside_the_card() {
        let card = Rect::new(10, 3, 80, 15);
        let assets = vec![asset("a", 1600, 900), asset("b", 400, 1200)];
        for slot in image_slots(card, &assets) {
            assert!(!slot.is_empty());
            assert_eq!(slot.intersect(&card), slot);
        }
        let portrait = fit_image(Rect::new(0, 0, 20, 6), 400, 1200);
        assert_eq!(portrait.h, 6);
        assert!(portrait.w < 20);
    }

    #[test]
    fn png_validation_requires_a_complete_chunk_envelope() {
        let valid = asset("valid", 1, 1);
        assert_eq!(validate_png_dimensions(&valid.png), Ok((1, 1)));
        assert!(validate_png_dimensions(&valid.png[..33]).is_err());
        let mut oversized = valid.png.to_vec();
        oversized[16..20].copy_from_slice(&16_385u32.to_be_bytes());
        assert!(validate_png_dimensions(&oversized).is_err());
    }

    #[test]
    fn state_deduplicates_the_same_message() {
        let mut state = State {
            enabled: true,
            loopback: false,
            latest: HashMap::new(),
            visible_placements: HashSet::new(),
            visible_assets: HashSet::new(),
        };
        let message = ImageMessage {
            message_id: "m".to_string(),
            assets: vec![asset("m:0", 2, 2)],
        };
        assert!(state.apply("pane".to_string(), message.clone()));
        assert!(!state.apply("pane".to_string(), message));
    }
}
