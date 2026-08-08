use std::{collections::HashMap, fmt, sync::Arc};

/// Stable identifier for an asset in a [`TextureStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureId(u64);

/// How RGB channels relate to the texture's alpha channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaMode {
    Straight,
    Premultiplied,
    Opaque,
}

/// Sampling used when a texture is scaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sampling {
    Nearest,
    Linear,
}

/// Pixel storage used by a texture asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    /// Four bytes per pixel in red, green, blue and alpha order.
    Rgba8,
    /// One byte per pixel containing coverage. The draw tint supplies color.
    Alpha8,
}

impl TextureFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8 => 4,
            Self::Alpha8 => 1,
        }
    }
}

/// One CPU-side RGBA8 texture revision.
#[derive(Debug, Clone)]
pub struct TextureAsset {
    width: u32,
    height: u32,
    pixels: Arc<[u8]>,
    format: TextureFormat,
    alpha_mode: AlphaMode,
    revision: u64,
}

impl TextureAsset {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn format(&self) -> TextureFormat {
        self.format
    }

    pub fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextureError {
    EmptySize,
    SizeOverflow,
    UnexpectedDataLength { expected: usize, actual: usize },
    UnknownTexture(TextureId),
}

impl fmt::Display for TextureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySize => write!(formatter, "texture dimensions must be non-zero"),
            Self::SizeOverflow => write!(formatter, "texture byte length overflows usize"),
            Self::UnexpectedDataLength { expected, actual } => write!(
                formatter,
                "texture needs {expected} RGBA bytes, received {actual}"
            ),
            Self::UnknownTexture(id) => write!(formatter, "unknown texture {id:?}"),
        }
    }
}

impl std::error::Error for TextureError {}

/// CPU-side texture assets shared by all GLES contexts.
#[derive(Debug, Default)]
pub struct TextureStore {
    next_id: u64,
    next_revision: u64,
    assets: HashMap<TextureId, TextureAsset>,
}

impl TextureStore {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            next_revision: 1,
            assets: HashMap::new(),
        }
    }

    pub fn insert(
        &mut self,
        width: u32,
        height: u32,
        pixels: impl Into<Arc<[u8]>>,
        alpha_mode: AlphaMode,
    ) -> Result<TextureId, TextureError> {
        self.insert_with_format(width, height, pixels, TextureFormat::Rgba8, alpha_mode)
    }

    /// Insert a single-channel coverage mask. Masks are tinted when recorded
    /// into a display list and do not need one texture revision per color.
    pub fn insert_mask(
        &mut self,
        width: u32,
        height: u32,
        pixels: impl Into<Arc<[u8]>>,
    ) -> Result<TextureId, TextureError> {
        self.insert_with_format(
            width,
            height,
            pixels,
            TextureFormat::Alpha8,
            AlphaMode::Straight,
        )
    }

    fn insert_with_format(
        &mut self,
        width: u32,
        height: u32,
        pixels: impl Into<Arc<[u8]>>,
        format: TextureFormat,
        alpha_mode: AlphaMode,
    ) -> Result<TextureId, TextureError> {
        let pixels = pixels.into();
        validate_data(width, height, pixels.len(), format)?;
        let id = TextureId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(TextureError::SizeOverflow)?;
        let revision = self.take_revision()?;
        self.assets.insert(
            id,
            TextureAsset {
                width,
                height,
                pixels,
                format,
                alpha_mode,
                revision,
            },
        );
        Ok(id)
    }

    pub fn update(
        &mut self,
        id: TextureId,
        width: u32,
        height: u32,
        pixels: impl Into<Arc<[u8]>>,
        alpha_mode: AlphaMode,
    ) -> Result<(), TextureError> {
        self.update_with_format(id, width, height, pixels, TextureFormat::Rgba8, alpha_mode)
    }

    pub fn update_mask(
        &mut self,
        id: TextureId,
        width: u32,
        height: u32,
        pixels: impl Into<Arc<[u8]>>,
    ) -> Result<(), TextureError> {
        self.update_with_format(
            id,
            width,
            height,
            pixels,
            TextureFormat::Alpha8,
            AlphaMode::Straight,
        )
    }

    fn update_with_format(
        &mut self,
        id: TextureId,
        width: u32,
        height: u32,
        pixels: impl Into<Arc<[u8]>>,
        format: TextureFormat,
        alpha_mode: AlphaMode,
    ) -> Result<(), TextureError> {
        if !self.assets.contains_key(&id) {
            return Err(TextureError::UnknownTexture(id));
        }
        let pixels = pixels.into();
        validate_data(width, height, pixels.len(), format)?;
        let revision = self.take_revision()?;
        self.assets.insert(
            id,
            TextureAsset {
                width,
                height,
                pixels,
                format,
                alpha_mode,
                revision,
            },
        );
        Ok(())
    }

    pub fn remove(&mut self, id: TextureId) -> Option<TextureAsset> {
        self.assets.remove(&id)
    }

    pub fn get(&self, id: TextureId) -> Option<&TextureAsset> {
        self.assets.get(&id)
    }

    pub fn contains(&self, id: TextureId) -> bool {
        self.assets.contains_key(&id)
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = TextureId> + '_ {
        self.assets.keys().copied()
    }

    fn take_revision(&mut self) -> Result<u64, TextureError> {
        let revision = self.next_revision;
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or(TextureError::SizeOverflow)?;
        Ok(revision)
    }
}

fn validate_data(
    width: u32,
    height: u32,
    actual: usize,
    format: TextureFormat,
) -> Result<(), TextureError> {
    if width == 0 || height == 0 {
        return Err(TextureError::EmptySize);
    }
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(format.bytes_per_pixel()))
        .ok_or(TextureError::SizeOverflow)?;
    if expected != actual {
        return Err(TextureError::UnexpectedDataLength { expected, actual });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_change_without_changing_the_stable_id() {
        let mut store = TextureStore::new();
        let id = store
            .insert(1, 1, vec![1, 2, 3, 4], AlphaMode::Straight)
            .unwrap();
        let first = store.get(id).unwrap().revision();
        store
            .update(id, 1, 1, vec![5, 6, 7, 8], AlphaMode::Premultiplied)
            .unwrap();
        assert!(store.get(id).unwrap().revision() > first);
        assert_eq!(store.get(id).unwrap().pixels(), &[5, 6, 7, 8]);
    }

    #[test]
    fn masks_use_one_byte_per_pixel() {
        let mut store = TextureStore::new();
        let id = store.insert_mask(2, 2, vec![0, 64, 128, 255]).unwrap();
        assert_eq!(store.get(id).unwrap().format(), TextureFormat::Alpha8);
        assert_eq!(
            store.insert_mask(2, 2, vec![0; 16]),
            Err(TextureError::UnexpectedDataLength {
                expected: 4,
                actual: 16,
            })
        );
    }
}
