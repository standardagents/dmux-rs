//! Stateful direct-PNG Kitty graphics encoder.

use std::collections::HashMap;

const ST: &[u8] = b"\x1b\\";
const MAX_BASE64_CHARS: usize = 4096;
const RAW_CHUNK_BYTES: usize = (MAX_BASE64_CHARS / 4) * 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    pub z_index: i32,
}

#[derive(Debug, Default)]
pub struct KittyGraphics {
    next_image_id: u32,
    next_placement_id: u32,
    images: HashMap<u64, u32>,
    placements: HashMap<u64, (u32, u32, Placement)>,
}

impl KittyGraphics {
    pub fn new() -> Self {
        Self {
            next_image_id: 1,
            next_placement_id: 1,
            ..Self::default()
        }
    }

    pub fn upload_png(&mut self, asset_key: u64, png: &[u8]) -> (u32, Vec<u8>) {
        if let Some(&id) = self.images.get(&asset_key) {
            return (id, Vec::new());
        }
        let id = self.next_image_id;
        self.next_image_id = self.next_image_id.wrapping_add(1).max(1);
        self.images.insert(asset_key, id);
        (id, encode_upload(id, png))
    }

    pub fn place(&mut self, placement_key: u64, image_id: u32, p: Placement) -> Vec<u8> {
        let id = if let Some(&(id, previous_image, previous)) = self.placements.get(&placement_key)
        {
            if previous_image == image_id && previous == p {
                return Vec::new();
            }
            id
        } else {
            let id = self.next_placement_id;
            self.next_placement_id = self.next_placement_id.wrapping_add(1).max(1);
            id
        };
        self.placements.insert(placement_key, (id, image_id, p));
        encode_place(id, image_id, p)
    }

    pub fn delete_placement(&mut self, placement_key: u64) -> Vec<u8> {
        self.placements
            .remove(&placement_key)
            .map_or_else(Vec::new, |(id, image_id, _)| {
                encode_delete_placement(id, image_id)
            })
    }

    pub fn release_image(&mut self, asset_key: u64) -> Vec<u8> {
        self.images
            .remove(&asset_key)
            .map_or_else(Vec::new, encode_delete_image)
    }

    pub fn delete_all(&mut self) -> Vec<u8> {
        if self.images.is_empty() && self.placements.is_empty() {
            return Vec::new();
        }
        self.images.clear();
        self.placements.clear();
        let mut out = Vec::new();
        command(&mut out, "a=d,d=A,q=2", b"");
        out
    }

    pub fn image_id(&self, asset_key: u64) -> Option<u32> {
        self.images.get(&asset_key).copied()
    }
    pub fn placement_id(&self, placement_key: u64) -> Option<u32> {
        self.placements.get(&placement_key).map(|(id, _, _)| *id)
    }
}

fn encode_upload(image_id: u32, png: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(png.len() * 4 / 3 + 96);
    if png.is_empty() {
        command(
            &mut out,
            &format!("a=t,f=100,t=d,i={image_id},m=0,q=2"),
            b"",
        );
        return out;
    }
    for (index, chunk) in png.chunks(RAW_CHUNK_BYTES).enumerate() {
        let more = index + 1 < png.len().div_ceil(RAW_CHUNK_BYTES);
        let payload = base64(chunk);
        let params = if index == 0 {
            format!(
                "a=t,f=100,t=d,i={image_id},m={},q=2",
                if more { 1 } else { 0 }
            )
        } else {
            format!("m={},q=2", if more { 1 } else { 0 })
        };
        command(&mut out, &params, payload.as_bytes());
    }
    out
}

fn encode_place(id: u32, image_id: u32, p: Placement) -> Vec<u8> {
    let mut out = Vec::new();
    command(
        &mut out,
        &format!(
            "a=p,i={image_id},p={id},c={},r={},C=1,z={},q=2",
            p.cols, p.rows, p.z_index
        ),
        b"",
    );
    out
}

fn encode_delete_placement(id: u32, image_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    command(&mut out, &format!("a=d,d=i,i={image_id},p={id},q=2"), b"");
    out
}

fn encode_delete_image(image_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    command(&mut out, &format!("a=d,d=I,i={image_id},q=2"), b"");
    out
}

fn command(out: &mut Vec<u8>, params: &str, payload: &[u8]) {
    out.extend_from_slice(b"\x1b_G");
    out.extend_from_slice(params.as_bytes());
    out.push(b';');
    out.extend_from_slice(payload);
    out.extend_from_slice(ST);
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as usize;
        let b = chunk.get(1).copied().unwrap_or(0) as usize;
        let c = chunk.get(2).copied().unwrap_or(0) as usize;
        out.push(A[a >> 2] as char);
        out.push(A[((a & 3) << 4) | (b >> 4)] as char);
        out.push(if chunk.len() > 1 {
            A[((b & 15) << 2) | (c >> 6)] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[c & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_limited_and_quiet() {
        let mut g = KittyGraphics::new();
        let (_, bytes) = g.upload_png(1, &vec![0x5a; RAW_CHUNK_BYTES * 2 + 1]);
        for part in bytes.split(|b| *b == 0x1b).filter(|p| p.starts_with(b"_G")) {
            let payload = part.split(|b| *b == b';').nth(1).unwrap();
            assert!(payload.strip_suffix(ST).unwrap_or(payload).len() <= MAX_BASE64_CHARS);
        }
        assert_eq!(bytes.windows(3).filter(|w| *w == b"m=1").count(), 2);
        assert_eq!(bytes.windows(3).filter(|w| *w == b"m=0").count(), 1);
        assert!(bytes.windows(3).any(|w| w == b"q=2"));
    }

    #[test]
    fn ids_are_stable_until_cleanup() {
        let mut g = KittyGraphics::new();
        let (id, first) = g.upload_png(1, b"png");
        let (same, second) = g.upload_png(1, b"other");
        assert_eq!(id, same);
        assert!(!first.is_empty());
        assert!(second.is_empty());
        let p = Placement {
            x: 1,
            y: 2,
            cols: 3,
            rows: 4,
            z_index: 1,
        };
        let one = g.place(9, id, p);
        let pid = g.placement_id(9).unwrap();
        let two = g.place(9, id, Placement { x: 2, ..p });
        assert_eq!(g.placement_id(9), Some(pid));
        assert!(!one.is_empty());
        assert!(!two.is_empty());
        assert!(g.place(9, id, Placement { x: 2, ..p }).is_empty());
        assert!(!g.delete_placement(9).is_empty());
        assert!(g.delete_placement(9).is_empty());
        assert!(!g.release_image(1).is_empty());
        assert!(g.image_id(1).is_none());
        let _ = g.upload_png(2, b"png");
        assert!(!g.delete_all().is_empty());
        assert!(!g.upload_png(2, b"png").1.is_empty());
    }

    #[test]
    fn upload_does_not_create_an_implicit_placement() {
        let mut g = KittyGraphics::new();
        let (_, bytes) = g.upload_png(1, b"png");
        assert!(bytes.windows(3).any(|w| w == b"a=t"));
        assert!(!bytes.windows(3).any(|w| w == b"a=T"));
    }

    #[test]
    fn continuation_chunks_only_repeat_chunk_controls() {
        let mut g = KittyGraphics::new();
        let (_, bytes) = g.upload_png(1, &vec![0x5a; RAW_CHUNK_BYTES + 1]);
        let second = bytes
            .windows(3)
            .enumerate()
            .filter(|(_, w)| *w == b"\x1b_G")
            .nth(1)
            .map(|(i, _)| &bytes[i..])
            .unwrap();
        assert!(second.starts_with(b"\x1b_Gm=0,q=2;"));
        assert!(!second.starts_with(b"\x1b_Ga="));
    }

    #[test]
    fn base64_known_values() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }
}
